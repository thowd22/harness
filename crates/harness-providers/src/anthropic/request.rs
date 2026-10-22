//! Builds Anthropic Messages API request bodies from a [`ChatRequest`].
//!
//! The body follows the stable-prefix layout `tools -> system -> messages`.
//! With [`CacheMode::Auto`], [`build_body`] places up to
//! [`MAX_CACHE_BREAKPOINTS`] `cache_control` breakpoints on the last tool,
//! the system prompt, the last block of the final message and the last block
//! of the user turn before it, so the next call reads everything this call
//! wrote.
//!
//! Thinking blocks are sent back exactly as received: a
//! [`ContentBlock::Reasoning`] with a signature becomes a `thinking` block
//! with the same text and signature, and an `anthropic`
//! [`ContentBlock::Opaque`] (for example `redacted_thinking`) is sent
//! byte-for-byte. Unsigned reasoning and other providers' opaque blocks
//! cannot be verified by the API and are left out.

use serde_json::{Map, Value, json};

use crate::model::{
    Capabilities, ChatRequest, ContentBlock, ImageSource, Message, ReasoningEffort, Role,
    ToolChoice, ToolResultPart,
};
use crate::provider::ProviderError;
use crate::registry::ThinkingStyle;

/// The Anthropic provider name used in opaque blocks.
pub const PROVIDER: &str = "anthropic";

/// The API allows at most this many `cache_control` breakpoints.
pub const MAX_CACHE_BREAKPOINTS: usize = 4;

/// Smallest thinking budget the API accepts.
pub const MIN_THINKING_BUDGET: u32 = 1024;

/// `max_tokens` used when the request leaves it unset (capped by the
/// model's output limit).
pub const DEFAULT_MAX_TOKENS: u32 = 32_000;

/// Whether the builder places prompt-cache breakpoints.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CacheMode {
    /// Place breakpoints from the stable-prefix layout (default).
    #[default]
    Auto,
    /// Send no `cache_control` markers.
    Off,
}

/// What the builder needs to know about the target model.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ModelProfile {
    /// Model capabilities; `caching` gates breakpoints and
    /// `max_output_tokens` caps `max_tokens` when non-zero.
    pub capabilities: Capabilities,
    /// How thinking is requested.
    pub thinking: ThinkingStyle,
}

/// Request builder options.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RequestOptions {
    /// Cache breakpoint placement.
    pub cache: CacheMode,
    /// `max_tokens` when the request does not set one.
    pub default_max_tokens: u32,
}

impl Default for RequestOptions {
    fn default() -> Self {
        Self {
            cache: CacheMode::Auto,
            default_max_tokens: DEFAULT_MAX_TOKENS,
        }
    }
}

/// Thinking budget used for an effort level on budget-style models.
pub fn effort_budget(effort: ReasoningEffort) -> u32 {
    match effort {
        ReasoningEffort::Low => 2_048,
        ReasoningEffort::Medium => 8_192,
        ReasoningEffort::High => 24_576,
    }
}

/// Effort level used for a token budget on adaptive models.
pub fn budget_effort(budget: u32) -> ReasoningEffort {
    match budget {
        0..=4_096 => ReasoningEffort::Low,
        4_097..=16_384 => ReasoningEffort::Medium,
        _ => ReasoningEffort::High,
    }
}

fn effort_str(effort: ReasoningEffort) -> &'static str {
    match effort {
        ReasoningEffort::Low => "low",
        ReasoningEffort::Medium => "medium",
        ReasoningEffort::High => "high",
    }
}

/// Builds the streaming request body for `request`.
///
/// `temperature` is not sent: API version 2026-10-21 rejects sampling
/// parameters on every model.
pub fn build_body(
    request: &ChatRequest,
    profile: &ModelProfile,
    options: &RequestOptions,
) -> Result<Value, ProviderError> {
    if request.model.trim().is_empty() {
        return Err(ProviderError::InvalidRequest("model id is empty".into()));
    }
    let caps = profile.capabilities;
    let cap = |n: u32| {
        if caps.max_output_tokens > 0 {
            n.min(caps.max_output_tokens)
        } else {
            n
        }
    };
    let mut max_tokens = cap(request.max_tokens.unwrap_or(options.default_max_tokens)).max(1);

    let mut body = Map::new();
    body.insert("model".into(), json!(request.model));
    body.insert("stream".into(), json!(true));

    if let Some(thinking) = &request.thinking {
        match profile.thinking {
            ThinkingStyle::Adaptive => {
                body.insert(
                    "thinking".into(),
                    json!({"type": "adaptive", "display": "summarized"}),
                );
                if let Some(effort) = thinking
                    .effort
                    .or(thinking.budget_tokens.map(budget_effort))
                {
                    body.insert(
                        "output_config".into(),
                        json!({"effort": effort_str(effort)}),
                    );
                }
            }
            ThinkingStyle::Budget => {
                let mut budget = thinking
                    .budget_tokens
                    .unwrap_or_else(|| {
                        effort_budget(thinking.effort.unwrap_or(ReasoningEffort::Medium))
                    })
                    .max(MIN_THINKING_BUDGET);
                if max_tokens <= budget {
                    max_tokens = cap(budget.saturating_add(options.default_max_tokens.min(8_192)));
                }
                if max_tokens <= budget {
                    budget = max_tokens.saturating_sub(1);
                }
                if budget >= MIN_THINKING_BUDGET {
                    body.insert(
                        "thinking".into(),
                        json!({"type": "enabled", "budget_tokens": budget}),
                    );
                } else {
                    tracing::warn!(
                        max_tokens,
                        "max_tokens too small for thinking; sending none"
                    );
                }
            }
            ThinkingStyle::None | ThinkingStyle::Effort => {}
        }
    }
    body.insert("max_tokens".into(), json!(max_tokens));
    if request.temperature.is_some() {
        tracing::debug!("anthropic: dropping temperature (not accepted by API 2026-10-21)");
    }
    if !request.stop_sequences.is_empty() {
        body.insert("stop_sequences".into(), json!(request.stop_sequences));
    }

    let cache = options.cache == CacheMode::Auto && caps.caching;
    let mut breakpoints = 0usize;

    if !request.tools.is_empty() {
        let mut tools: Vec<Value> = request
            .tools
            .iter()
            .map(|t| json!({"name": t.name, "description": t.description, "input_schema": t.input_schema}))
            .collect();
        if cache && let Some(last) = tools.last_mut() {
            mark(last);
            breakpoints += 1;
        }
        body.insert("tools".into(), Value::Array(tools));
        match &request.tool_choice {
            ToolChoice::Auto => {}
            ToolChoice::Any => {
                body.insert("tool_choice".into(), json!({"type": "any"}));
            }
            ToolChoice::None => {
                body.insert("tool_choice".into(), json!({"type": "none"}));
            }
            ToolChoice::Tool { name } => {
                body.insert("tool_choice".into(), json!({"type": "tool", "name": name}));
            }
        }
    }

    // Messages always keep at least this many breakpoints.
    const MESSAGE_BREAKPOINTS_MIN: usize = 1;
    if let Some(system) = request.system.as_deref().filter(|s| !s.is_empty()) {
        let mut blocks: Vec<Value> = system_layers(system, &request.system_layers)
            .into_iter()
            .map(|text| json!({"type": "text", "text": text}))
            .collect();
        if cache {
            // The end of the frozen prefix first, then the layer boundaries
            // from the most stable one (end of the base prompt), while the
            // messages keep their minimum.
            if let Some(last) = blocks.last_mut() {
                mark(last);
                breakpoints += 1;
            }
            let inner = blocks.len().saturating_sub(1);
            for b in blocks.iter_mut().take(inner) {
                if breakpoints + MESSAGE_BREAKPOINTS_MIN >= MAX_CACHE_BREAKPOINTS {
                    break;
                }
                mark(b);
                breakpoints += 1;
            }
        }
        body.insert("system".into(), Value::Array(blocks));
    }

    let mut messages: Vec<(Role, Vec<Value>)> = request
        .messages
        .iter()
        .map(|m| (m.role, convert_message(m)))
        .filter(|(_, blocks)| !blocks.is_empty())
        .collect();
    if cache {
        place_message_breakpoints(&mut messages, MAX_CACHE_BREAKPOINTS - breakpoints);
    }
    let messages: Vec<Value> = messages
        .into_iter()
        .map(|(role, content)| json!({"role": role_str(role), "content": content}))
        .collect();
    body.insert("messages".into(), Value::Array(messages));
    Ok(Value::Object(body))
}

/// `system` split at `layers` (byte offsets of later layers; invalid or
/// out-of-order offsets are ignored). Empty pieces are dropped.
pub fn system_layers<'a>(system: &'a str, layers: &[usize]) -> Vec<&'a str> {
    let mut out = Vec::new();
    let mut start = 0;
    for &at in layers {
        if at <= start || at >= system.len() || !system.is_char_boundary(at) {
            continue;
        }
        out.push(&system[start..at]);
        start = at;
    }
    out.push(&system[start..]);
    out.retain(|p| !p.is_empty());
    out
}

fn role_str(role: Role) -> &'static str {
    match role {
        Role::User => "user",
        Role::Assistant => "assistant",
    }
}

fn mark(block: &mut Value) {
    if let Some(obj) = block.as_object_mut() {
        obj.insert("cache_control".into(), json!({"type": "ephemeral"}));
    }
}

/// Blocks that may carry `cache_control`. Thinking and opaque blocks must
/// be sent unchanged and cannot.
fn cacheable(block: &Value) -> bool {
    matches!(
        block.get("type").and_then(Value::as_str),
        Some("text" | "image" | "tool_use" | "tool_result")
    ) && block.get("cache_control").is_none()
}

/// Marks the last cacheable block of the final message and of the user
/// message before the final user message.
fn place_message_breakpoints(messages: &mut [(Role, Vec<Value>)], budget: usize) {
    let mut targets = Vec::new();
    if let Some(last) = messages.len().checked_sub(1) {
        targets.push(last);
        let last_user = messages.iter().rposition(|(r, _)| *r == Role::User);
        let before =
            last_user.and_then(|u| messages[..u].iter().rposition(|(r, _)| *r == Role::User));
        if let Some(prev) = before {
            targets.push(prev);
        }
    }
    for idx in targets.into_iter().take(budget) {
        if let Some(block) = messages[idx].1.iter_mut().rev().find(|b| cacheable(b)) {
            mark(block);
        }
    }
}

fn image_json(source: &ImageSource) -> Value {
    match source {
        ImageSource::Base64 { media_type, data } => json!({
            "type": "image",
            "source": {"type": "base64", "media_type": media_type, "data": data}
        }),
        ImageSource::Url { url } => json!({
            "type": "image",
            "source": {"type": "url", "url": url}
        }),
    }
}

/// Converts one message's blocks, dropping blocks the API cannot accept.
pub fn convert_message(message: &Message) -> Vec<Value> {
    message.content.iter().filter_map(convert_block).collect()
}

fn convert_block(block: &ContentBlock) -> Option<Value> {
    Some(match block {
        ContentBlock::Text { text } if text.is_empty() => return None,
        ContentBlock::Text { text } => json!({"type": "text", "text": text}),
        ContentBlock::Image { source } => image_json(source),
        ContentBlock::ToolUse { id, name, input } => {
            let input = if input.is_object() {
                input.clone()
            } else {
                tracing::warn!(tool = %name, "tool_use input is not a JSON object; sending {{}}");
                json!({})
            };
            json!({"type": "tool_use", "id": id, "name": name, "input": input})
        }
        ContentBlock::ToolResult {
            tool_use_id,
            content,
            is_error,
        } => {
            let parts: Vec<Value> = content
                .iter()
                .filter_map(|p| match p {
                    ToolResultPart::Text { text } if text.is_empty() => None,
                    ToolResultPart::Text { text } => Some(json!({"type": "text", "text": text})),
                    ToolResultPart::Image { source } => Some(image_json(source)),
                })
                .collect();
            let mut v = json!({"type": "tool_result", "tool_use_id": tool_use_id});
            if !parts.is_empty() {
                v["content"] = Value::Array(parts);
            }
            if *is_error {
                v["is_error"] = json!(true);
            }
            v
        }
        ContentBlock::Reasoning {
            text,
            signature: Some(signature),
        } => json!({"type": "thinking", "thinking": text, "signature": signature}),
        ContentBlock::Reasoning {
            signature: None, ..
        } => return None,
        ContentBlock::Opaque { provider, data } if provider == PROVIDER => data.clone(),
        ContentBlock::Opaque { .. } => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ThinkingConfig, ToolSpec};

    fn profile(thinking: ThinkingStyle) -> ModelProfile {
        ModelProfile {
            capabilities: Capabilities {
                tools: true,
                parallel_tool_calls: true,
                thinking: true,
                caching: true,
                images: true,
                context_window: 1_000_000,
                max_output_tokens: 128_000,
            },
            thinking,
        }
    }

    fn tool(name: &str) -> ToolSpec {
        ToolSpec {
            name: name.into(),
            description: format!("{name} tool"),
            input_schema: json!({"type": "object"}),
        }
    }

    fn breakpoints(v: &Value) -> Vec<String> {
        let mut out = Vec::new();
        for (i, t) in v["tools"].as_array().into_iter().flatten().enumerate() {
            if t.get("cache_control").is_some() {
                out.push(format!("tools[{i}]"));
            }
        }
        for (i, s) in v["system"].as_array().into_iter().flatten().enumerate() {
            if s.get("cache_control").is_some() {
                out.push(format!("system[{i}]"));
            }
        }
        for (m, msg) in v["messages"].as_array().unwrap().iter().enumerate() {
            for (b, block) in msg["content"].as_array().unwrap().iter().enumerate() {
                if block.get("cache_control").is_some() {
                    out.push(format!("messages[{m}][{b}]"));
                }
            }
        }
        out
    }

    fn tool_loop() -> ChatRequest {
        let mut req = ChatRequest::new("claude-opus-5-5");
        req.system = Some("You are terse.".into());
        req.tools = vec![tool("read"), tool("bash")];
        req.messages = vec![
            Message::user("fix it"),
            Message {
                role: Role::Assistant,
                content: vec![
                    ContentBlock::Reasoning {
                        text: "look first".into(),
                        signature: Some("sig-1".into()),
                    },
                    ContentBlock::Opaque {
                        provider: "anthropic".into(),
                        data: json!({"type": "redacted_thinking", "data": "EuYB"}),
                    },
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
    fn thinking_blocks_and_signatures_round_trip_exactly() {
        let body = build_body(
            &tool_loop(),
            &profile(ThinkingStyle::Adaptive),
            &RequestOptions::default(),
        )
        .unwrap();
        let assistant = &body["messages"][1]["content"];
        assert_eq!(
            assistant[0],
            json!({"type": "thinking", "thinking": "look first", "signature": "sig-1"})
        );
        assert_eq!(
            assistant[1],
            json!({"type": "redacted_thinking", "data": "EuYB"})
        );
        assert_eq!(assistant[2], json!({"type": "text", "text": "Reading."}));
        assert_eq!(assistant[3]["type"], "tool_use");
        assert_eq!(assistant[4]["id"], "t2");
        let results = &body["messages"][2]["content"];
        assert_eq!(results[0]["tool_use_id"], "t1");
        assert_eq!(results[1]["is_error"], true);
        assert!(results[0].get("is_error").is_none());
    }

    #[test]
    fn unsigned_reasoning_and_foreign_opaque_blocks_are_dropped() {
        let msg = Message {
            role: Role::Assistant,
            content: vec![
                ContentBlock::Reasoning {
                    text: "x".into(),
                    signature: None,
                },
                ContentBlock::Opaque {
                    provider: "openai".into(),
                    data: json!({"type": "reasoning"}),
                },
                ContentBlock::text(""),
                ContentBlock::text("kept"),
            ],
        };
        assert_eq!(
            convert_message(&msg),
            vec![json!({"type": "text", "text": "kept"})]
        );
    }

    #[test]
    fn cache_breakpoints_follow_the_stable_prefix_layout() {
        let mut req = tool_loop();
        req.messages.push(Message::assistant("done"));
        req.messages.push(Message::user("next"));
        let body = build_body(
            &req,
            &profile(ThinkingStyle::Adaptive),
            &RequestOptions::default(),
        )
        .unwrap();
        // last tool, system, previous user turn (tool results), final message.
        assert_eq!(
            breakpoints(&body),
            vec!["tools[1]", "system[0]", "messages[2][1]", "messages[4][0]"]
        );
        assert!(breakpoints(&body).len() <= MAX_CACHE_BREAKPOINTS);
    }

    #[test]
    fn breakpoints_skip_thinking_blocks_and_respect_off_and_capabilities() {
        let mut req = ChatRequest::new("m");
        req.messages = vec![
            Message::user("q"),
            Message {
                role: Role::Assistant,
                content: vec![
                    ContentBlock::text("a"),
                    ContentBlock::Reasoning {
                        text: "t".into(),
                        signature: Some("s".into()),
                    },
                ],
            },
        ];
        let body = build_body(
            &req,
            &profile(ThinkingStyle::Adaptive),
            &RequestOptions::default(),
        )
        .unwrap();
        assert_eq!(breakpoints(&body), vec!["messages[1][0]"]);

        let off = RequestOptions {
            cache: CacheMode::Off,
            ..Default::default()
        };
        assert!(
            breakpoints(
                &build_body(&tool_loop(), &profile(ThinkingStyle::Adaptive), &off).unwrap()
            )
            .is_empty()
        );
        let mut no_cache = profile(ThinkingStyle::Adaptive);
        no_cache.capabilities.caching = false;
        assert!(
            breakpoints(&build_body(&tool_loop(), &no_cache, &RequestOptions::default()).unwrap())
                .is_empty()
        );
    }

    #[test]
    fn effort_maps_to_adaptive_thinking_and_output_config() {
        let mut req = ChatRequest::new("claude-opus-5-5");
        req.messages.push(Message::user("hi"));
        req.temperature = Some(0.2);
        req.thinking = Some(ThinkingConfig {
            budget_tokens: None,
            effort: Some(ReasoningEffort::High),
        });
        let body = build_body(
            &req,
            &profile(ThinkingStyle::Adaptive),
            &RequestOptions::default(),
        )
        .unwrap();
        assert_eq!(
            body["thinking"],
            json!({"type": "adaptive", "display": "summarized"})
        );
        assert_eq!(body["output_config"], json!({"effort": "high"}));
        assert!(body.get("temperature").is_none());
        assert_eq!(body["stream"], true);
        assert_eq!(body["max_tokens"], DEFAULT_MAX_TOKENS);

        req.thinking = Some(ThinkingConfig {
            budget_tokens: Some(2_000),
            effort: None,
        });
        let body = build_body(
            &req,
            &profile(ThinkingStyle::Adaptive),
            &RequestOptions::default(),
        )
        .unwrap();
        assert_eq!(body["output_config"], json!({"effort": "low"}));

        req.thinking = None;
        let body = build_body(
            &req,
            &profile(ThinkingStyle::Adaptive),
            &RequestOptions::default(),
        )
        .unwrap();
        assert!(body.get("thinking").is_none() && body.get("output_config").is_none());
    }

    #[test]
    fn effort_maps_to_budget_on_budget_models() {
        let mut req = ChatRequest::new("claude-haiku-4-5");
        req.messages.push(Message::user("hi"));
        req.max_tokens = Some(4_000);
        req.thinking = Some(ThinkingConfig {
            budget_tokens: None,
            effort: Some(ReasoningEffort::Medium),
        });
        let mut p = profile(ThinkingStyle::Budget);
        p.capabilities.max_output_tokens = 64_000;
        let body = build_body(&req, &p, &RequestOptions::default()).unwrap();
        assert_eq!(
            body["thinking"],
            json!({"type": "enabled", "budget_tokens": 8_192})
        );
        assert!(body["max_tokens"].as_u64().unwrap() > 8_192);
        assert!(body.get("output_config").is_none());

        req.thinking = Some(ThinkingConfig {
            budget_tokens: Some(100),
            effort: None,
        });
        let body = build_body(&req, &p, &RequestOptions::default()).unwrap();
        assert_eq!(body["thinking"]["budget_tokens"], MIN_THINKING_BUDGET);
        assert_eq!(body["max_tokens"], 4_000);
    }

    #[test]
    fn tool_choice_images_and_stop_sequences() {
        let mut req = tool_loop();
        req.tool_choice = ToolChoice::Tool {
            name: "read".into(),
        };
        req.stop_sequences = vec!["END".into()];
        req.max_tokens = Some(500_000);
        req.messages[0].content.push(ContentBlock::Image {
            source: ImageSource::Base64 {
                media_type: "image/png".into(),
                data: "AAA=".into(),
            },
        });
        let body = build_body(
            &req,
            &profile(ThinkingStyle::Adaptive),
            &RequestOptions::default(),
        )
        .unwrap();
        assert_eq!(body["tool_choice"], json!({"type": "tool", "name": "read"}));
        assert_eq!(body["stop_sequences"], json!(["END"]));
        assert_eq!(body["max_tokens"], 128_000);
        assert_eq!(
            body["messages"][0]["content"][1]["source"]["type"],
            "base64"
        );
        assert!(
            build_body(
                &ChatRequest::new(""),
                &profile(ThinkingStyle::None),
                &RequestOptions::default()
            )
            .is_err()
        );
    }

    #[test]
    fn system_layers_get_their_own_blocks_and_boundary_breakpoints() {
        let base = "You are terse.\n\n";
        let memory = "## Memory\n- prefer nextest\n\n";
        let skills = "## Skills\n- /deploy";
        let mut req = tool_loop();
        req.system = Some(format!("{base}{memory}{skills}"));
        req.system_layers = vec![base.len(), base.len() + memory.len()];
        let body = build_body(
            &req,
            &profile(ThinkingStyle::None),
            &RequestOptions::default(),
        )
        .unwrap();
        let system = body["system"].as_array().unwrap();
        let texts: Vec<&str> = system.iter().map(|b| b["text"].as_str().unwrap()).collect();
        assert_eq!(
            texts,
            [base, memory, skills],
            "one block per layer, same text"
        );
        // Last tool, end of the base prompt, end of the frozen prefix, and
        // the final message: the four-breakpoint limit.
        assert_eq!(
            breakpoints(&body),
            ["tools[1]", "system[0]", "system[2]", "messages[2][1]"]
        );
        // Without tools there is room for every layer boundary.
        req.tools.clear();
        let body = build_body(
            &req,
            &profile(ThinkingStyle::None),
            &RequestOptions::default(),
        )
        .unwrap();
        let bp = breakpoints(&body);
        assert!(bp.contains(&"system[0]".to_owned()), "{bp:?}");
        assert!(bp.contains(&"system[1]".to_owned()), "{bp:?}");
        assert!(bp.contains(&"system[2]".to_owned()), "{bp:?}");
        assert!(bp.len() <= MAX_CACHE_BREAKPOINTS, "{bp:?}");
        assert!(bp.iter().any(|b| b.starts_with("messages")), "{bp:?}");
    }

    #[test]
    fn bad_layer_offsets_are_ignored() {
        assert_eq!(system_layers("abc", &[0, 5, 2, 1]), ["ab", "c"]);
        assert_eq!(system_layers("é!", &[1]), ["é!"], "not a char boundary");
        assert_eq!(system_layers("abc", &[]), ["abc"]);
    }
}
