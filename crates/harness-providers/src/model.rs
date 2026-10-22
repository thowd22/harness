//! Normalized, provider-independent message model.
//!
//! Every provider (Anthropic, OpenAI, OpenRouter, Fake) translates to and from
//! these types, so the rest of the harness never sees a vendor wire format.
//! All types are `serde` (de)serializable so they can be stored in the event
//! log, used in fixtures and written in YAML test scripts.

use std::ops::{Add, AddAssign};

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Who authored a [`Message`].
///
/// The system prompt is not a message; it lives in [`ChatRequest::system`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    /// The user, or tool results sent back on the user's behalf.
    User,
    /// The model.
    Assistant,
}

/// One conversation turn made of ordered content blocks.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Message {
    /// Author of the message.
    pub role: Role,
    /// Ordered content blocks.
    pub content: Vec<ContentBlock>,
}

impl Message {
    /// A user message holding a single text block.
    pub fn user(text: impl Into<String>) -> Self {
        Self {
            role: Role::User,
            content: vec![ContentBlock::text(text)],
        }
    }

    /// An assistant message holding a single text block.
    pub fn assistant(text: impl Into<String>) -> Self {
        Self {
            role: Role::Assistant,
            content: vec![ContentBlock::text(text)],
        }
    }

    /// Concatenation of all [`ContentBlock::Text`] blocks.
    pub fn text(&self) -> String {
        self.content
            .iter()
            .filter_map(|b| match b {
                ContentBlock::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect()
    }

    /// All tool calls in this message, in order.
    pub fn tool_uses(&self) -> impl Iterator<Item = (&str, &str, &Value)> {
        self.content.iter().filter_map(|b| match b {
            ContentBlock::ToolUse { id, name, input } => Some((id.as_str(), name.as_str(), input)),
            _ => None,
        })
    }
}

/// Where image bytes come from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ImageSource {
    /// Inline base64 data.
    Base64 {
        /// MIME type such as `image/png`.
        media_type: String,
        /// Base64-encoded bytes.
        data: String,
    },
    /// A URL the provider fetches itself.
    Url {
        /// Absolute URL.
        url: String,
    },
}

/// A part of a tool result. Tool results may carry text and images.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ToolResultPart {
    /// Plain text output.
    Text {
        /// The text.
        text: String,
    },
    /// An image output (for example a screenshot).
    Image {
        /// Image source.
        source: ImageSource,
    },
}

/// One normalized content block.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    /// Plain text.
    Text {
        /// The text.
        text: String,
    },
    /// An image input.
    Image {
        /// Image source.
        source: ImageSource,
    },
    /// A tool call made by the model.
    ToolUse {
        /// Provider-assigned call id, echoed back in [`ContentBlock::ToolResult`].
        id: String,
        /// Tool name.
        name: String,
        /// Parsed JSON arguments.
        input: Value,
    },
    /// The result of a tool call, sent in a user message.
    ToolResult {
        /// Id of the [`ContentBlock::ToolUse`] this answers.
        tool_use_id: String,
        /// Output parts.
        content: Vec<ToolResultPart>,
        /// True when the tool failed.
        #[serde(default)]
        is_error: bool,
    },
    /// Visible reasoning / extended thinking.
    ///
    /// `signature` must be sent back unchanged on later turns; providers
    /// reject edited or unsigned thinking.
    Reasoning {
        /// Reasoning text (may be a summary, depending on provider).
        text: String,
        /// Provider integrity signature, if any.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        signature: Option<String>,
    },
    /// A provider-specific block the harness cannot interpret but must
    /// round-trip verbatim (for example Anthropic `redacted_thinking` or
    /// OpenAI encrypted reasoning items).
    Opaque {
        /// Provider name that produced the block, e.g. `anthropic`.
        provider: String,
        /// The raw provider JSON, sent back unchanged.
        data: Value,
    },
}

impl ContentBlock {
    /// Shorthand for a [`ContentBlock::Text`].
    pub fn text(text: impl Into<String>) -> Self {
        Self::Text { text: text.into() }
    }

    /// Shorthand for a [`ContentBlock::ToolUse`].
    pub fn tool_use(id: impl Into<String>, name: impl Into<String>, input: Value) -> Self {
        Self::ToolUse {
            id: id.into(),
            name: name.into(),
            input,
        }
    }

    /// Shorthand for a text-only [`ContentBlock::ToolResult`].
    pub fn tool_result(
        tool_use_id: impl Into<String>,
        text: impl Into<String>,
        is_error: bool,
    ) -> Self {
        Self::ToolResult {
            tool_use_id: tool_use_id.into(),
            content: vec![ToolResultPart::Text { text: text.into() }],
            is_error,
        }
    }
}

/// A tool the model may call.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolSpec {
    /// Tool name.
    pub name: String,
    /// Description shown to the model.
    pub description: String,
    /// JSON Schema of the input object.
    pub input_schema: Value,
}

/// How the model should choose tools.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum ToolChoice {
    /// The model decides (default).
    #[default]
    Auto,
    /// The model must call some tool.
    Any,
    /// The model must not call tools.
    None,
    /// The model must call the named tool.
    Tool {
        /// Tool name.
        name: String,
    },
}

/// Requested reasoning effort for providers that use effort levels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReasoningEffort {
    /// Minimal reasoning.
    Low,
    /// Balanced reasoning.
    Medium,
    /// Maximum reasoning.
    High,
}

/// Reasoning / extended-thinking settings. Providers map whichever field
/// they support (token budget or effort level).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ThinkingConfig {
    /// Token budget for thinking (Anthropic style).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budget_tokens: Option<u32>,
    /// Effort level (OpenAI style).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<ReasoningEffort>,
}

/// A provider-independent model request.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ChatRequest {
    /// Provider-local model id (no `provider/` prefix).
    pub model: String,
    /// System prompt, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system: Option<String>,
    /// Byte offsets in `system` where a later layer starts (memory index,
    /// skill listing). Providers with explicit caching put the layers in
    /// separate blocks and cache breakpoints at their boundaries; the text
    /// sent is the same either way.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub system_layers: Vec<usize>,
    /// Conversation so far.
    #[serde(default)]
    pub messages: Vec<Message>,
    /// Tools offered to the model.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<ToolSpec>,
    /// Tool choice mode.
    #[serde(default)]
    pub tool_choice: ToolChoice,
    /// Maximum output tokens.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    /// Sampling temperature.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    /// Reasoning settings; `None` disables thinking.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking: Option<ThinkingConfig>,
    /// Extra stop sequences.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub stop_sequences: Vec<String>,
}

impl ChatRequest {
    /// A request for `model` with everything else defaulted.
    pub fn new(model: impl Into<String>) -> Self {
        Self {
            model: model.into(),
            ..Self::default()
        }
    }
}

/// Token usage of one call. All counts are tokens.
///
/// `input_tokens` excludes cache reads and writes, so the total billed input
/// is `input_tokens + cache_read_tokens + cache_write_tokens`.
/// `reasoning_tokens` is a subset of `output_tokens` where the provider
/// reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Usage {
    /// Uncached input tokens.
    #[serde(default)]
    pub input_tokens: u64,
    /// Output tokens, including reasoning.
    #[serde(default)]
    pub output_tokens: u64,
    /// Input tokens read from the prompt cache.
    #[serde(default)]
    pub cache_read_tokens: u64,
    /// Input tokens written to the prompt cache.
    #[serde(default)]
    pub cache_write_tokens: u64,
    /// Reasoning tokens (subset of `output_tokens`).
    #[serde(default)]
    pub reasoning_tokens: u64,
}

impl Usage {
    /// All input tokens: uncached plus cache reads and writes.
    pub fn total_input(&self) -> u64 {
        self.input_tokens + self.cache_read_tokens + self.cache_write_tokens
    }

    /// Input plus output tokens.
    pub fn total(&self) -> u64 {
        self.total_input() + self.output_tokens
    }
}

impl Add for Usage {
    type Output = Usage;
    fn add(mut self, rhs: Usage) -> Usage {
        self += rhs;
        self
    }
}

impl AddAssign for Usage {
    fn add_assign(&mut self, rhs: Usage) {
        self.input_tokens += rhs.input_tokens;
        self.output_tokens += rhs.output_tokens;
        self.cache_read_tokens += rhs.cache_read_tokens;
        self.cache_write_tokens += rhs.cache_write_tokens;
        self.reasoning_tokens += rhs.reasoning_tokens;
    }
}

/// Normalized reason a model stopped generating.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopKind {
    /// The model finished its turn.
    EndTurn,
    /// The model wants tool results.
    ToolUse,
    /// Output token limit reached.
    MaxTokens,
    /// A stop sequence matched.
    StopSequence,
    /// The provider paused a long turn; resend to continue.
    Pause,
    /// The model refused or a content filter stopped output.
    Refusal,
    /// The context window is full.
    ContextLimit,
    /// Any value not covered above (see [`StopReason::raw`]).
    Other,
}

/// A normalized stop reason plus the provider's raw value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StopReason {
    /// Normalized kind.
    pub kind: StopKind,
    /// Raw provider value, e.g. `end_turn` or `length`, when one was sent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw: Option<String>,
}

impl StopReason {
    /// A stop reason with no raw provider value.
    pub fn new(kind: StopKind) -> Self {
        Self { kind, raw: None }
    }

    /// Maps an Anthropic `stop_reason`.
    pub fn from_anthropic(raw: &str) -> Self {
        let kind = match raw {
            "end_turn" => StopKind::EndTurn,
            "tool_use" => StopKind::ToolUse,
            "max_tokens" => StopKind::MaxTokens,
            "stop_sequence" => StopKind::StopSequence,
            "pause_turn" => StopKind::Pause,
            "refusal" => StopKind::Refusal,
            "model_context_window_exceeded" => StopKind::ContextLimit,
            _ => StopKind::Other,
        };
        Self {
            kind,
            raw: Some(raw.to_owned()),
        }
    }

    /// Maps an OpenAI-style `finish_reason` (Chat Completions and OpenRouter)
    /// or a Responses API `incomplete_details.reason`.
    pub fn from_openai(raw: &str) -> Self {
        let kind = match raw {
            "stop" | "completed" => StopKind::EndTurn,
            "tool_calls" | "function_call" => StopKind::ToolUse,
            "length" | "max_output_tokens" => StopKind::MaxTokens,
            "content_filter" => StopKind::Refusal,
            _ => StopKind::Other,
        };
        Self {
            kind,
            raw: Some(raw.to_owned()),
        }
    }
}

/// What a model (as served by a provider) supports.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Capabilities {
    /// Tool calling.
    #[serde(default)]
    pub tools: bool,
    /// Several tool calls in one response.
    #[serde(default)]
    pub parallel_tool_calls: bool,
    /// Reasoning / extended thinking.
    #[serde(default)]
    pub thinking: bool,
    /// Prompt caching.
    #[serde(default)]
    pub caching: bool,
    /// Image inputs.
    #[serde(default)]
    pub images: bool,
    /// Context window in tokens.
    #[serde(default)]
    pub context_window: u32,
    /// Maximum output tokens per call, 0 if unknown.
    #[serde(default)]
    pub max_output_tokens: u32,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn every_block_kind_round_trips_through_json() {
        let msg = Message {
            role: Role::Assistant,
            content: vec![
                ContentBlock::text("hi"),
                ContentBlock::Image {
                    source: ImageSource::Base64 {
                        media_type: "image/png".into(),
                        data: "AAA=".into(),
                    },
                },
                ContentBlock::Image {
                    source: ImageSource::Url {
                        url: "https://x/y.png".into(),
                    },
                },
                ContentBlock::tool_use("t1", "read", json!({"path": "a.rs"})),
                ContentBlock::ToolResult {
                    tool_use_id: "t1".into(),
                    content: vec![
                        ToolResultPart::Text { text: "ok".into() },
                        ToolResultPart::Image {
                            source: ImageSource::Url { url: "u".into() },
                        },
                    ],
                    is_error: true,
                },
                ContentBlock::Reasoning {
                    text: "think".into(),
                    signature: Some("sig".into()),
                },
                ContentBlock::Opaque {
                    provider: "anthropic".into(),
                    data: json!({"type": "redacted_thinking", "data": "xyz"}),
                },
            ],
        };
        let s = serde_json::to_string(&msg).unwrap();
        let back: Message = serde_json::from_str(&s).unwrap();
        assert_eq!(back, msg);
    }

    #[test]
    fn opaque_block_preserves_raw_json_exactly() {
        let raw =
            json!({"type": "reasoning", "encrypted_content": "gAAA", "nested": {"a": [1, 2]}});
        let block = ContentBlock::Opaque {
            provider: "openai".into(),
            data: raw.clone(),
        };
        let v = serde_json::to_value(&block).unwrap();
        assert_eq!(v["data"], raw);
        let back: ContentBlock = serde_json::from_value(v).unwrap();
        assert_eq!(back, block);
    }

    #[test]
    fn request_round_trips_and_defaults() {
        let mut req = ChatRequest::new("m");
        req.system = Some("sys".into());
        req.messages.push(Message::user("hello"));
        req.tools.push(ToolSpec {
            name: "t".into(),
            description: "d".into(),
            input_schema: json!({}),
        });
        req.tool_choice = ToolChoice::Tool { name: "t".into() };
        req.thinking = Some(ThinkingConfig {
            budget_tokens: Some(1024),
            effort: Some(ReasoningEffort::High),
        });
        let back: ChatRequest =
            serde_json::from_str(&serde_json::to_string(&req).unwrap()).unwrap();
        assert_eq!(back, req);
        let minimal: ChatRequest = serde_json::from_str(r#"{"model":"m"}"#).unwrap();
        assert_eq!(minimal, ChatRequest::new("m"));
    }

    #[test]
    fn message_helpers() {
        let m = Message {
            role: Role::Assistant,
            content: vec![
                ContentBlock::text("a"),
                ContentBlock::tool_use("1", "x", json!(null)),
                ContentBlock::text("b"),
            ],
        };
        assert_eq!(m.text(), "ab");
        assert_eq!(m.tool_uses().map(|t| t.1).collect::<Vec<_>>(), vec!["x"]);
    }

    #[test]
    fn usage_has_all_counters_and_adds() {
        let a = Usage {
            input_tokens: 1,
            output_tokens: 2,
            cache_read_tokens: 3,
            cache_write_tokens: 4,
            reasoning_tokens: 1,
        };
        let sum = a + a;
        assert_eq!(
            sum,
            Usage {
                input_tokens: 2,
                output_tokens: 4,
                cache_read_tokens: 6,
                cache_write_tokens: 8,
                reasoning_tokens: 2
            }
        );
        assert_eq!(a.total_input(), 8);
        assert_eq!(a.total(), 10);
    }

    #[test]
    fn stop_reasons_normalize_and_keep_raw() {
        for (raw, kind) in [
            ("end_turn", StopKind::EndTurn),
            ("tool_use", StopKind::ToolUse),
            ("max_tokens", StopKind::MaxTokens),
            ("stop_sequence", StopKind::StopSequence),
            ("pause_turn", StopKind::Pause),
            ("refusal", StopKind::Refusal),
            ("model_context_window_exceeded", StopKind::ContextLimit),
            ("brand_new", StopKind::Other),
        ] {
            let s = StopReason::from_anthropic(raw);
            assert_eq!(s.kind, kind, "{raw}");
            assert_eq!(s.raw.as_deref(), Some(raw));
        }
        for (raw, kind) in [
            ("stop", StopKind::EndTurn),
            ("tool_calls", StopKind::ToolUse),
            ("length", StopKind::MaxTokens),
            ("content_filter", StopKind::Refusal),
            ("weird", StopKind::Other),
        ] {
            let s = StopReason::from_openai(raw);
            assert_eq!(s.kind, kind, "{raw}");
            assert_eq!(s.raw.as_deref(), Some(raw));
        }
        assert_eq!(StopReason::new(StopKind::EndTurn).raw, None);
    }

    #[test]
    fn capabilities_cover_required_fields() {
        let c: Capabilities = serde_json::from_value(json!({
            "tools": true, "parallel_tool_calls": true, "thinking": true,
            "caching": true, "images": true, "context_window": 200000
        }))
        .unwrap();
        assert!(c.tools && c.parallel_tool_calls && c.thinking && c.caching && c.images);
        assert_eq!(c.context_window, 200_000);
        assert_eq!(c.max_output_tokens, 0);
    }
}
