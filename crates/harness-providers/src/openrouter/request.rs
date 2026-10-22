//! Builds OpenRouter Chat Completions request bodies.

use serde_json::{Map, Value, json};

use crate::model::{
    ChatRequest, ContentBlock, ImageSource, Message, ReasoningEffort, Role, ToolChoice,
    ToolResultPart,
};

use super::{OpenRouterOptions, PROVIDER_NAME};

/// Builds the JSON body for `POST /chat/completions`.
///
/// Always requests usage accounting (`usage.include`), so the final chunk
/// carries token counts and the call's cost. Routing preferences from
/// `options` go into the `provider` object.
pub fn build_body(request: &ChatRequest, options: &OpenRouterOptions) -> Value {
    let mut body = Map::new();
    body.insert("model".into(), json!(request.model));
    body.insert("stream".into(), json!(true));
    body.insert("usage".into(), json!({"include": true}));
    body.insert("messages".into(), Value::Array(messages(request)));
    if !request.tools.is_empty() {
        let tools = request
            .tools
            .iter()
            .map(|t| {
                json!({"type": "function", "function": {
                    "name": t.name, "description": t.description, "parameters": t.input_schema}})
            })
            .collect();
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

fn messages(request: &ChatRequest) -> Vec<Value> {
    let mut out = Vec::new();
    if let Some(system) = request.system.as_deref().filter(|s| !s.is_empty()) {
        out.push(json!({"role": "system", "content": system}));
    }
    for m in &request.messages {
        match m.role {
            Role::User => user(m, &mut out),
            Role::Assistant => out.push(assistant(m)),
        }
    }
    out
}

/// Tool results become `tool` messages (which must directly follow the
/// assistant's tool calls); remaining text and images, including images
/// from tool results, follow in one user message.
fn user(m: &Message, out: &mut Vec<Value>) {
    let mut parts = Vec::new();
    for block in &m.content {
        match block {
            ContentBlock::Text { text } => parts.push(json!({"type": "text", "text": text})),
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
                out.push(json!({"role": "tool", "tool_call_id": tool_use_id,
                    "content": format!("{prefix}{}", text.join("\n"))}));
            }
            _ => {}
        }
    }
    if parts.is_empty() {
        return;
    }
    let content = if parts.iter().all(|p| p["type"] == "text") {
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

    fn opts() -> OpenRouterOptions {
        OpenRouterOptions {
            app_name: Some("harness".into()),
            provider_order: vec!["anthropic".into(), "openai".into()],
            allow_fallbacks: Some(false),
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
        assert_eq!(
            b["messages"][0],
            json!({"role": "system", "content": "sys"})
        );
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
}
