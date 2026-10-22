//! Decodes Anthropic Messages API SSE events into [`StreamEvent`]s.
//!
//! Text, thinking, signature and tool-argument deltas are forwarded as they
//! arrive, so reasoning is shown live rather than at block end. Blocks the
//! harness cannot interpret (`redacted_thinking`, server tool blocks and
//! future types) are buffered and emitted once, complete, as
//! [`StreamEvent::Opaque`] when the block stops.

use std::collections::{HashMap, VecDeque};

use futures::{Stream, StreamExt};
use serde_json::Value;

use crate::model::{StopReason, Usage};
use crate::provider::{ProviderError, StreamEvent};
use crate::sse::SseEvent;

use super::request::PROVIDER;

#[derive(Debug)]
enum Block {
    Text,
    Thinking,
    ToolUse,
    Opaque { data: Value, json: String },
}

/// Per-attempt decoder state. Build a fresh one for every attempt.
#[derive(Debug, Default)]
pub struct Decoder {
    blocks: HashMap<usize, Block>,
    usage: Usage,
    stop: Option<StopReason>,
    done: bool,
}

fn decode_err(what: &str, e: impl std::fmt::Display) -> ProviderError {
    ProviderError::Decode(format!("anthropic {what}: {e}"))
}

fn u64_field(v: &Value, key: &str) -> Option<u64> {
    v.get(key).and_then(Value::as_u64)
}

/// Maps an `error` stream event or error body to a typed error. Overload,
/// rate-limit and server errors are retryable.
pub fn stream_error(error: &Value) -> ProviderError {
    let kind = error.get("type").and_then(Value::as_str).unwrap_or("");
    let message = error
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or(kind)
        .to_owned();
    match kind {
        "overloaded_error" => ProviderError::Overloaded {
            message,
            retry_after: None,
        },
        "rate_limit_error" => ProviderError::RateLimited {
            message,
            retry_after: None,
        },
        "api_error" => ProviderError::Http {
            status: 500,
            message,
            retry_after: None,
        },
        "timeout_error" => ProviderError::Http {
            status: 504,
            message,
            retry_after: None,
        },
        "authentication_error" | "permission_error" => ProviderError::Auth(message),
        "invalid_request_error" | "request_too_large" | "not_found_error" => {
            ProviderError::InvalidRequest(message)
        }
        _ => ProviderError::Other(format!("anthropic {kind}: {message}")),
    }
}

impl Decoder {
    /// A fresh decoder.
    pub fn new() -> Self {
        Self::default()
    }

    /// True once `message_stop` has been decoded.
    pub fn is_done(&self) -> bool {
        self.done
    }

    fn update_usage(&mut self, u: &Value) {
        if let Some(n) = u64_field(u, "input_tokens") {
            self.usage.input_tokens = n;
        }
        if let Some(n) = u64_field(u, "output_tokens") {
            self.usage.output_tokens = n;
        }
        if let Some(n) = u64_field(u, "cache_read_input_tokens") {
            self.usage.cache_read_tokens = n;
        }
        if let Some(n) = u64_field(u, "cache_creation_input_tokens") {
            self.usage.cache_write_tokens = n;
        }
    }

    /// Decodes one SSE event into zero or more stream events.
    pub fn decode(&mut self, event: &SseEvent) -> Result<Vec<StreamEvent>, ProviderError> {
        if event.data.trim().is_empty() {
            return Ok(Vec::new());
        }
        let v: Value =
            serde_json::from_str(&event.data).map_err(|e| decode_err("event JSON", e))?;
        let kind = v
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or(event.event.as_str());
        let index = || {
            v.get("index")
                .and_then(Value::as_u64)
                .map(|i| i as usize)
                .ok_or_else(|| decode_err(kind, "missing index"))
        };
        let mut out = Vec::new();
        match kind {
            "message_start" => {
                let msg = v.get("message").unwrap_or(&Value::Null);
                if let Some(u) = msg.get("usage") {
                    self.update_usage(u);
                }
                out.push(StreamEvent::MessageStart {
                    id: msg.get("id").and_then(Value::as_str).map(str::to_owned),
                    model: msg.get("model").and_then(Value::as_str).map(str::to_owned),
                });
            }
            "content_block_start" => {
                let index = index()?;
                let block = v.get("content_block").cloned().unwrap_or(Value::Null);
                let str_of = |k: &str| block.get(k).and_then(Value::as_str).unwrap_or("");
                match str_of("type") {
                    "text" => {
                        self.blocks.insert(index, Block::Text);
                        if !str_of("text").is_empty() {
                            out.push(StreamEvent::TextDelta {
                                index,
                                text: str_of("text").to_owned(),
                            });
                        }
                    }
                    "thinking" => {
                        self.blocks.insert(index, Block::Thinking);
                        if !str_of("thinking").is_empty() {
                            out.push(StreamEvent::ReasoningDelta {
                                index,
                                text: str_of("thinking").to_owned(),
                            });
                        }
                        if !str_of("signature").is_empty() {
                            out.push(StreamEvent::ReasoningSignature {
                                index,
                                signature: str_of("signature").to_owned(),
                            });
                        }
                    }
                    "tool_use" => {
                        self.blocks.insert(index, Block::ToolUse);
                        out.push(StreamEvent::ToolUseStart {
                            index,
                            id: str_of("id").to_owned(),
                            name: str_of("name").to_owned(),
                        });
                    }
                    _ => {
                        self.blocks.insert(
                            index,
                            Block::Opaque {
                                data: block,
                                json: String::new(),
                            },
                        );
                    }
                }
            }
            "content_block_delta" => {
                let index = index()?;
                let delta = v.get("delta").unwrap_or(&Value::Null);
                let text_of = |k: &str| {
                    delta
                        .get(k)
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_owned()
                };
                match (
                    delta.get("type").and_then(Value::as_str),
                    self.blocks.get_mut(&index),
                ) {
                    (Some("text_delta"), _) => out.push(StreamEvent::TextDelta {
                        index,
                        text: text_of("text"),
                    }),
                    (Some("thinking_delta"), _) => out.push(StreamEvent::ReasoningDelta {
                        index,
                        text: text_of("thinking"),
                    }),
                    (Some("signature_delta"), _) => out.push(StreamEvent::ReasoningSignature {
                        index,
                        signature: text_of("signature"),
                    }),
                    (Some("input_json_delta"), Some(Block::Opaque { json, .. })) => {
                        json.push_str(&text_of("partial_json"));
                    }
                    (Some("input_json_delta"), _) => out.push(StreamEvent::ToolInputDelta {
                        index,
                        partial_json: text_of("partial_json"),
                    }),
                    // citations_delta and future delta types carry nothing
                    // the normalized model represents.
                    _ => {}
                }
            }
            "content_block_stop" => {
                let index = index()?;
                if let Some(Block::Opaque { mut data, json }) = self.blocks.remove(&index) {
                    if !json.trim().is_empty() {
                        let input: Value = serde_json::from_str(&json)
                            .map_err(|e| decode_err("server tool input", e))?;
                        data["input"] = input;
                    }
                    out.push(StreamEvent::Opaque {
                        index,
                        provider: PROVIDER.to_owned(),
                        data,
                    });
                }
                out.push(StreamEvent::BlockStop { index });
            }
            "message_delta" => {
                if let Some(u) = v.get("usage") {
                    self.update_usage(u);
                }
                if let Some(r) = v
                    .get("delta")
                    .and_then(|d| d.get("stop_reason"))
                    .and_then(Value::as_str)
                {
                    self.stop = Some(StopReason::from_anthropic(r));
                }
            }
            "message_stop" => {
                self.done = true;
                let stop = self
                    .stop
                    .take()
                    .unwrap_or_else(|| StopReason::from_anthropic("end_turn"));
                out.push(StreamEvent::MessageStop {
                    stop,
                    usage: self.usage,
                });
            }
            "error" => return Err(stream_error(v.get("error").unwrap_or(&Value::Null))),
            // ping and unknown future events.
            _ => {}
        }
        Ok(out)
    }
}

/// Adapts an SSE stream into normalized events. A stream that ends before
/// `message_stop` fails with a retryable [`ProviderError::Connection`].
pub fn decode_stream<S>(sse: S) -> impl Stream<Item = Result<StreamEvent, ProviderError>> + Send
where
    S: Stream<Item = Result<SseEvent, ProviderError>> + Send + 'static,
{
    struct St<S> {
        sse: std::pin::Pin<Box<S>>,
        decoder: Decoder,
        pending: VecDeque<StreamEvent>,
        finished: bool,
    }
    let st = St {
        sse: Box::pin(sse),
        decoder: Decoder::new(),
        pending: VecDeque::new(),
        finished: false,
    };
    futures::stream::unfold(st, |mut st| async move {
        loop {
            if let Some(ev) = st.pending.pop_front() {
                return Some((Ok(ev), st));
            }
            if st.finished || st.decoder.is_done() {
                return None;
            }
            match st.sse.next().await {
                Some(Ok(event)) => match st.decoder.decode(&event) {
                    Ok(evs) => st.pending.extend(evs),
                    Err(e) => {
                        st.finished = true;
                        return Some((Err(e), st));
                    }
                },
                Some(Err(e)) => {
                    st.finished = true;
                    return Some((Err(e), st));
                }
                None => {
                    st.finished = true;
                    return Some((
                        Err(ProviderError::Connection(
                            "anthropic stream ended before message_stop".into(),
                        )),
                        st,
                    ));
                }
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::StopKind;
    use serde_json::json;

    fn ev(data: Value) -> SseEvent {
        SseEvent {
            event: data["type"].as_str().unwrap().to_owned(),
            data: data.to_string(),
            id: None,
            retry: None,
        }
    }

    fn run(events: Vec<Value>) -> Result<Vec<StreamEvent>, ProviderError> {
        let mut d = Decoder::new();
        let mut out = Vec::new();
        for e in events {
            out.extend(d.decode(&ev(e))?);
        }
        Ok(out)
    }

    #[test]
    fn usage_combines_message_start_and_delta_with_cache_counters() {
        let out = run(vec![
            json!({"type": "message_start", "message": {"id": "msg_1", "model": "claude-opus-5-5",
                "usage": {"input_tokens": 12, "output_tokens": 1,
                          "cache_read_input_tokens": 9000, "cache_creation_input_tokens": 300}}}),
            json!({"type": "ping"}),
            json!({"type": "message_delta", "delta": {"stop_reason": "end_turn"}, "usage": {"output_tokens": 42}}),
            json!({"type": "message_stop"}),
        ])
        .unwrap();
        assert_eq!(
            out[0],
            StreamEvent::MessageStart {
                id: Some("msg_1".into()),
                model: Some("claude-opus-5-5".into())
            }
        );
        match &out[1] {
            StreamEvent::MessageStop { stop, usage } => {
                assert_eq!(stop.kind, StopKind::EndTurn);
                assert_eq!(usage.input_tokens, 12);
                assert_eq!(usage.output_tokens, 42);
                assert_eq!(usage.cache_read_tokens, 9000);
                assert_eq!(usage.cache_write_tokens, 300);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn thinking_streams_live_and_redacted_blocks_are_opaque() {
        let out = run(vec![
            json!({"type": "content_block_start", "index": 0, "content_block": {"type": "thinking", "thinking": "", "signature": ""}}),
            json!({"type": "content_block_delta", "index": 0, "delta": {"type": "thinking_delta", "thinking": "Let me "}}),
            json!({"type": "content_block_delta", "index": 0, "delta": {"type": "thinking_delta", "thinking": "think"}}),
            json!({"type": "content_block_delta", "index": 0, "delta": {"type": "signature_delta", "signature": "SIG"}}),
            json!({"type": "content_block_stop", "index": 0}),
            json!({"type": "content_block_start", "index": 1, "content_block": {"type": "redacted_thinking", "data": "EuYB"}}),
            json!({"type": "content_block_stop", "index": 1}),
        ])
        .unwrap();
        assert_eq!(
            out,
            vec![
                StreamEvent::ReasoningDelta {
                    index: 0,
                    text: "Let me ".into()
                },
                StreamEvent::ReasoningDelta {
                    index: 0,
                    text: "think".into()
                },
                StreamEvent::ReasoningSignature {
                    index: 0,
                    signature: "SIG".into()
                },
                StreamEvent::BlockStop { index: 0 },
                StreamEvent::Opaque {
                    index: 1,
                    provider: "anthropic".into(),
                    data: json!({"type": "redacted_thinking", "data": "EuYB"}),
                },
                StreamEvent::BlockStop { index: 1 },
            ]
        );
    }

    #[test]
    fn server_tool_blocks_collect_their_input() {
        let out = run(vec![
            json!({"type": "content_block_start", "index": 2, "content_block": {"type": "server_tool_use", "id": "srv", "name": "web_search", "input": {}}}),
            json!({"type": "content_block_delta", "index": 2, "delta": {"type": "input_json_delta", "partial_json": "{\"q\":"}}),
            json!({"type": "content_block_delta", "index": 2, "delta": {"type": "input_json_delta", "partial_json": "\"rust\"}"}}),
            json!({"type": "content_block_stop", "index": 2}),
        ])
        .unwrap();
        assert_eq!(
            out[0],
            StreamEvent::Opaque {
                index: 2,
                provider: "anthropic".into(),
                data: json!({"type": "server_tool_use", "id": "srv", "name": "web_search", "input": {"q": "rust"}}),
            }
        );
    }

    #[test]
    fn error_events_map_to_typed_errors() {
        let err = run(vec![json!({"type": "error", "error": {"type": "overloaded_error", "message": "Overloaded"}})]).unwrap_err();
        assert!(matches!(err, ProviderError::Overloaded { .. }));
        assert!(err.retry_reason().is_some());
        assert!(matches!(
            stream_error(&json!({"type": "invalid_request_error", "message": "bad"})),
            ProviderError::InvalidRequest(_)
        ));
        assert!(
            stream_error(&json!({"type": "api_error", "message": "x"}))
                .retry_reason()
                .is_some()
        );
        assert!(matches!(
            run(vec![json!({"type": "content_block_start"})]),
            Err(ProviderError::Decode(_))
        ));
    }

    #[tokio::test]
    async fn truncated_stream_is_a_retryable_connection_error() {
        let events = vec![Ok(ev(
            json!({"type": "message_start", "message": {"id": "m"}}),
        ))];
        let out: Vec<_> = decode_stream(futures::stream::iter(events)).collect().await;
        assert_eq!(out.len(), 2);
        let err = out[1].clone().unwrap_err();
        assert!(matches!(err, ProviderError::Connection(_)));
        assert!(err.retry_reason().is_some());
    }
}
