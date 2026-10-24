//! Maps OpenRouter Chat Completions chunks to normalized [`StreamEvent`]s.
//!
//! Block indices are assigned in order of first appearance (reasoning, text,
//! each tool call). Chat Completions streams tool calls one after another,
//! so a tool call is complete (and gets its [`StreamEvent::BlockStop`]) as
//! soon as the next tool call or visible text starts, or the choice reports
//! its `finish_reason`; the agent can then start it before the message
//! ends. At the end, merged `reasoning_details` become one opaque
//! block (provider `openrouter`) that is sent back on the next turn, and
//! notices report the call's cost and missing reasoning.

use std::collections::{BTreeMap, HashMap, HashSet};

use serde_json::{Map, Value};

use crate::model::{StopKind, StopReason, Usage};
use crate::openai::notice::{HiddenReason, Notice};
use crate::openai::transport::{EventMapper, parse_json, stream_error, u64_at};
use crate::provider::{ProviderError, StreamEvent};
use crate::sse::SseEvent;

use super::PROVIDER_NAME;

/// Per-attempt mapping state.
#[derive(Debug, Default)]
pub(crate) struct ChatMapper {
    /// Reasoning was requested, so its absence is flagged.
    pub reasoning_requested: bool,
    started: bool,
    next: usize,
    reasoning: Option<usize>,
    text: Option<usize>,
    tools: HashMap<u64, usize>,
    /// Tool blocks whose `BlockStop` was already sent.
    closed: HashSet<usize>,
    details: BTreeMap<u64, Map<String, Value>>,
    encrypted: bool,
    finish: Option<String>,
    usage: Option<Value>,
    done: bool,
}

/// Normalizes Chat Completions usage. `prompt_tokens` includes cache reads
/// and writes; the normalized `input_tokens` excludes them.
pub(crate) fn usage(v: &Value) -> Usage {
    let cached = u64_at(v, "/prompt_tokens_details/cached_tokens");
    let written = u64_at(v, "/prompt_tokens_details/cache_write_tokens");
    Usage {
        input_tokens: u64_at(v, "/prompt_tokens").saturating_sub(cached + written),
        output_tokens: u64_at(v, "/completion_tokens"),
        cache_read_tokens: cached,
        cache_write_tokens: written,
        reasoning_tokens: u64_at(v, "/completion_tokens_details/reasoning_tokens"),
    }
}

impl ChatMapper {
    /// A mapper that flags missing reasoning when `reasoning_requested`.
    pub fn new(reasoning_requested: bool) -> Self {
        Self {
            reasoning_requested,
            ..Self::default()
        }
    }

    fn alloc(&mut self) -> usize {
        self.next += 1;
        self.next - 1
    }

    /// Sends `BlockStop` for every open tool block, in index order: a later
    /// tool call, text or the finish reason means they are complete.
    fn close_tools(&mut self, out: &mut Vec<StreamEvent>) {
        let mut open: Vec<usize> = self
            .tools
            .values()
            .copied()
            .filter(|i| !self.closed.contains(i))
            .collect();
        open.sort_unstable();
        for index in open {
            self.closed.insert(index);
            out.push(StreamEvent::BlockStop { index });
        }
    }

    fn reasoning_text(&mut self, text: &str, out: &mut Vec<StreamEvent>) {
        if text.is_empty() {
            return;
        }
        let index = match self.reasoning {
            Some(i) => i,
            None => {
                let i = self.alloc();
                self.reasoning = Some(i);
                i
            }
        };
        out.push(StreamEvent::ReasoningDelta {
            index,
            text: text.to_owned(),
        });
    }

    /// Merges streamed `reasoning_details` fragments by their `index`:
    /// string fields `text`, `summary` and `data` are concatenated, other
    /// fields are overwritten. Returns the visible text in the fragments.
    fn merge_details(&mut self, details: &[Value]) -> String {
        let mut visible = String::new();
        for (pos, d) in details.iter().enumerate() {
            let Some(obj) = d.as_object() else { continue };
            let key = obj
                .get("index")
                .and_then(Value::as_u64)
                .unwrap_or(pos as u64);
            match obj.get("type").and_then(Value::as_str) {
                Some("reasoning.encrypted") => self.encrypted = true,
                Some("reasoning.text") => {
                    visible.push_str(obj.get("text").and_then(Value::as_str).unwrap_or_default());
                }
                Some("reasoning.summary") => {
                    visible.push_str(
                        obj.get("summary")
                            .and_then(Value::as_str)
                            .unwrap_or_default(),
                    );
                }
                _ => {}
            }
            let entry = self.details.entry(key).or_default();
            for (k, v) in obj {
                if matches!(k.as_str(), "text" | "summary" | "data")
                    && let (Some(Value::String(acc)), Value::String(s)) = (entry.get_mut(k), v)
                {
                    acc.push_str(s);
                    continue;
                }
                entry.insert(k.clone(), v.clone());
            }
        }
        visible
    }

    fn chunk(&mut self, v: &Value, out: &mut Vec<StreamEvent>) -> Result<(), ProviderError> {
        if let Some(err) = v.get("error").filter(|e| e.is_object()) {
            return Err(stream_error(
                err.get("code"),
                err["message"].as_str().unwrap_or("upstream error"),
            ));
        }
        if !self.started {
            self.started = true;
            out.push(StreamEvent::MessageStart {
                id: v.get("id").and_then(Value::as_str).map(str::to_owned),
                model: v.get("model").and_then(Value::as_str).map(str::to_owned),
            });
        }
        if let Some(u) = v.get("usage").filter(|u| u.is_object()) {
            self.usage = Some(u.clone());
        }
        let Some(choice) = v.pointer("/choices/0") else {
            return Ok(());
        };
        let delta = &choice["delta"];
        let streamed_reasoning = delta.get("reasoning").and_then(Value::as_str);
        let detail_text = match delta.get("reasoning_details").and_then(Value::as_array) {
            Some(details) => self.merge_details(details),
            None => String::new(),
        };
        match streamed_reasoning {
            Some(text) if !text.is_empty() => self.reasoning_text(text, out),
            _ => self.reasoning_text(&detail_text, out),
        }
        if let Some(text) = delta
            .get("content")
            .and_then(Value::as_str)
            .filter(|t| !t.is_empty())
        {
            self.close_tools(out);
            let index = match self.text {
                Some(i) => i,
                None => {
                    let i = self.alloc();
                    self.text = Some(i);
                    i
                }
            };
            out.push(StreamEvent::TextDelta {
                index,
                text: text.to_owned(),
            });
        }
        for call in delta
            .get("tool_calls")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let key = call
                .get("index")
                .and_then(Value::as_u64)
                .unwrap_or(self.tools.len() as u64);
            let index = match self.tools.get(&key) {
                Some(i) => *i,
                None => {
                    self.close_tools(out);
                    let i = self.alloc();
                    self.tools.insert(key, i);
                    out.push(StreamEvent::ToolUseStart {
                        index: i,
                        id: call["id"]
                            .as_str()
                            .map(str::to_owned)
                            .unwrap_or_else(|| format!("call_{key}")),
                        name: call["function"]["name"]
                            .as_str()
                            .unwrap_or_default()
                            .to_owned(),
                    });
                    i
                }
            };
            if let Some(args) = call["function"]["arguments"]
                .as_str()
                .filter(|a| !a.is_empty())
            {
                out.push(StreamEvent::ToolInputDelta {
                    index,
                    partial_json: args.to_owned(),
                });
            }
        }
        if let Some(reason) = choice.get("finish_reason").and_then(Value::as_str) {
            if reason == "error" {
                return Err(ProviderError::Http {
                    status: 502,
                    message: "upstream provider ended the stream with an error".into(),
                    retry_after: None,
                });
            }
            self.finish = Some(reason.to_owned());
            // The choice is done: its tool calls are complete even though
            // the usage chunk and `[DONE]` are still to come.
            self.close_tools(out);
        }
        Ok(())
    }

    fn finalize(&mut self, out: &mut Vec<StreamEvent>) -> Result<(), ProviderError> {
        let Some(reason) = self.finish.take() else {
            return Err(ProviderError::Connection(
                "OpenRouter stream ended without a finish_reason".into(),
            ));
        };
        let mut open: Vec<usize> = self
            .reasoning
            .into_iter()
            .chain(self.text)
            .chain(self.tools.values().copied())
            .filter(|i| !self.closed.contains(i))
            .collect();
        open.sort_unstable();
        out.extend(
            open.into_iter()
                .map(|index| StreamEvent::BlockStop { index }),
        );
        if !self.details.is_empty() {
            let details: Vec<Value> = std::mem::take(&mut self.details)
                .into_values()
                .map(Value::Object)
                .collect();
            let index = self.alloc();
            out.push(StreamEvent::Opaque {
                index,
                provider: PROVIDER_NAME.into(),
                data: serde_json::json!({ "reasoning_details": details }),
            });
        }
        let usage_v = self.usage.take().unwrap_or(Value::Null);
        let usage = usage(&usage_v);
        if self.reasoning.is_none() && (self.reasoning_requested || usage.reasoning_tokens > 0) {
            let reason = if self.encrypted {
                HiddenReason::EncryptedOnly
            } else {
                HiddenReason::NotProvided
            };
            let index = self.alloc();
            out.push(
                Notice::ReasoningHidden {
                    provider: PROVIDER_NAME.into(),
                    reason,
                }
                .to_event(index),
            );
        }
        if let Some(usd) = usage_v.get("cost").and_then(Value::as_f64) {
            let index = self.alloc();
            out.push(
                Notice::Cost {
                    provider: PROVIDER_NAME.into(),
                    usd,
                }
                .to_event(index),
            );
        }
        let mut stop = StopReason::from_openai(&reason);
        if !self.tools.is_empty() && stop.kind == StopKind::EndTurn {
            stop.kind = StopKind::ToolUse;
        }
        out.push(StreamEvent::MessageStop { stop, usage });
        self.done = true;
        Ok(())
    }
}

impl EventMapper for ChatMapper {
    fn on_event(&mut self, event: &SseEvent) -> Result<Vec<StreamEvent>, ProviderError> {
        let mut out = Vec::new();
        if event.data.trim() == "[DONE]" {
            self.finalize(&mut out)?;
        } else {
            let v = parse_json(event)?;
            self.chunk(&v, &mut out)?;
        }
        Ok(out)
    }

    fn on_end(&mut self) -> Result<Vec<StreamEvent>, ProviderError> {
        let mut out = Vec::new();
        self.finalize(&mut out)?;
        Ok(out)
    }

    fn finished(&self) -> bool {
        self.done
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn run(mapper: &mut ChatMapper, chunks: &[Value]) -> Result<Vec<StreamEvent>, ProviderError> {
        let mut out = Vec::new();
        for c in chunks {
            out.extend(mapper.on_event(&SseEvent {
                event: "message".into(),
                data: c.to_string(),
                id: None,
                retry: None,
            })?);
        }
        out.extend(mapper.on_event(&SseEvent {
            event: "message".into(),
            data: "[DONE]".into(),
            id: None,
            retry: None,
        })?);
        Ok(out)
    }

    #[test]
    fn usage_normalization() {
        let u = usage(&json!({"prompt_tokens": 100, "completion_tokens": 40,
            "prompt_tokens_details": {"cached_tokens": 50, "cache_write_tokens": 10},
            "completion_tokens_details": {"reasoning_tokens": 25}, "cost": 0.01}));
        assert_eq!(
            u,
            Usage {
                input_tokens: 40,
                output_tokens: 40,
                cache_read_tokens: 50,
                cache_write_tokens: 10,
                reasoning_tokens: 25
            }
        );
    }

    #[test]
    fn missing_reasoning_is_flagged_when_requested() {
        let mut m = ChatMapper::new(true);
        let out = run(
            &mut m,
            &[json!({"id": "g", "choices": [{"delta": {"content": "hi"}, "finish_reason": "stop"}]})],
        )
        .unwrap();
        assert!(out.iter().any(|e| Notice::from_event(e)
            == Some(Notice::ReasoningHidden {
                provider: "openrouter".into(),
                reason: HiddenReason::NotProvided
            })));
        let mut m = ChatMapper::new(false);
        let out = run(
            &mut m,
            &[json!({"choices": [{"delta": {"content": "hi"}, "finish_reason": "stop"}]})],
        )
        .unwrap();
        assert!(out.iter().all(|e| Notice::from_event(e).is_none()));
    }

    #[test]
    fn encrypted_only_details_are_flagged_and_kept() {
        let mut m = ChatMapper::new(true);
        let out = run(
            &mut m,
            &[json!({"choices": [{"delta": {"reasoning_details": [{"type": "reasoning.encrypted", "data": "xx", "index": 0}]},
                "finish_reason": "stop"}]})],
        )
        .unwrap();
        assert!(out.iter().any(
            |e| matches!(e, StreamEvent::Opaque { provider, .. } if provider == "openrouter")
        ));
        assert!(out.iter().any(|e| Notice::from_event(e)
            == Some(Notice::ReasoningHidden {
                provider: "openrouter".into(),
                reason: HiddenReason::EncryptedOnly
            })));
    }

    #[test]
    fn reasoning_from_details_when_no_reasoning_field_and_merged() {
        let mut m = ChatMapper::new(true);
        let out = run(
            &mut m,
            &[
                json!({"choices": [{"delta": {"reasoning_details": [{"type": "reasoning.text", "text": "ab", "index": 0}]}}]}),
                json!({"choices": [{"delta": {"reasoning_details": [{"type": "reasoning.text", "text": "cd", "index": 0, "signature": "sig"}]},
                    "finish_reason": "stop"}]}),
            ],
        )
        .unwrap();
        let text: String = out
            .iter()
            .filter_map(|e| match e {
                StreamEvent::ReasoningDelta { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(text, "abcd");
        let opaque = out
            .iter()
            .find_map(|e| match e {
                StreamEvent::Opaque { provider, data, .. } if provider == "openrouter" => {
                    Some(data.clone())
                }
                _ => None,
            })
            .unwrap();
        assert_eq!(
            opaque,
            json!({"reasoning_details": [{"type": "reasoning.text", "text": "abcd", "index": 0, "signature": "sig"}]})
        );
    }

    #[test]
    fn mid_stream_errors_are_typed() {
        let mut m = ChatMapper::default();
        let err = run(
            &mut m,
            &[json!({"error": {"code": 503, "message": "no providers"}, "choices": [{"delta": {}, "finish_reason": "error"}]})],
        )
        .unwrap_err();
        assert!(err.retry_reason().is_some());
        let mut m = ChatMapper::default();
        let err = run(
            &mut m,
            &[json!({"choices": [{"delta": {}, "finish_reason": "error"}]})],
        )
        .unwrap_err();
        assert!(err.retry_reason().is_some());
    }

    /// Feeds chunks one at a time and returns, for each chunk, the events
    /// it produced (the last entry is `[DONE]`).
    fn steps(mapper: &mut ChatMapper, chunks: &[Value]) -> Vec<Vec<StreamEvent>> {
        let mut out: Vec<Vec<StreamEvent>> = chunks
            .iter()
            .map(|c| {
                mapper
                    .on_event(&SseEvent {
                        event: "message".into(),
                        data: c.to_string(),
                        id: None,
                        retry: None,
                    })
                    .unwrap()
            })
            .collect();
        out.push(
            mapper
                .on_event(&SseEvent {
                    event: "message".into(),
                    data: "[DONE]".into(),
                    id: None,
                    retry: None,
                })
                .unwrap(),
        );
        out
    }

    fn stop_step(steps: &[Vec<StreamEvent>], index: usize) -> usize {
        let found: Vec<usize> = steps
            .iter()
            .enumerate()
            .flat_map(|(n, evs)| {
                evs.iter()
                    .filter(move |e| **e == StreamEvent::BlockStop { index })
                    .map(move |_| n)
            })
            .collect();
        assert_eq!(
            found.len(),
            1,
            "block {index} stops exactly once: {steps:?}"
        );
        found[0]
    }

    #[test]
    fn tool_calls_stop_when_the_next_one_starts_and_at_finish() {
        let call = |i: u64, id: Option<&str>, name: Option<&str>, args: &str| {
            let mut f = json!({"arguments": args});
            if let Some(n) = name {
                f["name"] = json!(n);
            }
            let mut c = json!({"index": i, "function": f});
            if let Some(id) = id {
                c["id"] = json!(id);
            }
            json!({"choices": [{"delta": {"tool_calls": [c]}}]})
        };
        let mut m = ChatMapper::default();
        let steps = steps(
            &mut m,
            &[
                json!({"id": "g", "choices": [{"delta": {"content": "Reading."}}]}),
                call(0, Some("c0"), Some("read"), ""),
                call(0, None, None, "{\"path\":"),
                call(0, None, None, "\"a.rs\"}"),
                call(1, Some("c1"), Some("grep"), "{\"pattern\""),
                call(1, None, None, ":\"x\"}"),
                json!({"choices": [{"delta": {}, "finish_reason": "tool_calls"}]}),
                json!({"choices": [], "usage": {"prompt_tokens": 5, "completion_tokens": 3}}),
            ],
        );
        // Text block 0, tool blocks 1 and 2.
        assert_eq!(
            stop_step(&steps, 1),
            4,
            "first call stops when the second starts"
        );
        let pos = |evs: &[StreamEvent], want: &StreamEvent| evs.iter().position(|e| e == want);
        assert!(
            pos(&steps[4], &StreamEvent::BlockStop { index: 1 })
                < steps[4]
                    .iter()
                    .position(|e| matches!(e, StreamEvent::ToolUseStart { .. })),
            "the stop precedes the next call's start"
        );
        assert_eq!(
            stop_step(&steps, 2),
            6,
            "last call stops at finish_reason, before usage and [DONE]"
        );
        assert_eq!(stop_step(&steps, 0), 8, "text stops at the end");
        let all: Vec<StreamEvent> = steps.concat();
        assert!(matches!(
            all.last(),
            Some(StreamEvent::MessageStop { stop, .. }) if stop.kind == StopKind::ToolUse
        ));
        // The accumulated message is unchanged by the earlier stops.
        let mut b = crate::provider::ResponseBuilder::new();
        for e in all {
            b.apply(e).unwrap();
        }
        let msg = b.finish().unwrap().message;
        assert_eq!(
            msg.content[1],
            crate::model::ContentBlock::ToolUse {
                id: "c0".into(),
                name: "read".into(),
                input: json!({"path": "a.rs"})
            }
        );
        assert_eq!(
            msg.content[2],
            crate::model::ContentBlock::ToolUse {
                id: "c1".into(),
                name: "grep".into(),
                input: json!({"pattern": "x"})
            }
        );
    }

    #[test]
    fn text_after_a_tool_call_completes_it() {
        let mut m = ChatMapper::default();
        let steps = steps(
            &mut m,
            &[
                json!({"choices": [{"delta": {"tool_calls": [{"index": 0, "id": "c0", "function": {"name": "read", "arguments": "{}"}}]}}]}),
                json!({"choices": [{"delta": {"content": "and then"}}]}),
                json!({"choices": [{"delta": {}, "finish_reason": "stop"}]}),
            ],
        );
        assert_eq!(stop_step(&steps, 0), 1);
        assert_eq!(stop_step(&steps, 1), 3);
    }

    #[test]
    fn end_without_finish_is_retryable() {
        let mut m = ChatMapper::default();
        assert!(m.on_end().unwrap_err().retry_reason().is_some());
    }
}
