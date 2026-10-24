//! Builds OpenRouter Chat Completions request bodies.
//!
//! Anthropic models on OpenRouter cache only with explicit `cache_control`
//! markers. For them ([`uses_cache_breakpoints`]) the body mirrors the
//! Anthropic provider's placement (`anthropic::request`): up to
//! [`MAX_CACHE_BREAKPOINTS`] breakpoints on the last tool, the system prompt
//! layers, the final message and the user turn before the last user turn,
//! so the next call reads everything this call wrote. The system prompt is
//! then a content array (one text part per layer), and user and tool text
//! is always sent as content parts, so a message's bytes do not change
//! when its marker moves on. Other models cache automatically and get the
//! plain body.

use serde_json::{Map, Value, json};

use crate::anthropic::request::system_layers;
use crate::model::{
    ChatRequest, ContentBlock, ImageSource, Message, ReasoningEffort, Role, ToolChoice,
    ToolResultPart,
};

use super::{CacheTtl, OpenRouterOptions, PROVIDER_NAME};

/// Anthropic allows at most this many `cache_control` breakpoints.
pub const MAX_CACHE_BREAKPOINTS: usize = crate::anthropic::request::MAX_CACHE_BREAKPOINTS;

/// Messages always keep at least this many breakpoints.
const MESSAGE_BREAKPOINTS_MIN: usize = 1;

/// True for models that need explicit cache breakpoints: Anthropic models
/// (`anthropic/...`, including `~anthropic/...` aliases).
pub fn uses_cache_breakpoints(model: &str) -> bool {
    model
        .strip_prefix('~')
        .unwrap_or(model)
        .starts_with("anthropic/")
}

/// The `cache_control` value for `ttl`.
pub fn cache_control(ttl: Option<CacheTtl>) -> Value {
    match ttl.unwrap_or_default() {
        CacheTtl::FiveMinutes => json!({"type": "ephemeral"}),
        CacheTtl::OneHour => json!({"type": "ephemeral", "ttl": "1h"}),
    }
}

/// Builds the JSON body for `POST /chat/completions`.
///
/// Always requests usage accounting (`usage.include`), so the final chunk
/// carries token counts and the call's cost. Routing preferences from
/// `options` go into the `provider` object. Anthropic models get prompt
/// cache breakpoints (see the module docs).
pub fn build_body(request: &ChatRequest, options: &OpenRouterOptions) -> Value {
    let cache = uses_cache_breakpoints(&request.model).then(|| cache_control(options.cache_ttl));
    let mut breakpoints = 0usize;
    let mut body = Map::new();
    body.insert("model".into(), json!(request.model));
    body.insert("stream".into(), json!(true));
    body.insert("usage".into(), json!({"include": true}));
    if let Some(id) = &options.session_id {
        body.insert("session_id".into(), json!(id));
    }
    if let Some(tier) = options.service_tier {
        body.insert("service_tier".into(), json!(tier.as_str()));
    }
    if !request.tools.is_empty() {
        let mut tools: Vec<Value> = request
            .tools
            .iter()
            .map(|t| {
                json!({"type": "function", "function": {
                    "name": t.name, "description": t.description, "parameters": t.input_schema}})
            })
            .collect();
        if let (Some(cc), Some(last)) = (&cache, tools.last_mut()) {
            last["cache_control"] = cc.clone();
            breakpoints += 1;
        }
        body.insert("tools".into(), Value::Array(tools));
        body.insert(
            "tool_choice".into(),
            match &request.tool_choice {
                ToolChoice::Auto => json!("auto"),
                ToolChoice::Any => json!("required"),
                ToolChoice::None => json!("none"),
                ToolChoice::Tool { name } => {
                    json!({"type": "function", "function": {"name": name}})
                }
            },
        );
    }
    body.insert(
        "messages".into(),
        Value::Array(messages(request, cache.as_ref(), breakpoints)),
    );
    if let Some(max) = request.max_tokens {
        body.insert("max_tokens".into(), json!(max));
    }
    if let Some(t) = request.temperature {
        body.insert("temperature".into(), json!(t));
    }
    if !request.stop_sequences.is_empty() {
        body.insert("stop".into(), json!(request.stop_sequences));
    }
    if let Some(thinking) = &request.thinking {
        let reasoning = match (thinking.effort, thinking.budget_tokens) {
            (Some(effort), _) => json!({"effort": match effort {
                ReasoningEffort::Low => "low",
                ReasoningEffort::Medium => "medium",
                ReasoningEffort::High => "high",
            }}),
            (None, Some(budget)) => json!({"max_tokens": budget}),
            (None, None) => json!({"enabled": true}),
        };
        let mut reasoning = reasoning;
        if options.reasoning_exclude {
            reasoning["exclude"] = json!(true);
        }
        body.insert("reasoning".into(), reasoning);
    }
    if let Some(provider) = routing(options) {
        body.insert("provider".into(), provider);
    }
    Value::Object(body)
}

/// The `provider` routing object, or `None` when no preference is set.
pub fn routing(options: &OpenRouterOptions) -> Option<Value> {
    let mut p = Map::new();
    if !options.provider_order.is_empty() {
        p.insert("order".into(), json!(options.provider_order));
    }
    if let Some(allow) = options.allow_fallbacks {
        p.insert("allow_fallbacks".into(), json!(allow));
    }
    (!p.is_empty()).then_some(Value::Object(p))
}

fn image_part(source: &ImageSource) -> Value {
    let url = match source {
        ImageSource::Base64 { media_type, data } => format!("data:{media_type};base64,{data}"),
        ImageSource::Url { url } => url.clone(),
    };
    json!({"type": "image_url", "image_url": {"url": url}})
}

fn text_part(text: &str) -> Value {
    json!({"type": "text", "text": text})
}

/// The chat messages. With `cache` set, the system prompt is split into
/// one part per layer and breakpoints are placed; `breakpoints` were
/// already used (on tools).
fn messages(request: &ChatRequest, cache: Option<&Value>, mut breakpoints: usize) -> Vec<Value> {
    let mut out = Vec::new();
    if let Some(system) = request.system.as_deref().filter(|s| !s.is_empty()) {
        match cache {
            None => out.push(json!({"role": "system", "content": system})),
            Some(cc) => {
                let mut parts: Vec<Value> = system_layers(system, &request.system_layers)
                    .into_iter()
                    .map(text_part)
                    .collect();
                // The end of the frozen prefix first, then the layer
                // boundaries from the most stable one, while the messages
                // keep their minimum (as in the Anthropic provider).
                if let Some(last) = parts.last_mut() {
                    last["cache_control"] = cc.clone();
                    breakpoints += 1;
                }
                let inner = parts.len().saturating_sub(1);
                for p in parts.iter_mut().take(inner) {
                    if breakpoints + MESSAGE_BREAKPOINTS_MIN >= MAX_CACHE_BREAKPOINTS {
                        break;
                    }
                    p["cache_control"] = cc.clone();
                    breakpoints += 1;
                }
                out.push(json!({"role": "system", "content": parts}));
            }
        }
    }
    // One group of chat messages per conversation message, so breakpoints
    // land on the same conversation turns as with the Anthropic provider.
    let mut groups: Vec<(Role, Vec<Value>)> = request
        .messages
        .iter()
        .map(|m| {
            let mut group = Vec::new();
            match m.role {
                Role::User => user(m, cache.is_some(), &mut group),
                Role::Assistant => group.push(assistant(m)),
            }
            (m.role, group)
        })
        .filter(|(_, g)| !g.is_empty())
        .collect();
    if let Some(cc) = cache {
        let budget = MAX_CACHE_BREAKPOINTS.saturating_sub(breakpoints);
        for idx in breakpoint_targets(&groups).into_iter().take(budget) {
            mark_group(&mut groups[idx].1, cc);
        }
    }
    out.extend(groups.into_iter().flat_map(|(_, g)| g));
    out
}

/// The final message and the user message before the last user message.
fn breakpoint_targets(groups: &[(Role, Vec<Value>)]) -> Vec<usize> {
    let mut targets = Vec::new();
    if let Some(last) = groups.len().checked_sub(1) {
        targets.push(last);
        let last_user = groups.iter().rposition(|(r, _)| *r == Role::User);
        let before =
            last_user.and_then(|u| groups[..u].iter().rposition(|(r, _)| *r == Role::User));
        targets.extend(before);
    }
    targets
}

/// Marks the last non-empty text of the group's last message that has one.
/// String content (assistant text) becomes a single text part.
fn mark_group(group: &mut [Value], cc: &Value) {
    for msg in group.iter_mut().rev() {
        match &mut msg["content"] {
            Value::String(text) if !text.is_empty() => {
                let mut part = text_part(text);
                part["cache_control"] = cc.clone();
                msg["content"] = json!([part]);
                return;
            }
            Value::Array(parts) => {
                let target = parts.iter_mut().rev().find(|p| {
                    p["type"] == "text"
                        && p["text"].as_str().is_some_and(|t| !t.is_empty())
                        && p.get("cache_control").is_none()
                });
                if let Some(p) = target {
                    p["cache_control"] = cc.clone();
                    return;
                }
            }
            _ => {}
        }
    }
}

/// Tool results become `tool` messages (which must directly follow the
/// assistant's tool calls); remaining text and images, including images
/// from tool results, follow in one user message. With `parts` set (cache
/// mode), non-empty text is always sent as content parts.
fn user(m: &Message, parts_mode: bool, out: &mut Vec<Value>) {
    let mut parts = Vec::new();
    for block in &m.content {
        match block {
            ContentBlock::Text { text } if parts_mode && text.is_empty() => {}
            ContentBlock::Text { text } => parts.push(text_part(text)),
            ContentBlock::Image { source } => parts.push(image_part(source)),
            ContentBlock::ToolResult {
                tool_use_id,
                content,
                is_error,
            } => {
                let mut text: Vec<&str> = Vec::new();
                for p in content {
                    match p {
                        ToolResultPart::Text { text: t } => text.push(t),
                        ToolResultPart::Image { source } => parts.push(image_part(source)),
                    }
                }
                let prefix = if *is_error { "Error: " } else { "" };
                let text = format!("{prefix}{}", text.join("\n"));
                let content = if parts_mode && !text.is_empty() {
                    json!([text_part(&text)])
                } else {
                    json!(text)
                };
                out.push(json!({"role": "tool", "tool_call_id": tool_use_id, "content": content}));
            }
            _ => {}
        }
    }
    if parts.is_empty() {
        return;
    }
    let content = if !parts_mode && parts.iter().all(|p| p["type"] == "text") {
        json!(
            parts
                .iter()
                .filter_map(|p| p["text"].as_str())
                .collect::<Vec<_>>()
                .join("\n")
        )
    } else {
        Value::Array(parts)
    };
    out.push(json!({"role": "user", "content": content}));
}

fn assistant(m: &Message) -> Value {
    let mut msg = Map::new();
    msg.insert("role".into(), json!("assistant"));
    let text = m.text();
    msg.insert(
        "content".into(),
        if text.is_empty() {
            Value::Null
        } else {
            json!(text)
        },
    );
    let calls: Vec<Value> = m
        .tool_uses()
        .map(|(id, name, input)| {
            let arguments = match input {
                Value::String(raw) => raw.clone(),
                other => other.to_string(),
            };
            json!({"id": id, "type": "function", "function": {"name": name, "arguments": arguments}})
        })
        .collect();
    if !calls.is_empty() {
        msg.insert("tool_calls".into(), Value::Array(calls));
    }
    let details: Vec<Value> = m
        .content
        .iter()
        .filter_map(|b| match b {
            ContentBlock::Opaque { provider, data } if provider == PROVIDER_NAME => {
                data.get("reasoning_details").and_then(Value::as_array)
            }
            _ => None,
        })
        .flatten()
        .cloned()
        .collect();
    if !details.is_empty() {
        msg.insert("reasoning_details".into(), Value::Array(details));
    }
    Value::Object(msg)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ThinkingConfig, ToolSpec};
    use crate::openrouter::ServiceTier;

    fn opts() -> OpenRouterOptions {
        OpenRouterOptions {
            app_name: Some("harness".into()),
            provider_order: vec!["anthropic".into(), "openai".into()],
            allow_fallbacks: Some(false),
            cache_ttl: None,
            session_id: None,
            reasoning_exclude: false,
            service_tier: None,
        }
    }

    #[test]
    fn body_has_routing_usage_tools_and_reasoning() {
        let mut r = ChatRequest::new("anthropic/claude-sonnet-4");
        r.system = Some("sys".into());
        r.tools = vec![ToolSpec {
            name: "read".into(),
            description: "d".into(),
            input_schema: json!({"type": "object"}),
        }];
        r.thinking = Some(ThinkingConfig {
            budget_tokens: Some(2048),
            effort: None,
        });
        r.stop_sequences = vec!["END".into()];
        let b = build_body(&r, &opts());
        assert_eq!(
            b["provider"],
            json!({"order": ["anthropic", "openai"], "allow_fallbacks": false})
        );
        assert_eq!(b["usage"], json!({"include": true}));
        assert_eq!(b["reasoning"], json!({"max_tokens": 2048}));
        assert_eq!(b["stop"], json!(["END"]));
        assert_eq!(b["tool_choice"], json!("auto"));
        assert_eq!(b["tools"][0]["function"]["name"], json!("read"));
        // An Anthropic model: the system prompt is a content array with a
        // breakpoint (see the cache tests below).
        assert_eq!(
            b["messages"][0],
            json!({"role": "system", "content": [
                {"type": "text", "text": "sys", "cache_control": {"type": "ephemeral"}}]})
        );
        r.model = "openai/gpt-5.1".into();
        let b = build_body(&r, &opts());
        assert_eq!(
            b["messages"][0],
            json!({"role": "system", "content": "sys"})
        );
    }

    #[test]
    fn session_id_is_sent_only_when_set() {
        let r = ChatRequest::new("anthropic/claude-opus-5.5");
        assert!(
            build_body(&r, &OpenRouterOptions::default())
                .get("session_id")
                .is_none()
        );
        let sticky = OpenRouterOptions {
            session_id: Some("harness-abc".into()),
            ..Default::default()
        };
        assert_eq!(build_body(&r, &sticky)["session_id"], json!("harness-abc"));
    }

    #[test]
    fn service_tier_is_sent_only_when_set() {
        let r = ChatRequest::new("anthropic/claude-opus-5.5");
        assert!(
            build_body(&r, &OpenRouterOptions::default())
                .get("service_tier")
                .is_none()
        );
        for (tier, want) in [
            (ServiceTier::Fast, "fast"),
            (ServiceTier::Default, "default"),
        ] {
            let o = OpenRouterOptions {
                service_tier: Some(tier),
                ..Default::default()
            };
            assert_eq!(build_body(&r, &o)["service_tier"], json!(want));
        }
    }

    #[test]
    fn reasoning_exclusion_is_opt_in_and_keeps_the_effort() {
        let mut r = ChatRequest::new("anthropic/claude-opus-5.5");
        r.thinking = Some(ThinkingConfig {
            effort: Some(ReasoningEffort::High),
            budget_tokens: None,
        });
        assert_eq!(
            build_body(&r, &OpenRouterOptions::default())["reasoning"],
            json!({"effort": "high"})
        );
        let exclude = OpenRouterOptions {
            reasoning_exclude: true,
            ..Default::default()
        };
        assert_eq!(
            build_body(&r, &exclude)["reasoning"],
            json!({"effort": "high", "exclude": true})
        );
        r.thinking = None;
        assert!(build_body(&r, &exclude).get("reasoning").is_none());
    }

    #[test]
    fn routing_is_omitted_without_preferences() {
        assert_eq!(routing(&OpenRouterOptions::default()), None);
        let only = OpenRouterOptions {
            allow_fallbacks: Some(true),
            ..Default::default()
        };
        assert_eq!(routing(&only), Some(json!({"allow_fallbacks": true})));
        let mut r = ChatRequest::new("m");
        r.thinking = Some(ThinkingConfig {
            effort: Some(ReasoningEffort::Medium),
            budget_tokens: None,
        });
        let b = build_body(&r, &OpenRouterOptions::default());
        assert!(b.get("provider").is_none());
        assert_eq!(b["reasoning"], json!({"effort": "medium"}));
    }

    #[test]
    fn conversation_conversion() {
        let mut r = ChatRequest::new("m");
        r.messages = vec![
            Message::user("hi"),
            Message {
                role: Role::Assistant,
                content: vec![
                    ContentBlock::Reasoning {
                        text: "t".into(),
                        signature: None,
                    },
                    ContentBlock::Opaque {
                        provider: "openrouter".into(),
                        data: json!({"reasoning_details": [{"type": "reasoning.text", "text": "t", "signature": "s"}]}),
                    },
                    ContentBlock::Opaque {
                        provider: "harness".into(),
                        data: json!({"type": "cost", "provider": "openrouter", "usd": 0.1}),
                    },
                    ContentBlock::tool_use("c1", "read", json!({"path": "a"})),
                ],
            },
            Message {
                role: Role::User,
                content: vec![ContentBlock::ToolResult {
                    tool_use_id: "c1".into(),
                    content: vec![
                        ToolResultPart::Text { text: "out".into() },
                        ToolResultPart::Image {
                            source: ImageSource::Url { url: "u".into() },
                        },
                    ],
                    is_error: false,
                }],
            },
        ];
        let b = build_body(&r, &OpenRouterOptions::default());
        assert_eq!(
            b["messages"],
            json!([
                {"role": "user", "content": "hi"},
                {"role": "assistant", "content": null,
                 "tool_calls": [{"id": "c1", "type": "function", "function": {"name": "read", "arguments": "{\"path\":\"a\"}"}}],
                 "reasoning_details": [{"type": "reasoning.text", "text": "t", "signature": "s"}]},
                {"role": "tool", "tool_call_id": "c1", "content": "out"},
                {"role": "user", "content": [{"type": "image_url", "image_url": {"url": "u"}}]},
            ])
        );
    }

    fn tool(name: &str) -> ToolSpec {
        ToolSpec {
            name: name.into(),
            description: format!("{name} tool"),
            input_schema: json!({"type": "object"}),
        }
    }

    /// Where the body carries `cache_control`, as `tools[i]`,
    /// `messages[m]` (string content) or `messages[m][p]` (part).
    fn breakpoints(v: &Value) -> Vec<String> {
        let mut out = Vec::new();
        for (i, t) in v["tools"].as_array().into_iter().flatten().enumerate() {
            if t.get("cache_control").is_some() {
                out.push(format!("tools[{i}]"));
            }
        }
        for (m, msg) in v["messages"].as_array().unwrap().iter().enumerate() {
            for (p, part) in msg["content"].as_array().into_iter().flatten().enumerate() {
                if part.get("cache_control").is_some() {
                    out.push(format!("messages[{m}][{p}]"));
                }
            }
        }
        out
    }

    /// The Anthropic provider's `tool_loop` conversation, on OpenRouter.
    fn tool_loop(model: &str) -> ChatRequest {
        let mut req = ChatRequest::new(model);
        req.system = Some("You are terse.".into());
        req.tools = vec![tool("read"), tool("bash")];
        req.messages = vec![
            Message::user("fix it"),
            Message {
                role: Role::Assistant,
                content: vec![
                    ContentBlock::text("Reading."),
                    ContentBlock::tool_use("t1", "read", json!({"path": "a"})),
                    ContentBlock::tool_use("t2", "read", json!({"path": "b"})),
                ],
            },
            Message {
                role: Role::User,
                content: vec![
                    ContentBlock::tool_result("t1", "A", false),
                    ContentBlock::tool_result("t2", "B", true),
                ],
            },
        ];
        req
    }

    #[test]
    fn anthropic_models_get_breakpoints_like_the_anthropic_provider() {
        let mut req = tool_loop("anthropic/claude-sonnet-5.5");
        req.messages.push(Message::assistant("done"));
        req.messages.push(Message::user("next"));
        let body = build_body(&req, &OpenRouterOptions::default());
        // Messages: system, user, assistant, tool t1, tool t2, assistant,
        // user. Same turns as the Anthropic provider: last tool, system,
        // previous user turn (its last tool result), final message.
        assert_eq!(
            breakpoints(&body),
            [
                "tools[1]",
                "messages[0][0]",
                "messages[4][0]",
                "messages[6][0]"
            ]
        );
        assert_eq!(body["messages"][4]["role"], "tool");
        assert_eq!(
            body["messages"][4]["content"],
            json!([{"type": "text", "text": "Error: B", "cache_control": {"type": "ephemeral"}}])
        );
        // Unmarked user and tool text is still sent as parts.
        assert_eq!(
            body["messages"][1]["content"],
            json!([{"type": "text", "text": "fix it"}])
        );
        assert_eq!(
            body["messages"][3]["content"],
            json!([{"type": "text", "text": "A"}])
        );
        // Assistant text stays a string unless it carries the marker.
        assert_eq!(body["messages"][2]["content"], "Reading.");
    }

    #[test]
    fn other_models_are_unchanged() {
        for model in ["openai/gpt-5.1", "google/gemini-3-pro", "m"] {
            let body = build_body(&tool_loop(model), &OpenRouterOptions::default());
            assert!(breakpoints(&body).is_empty(), "{model}");
            assert!(!body.to_string().contains("cache_control"), "{model}");
            assert_eq!(body["messages"][0]["content"], "You are terse.");
            assert_eq!(body["messages"][1]["content"], "fix it");
            assert_eq!(body["messages"][3]["content"], "A");
        }
        assert!(uses_cache_breakpoints("anthropic/claude-haiku-5.5"));
        assert!(uses_cache_breakpoints("~anthropic/claude-sonnet-latest"));
        assert!(!uses_cache_breakpoints("openai/anthropic-like"));
    }

    #[test]
    fn system_layers_and_the_four_breakpoint_cap() {
        let base = "You are terse.\n\n";
        let memory = "## Memory\n- prefer nextest\n\n";
        let skills = "## Skills\n- /deploy";
        let mut req = tool_loop("anthropic/claude-opus-5.5");
        req.system = Some(format!("{base}{memory}{skills}"));
        req.system_layers = vec![base.len(), base.len() + memory.len()];
        let body = build_body(&req, &OpenRouterOptions::default());
        let texts: Vec<&str> = body["messages"][0]["content"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| p["text"].as_str().unwrap())
            .collect();
        assert_eq!(texts, [base, memory, skills], "one part per layer");
        // Last tool, end of the frozen prefix, end of the base prompt and
        // the final message: the limit.
        assert_eq!(
            breakpoints(&body),
            [
                "tools[1]",
                "messages[0][0]",
                "messages[0][2]",
                "messages[4][0]"
            ]
        );
        // Without tools there is room for every layer boundary, and the
        // messages keep at least one.
        req.tools.clear();
        let bp = breakpoints(&build_body(&req, &OpenRouterOptions::default()));
        assert_eq!(bp.len(), MAX_CACHE_BREAKPOINTS, "{bp:?}");
        for b in ["messages[0][0]", "messages[0][1]", "messages[0][2]"] {
            assert!(bp.contains(&b.to_owned()), "{bp:?}");
        }
        assert!(bp.iter().any(|b| b.starts_with("messages[4]")), "{bp:?}");
        // Many layers never exceed the cap.
        req.system = Some("abcdefgh".into());
        req.system_layers = vec![1, 2, 3, 4, 5, 6, 7];
        let bp = breakpoints(&build_body(&req, &OpenRouterOptions::default()));
        assert_eq!(bp.len(), MAX_CACHE_BREAKPOINTS, "{bp:?}");
    }

    #[test]
    fn ttl_and_unmarkable_messages() {
        let one_hour = OpenRouterOptions {
            cache_ttl: Some(CacheTtl::OneHour),
            ..Default::default()
        };
        let mut req = ChatRequest::new("anthropic/claude-haiku-5.5");
        req.messages = vec![
            Message::user("q"),
            // A final assistant message with only text: marked as a part.
            Message::assistant("a"),
        ];
        let body = build_body(&req, &one_hour);
        assert_eq!(
            body["messages"][1]["content"],
            json!([{"type": "text", "text": "a", "cache_control": {"type": "ephemeral", "ttl": "1h"}}])
        );
        assert_eq!(breakpoints(&body), ["messages[1][0]"]);
        // Empty tool output stays an unmarked string; the marker moves to
        // the last message that has text.
        req.messages = vec![
            Message::user("q"),
            Message {
                role: Role::Assistant,
                content: vec![ContentBlock::tool_use("c1", "bash", json!({}))],
            },
            Message {
                role: Role::User,
                content: vec![
                    ContentBlock::tool_result("c1", "", false),
                    ContentBlock::text(""),
                ],
            },
        ];
        let body = build_body(&req, &OpenRouterOptions::default());
        assert_eq!(body["messages"][2]["content"], "");
        assert_eq!(body["messages"].as_array().unwrap().len(), 3);
        assert_eq!(breakpoints(&body), ["messages[0][0]"]);
    }
}
