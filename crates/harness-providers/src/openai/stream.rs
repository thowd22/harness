//! Maps OpenAI Responses API stream events to normalized [`StreamEvent`]s.
//!
//! Block indices: output item `k` uses block `2k` for its visible content
//! (text, tool call or reasoning summary) and block `2k + 1` for the opaque
//! encrypted reasoning item that is sent back on the next turn.

use std::collections::HashMap;

use serde_json::Value;

use crate::model::{StopKind, StopReason, Usage};
use crate::provider::{ProviderError, StreamEvent};
use crate::sse::SseEvent;

use super::PROVIDER_NAME;
use super::notice::{HiddenReason, Notice};
use super::transport::{EventMapper, parse_json, stream_error, u64_at};

#[derive(Debug, Default)]
struct Item {
    started: bool,
    streamed: bool,
    last_summary: Option<u64>,
}

/// Per-attempt mapping state.
#[derive(Debug, Default)]
pub(crate) struct ResponsesMapper {
    started: bool,
    items: HashMap<u64, Item>,
    tool_calls: bool,
    refusal: bool,
    done: bool,
}

fn text_index(output_index: u64) -> usize {
    (output_index as usize) * 2
}

/// Normalizes Responses API usage. `input_tokens` there includes cached
/// tokens; the normalized form excludes them.
pub(crate) fn usage(v: &Value) -> Usage {
    let input = u64_at(v, "/input_tokens");
    let cached = u64_at(v, "/input_tokens_details/cached_tokens");
    Usage {
        input_tokens: input.saturating_sub(cached),
        output_tokens: u64_at(v, "/output_tokens"),
        cache_read_tokens: cached,
        cache_write_tokens: 0,
        reasoning_tokens: u64_at(v, "/output_tokens_details/reasoning_tokens"),
    }
}

impl ResponsesMapper {
    fn item(&mut self, v: &Value) -> (u64, &mut Item) {
        let k = u64_at(v, "/output_index");
        (k, self.items.entry(k).or_default())
    }

    fn start(&mut self, response: &Value, out: &mut Vec<StreamEvent>) {
        if !self.started {
            self.started = true;
            out.push(StreamEvent::MessageStart {
                id: response
                    .get("id")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                model: response
                    .get("model")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
            });
        }
    }

    fn item_added(&mut self, v: &Value, out: &mut Vec<StreamEvent>) {
        let item_v = &v["item"];
        let (k, item) = self.item(v);
        if item_v["type"] == "function_call" && !item.started {
            item.started = true;
            self.tool_calls = true;
            out.push(tool_start(k, item_v));
        }
    }

    fn item_done(&mut self, v: &Value, out: &mut Vec<StreamEvent>) {
        let item_v = &v["item"];
        let kind = item_v["type"].as_str().unwrap_or_default().to_owned();
        let (k, item) = self.item(v);
        let index = text_index(k);
        match kind.as_str() {
            "function_call" => {
                if !item.started {
                    item.started = true;
                    out.push(tool_start(k, item_v));
                }
                if !item.streamed
                    && let Some(args) = item_v["arguments"].as_str().filter(|a| !a.is_empty())
                {
                    out.push(StreamEvent::ToolInputDelta {
                        index,
                        partial_json: args.to_owned(),
                    });
                }
                self.tool_calls = true;
                out.push(StreamEvent::BlockStop { index });
            }
            "message" => {
                if !item.streamed {
                    let text: String = item_v["content"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(|p| p.get("text").or_else(|| p.get("refusal")))
                        .filter_map(Value::as_str)
                        .collect();
                    if text.is_empty() {
                        return;
                    }
                    out.push(StreamEvent::TextDelta { index, text });
                }
                out.push(StreamEvent::BlockStop { index });
            }
            "reasoning" => {
                let encrypted = item_v
                    .get("encrypted_content")
                    .and_then(Value::as_str)
                    .is_some_and(|s| !s.is_empty());
                if !item.streamed {
                    let summary: Vec<&str> = item_v["summary"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(|p| p.get("text").and_then(Value::as_str))
                        .filter(|t| !t.is_empty())
                        .collect();
                    if summary.is_empty() {
                        let reason = if encrypted {
                            HiddenReason::EncryptedOnly
                        } else {
                            HiddenReason::NotProvided
                        };
                        out.push(
                            Notice::ReasoningHidden {
                                provider: PROVIDER_NAME.into(),
                                reason,
                            }
                            .to_event(index),
                        );
                    } else {
                        out.push(StreamEvent::ReasoningDelta {
                            index,
                            text: summary.join("\n\n"),
                        });
                        item.streamed = true;
                    }
                }
                if item.streamed {
                    out.push(StreamEvent::BlockStop { index });
                }
                if encrypted {
                    out.push(StreamEvent::Opaque {
                        index: index + 1,
                        provider: PROVIDER_NAME.into(),
                        data: item_v.clone(),
                    });
                    out.push(StreamEvent::BlockStop { index: index + 1 });
                }
            }
            _ => {}
        }
    }

    fn finish(&mut self, response: &Value, incomplete: bool, out: &mut Vec<StreamEvent>) {
        self.start(response, out);
        let stop = if incomplete {
            let reason = response
                .pointer("/incomplete_details/reason")
                .and_then(Value::as_str)
                .unwrap_or("incomplete");
            StopReason::from_openai(reason)
        } else {
            let kind = if self.refusal {
                StopKind::Refusal
            } else if self.tool_calls {
                StopKind::ToolUse
            } else {
                StopKind::EndTurn
            };
            StopReason {
                kind,
                raw: Some("completed".into()),
            }
        };
        out.push(StreamEvent::MessageStop {
            stop,
            usage: usage(&response["usage"]),
        });
        self.done = true;
    }
}

fn tool_start(k: u64, item: &Value) -> StreamEvent {
    StreamEvent::ToolUseStart {
        index: text_index(k),
        id: item["call_id"]
            .as_str()
            .or_else(|| item["id"].as_str())
            .unwrap_or_default()
            .to_owned(),
        name: item["name"].as_str().unwrap_or_default().to_owned(),
    }
}

impl EventMapper for ResponsesMapper {
    fn on_event(&mut self, event: &SseEvent) -> Result<Vec<StreamEvent>, ProviderError> {
        let v = parse_json(event)?;
        let kind = v["type"].as_str().unwrap_or(&event.event).to_owned();
        let mut out = Vec::new();
        let delta = || v["delta"].as_str().unwrap_or_default().to_owned();
        match kind.as_str() {
            "response.created" | "response.in_progress" => self.start(&v["response"], &mut out),
            "response.output_item.added" => self.item_added(&v, &mut out),
            "response.output_item.done" => self.item_done(&v, &mut out),
            "response.output_text.delta" | "response.refusal.delta" => {
                if kind == "response.refusal.delta" {
                    self.refusal = true;
                }
                let (k, item) = self.item(&v);
                item.streamed = true;
                out.push(StreamEvent::TextDelta {
                    index: text_index(k),
                    text: delta(),
                });
            }
            "response.function_call_arguments.delta" => {
                let (k, item) = self.item(&v);
                item.streamed = true;
                out.push(StreamEvent::ToolInputDelta {
                    index: text_index(k),
                    partial_json: delta(),
                });
            }
            "response.reasoning_summary_text.delta" | "response.reasoning_text.delta" => {
                let part = v
                    .get("summary_index")
                    .or_else(|| v.get("content_index"))
                    .and_then(Value::as_u64)
                    .unwrap_or(0);
                let (k, item) = self.item(&v);
                let mut text = delta();
                if item.last_summary.is_some_and(|p| p != part) {
                    text.insert_str(0, "\n\n");
                }
                item.last_summary = Some(part);
                item.streamed = true;
                out.push(StreamEvent::ReasoningDelta {
                    index: text_index(k),
                    text,
                });
            }
            "response.completed" => self.finish(&v["response"], false, &mut out),
            "response.incomplete" => self.finish(&v["response"], true, &mut out),
            "response.failed" => {
                let err = &v["response"]["error"];
                return Err(stream_error(
                    err.get("code"),
                    err["message"].as_str().unwrap_or("response failed"),
                ));
            }
            "error" => {
                let err = v.get("error").filter(|e| e.is_object()).unwrap_or(&v);
                return Err(stream_error(
                    err.get("code"),
                    err["message"].as_str().unwrap_or("stream error"),
                ));
            }
            _ => {}
        }
        Ok(out)
    }

    fn on_end(&mut self) -> Result<Vec<StreamEvent>, ProviderError> {
        Err(ProviderError::Connection(
            "OpenAI stream ended before response.completed".into(),
        ))
    }

    fn finished(&self) -> bool {
        self.done
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn run(events: &[Value]) -> Result<Vec<StreamEvent>, ProviderError> {
        let mut m = ResponsesMapper::default();
        let mut out = Vec::new();
        for e in events {
            out.extend(m.on_event(&SseEvent {
                event: e["type"].as_str().unwrap().into(),
                data: e.to_string(),
                id: None,
                retry: None,
            })?);
        }
        Ok(out)
    }

    #[test]
    fn usage_splits_cached_and_reports_reasoning() {
        let u = usage(&json!({
            "input_tokens": 100,
            "input_tokens_details": {"cached_tokens": 60},
            "output_tokens": 50,
            "output_tokens_details": {"reasoning_tokens": 30}
        }));
        assert_eq!(
            u,
            Usage {
                input_tokens: 40,
                output_tokens: 50,
                cache_read_tokens: 60,
                cache_write_tokens: 0,
                reasoning_tokens: 30
            }
        );
    }

    #[test]
    fn encrypted_only_reasoning_is_flagged_and_kept_opaque() {
        let item =
            json!({"type": "reasoning", "id": "rs_1", "summary": [], "encrypted_content": "gAAA"});
        let out = run(&[
            json!({"type": "response.output_item.added", "output_index": 0, "item": {"type": "reasoning", "id": "rs_1"}}),
            json!({"type": "response.output_item.done", "output_index": 0, "item": item}),
        ])
        .unwrap();
        assert_eq!(
            Notice::from_event(&out[0]),
            Some(Notice::ReasoningHidden {
                provider: "openai".into(),
                reason: HiddenReason::EncryptedOnly
            })
        );
        assert_eq!(
            out[1],
            StreamEvent::Opaque {
                index: 1,
                provider: "openai".into(),
                data: item
            }
        );
    }

    #[test]
    fn summary_from_done_item_when_not_streamed() {
        let out = run(&[json!({"type": "response.output_item.done", "output_index": 1,
            "item": {"type": "reasoning", "id": "rs", "summary": [{"type": "summary_text", "text": "a"}, {"type": "summary_text", "text": "b"}]}})])
        .unwrap();
        assert_eq!(
            out,
            vec![
                StreamEvent::ReasoningDelta {
                    index: 2,
                    text: "a\n\nb".into()
                },
                StreamEvent::BlockStop { index: 2 }
            ]
        );
    }

    #[test]
    fn failed_response_is_a_typed_error() {
        let err = run(&[json!({"type": "response.failed", "response": {"error": {"code": "server_error", "message": "boom"}}})])
            .unwrap_err();
        assert!(err.retry_reason().is_some());
        let err =
            run(&[json!({"type": "error", "code": "rate_limit_exceeded", "message": "slow"})])
                .unwrap_err();
        assert!(matches!(err, ProviderError::RateLimited { .. }));
    }

    #[test]
    fn incomplete_maps_max_tokens() {
        let out = run(&[json!({"type": "response.incomplete", "response": {"id": "r", "incomplete_details": {"reason": "max_output_tokens"}, "usage": {}}})])
            .unwrap();
        assert!(matches!(
            out.last().unwrap(),
            StreamEvent::MessageStop { stop, .. } if stop.kind == StopKind::MaxTokens
        ));
    }

    #[test]
    fn function_call_args_from_done_item_when_not_streamed() {
        let out = run(&[json!({"type": "response.output_item.done", "output_index": 0,
            "item": {"type": "function_call", "id": "fc", "call_id": "call_9", "name": "bash", "arguments": "{\"command\":\"ls\"}"}})])
        .unwrap();
        assert_eq!(
            out,
            vec![
                StreamEvent::ToolUseStart {
                    index: 0,
                    id: "call_9".into(),
                    name: "bash".into()
                },
                StreamEvent::ToolInputDelta {
                    index: 0,
                    partial_json: "{\"command\":\"ls\"}".into()
                },
                StreamEvent::BlockStop { index: 0 }
            ]
        );
    }
}
