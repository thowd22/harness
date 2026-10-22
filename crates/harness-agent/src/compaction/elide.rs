//! Tool-result elision.

use std::collections::HashMap;
use std::sync::Arc;

use harness_providers::model::{ContentBlock, Message, Role, ToolResultPart};
use harness_tools::spill::{BlobSink, SPILL_SCHEME};

use crate::context::estimate_text;

/// Every elision stub starts with this text.
pub const ELIDED_PREFIX: &str = "[harness: earlier output elided]";

/// One elided tool result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ElidedResult {
    /// The tool call id.
    pub tool_use_id: String,
    /// Tool name (from the matching `tool_use`).
    pub tool: String,
    /// Estimated tokens removed.
    pub tokens: u64,
    /// Blob id of the full text.
    pub blob_id: String,
    /// Index of the message that held it.
    pub message_index: usize,
}

/// What one elision pass did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ElisionReport {
    /// Elided results, oldest first.
    pub elided: Vec<ElidedResult>,
    /// Index of the first changed message.
    pub first_changed: Option<usize>,
}

impl ElisionReport {
    /// Estimated tokens saved.
    pub fn tokens_saved(&self) -> u64 {
        self.elided.iter().map(|e| e.tokens).sum()
    }
}

/// Replaces old, large tool results with stubs and blob pointers.
#[derive(Clone)]
pub struct Elider {
    over_tokens: u64,
    after_turns: u32,
    sink: Arc<dyn BlobSink>,
}

impl std::fmt::Debug for Elider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Elider")
            .field("over_tokens", &self.over_tokens)
            .field("after_turns", &self.after_turns)
            .finish_non_exhaustive()
    }
}

/// The stub that replaces an elided result.
pub fn stub(tool: &str, tokens: u64, blob_id: &str) -> String {
    format!(
        "{ELIDED_PREFIX} This `{tool}` result (about {tokens} tokens) is stored by the \
         harness. To see it again, call the read tool with path \"{SPILL_SCHEME}{blob_id}\"."
    )
}

impl Elider {
    /// Elides text results over `over_tokens` once they are `after_turns`
    /// assistant turns old, storing the full text in `sink` (use the same
    /// sink as the read tool so `spill:<id>` resolves).
    pub fn new(over_tokens: u64, after_turns: u32, sink: Arc<dyn BlobSink>) -> Self {
        Self {
            over_tokens,
            after_turns,
            sink,
        }
    }

    /// Assistant turns after each message.
    fn ages(messages: &[Message]) -> Vec<u32> {
        let mut ages = vec![0; messages.len()];
        let mut seen = 0;
        for (i, m) in messages.iter().enumerate().rev() {
            ages[i] = seen;
            if m.role == Role::Assistant {
                seen += 1;
            }
        }
        ages
    }

    fn candidates(&self, messages: &[Message]) -> Vec<(usize, usize, usize, u64)> {
        let ages = Self::ages(messages);
        let mut out = Vec::new();
        for (mi, m) in messages.iter().enumerate() {
            if m.role != Role::User || ages[mi] < self.after_turns {
                continue;
            }
            for (bi, b) in m.content.iter().enumerate() {
                let ContentBlock::ToolResult { content, .. } = b else {
                    continue;
                };
                for (pi, p) in content.iter().enumerate() {
                    if let ToolResultPart::Text { text } = p
                        && !text.starts_with(ELIDED_PREFIX)
                        && estimate_text(text) > self.over_tokens
                    {
                        out.push((mi, bi, pi, u64::from(ages[mi])));
                    }
                }
            }
        }
        out
    }

    /// Elides in batches: nothing happens until the oldest eligible result
    /// is `2 * after_turns` turns old; then every result older than
    /// `after_turns` is elided at once. Between batches the history (and
    /// so the cached prefix) is unchanged.
    pub fn elide(&self, messages: &mut [Message]) -> std::io::Result<ElisionReport> {
        let candidates = self.candidates(messages);
        let due = candidates
            .iter()
            .map(|c| c.3)
            .max()
            .is_some_and(|oldest| oldest >= u64::from(self.after_turns) * 2);
        if !due {
            return Ok(ElisionReport::default());
        }
        let names: HashMap<String, String> = messages
            .iter()
            .flat_map(|m| &m.content)
            .filter_map(|b| match b {
                ContentBlock::ToolUse { id, name, .. } => Some((id.clone(), name.clone())),
                _ => None,
            })
            .collect();
        let mut report = ElisionReport::default();
        for (mi, bi, pi, _) in candidates {
            let ContentBlock::ToolResult {
                tool_use_id,
                content,
                ..
            } = &mut messages[mi].content[bi]
            else {
                continue;
            };
            let ToolResultPart::Text { text } = &mut content[pi] else {
                continue;
            };
            let blob_id = self.sink.put(text.as_bytes())?;
            let tokens = estimate_text(text);
            let tool = names
                .get(tool_use_id.as_str())
                .cloned()
                .unwrap_or_else(|| "tool".into());
            *text = stub(&tool, tokens, &blob_id);
            report.first_changed.get_or_insert(mi);
            report.elided.push(ElidedResult {
                tool_use_id: tool_use_id.clone(),
                tool,
                tokens,
                blob_id,
                message_index: mi,
            });
        }
        Ok(report)
    }
}
