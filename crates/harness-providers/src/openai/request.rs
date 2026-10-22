//! Builds OpenAI Responses API request bodies from a [`ChatRequest`].

use serde_json::{Map, Value, json};

use crate::model::{
    ChatRequest, ContentBlock, ImageSource, Message, ReasoningEffort, Role, ThinkingConfig,
    ToolChoice, ToolResultPart, ToolSpec,
};
use crate::provider::ProviderError;

use super::PROVIDER_NAME;

/// Maps the agent's reasoning settings to an OpenAI `reasoning.effort`
/// value.
///
/// An explicit effort wins. Otherwise a token budget is bucketed: up to
/// 4096 tokens is `low`, up to 16384 is `medium`, anything larger is
/// `high`. `None` means "let the model use its default".
pub fn effort_for(thinking: &ThinkingConfig) -> Option<&'static str> {
    if let Some(effort) = thinking.effort {
        return Some(match effort {
            ReasoningEffort::Low => "low",
            ReasoningEffort::Medium => "medium",
            ReasoningEffort::High => "high",
        });
    }
    thinking.budget_tokens.map(|b| match b {
        0..=4096 => "low",
        4097..=16384 => "medium",
        _ => "high",
    })
}

/// Builds the JSON body for `POST /responses`.
///
/// The call is always stateless (`store: false`). For reasoning models the
/// body asks for reasoning summaries and for encrypted reasoning content,
/// which the stream returns as opaque blocks that are sent back verbatim on
/// the next turn.
pub fn build_body(request: &ChatRequest, reasoning_model: bool) -> Result<Value, ProviderError> {
    let mut body = Map::new();
    body.insert("model".into(), json!(request.model));
    body.insert("stream".into(), json!(true));
    body.insert("store".into(), json!(false));
    if let Some(system) = request.system.as_deref().filter(|s| !s.is_empty()) {
        body.insert("instructions".into(), json!(system));
    }
    body.insert(
        "input".into(),
        Value::Array(input_items(&request.messages)?),
    );
    if !request.tools.is_empty() {
        body.insert(
            "tools".into(),
            Value::Array(request.tools.iter().map(tool).collect()),
        );
        body.insert("tool_choice".into(), tool_choice(&request.tool_choice));
        body.insert("parallel_tool_calls".into(), json!(true));
    }
    if let Some(max) = request.max_tokens {
        body.insert("max_output_tokens".into(), json!(max));
    }
    if reasoning_model {
        let mut reasoning = Map::new();
        if let Some(effort) = request.thinking.as_ref().and_then(effort_for) {
            reasoning.insert("effort".into(), json!(effort));
        }
        reasoning.insert("summary".into(), json!("auto"));
        body.insert("reasoning".into(), Value::Object(reasoning));
        body.insert("include".into(), json!(["reasoning.encrypted_content"]));
        if request.temperature.is_some() {
            tracing::debug!(model = %request.model, "temperature is not supported by OpenAI reasoning models; omitted");
        }
    } else if let Some(t) = request.temperature {
        body.insert("temperature".into(), json!(t));
    }
    if !request.stop_sequences.is_empty() {
        tracing::debug!("stop sequences are not supported by the OpenAI Responses API; omitted");
    }
    Ok(Value::Object(body))
}

/// Adapts a body for Sign in with ChatGPT: `store: false` and streaming
/// are kept, and every field the plan route rejects
/// ([`crate::chatgpt::UNSUPPORTED_REQUEST_FIELDS`], such as `temperature`
/// and `max_output_tokens`) is removed.
pub fn for_chatgpt(body: &mut Value) {
    if let Some(map) = body.as_object_mut() {
        for field in crate::chatgpt::UNSUPPORTED_REQUEST_FIELDS {
            map.remove(*field);
        }
        map.insert("store".into(), json!(false));
        map.insert("stream".into(), json!(true));
    }
}

fn tool(spec: &ToolSpec) -> Value {
    json!({
        "type": "function",
        "name": spec.name,
        "description": spec.description,
        "parameters": spec.input_schema,
        "strict": false,
    })
}

fn tool_choice(choice: &ToolChoice) -> Value {
    match choice {
        ToolChoice::Auto => json!("auto"),
        ToolChoice::Any => json!("required"),
        ToolChoice::None => json!("none"),
        ToolChoice::Tool { name } => json!({"type": "function", "name": name}),
    }
}

fn image_url(source: &ImageSource) -> String {
    match source {
        ImageSource::Base64 { media_type, data } => format!("data:{media_type};base64,{data}"),
        ImageSource::Url { url } => url.clone(),
    }
}

/// Converts the conversation into Responses API input items.
///
/// Message content is grouped into `message` items; tool calls become
/// `function_call` items and tool results `function_call_output` items, in
/// conversation order. Opaque OpenAI blocks (encrypted reasoning items) are
/// sent back unchanged. Reasoning text and opaque blocks from other
/// providers are dropped because OpenAI cannot verify them.
fn input_items(messages: &[Message]) -> Result<Vec<Value>, ProviderError> {
    let mut items = Vec::new();
    for message in messages {
        let mut parts: Vec<Value> = Vec::new();
        let (role, text_type) = match message.role {
            Role::User => ("user", "input_text"),
            Role::Assistant => ("assistant", "output_text"),
        };
        let flush = |parts: &mut Vec<Value>, items: &mut Vec<Value>| {
            if !parts.is_empty() {
                items.push(json!({
                    "type": "message",
                    "role": role,
                    "content": std::mem::take(parts),
                }));
            }
        };
        for block in &message.content {
            match block {
                ContentBlock::Text { text } => parts.push(json!({"type": text_type, "text": text})),
                ContentBlock::Image { source } => {
                    if message.role == Role::Assistant {
                        return Err(ProviderError::InvalidRequest(
                            "OpenAI does not accept images in assistant messages".into(),
                        ));
                    }
                    parts.push(json!({"type": "input_image", "image_url": image_url(source)}));
                }
                ContentBlock::ToolUse { id, name, input } => {
                    flush(&mut parts, &mut items);
                    let arguments = match input {
                        Value::String(raw) => raw.clone(),
                        other => other.to_string(),
                    };
                    items.push(json!({
                        "type": "function_call",
                        "call_id": id,
                        "name": name,
                        "arguments": arguments,
                    }));
                }
                ContentBlock::ToolResult {
                    tool_use_id,
                    content,
                    is_error,
                } => {
                    flush(&mut parts, &mut items);
                    items.push(json!({
                        "type": "function_call_output",
                        "call_id": tool_use_id,
                        "output": tool_output(content, *is_error),
                    }));
                }
                ContentBlock::Opaque { provider, data } if provider == PROVIDER_NAME => {
                    flush(&mut parts, &mut items);
                    items.push(data.clone());
                }
                ContentBlock::Reasoning { .. } | ContentBlock::Opaque { .. } => {}
            }
        }
        flush(&mut parts, &mut items);
    }
    Ok(items)
}

/// A tool result as a plain string, or as content parts when it has images.
fn tool_output(content: &[ToolResultPart], is_error: bool) -> Value {
    let prefix = if is_error { "Error: " } else { "" };
    let has_images = content
        .iter()
        .any(|p| matches!(p, ToolResultPart::Image { .. }));
    if !has_images {
        let text: Vec<&str> = content
            .iter()
            .filter_map(|p| match p {
                ToolResultPart::Text { text } => Some(text.as_str()),
                ToolResultPart::Image { .. } => None,
            })
            .collect();
        return json!(format!("{prefix}{}", text.join("\n")));
    }
    let mut parts = Vec::new();
    if is_error {
        parts.push(json!({"type": "input_text", "text": "Error:"}));
    }
    for p in content {
        parts.push(match p {
            ToolResultPart::Text { text } => json!({"type": "input_text", "text": text}),
            ToolResultPart::Image { source } => {
                json!({"type": "input_image", "image_url": image_url(source)})
            }
        });
    }
    Value::Array(parts)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn effort_mapping() {
        let e = |effort| ThinkingConfig {
            effort: Some(effort),
            budget_tokens: Some(100_000),
        };
        assert_eq!(effort_for(&e(ReasoningEffort::Low)), Some("low"));
        assert_eq!(effort_for(&e(ReasoningEffort::Medium)), Some("medium"));
        assert_eq!(effort_for(&e(ReasoningEffort::High)), Some("high"));
        let b = |n| ThinkingConfig {
            effort: None,
            budget_tokens: Some(n),
        };
        assert_eq!(effort_for(&b(1024)), Some("low"));
        assert_eq!(effort_for(&b(8000)), Some("medium"));
        assert_eq!(effort_for(&b(32000)), Some("high"));
        assert_eq!(effort_for(&ThinkingConfig::default()), None);
    }

    fn sample() -> ChatRequest {
        let mut r = ChatRequest::new("o4-mini");
        r.system = Some("be brief".into());
        r.tools = vec![ToolSpec {
            name: "read".into(),
            description: "Read a file".into(),
            input_schema: json!({"type": "object"}),
        }];
        r.thinking = Some(ThinkingConfig {
            effort: Some(ReasoningEffort::High),
            budget_tokens: None,
        });
        r.max_tokens = Some(1000);
        r.temperature = Some(0.2);
        r.messages = vec![
            Message::user("hi"),
            Message {
                role: Role::Assistant,
                content: vec![
                    ContentBlock::Reasoning {
                        text: "summary".into(),
                        signature: None,
                    },
                    ContentBlock::Opaque {
                        provider: "openai".into(),
                        data: json!({"type": "reasoning", "id": "rs_1", "encrypted_content": "enc", "summary": []}),
                    },
                    ContentBlock::Opaque {
                        provider: "anthropic".into(),
                        data: json!({"type": "redacted_thinking"}),
                    },
                    ContentBlock::Opaque {
                        provider: "harness".into(),
                        data: json!({"type": "cost", "provider": "x", "usd": 1.0}),
                    },
                    ContentBlock::text("calling"),
                    ContentBlock::tool_use("call_a", "read", json!({"path": "a"})),
                    ContentBlock::tool_use("call_b", "read", Value::String("{bad".into())),
                ],
            },
            Message {
                role: Role::User,
                content: vec![
                    ContentBlock::tool_result("call_a", "A", false),
                    ContentBlock::ToolResult {
                        tool_use_id: "call_b".into(),
                        content: vec![
                            ToolResultPart::Text {
                                text: "shot".into(),
                            },
                            ToolResultPart::Image {
                                source: ImageSource::Base64 {
                                    media_type: "image/png".into(),
                                    data: "AAA=".into(),
                                },
                            },
                        ],
                        is_error: true,
                    },
                ],
            },
        ];
        r
    }

    #[test]
    fn reasoning_model_body() {
        let body = build_body(&sample(), true).unwrap();
        assert_eq!(body["store"], json!(false));
        assert_eq!(body["stream"], json!(true));
        assert_eq!(body["instructions"], json!("be brief"));
        assert_eq!(
            body["reasoning"],
            json!({"effort": "high", "summary": "auto"})
        );
        assert_eq!(body["include"], json!(["reasoning.encrypted_content"]));
        assert_eq!(body["parallel_tool_calls"], json!(true));
        assert_eq!(body["tool_choice"], json!("auto"));
        assert_eq!(body["max_output_tokens"], json!(1000));
        assert!(body.get("temperature").is_none());
        assert_eq!(
            body["tools"][0],
            json!({"type": "function", "name": "read", "description": "Read a file",
                   "parameters": {"type": "object"}, "strict": false})
        );
        assert_eq!(
            body["input"],
            json!([
                {"type": "message", "role": "user", "content": [{"type": "input_text", "text": "hi"}]},
                {"type": "reasoning", "id": "rs_1", "encrypted_content": "enc", "summary": []},
                {"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": "calling"}]},
                {"type": "function_call", "call_id": "call_a", "name": "read", "arguments": "{\"path\":\"a\"}"},
                {"type": "function_call", "call_id": "call_b", "name": "read", "arguments": "{bad"},
                {"type": "function_call_output", "call_id": "call_a", "output": "A"},
                {"type": "function_call_output", "call_id": "call_b", "output": [
                    {"type": "input_text", "text": "Error:"},
                    {"type": "input_text", "text": "shot"},
                    {"type": "input_image", "image_url": "data:image/png;base64,AAA="}
                ]},
            ])
        );
    }

    #[test]
    fn non_reasoning_model_body_has_no_reasoning_fields() {
        let mut r = sample();
        r.tool_choice = ToolChoice::Tool {
            name: "read".into(),
        };
        let body = build_body(&r, false).unwrap();
        assert!(body.get("reasoning").is_none());
        assert!(body.get("include").is_none());
        assert_eq!(body["store"], json!(false));
        assert!((body["temperature"].as_f64().unwrap() - 0.2).abs() < 1e-6);
        assert_eq!(
            body["tool_choice"],
            json!({"type": "function", "name": "read"})
        );
    }

    #[test]
    fn chatgpt_bodies_drop_unsupported_sampling_fields() {
        let mut body = build_body(&sample(), false).unwrap();
        assert!(body.get("temperature").is_some());
        assert!(body.get("max_output_tokens").is_some());
        body["top_p"] = json!(0.9);
        for_chatgpt(&mut body);
        for f in ["temperature", "max_output_tokens", "top_p"] {
            assert!(body.get(f).is_none(), "{f} kept");
        }
        assert_eq!(body["store"], json!(false));
        assert_eq!(body["stream"], json!(true));
        assert_eq!(body["instructions"], json!("be brief"));
        assert!(body["tools"].is_array());
    }

    #[test]
    fn reasoning_without_effort_still_requests_summaries() {
        let mut r = sample();
        r.thinking = None;
        let body = build_body(&r, true).unwrap();
        assert_eq!(body["reasoning"], json!({"summary": "auto"}));
    }
}
