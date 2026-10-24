//! Deterministic fake provider for tests, evals and UI demos.
//!
//! A [`FakeProvider`] replays a [`FakeScript`]: one [`FakeTurn`] per call,
//! in order. Turns can stream text, reasoning and tool calls, wait before
//! and between events, and fail with any [`ProviderError`]. Every request is
//! recorded for assertions. Scripts can be written in YAML:
//!
//! ```yaml
//! responses:
//!   - delay_ms: 50
//!     content:
//!       - reasoning: { text: "Need to read the file", signature: sig-1 }
//!       - text: "Reading it now."
//!       - tool_use: { name: read, input: { path: src/main.rs } }
//!     usage: { input_tokens: 120, output_tokens: 30 }
//!   - error: { kind: rate_limited, message: slow down, retry_after_ms: 100 }
//!   - content: [ { text: "Done." } ]
//! ```

use std::collections::VecDeque;
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use async_trait::async_trait;
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::model::{Capabilities, ChatRequest, StopKind, StopReason, Usage};
use crate::provider::{EventStream, Provider, ProviderError, StreamEvent};

/// One scripted content block.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FakeBlock {
    /// Text, streamed in `chunk_size` pieces.
    Text(String),
    /// Reasoning text with an optional signature.
    Reasoning {
        /// Reasoning text.
        text: String,
        /// Signature to round-trip.
        #[serde(default)]
        signature: Option<String>,
    },
    /// A tool call. `id` defaults to `fake_call_<call>_<block>`.
    ToolUse {
        /// Call id.
        #[serde(default)]
        id: Option<String>,
        /// Tool name.
        name: String,
        /// Arguments; streamed as JSON text.
        #[serde(default)]
        input: Value,
    },
    /// A provider-opaque block.
    Opaque {
        /// Provider name.
        provider: String,
        /// Raw JSON.
        data: Value,
    },
}

/// Kind of scripted error.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FakeErrorKind {
    /// [`ProviderError::RateLimited`].
    RateLimited,
    /// [`ProviderError::Overloaded`].
    Overloaded,
    /// [`ProviderError::Http`] with `status` (default 500).
    Server,
    /// [`ProviderError::Auth`].
    Auth,
    /// [`ProviderError::InvalidRequest`].
    InvalidRequest,
    /// [`ProviderError::Connection`].
    Connection,
    /// [`ProviderError::Timeout`].
    Timeout,
    /// [`ProviderError::Stalled`].
    Stalled,
    /// [`ProviderError::Other`].
    Other,
    /// [`ProviderError::UsageLimit`] (a ChatGPT plan usage limit), with
    /// [`crate::chatgpt::MANAGE_USAGE_URL`] as the place to manage usage.
    UsageLimit,
}

/// A scripted error.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FakeError {
    /// Which error.
    pub kind: FakeErrorKind,
    /// Error message.
    #[serde(default)]
    pub message: String,
    /// HTTP status for [`FakeErrorKind::Server`].
    #[serde(default)]
    pub status: Option<u16>,
    /// `retry-after` in milliseconds for rate-limit, overload and server errors.
    #[serde(default)]
    pub retry_after_ms: Option<u64>,
}

impl FakeError {
    /// An error of `kind` with `message`.
    pub fn new(kind: FakeErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            status: None,
            retry_after_ms: None,
        }
    }

    /// The equivalent [`ProviderError`].
    pub fn to_provider_error(&self) -> ProviderError {
        let message = self.message.clone();
        let retry_after = self.retry_after_ms.map(Duration::from_millis);
        match self.kind {
            FakeErrorKind::RateLimited => ProviderError::RateLimited {
                message,
                retry_after,
            },
            FakeErrorKind::Overloaded => ProviderError::Overloaded {
                message,
                retry_after,
            },
            FakeErrorKind::Server => ProviderError::Http {
                status: self.status.unwrap_or(500),
                message,
                retry_after,
            },
            FakeErrorKind::Auth => ProviderError::Auth(message),
            FakeErrorKind::InvalidRequest => ProviderError::InvalidRequest(message),
            FakeErrorKind::Connection => ProviderError::Connection(message),
            FakeErrorKind::Timeout => ProviderError::Timeout(Duration::ZERO),
            FakeErrorKind::Stalled => ProviderError::Stalled(Duration::ZERO),
            FakeErrorKind::Other => ProviderError::Other(message),
            FakeErrorKind::UsageLimit => ProviderError::UsageLimit {
                message,
                manage_url: crate::chatgpt::MANAGE_USAGE_URL.into(),
            },
        }
    }
}

/// The scripted reply to one call.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FakeTurn {
    /// Wait before the first event (or before failing).
    #[serde(default)]
    pub delay_ms: u64,
    /// Wait between events.
    #[serde(default)]
    pub chunk_delay_ms: u64,
    /// Characters per text/reasoning/tool-argument delta; 0 sends each
    /// block in one delta.
    #[serde(default)]
    pub chunk_size: usize,
    /// Blocks to stream.
    #[serde(default)]
    pub content: Vec<FakeBlock>,
    /// Stop kind; defaults to `tool_use` if the turn has a tool call,
    /// otherwise `end_turn`.
    #[serde(default)]
    pub stop: Option<StopKind>,
    /// Usage reported in the final event (zeros by default).
    #[serde(default)]
    pub usage: Usage,
    /// Fail the call. With no content the call fails before streaming;
    /// otherwise the error follows the content, in place of `MessageStop`.
    #[serde(default)]
    pub error: Option<FakeError>,
    /// Serve this turn only to a request whose system prompt contains this
    /// text, whatever its position in the script (for calls whose timing
    /// varies, such as a background compaction summary). Requests that no
    /// such turn matches take the first turn without `match_system`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub match_system: Option<String>,
}

impl FakeTurn {
    /// A turn that replies with `text`.
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            content: vec![FakeBlock::Text(text.into())],
            ..Self::default()
        }
    }

    /// A turn that calls tool `name` with `input`.
    pub fn tool_use(name: impl Into<String>, input: Value) -> Self {
        Self {
            content: vec![FakeBlock::ToolUse {
                id: None,
                name: name.into(),
                input,
            }],
            ..Self::default()
        }
    }

    /// A turn that fails with `error`.
    pub fn error(error: FakeError) -> Self {
        Self {
            error: Some(error),
            ..Self::default()
        }
    }
}

/// A sequence of scripted turns.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FakeScript {
    /// Capabilities to report; defaults to everything supported with a
    /// 200k-token window.
    #[serde(default)]
    pub capabilities: Option<Capabilities>,
    /// Turns, consumed one per call.
    #[serde(default)]
    pub responses: Vec<FakeTurn>,
}

impl FakeScript {
    /// Parses a YAML script. Enum values such as content blocks are written
    /// as single-key maps (`- text: hi`), not YAML tags.
    pub fn from_yaml(yaml: &str) -> Result<Self, ProviderError> {
        serde_norway::with::singleton_map_recursive::deserialize(
            serde_norway::Deserializer::from_str(yaml),
        )
        .map_err(|e| ProviderError::InvalidRequest(format!("fake script: {e}")))
    }

    /// Reads and parses a YAML script file.
    pub fn from_yaml_file(path: impl AsRef<Path>) -> Result<Self, ProviderError> {
        let path = path.as_ref();
        let text = std::fs::read_to_string(path).map_err(|e| {
            ProviderError::InvalidRequest(format!("reading {}: {e}", path.display()))
        })?;
        Self::from_yaml(&text)
    }
}

/// Default capabilities of the fake provider.
pub const FAKE_CAPABILITIES: Capabilities = Capabilities {
    tools: true,
    parallel_tool_calls: true,
    thinking: true,
    caching: true,
    images: true,
    context_window: 200_000,
    max_output_tokens: 32_000,
};

#[derive(Debug, Default)]
struct Shared {
    turns: VecDeque<FakeTurn>,
    requests: Vec<ChatRequest>,
    calls: usize,
}

/// A [`Provider`] that replays a [`FakeScript`]. Clones share the script
/// and the request log.
#[derive(Debug, Clone)]
pub struct FakeProvider {
    capabilities: Capabilities,
    shared: Arc<Mutex<Shared>>,
}

impl FakeProvider {
    /// A provider replaying `script`.
    pub fn new(script: FakeScript) -> Self {
        Self {
            capabilities: script.capabilities.unwrap_or(FAKE_CAPABILITIES),
            shared: Arc::new(Mutex::new(Shared {
                turns: script.responses.into(),
                ..Shared::default()
            })),
        }
    }

    /// A provider replaying `turns` with default capabilities.
    pub fn with_turns(turns: impl IntoIterator<Item = FakeTurn>) -> Self {
        Self::new(FakeScript {
            capabilities: None,
            responses: turns.into_iter().collect(),
        })
    }

    /// Parses a YAML script; see [`FakeScript::from_yaml`].
    pub fn from_yaml(yaml: &str) -> Result<Self, ProviderError> {
        FakeScript::from_yaml(yaml).map(Self::new)
    }

    fn lock(&self) -> MutexGuard<'_, Shared> {
        self.shared.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Appends a turn to the script.
    pub fn push(&self, turn: FakeTurn) {
        self.lock().turns.push_back(turn);
    }

    /// Every request received so far, in order.
    pub fn requests(&self) -> Vec<ChatRequest> {
        self.lock().requests.clone()
    }

    /// The most recent request, if any.
    pub fn last_request(&self) -> Option<ChatRequest> {
        self.lock().requests.last().cloned()
    }

    /// Turns not yet replayed.
    pub fn remaining(&self) -> usize {
        self.lock().turns.len()
    }
}

fn chunks(text: &str, size: usize) -> Vec<String> {
    if size == 0 || text.is_empty() {
        return vec![text.to_owned()];
    }
    let chars: Vec<char> = text.chars().collect();
    chars.chunks(size).map(|c| c.iter().collect()).collect()
}

/// Expands a turn into its events, ending with `MessageStop` or the error.
fn script_events(
    turn: &FakeTurn,
    call: usize,
    model: &str,
) -> Vec<Result<StreamEvent, ProviderError>> {
    let mut out = vec![Ok(StreamEvent::MessageStart {
        id: Some(format!("fake_msg_{call}")),
        model: Some(model.to_owned()),
    })];
    let size = turn.chunk_size;
    let mut has_tool = false;
    for (index, block) in turn.content.iter().enumerate() {
        match block {
            FakeBlock::Text(text) => {
                out.extend(
                    chunks(text, size)
                        .into_iter()
                        .map(|text| Ok(StreamEvent::TextDelta { index, text })),
                );
            }
            FakeBlock::Reasoning { text, signature } => {
                out.extend(
                    chunks(text, size)
                        .into_iter()
                        .map(|text| Ok(StreamEvent::ReasoningDelta { index, text })),
                );
                if let Some(signature) = signature {
                    out.push(Ok(StreamEvent::ReasoningSignature {
                        index,
                        signature: signature.clone(),
                    }));
                }
            }
            FakeBlock::ToolUse { id, name, input } => {
                has_tool = true;
                let id = id
                    .clone()
                    .unwrap_or_else(|| format!("fake_call_{call}_{index}"));
                out.push(Ok(StreamEvent::ToolUseStart {
                    index,
                    id,
                    name: name.clone(),
                }));
                let json = if input.is_null() {
                    "{}".to_owned()
                } else {
                    input.to_string()
                };
                out.extend(chunks(&json, size).into_iter().map(|partial_json| {
                    Ok(StreamEvent::ToolInputDelta {
                        index,
                        partial_json,
                    })
                }));
            }
            FakeBlock::Opaque { provider, data } => {
                out.push(Ok(StreamEvent::Opaque {
                    index,
                    provider: provider.clone(),
                    data: data.clone(),
                }));
            }
        }
        out.push(Ok(StreamEvent::BlockStop { index }));
    }
    match &turn.error {
        Some(e) => out.push(Err(e.to_provider_error())),
        None => {
            let kind = turn.stop.unwrap_or(if has_tool {
                StopKind::ToolUse
            } else {
                StopKind::EndTurn
            });
            out.push(Ok(StreamEvent::MessageStop {
                stop: StopReason::new(kind),
                usage: turn.usage,
            }));
        }
    }
    out
}

async fn wait(ms: u64, cancel: &CancellationToken) -> Result<(), ProviderError> {
    if ms == 0 {
        return if cancel.is_cancelled() {
            Err(ProviderError::Cancelled)
        } else {
            Ok(())
        };
    }
    tokio::select! {
        biased;
        _ = cancel.cancelled() => Err(ProviderError::Cancelled),
        _ = tokio::time::sleep(Duration::from_millis(ms)) => Ok(()),
    }
}

#[async_trait]
impl Provider for FakeProvider {
    fn name(&self) -> &str {
        "fake"
    }

    /// The fake provider receives the normalized request itself.
    fn request_body(&self, request: &ChatRequest) -> Option<serde_json::Value> {
        serde_json::to_value(request).ok()
    }

    fn capabilities(&self, _model: &str) -> Capabilities {
        self.capabilities
    }

    async fn stream(
        &self,
        request: ChatRequest,
        cancel: CancellationToken,
    ) -> Result<EventStream, ProviderError> {
        let model = request.model.clone();
        let (turn, call) = {
            let mut shared = self.lock();
            let system = request.system.as_deref().unwrap_or_default();
            let pick = shared
                .turns
                .iter()
                .position(|t| {
                    t.match_system
                        .as_deref()
                        .is_some_and(|m| system.contains(m))
                })
                .or_else(|| shared.turns.iter().position(|t| t.match_system.is_none()));
            shared.requests.push(request);
            shared.calls += 1;
            let call = shared.calls;
            let Some(turn) = pick.and_then(|i| shared.turns.remove(i)) else {
                return Err(ProviderError::Other(format!(
                    "fake provider script exhausted at call {call}"
                )));
            };
            (turn, call)
        };
        wait(turn.delay_ms, &cancel).await?;
        if turn.content.is_empty()
            && let Some(e) = &turn.error
        {
            return Err(e.to_provider_error());
        }
        let gap = turn.chunk_delay_ms;
        let events = script_events(&turn, call, &model);
        let stream = futures::stream::iter(events.into_iter().enumerate()).then(move |(i, ev)| {
            let cancel = cancel.clone();
            async move {
                wait(if i == 0 { 0 } else { gap }, &cancel).await?;
                ev
            }
        });
        // Stop after the first error (scripted or cancellation).
        let stream = stream.scan(false, |failed, item| {
            let out = if *failed { None } else { Some(item) };
            if let Some(Err(_)) = &out {
                *failed = true;
            }
            futures::future::ready(out)
        });
        Ok(Box::pin(stream))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ContentBlock, Message};
    use serde_json::json;

    fn req(text: &str) -> ChatRequest {
        let mut r = ChatRequest::new("fake-model");
        r.messages.push(Message::user(text));
        r
    }

    #[tokio::test]
    async fn replays_text_then_tool_call_and_records_requests() {
        let p = FakeProvider::with_turns([
            FakeTurn {
                chunk_size: 2,
                ..FakeTurn::text("Hello")
            },
            FakeTurn::tool_use("read", json!({"path": "a.rs"})),
        ]);
        let r1 = p
            .complete(req("one"), CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(r1.message.text(), "Hello");
        assert_eq!(r1.stop.kind, StopKind::EndTurn);
        let r2 = p
            .complete(req("two"), CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(r2.stop.kind, StopKind::ToolUse);
        assert_eq!(
            r2.message.content,
            vec![ContentBlock::tool_use(
                "fake_call_2_0",
                "read",
                json!({"path": "a.rs"})
            )]
        );
        let seen: Vec<String> = p.requests().iter().map(|r| r.messages[0].text()).collect();
        assert_eq!(seen, vec!["one", "two"]);
        assert_eq!(p.last_request().unwrap().model, "fake-model");
        assert_eq!(p.remaining(), 0);
        let e = p
            .complete(req("three"), CancellationToken::new())
            .await
            .unwrap_err();
        assert!(matches!(e, ProviderError::Other(m) if m.contains("exhausted")));
        assert_eq!(p.requests().len(), 3);
    }

    #[tokio::test]
    async fn chunking_streams_many_deltas() {
        let p = FakeProvider::with_turns([FakeTurn {
            chunk_size: 3,
            content: vec![
                FakeBlock::Text("abcdefg".into()),
                FakeBlock::ToolUse {
                    id: Some("x".into()),
                    name: "t".into(),
                    input: json!({"k": "vvvv"}),
                },
            ],
            ..FakeTurn::default()
        }]);
        let events: Vec<_> = p
            .stream(req("q"), CancellationToken::new())
            .await
            .unwrap()
            .collect()
            .await;
        let text: Vec<_> = events
            .iter()
            .filter_map(|e| match e {
                Ok(StreamEvent::TextDelta { text, .. }) => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(text, vec!["abc", "def", "g"]);
        let tool_deltas = events
            .iter()
            .filter(|e| matches!(e, Ok(StreamEvent::ToolInputDelta { .. })))
            .count();
        assert!(tool_deltas > 1);
        assert!(matches!(
            events.last(),
            Some(Ok(StreamEvent::MessageStop { .. }))
        ));
    }

    #[tokio::test]
    async fn errors_before_and_after_content() {
        let p = FakeProvider::with_turns([
            FakeTurn::error(FakeError {
                retry_after_ms: Some(100),
                ..FakeError::new(FakeErrorKind::RateLimited, "slow")
            }),
            FakeTurn {
                error: Some(FakeError::new(FakeErrorKind::Overloaded, "busy")),
                ..FakeTurn::text("par")
            },
        ]);
        let e = p
            .stream(req("a"), CancellationToken::new())
            .await
            .err()
            .unwrap();
        assert_eq!(
            e,
            ProviderError::RateLimited {
                message: "slow".into(),
                retry_after: Some(Duration::from_millis(100))
            }
        );
        let events: Vec<_> = p
            .stream(req("b"), CancellationToken::new())
            .await
            .unwrap()
            .collect()
            .await;
        assert!(
            events
                .iter()
                .any(|e| matches!(e, Ok(StreamEvent::TextDelta { .. })))
        );
        assert!(matches!(
            events.last(),
            Some(Err(ProviderError::Overloaded { .. }))
        ));
    }

    #[tokio::test(start_paused = true)]
    async fn delays_are_applied() {
        let p = FakeProvider::with_turns([FakeTurn {
            delay_ms: 1000,
            chunk_delay_ms: 100,
            chunk_size: 1,
            ..FakeTurn::text("abc")
        }]);
        let start = tokio::time::Instant::now();
        let r = p
            .complete(req("x"), CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(r.message.text(), "abc");
        // MessageStart immediately after the delay, then 5 more events
        // (3 deltas, BlockStop, MessageStop) 100ms apart.
        assert_eq!(start.elapsed(), Duration::from_millis(1500));
    }

    #[tokio::test]
    async fn match_system_turns_serve_matching_requests_in_any_position() {
        let p = FakeProvider::from_yaml(
            "responses:\n  - content: [ { text: one } ]\n  - match_system: handoff notes\n    content: [ { text: summary } ]\n  - content: [ { text: two } ]\n",
        )
        .unwrap();
        let ask = |system: &str| ChatRequest {
            system: Some(system.to_owned()),
            ..ChatRequest::new("m")
        };
        let cancel = CancellationToken::new();
        let one = p.complete(ask("agent"), cancel.clone()).await.unwrap();
        assert_eq!(one.message.text(), "one");
        let two = p.complete(ask("agent"), cancel.clone()).await.unwrap();
        assert_eq!(two.message.text(), "two");
        let summary = p
            .complete(ask("You write handoff notes"), cancel.clone())
            .await
            .unwrap();
        assert_eq!(summary.message.text(), "summary");
        assert_eq!(p.remaining(), 0);
    }

    #[tokio::test(start_paused = true)]
    async fn cancellation_interrupts_delays() {
        let p = FakeProvider::with_turns([
            FakeTurn {
                delay_ms: 10_000,
                ..FakeTurn::text("late")
            },
            FakeTurn {
                chunk_delay_ms: 10_000,
                ..FakeTurn::text("slow")
            },
        ]);
        let cancel = CancellationToken::new();
        let c = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(10)).await;
            c.cancel();
        });
        assert_eq!(
            p.stream(req("a"), cancel).await.err(),
            Some(ProviderError::Cancelled)
        );
        let cancel = CancellationToken::new();
        let mut s = p.stream(req("b"), cancel.clone()).await.unwrap();
        assert!(matches!(
            s.next().await,
            Some(Ok(StreamEvent::MessageStart { .. }))
        ));
        cancel.cancel();
        assert_eq!(s.next().await, Some(Err(ProviderError::Cancelled)));
        assert_eq!(s.next().await, None);
    }

    const YAML: &str = r#"
capabilities: { tools: true, context_window: 1000 }
responses:
  - delay_ms: 5
    content:
      - reasoning: { text: "Need the file", signature: sig-1 }
      - text: "Reading it now."
      - tool_use: { id: call_1, name: read, input: { path: src/main.rs } }
      - opaque: { provider: anthropic, data: { type: redacted_thinking, data: xyz } }
    usage: { input_tokens: 120, output_tokens: 30, cache_read_tokens: 7 }
  - error: { kind: server, status: 503, message: down, retry_after_ms: 20 }
  - content: [ { text: "Done." } ]
    stop: max_tokens
"#;

    #[tokio::test]
    async fn yaml_script_drives_the_provider() {
        let p = FakeProvider::from_yaml(YAML).unwrap();
        let caps = p.capabilities("any");
        assert!(caps.tools && !caps.images);
        assert_eq!(caps.context_window, 1000);
        let r = p
            .complete(req("go"), CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(
            r.message.content,
            vec![
                ContentBlock::Reasoning {
                    text: "Need the file".into(),
                    signature: Some("sig-1".into())
                },
                ContentBlock::text("Reading it now."),
                ContentBlock::tool_use("call_1", "read", json!({"path": "src/main.rs"})),
                ContentBlock::Opaque {
                    provider: "anthropic".into(),
                    data: json!({"type": "redacted_thinking", "data": "xyz"})
                },
            ]
        );
        assert_eq!(r.stop.kind, StopKind::ToolUse);
        assert_eq!(r.usage.cache_read_tokens, 7);
        let e = p
            .complete(req("again"), CancellationToken::new())
            .await
            .unwrap_err();
        assert_eq!(
            e,
            ProviderError::Http {
                status: 503,
                message: "down".into(),
                retry_after: Some(Duration::from_millis(20))
            }
        );
        let r = p
            .complete(req("last"), CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(
            (r.message.text().as_str(), r.stop.kind),
            ("Done.", StopKind::MaxTokens)
        );
    }

    #[test]
    fn yaml_errors_are_reported_and_files_load() {
        assert!(FakeScript::from_yaml("responses: [ { bogus: 1 } ]").is_err());
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.yaml");
        std::fs::write(&path, YAML).unwrap();
        assert_eq!(
            FakeScript::from_yaml_file(&path).unwrap().responses.len(),
            3
        );
        assert!(FakeScript::from_yaml_file(dir.path().join("missing.yaml")).is_err());
    }

    #[test]
    fn every_error_kind_maps() {
        use FakeErrorKind::*;
        for kind in [
            RateLimited,
            Overloaded,
            Server,
            Auth,
            InvalidRequest,
            Connection,
            Timeout,
            Stalled,
            Other,
        ] {
            let e = FakeError::new(kind, "m").to_provider_error();
            let retryable = matches!(
                kind,
                RateLimited | Overloaded | Server | Connection | Timeout | Stalled
            );
            assert_eq!(e.retry_reason().is_some(), retryable, "{kind:?}");
        }
    }
}
