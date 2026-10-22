//! The [`Provider`] trait, normalized stream events, errors and a response
//! accumulator.
//!
//! A provider turns a [`ChatRequest`] into a stream of [`StreamEvent`]s. The
//! stream always ends with [`StreamEvent::MessageStop`], which carries the
//! normalized [`StopReason`] and the final [`Usage`] record, or with an error.
//! [`collect`] folds a stream into a complete [`Response`].

use std::collections::BTreeMap;
use std::pin::Pin;
use std::time::Duration;

use async_trait::async_trait;
use futures::{Stream, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::model::{Capabilities, ChatRequest, ContentBlock, Message, Role, StopReason, Usage};
use crate::retry::{RetryEvent, RetryReason};

/// Errors from a provider call. Retryable variants are listed in
/// [`ProviderError::retry_reason`].
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum ProviderError {
    /// HTTP 429.
    #[error("rate limited: {message}")]
    RateLimited {
        /// Provider message.
        message: String,
        /// Server-requested wait, from `retry-after` headers.
        retry_after: Option<Duration>,
    },
    /// The provider is overloaded (HTTP 529, or an `overloaded` error body or
    /// stream event).
    #[error("provider overloaded: {message}")]
    Overloaded {
        /// Provider message.
        message: String,
        /// Server-requested wait, if any.
        retry_after: Option<Duration>,
    },
    /// HTTP 401 or 403.
    #[error("authentication failed: {0}")]
    Auth(String),
    /// The ChatGPT plan's usage limit for this app was reached
    /// (`subscription_sharing_usage_limit_exceeded`). Never retried, and
    /// never answered by switching to another credential: the user decides,
    /// starting from [`ProviderError::action`] (Manage usage).
    #[error(
        "ChatGPT plan usage limit reached: {message}. Manage usage at {manage_url}; harness does not switch to an API key on its own"
    )]
    UsageLimit {
        /// Provider message.
        message: String,
        /// Where the user manages plan usage.
        manage_url: String,
    },
    /// Any other non-success HTTP status.
    #[error("HTTP {status}: {message}")]
    Http {
        /// Status code.
        status: u16,
        /// Provider message or truncated body.
        message: String,
        /// Server-requested wait, if any.
        retry_after: Option<Duration>,
    },
    /// Connecting or reading failed at the transport level.
    #[error("connection error: {0}")]
    Connection(String),
    /// The whole request exceeded its timeout.
    #[error("request timed out after {0:?}")]
    Timeout(Duration),
    /// No bytes arrived for the configured stall interval.
    #[error("stream stalled: no data for {0:?}")]
    Stalled(Duration),
    /// The call was cancelled by the caller.
    #[error("call cancelled")]
    Cancelled,
    /// The provider sent something that could not be decoded.
    #[error("invalid provider response: {0}")]
    Decode(String),
    /// The request was rejected or could not be built.
    #[error("invalid request: {0}")]
    InvalidRequest(String),
    /// Retries were exhausted; `last` is the final error.
    #[error("gave up after {attempts} attempts: {last}")]
    RetriesExhausted {
        /// Total attempts made (first try plus retries).
        attempts: u32,
        /// The last error seen.
        last: Box<ProviderError>,
    },
    /// Anything else.
    #[error("{0}")]
    Other(String),
}

impl ProviderError {
    /// Maps a non-success HTTP status and its decoded message.
    pub fn from_status(
        status: u16,
        message: impl Into<String>,
        retry_after: Option<Duration>,
    ) -> Self {
        let message = message.into();
        match status {
            429 => Self::RateLimited {
                message,
                retry_after,
            },
            529 => Self::Overloaded {
                message,
                retry_after,
            },
            401 | 403 => Self::Auth(message),
            _ if message.to_ascii_lowercase().contains("overloaded") => Self::Overloaded {
                message,
                retry_after,
            },
            _ => Self::Http {
                status,
                message,
                retry_after,
            },
        }
    }

    /// Why this error may be retried, or `None` if it is permanent.
    ///
    /// Retryable: rate limits, overload, HTTP 408/409/5xx, connection
    /// failures, timeouts and stalls.
    pub fn retry_reason(&self) -> Option<RetryReason> {
        match self {
            Self::RateLimited { .. } => Some(RetryReason::RateLimited),
            Self::Overloaded { .. } => Some(RetryReason::Overloaded),
            Self::Http { status, .. } if *status >= 500 || *status == 408 || *status == 409 => {
                Some(RetryReason::Server(*status))
            }
            Self::Connection(_) => Some(RetryReason::Connection),
            Self::Timeout(_) => Some(RetryReason::Timeout),
            Self::Stalled(_) => Some(RetryReason::Stalled),
            _ => None,
        }
    }

    /// Server-requested wait before retrying, if one was sent.
    pub fn retry_after(&self) -> Option<Duration> {
        match self {
            Self::RateLimited { retry_after, .. }
            | Self::Overloaded { retry_after, .. }
            | Self::Http { retry_after, .. } => *retry_after,
            _ => None,
        }
    }

    /// The action a UI should offer first for this error, if any: for
    /// [`ProviderError::UsageLimit`] it is *Manage usage*, opening the
    /// ChatGPT usage settings.
    pub fn action(&self) -> Option<ErrorAction> {
        match self.root() {
            Self::UsageLimit { manage_url, .. } => Some(ErrorAction {
                label: "Manage usage",
                url: manage_url.clone(),
            }),
            _ => None,
        }
    }

    /// The underlying error, unwrapping [`ProviderError::RetriesExhausted`].
    pub fn root(&self) -> &ProviderError {
        match self {
            Self::RetriesExhausted { last, .. } => last.root(),
            other => other,
        }
    }
}

/// A follow-up action for an error, such as *Manage usage* for a ChatGPT
/// plan usage limit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ErrorAction {
    /// Button label.
    pub label: &'static str,
    /// URL the action opens in the browser.
    pub url: String,
}

/// One normalized streaming event.
///
/// `index` identifies a content block within the assistant message; deltas
/// for the same index belong to the same block and blocks are ordered by
/// index. A block is created by its first event.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum StreamEvent {
    /// The provider accepted the call and started a message.
    MessageStart {
        /// Provider message id.
        #[serde(default)]
        id: Option<String>,
        /// Model that actually served the call.
        #[serde(default)]
        model: Option<String>,
    },
    /// Text appended to a text block.
    TextDelta {
        /// Block index.
        index: usize,
        /// Appended text.
        text: String,
    },
    /// Text appended to a reasoning block.
    ReasoningDelta {
        /// Block index.
        index: usize,
        /// Appended reasoning text.
        text: String,
    },
    /// Signature for a reasoning block (may arrive after its text).
    ReasoningSignature {
        /// Block index.
        index: usize,
        /// Signature to round-trip.
        signature: String,
    },
    /// A tool call block started.
    ToolUseStart {
        /// Block index.
        index: usize,
        /// Call id.
        id: String,
        /// Tool name.
        name: String,
    },
    /// A fragment of a tool call's JSON arguments.
    ToolInputDelta {
        /// Block index.
        index: usize,
        /// Raw JSON fragment.
        partial_json: String,
    },
    /// A complete provider-opaque block to round-trip.
    Opaque {
        /// Block index.
        index: usize,
        /// Provider name.
        provider: String,
        /// Raw provider JSON.
        data: Value,
    },
    /// A block is complete.
    BlockStop {
        /// Block index.
        index: usize,
    },
    /// The transport is retrying the call. If `discard_partial` is set,
    /// every event received since the last `MessageStart` belongs to the
    /// aborted attempt and must be discarded.
    Retry(RetryEvent),
    /// The message is complete. Always the last event of a successful stream.
    MessageStop {
        /// Normalized stop reason with the raw value.
        stop: StopReason,
        /// Final usage record for the call.
        usage: Usage,
    },
}

/// A boxed stream of normalized events.
pub type EventStream = Pin<Box<dyn Stream<Item = Result<StreamEvent, ProviderError>> + Send>>;

/// A complete, accumulated model response.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Response {
    /// Provider message id.
    pub id: Option<String>,
    /// Model that served the call.
    pub model: Option<String>,
    /// The assistant message.
    pub message: Message,
    /// Why generation stopped.
    pub stop: StopReason,
    /// Final usage.
    pub usage: Usage,
    /// Retries that happened during the call.
    pub retries: Vec<RetryEvent>,
}

/// A model provider.
///
/// Implementations must honour `cancel`: once it fires, the returned stream
/// should end promptly with [`ProviderError::Cancelled`].
#[async_trait]
pub trait Provider: Send + Sync {
    /// Provider name as used in `harness.ini`, e.g. `anthropic`.
    fn name(&self) -> &str;

    /// What `model` supports through this provider.
    fn capabilities(&self, model: &str) -> Capabilities;

    /// Starts a streaming call.
    async fn stream(
        &self,
        request: ChatRequest,
        cancel: CancellationToken,
    ) -> Result<EventStream, ProviderError>;

    /// The body this provider would send for `request` (redaction is the
    /// caller's job), for the request inspector. `None` when it has no
    /// wire format of its own.
    fn request_body(&self, _request: &ChatRequest) -> Option<Value> {
        None
    }

    /// Runs a call to completion and accumulates the response.
    async fn complete(
        &self,
        request: ChatRequest,
        cancel: CancellationToken,
    ) -> Result<Response, ProviderError> {
        collect(self.stream(request, cancel).await?).await
    }
}

#[derive(Debug)]
enum Partial {
    Text(String),
    Reasoning {
        text: String,
        signature: Option<String>,
    },
    ToolUse {
        id: String,
        name: String,
        json: String,
    },
    Opaque {
        provider: String,
        data: Value,
    },
}

/// Folds [`StreamEvent`]s into a [`Response`].
///
/// Tool arguments are parsed when the response is finished. Empty arguments
/// become `{}`; arguments that are not valid JSON are kept as a JSON string
/// so that the agent can report an actionable tool error.
#[derive(Debug, Default)]
pub struct ResponseBuilder {
    id: Option<String>,
    model: Option<String>,
    blocks: BTreeMap<usize, Partial>,
    stop: Option<(StopReason, Usage)>,
    retries: Vec<RetryEvent>,
}

impl ResponseBuilder {
    /// An empty builder.
    pub fn new() -> Self {
        Self::default()
    }

    /// Applies one event.
    pub fn apply(&mut self, event: StreamEvent) -> Result<(), ProviderError> {
        match event {
            StreamEvent::MessageStart { id, model } => {
                self.id = id;
                self.model = model;
            }
            StreamEvent::TextDelta { index, text } => {
                match self
                    .blocks
                    .entry(index)
                    .or_insert_with(|| Partial::Text(String::new()))
                {
                    Partial::Text(t) => t.push_str(&text),
                    _ => return Err(mismatch(index, "text")),
                }
            }
            StreamEvent::ReasoningDelta { index, text } => self.reasoning(index)?.0.push_str(&text),
            StreamEvent::ReasoningSignature { index, signature } => {
                *self.reasoning(index)?.1 = Some(signature);
            }
            StreamEvent::ToolUseStart { index, id, name } => {
                self.blocks.insert(
                    index,
                    Partial::ToolUse {
                        id,
                        name,
                        json: String::new(),
                    },
                );
            }
            StreamEvent::ToolInputDelta {
                index,
                partial_json,
            } => match self.blocks.get_mut(&index) {
                Some(Partial::ToolUse { json, .. }) => json.push_str(&partial_json),
                _ => return Err(mismatch(index, "tool_use")),
            },
            StreamEvent::Opaque {
                index,
                provider,
                data,
            } => {
                self.blocks
                    .insert(index, Partial::Opaque { provider, data });
            }
            StreamEvent::BlockStop { .. } => {}
            StreamEvent::Retry(event) => {
                if event.discard_partial {
                    self.blocks.clear();
                    self.stop = None;
                }
                self.retries.push(event);
            }
            StreamEvent::MessageStop { stop, usage } => self.stop = Some((stop, usage)),
        }
        Ok(())
    }

    fn reasoning(
        &mut self,
        index: usize,
    ) -> Result<(&mut String, &mut Option<String>), ProviderError> {
        match self
            .blocks
            .entry(index)
            .or_insert_with(|| Partial::Reasoning {
                text: String::new(),
                signature: None,
            }) {
            Partial::Reasoning { text, signature } => Ok((text, signature)),
            _ => Err(mismatch(index, "reasoning")),
        }
    }

    /// True once [`StreamEvent::MessageStop`] has been applied.
    pub fn is_complete(&self) -> bool {
        self.stop.is_some()
    }

    /// Builds the response. Fails if no `MessageStop` was received.
    pub fn finish(self) -> Result<Response, ProviderError> {
        let (stop, usage) = self
            .stop
            .ok_or_else(|| ProviderError::Decode("stream ended without a stop event".into()))?;
        let content = self
            .blocks
            .into_values()
            .map(|p| match p {
                Partial::Text(text) => ContentBlock::Text { text },
                Partial::Reasoning { text, signature } => {
                    ContentBlock::Reasoning { text, signature }
                }
                Partial::ToolUse { id, name, json } => {
                    let input = if json.trim().is_empty() {
                        Value::Object(Default::default())
                    } else {
                        serde_json::from_str(&json).unwrap_or(Value::String(json))
                    };
                    ContentBlock::ToolUse { id, name, input }
                }
                Partial::Opaque { provider, data } => ContentBlock::Opaque { provider, data },
            })
            .collect();
        Ok(Response {
            id: self.id,
            model: self.model,
            message: Message {
                role: Role::Assistant,
                content,
            },
            stop,
            usage,
            retries: self.retries,
        })
    }
}

fn mismatch(index: usize, expected: &str) -> ProviderError {
    ProviderError::Decode(format!(
        "block {index} received a {expected} event but has another kind"
    ))
}

/// Drains `stream` into a [`Response`].
pub async fn collect(mut stream: EventStream) -> Result<Response, ProviderError> {
    let mut builder = ResponseBuilder::new();
    while let Some(event) = stream.next().await {
        builder.apply(event?)?;
    }
    builder.finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::StopKind;
    use serde_json::json;

    fn ev(events: Vec<StreamEvent>) -> EventStream {
        Box::pin(futures::stream::iter(events.into_iter().map(Ok)))
    }

    fn stop(usage: Usage) -> StreamEvent {
        StreamEvent::MessageStop {
            stop: StopReason::from_anthropic("tool_use"),
            usage,
        }
    }

    #[tokio::test]
    async fn collect_builds_all_block_kinds_and_final_usage() {
        let usage = Usage {
            input_tokens: 10,
            output_tokens: 20,
            cache_read_tokens: 3,
            cache_write_tokens: 4,
            reasoning_tokens: 5,
        };
        let r = collect(ev(vec![
            StreamEvent::MessageStart {
                id: Some("m1".into()),
                model: Some("x".into()),
            },
            StreamEvent::ReasoningDelta {
                index: 0,
                text: "hm".into(),
            },
            StreamEvent::ReasoningDelta {
                index: 0,
                text: "m".into(),
            },
            StreamEvent::ReasoningSignature {
                index: 0,
                signature: "sig".into(),
            },
            StreamEvent::BlockStop { index: 0 },
            StreamEvent::TextDelta {
                index: 1,
                text: "Hel".into(),
            },
            StreamEvent::TextDelta {
                index: 1,
                text: "lo".into(),
            },
            StreamEvent::ToolUseStart {
                index: 2,
                id: "t1".into(),
                name: "read".into(),
            },
            StreamEvent::ToolInputDelta {
                index: 2,
                partial_json: "{\"pa".into(),
            },
            StreamEvent::ToolInputDelta {
                index: 2,
                partial_json: "th\":\"a\"}".into(),
            },
            StreamEvent::ToolUseStart {
                index: 3,
                id: "t2".into(),
                name: "ls".into(),
            },
            StreamEvent::Opaque {
                index: 4,
                provider: "anthropic".into(),
                data: json!({"type": "redacted_thinking"}),
            },
            stop(usage),
        ]))
        .await
        .unwrap();
        assert_eq!(r.id.as_deref(), Some("m1"));
        assert_eq!(r.usage, usage);
        assert_eq!(r.stop.kind, StopKind::ToolUse);
        assert_eq!(r.stop.raw.as_deref(), Some("tool_use"));
        assert_eq!(
            r.message.content,
            vec![
                ContentBlock::Reasoning {
                    text: "hmm".into(),
                    signature: Some("sig".into())
                },
                ContentBlock::text("Hello"),
                ContentBlock::tool_use("t1", "read", json!({"path": "a"})),
                ContentBlock::tool_use("t2", "ls", json!({})),
                ContentBlock::Opaque {
                    provider: "anthropic".into(),
                    data: json!({"type": "redacted_thinking"})
                },
            ]
        );
    }

    #[tokio::test]
    async fn invalid_tool_json_is_kept_as_string() {
        let r = collect(ev(vec![
            StreamEvent::ToolUseStart {
                index: 0,
                id: "t".into(),
                name: "x".into(),
            },
            StreamEvent::ToolInputDelta {
                index: 0,
                partial_json: "{bad".into(),
            },
            stop(Usage::default()),
        ]))
        .await
        .unwrap();
        assert_eq!(
            r.message.content[0],
            ContentBlock::tool_use("t", "x", json!("{bad"))
        );
    }

    #[tokio::test]
    async fn retry_with_discard_drops_partial_output() {
        let retry = RetryEvent {
            attempt: 1,
            max_retries: 3,
            delay: Duration::from_millis(5),
            reason: RetryReason::Stalled,
            error: "stalled".into(),
            discard_partial: true,
        };
        let r = collect(ev(vec![
            StreamEvent::TextDelta {
                index: 0,
                text: "partial".into(),
            },
            StreamEvent::Retry(retry.clone()),
            StreamEvent::TextDelta {
                index: 0,
                text: "full".into(),
            },
            stop(Usage::default()),
        ]))
        .await
        .unwrap();
        assert_eq!(r.message.text(), "full");
        assert_eq!(r.retries, vec![retry]);
    }

    #[tokio::test]
    async fn missing_stop_or_mismatched_block_is_an_error() {
        let e = collect(ev(vec![StreamEvent::TextDelta {
            index: 0,
            text: "x".into(),
        }]))
        .await
        .unwrap_err();
        assert!(matches!(e, ProviderError::Decode(_)));
        let e = collect(ev(vec![
            StreamEvent::TextDelta {
                index: 0,
                text: "x".into(),
            },
            StreamEvent::ReasoningDelta {
                index: 0,
                text: "y".into(),
            },
        ]))
        .await
        .unwrap_err();
        assert!(matches!(e, ProviderError::Decode(_)));
        let e = collect(Box::pin(futures::stream::iter(vec![Err(
            ProviderError::Cancelled,
        )])))
        .await
        .unwrap_err();
        assert_eq!(e, ProviderError::Cancelled);
    }

    #[test]
    fn status_mapping_and_retryability() {
        let ra = Some(Duration::from_secs(2));
        assert!(matches!(
            ProviderError::from_status(429, "x", ra),
            ProviderError::RateLimited { .. }
        ));
        assert!(matches!(
            ProviderError::from_status(529, "x", None),
            ProviderError::Overloaded { .. }
        ));
        assert!(matches!(
            ProviderError::from_status(500, "Overloaded", None),
            ProviderError::Overloaded { .. }
        ));
        assert!(matches!(
            ProviderError::from_status(401, "x", None),
            ProviderError::Auth(_)
        ));
        assert_eq!(ProviderError::from_status(429, "x", ra).retry_after(), ra);
        assert_eq!(
            ProviderError::from_status(503, "x", None).retry_reason(),
            Some(RetryReason::Server(503))
        );
        assert_eq!(
            ProviderError::from_status(400, "x", None).retry_reason(),
            None
        );
        assert_eq!(
            ProviderError::Stalled(Duration::ZERO).retry_reason(),
            Some(RetryReason::Stalled)
        );
        assert_eq!(ProviderError::Cancelled.retry_reason(), None);
        let wrapped = ProviderError::RetriesExhausted {
            attempts: 3,
            last: Box::new(ProviderError::Cancelled),
        };
        assert_eq!(wrapped.root(), &ProviderError::Cancelled);
    }

    struct Echo;

    #[async_trait]
    impl Provider for Echo {
        fn name(&self) -> &str {
            "echo"
        }
        fn capabilities(&self, _model: &str) -> Capabilities {
            Capabilities {
                tools: true,
                context_window: 1000,
                ..Default::default()
            }
        }
        async fn stream(
            &self,
            request: ChatRequest,
            _cancel: CancellationToken,
        ) -> Result<EventStream, ProviderError> {
            let text = request
                .messages
                .last()
                .map(Message::text)
                .unwrap_or_default();
            Ok(ev(vec![
                StreamEvent::TextDelta { index: 0, text },
                StreamEvent::MessageStop {
                    stop: StopReason::new(StopKind::EndTurn),
                    usage: Usage::default(),
                },
            ]))
        }
    }

    #[tokio::test]
    async fn provider_trait_is_object_safe_and_complete_works() {
        let p: Box<dyn Provider> = Box::new(Echo);
        let mut req = ChatRequest::new("m");
        req.messages.push(Message::user("ping"));
        let r = p.complete(req, CancellationToken::new()).await.unwrap();
        assert_eq!(r.message.text(), "ping");
        assert!(p.capabilities("m").tools);
    }
}
