//! Token estimates for requests and per-node context tracking.
//!
//! Estimates use the same heuristic as the tools crate (4 bytes per token)
//! plus small per-message and per-image constants. A [`ContextTracker`]
//! anchors the estimate on the provider-reported size of the previous call
//! and only estimates what was added since, so the error does not grow with
//! the conversation.

use harness_providers::model::{
    ChatRequest, ContentBlock, Message, ToolResultPart, ToolSpec, Usage,
};

use crate::agent_loop::CallReport;

/// Tokens charged per message for role and framing.
pub const MESSAGE_OVERHEAD: u64 = 4;
/// Tokens charged per image (a typical screenshot).
pub const IMAGE_TOKENS: u64 = 1600;
/// Tokens charged per tool definition for framing.
pub const TOOL_OVERHEAD: u64 = 8;

/// `ceil(bytes / 4)`.
pub fn estimate_text(text: &str) -> u64 {
    (text.len() as u64).div_ceil(4)
}

fn estimate_json(value: &serde_json::Value) -> u64 {
    estimate_text(&value.to_string())
}

/// Estimated tokens of one message.
pub fn estimate_message(message: &Message) -> u64 {
    let blocks: u64 = message
        .content
        .iter()
        .map(|b| match b {
            ContentBlock::Text { text } => estimate_text(text),
            ContentBlock::Image { .. } => IMAGE_TOKENS,
            ContentBlock::ToolUse { name, input, .. } => estimate_text(name) + estimate_json(input),
            ContentBlock::ToolResult { content, .. } => content
                .iter()
                .map(|p| match p {
                    ToolResultPart::Text { text } => estimate_text(text),
                    ToolResultPart::Image { .. } => IMAGE_TOKENS,
                })
                .sum(),
            ContentBlock::Reasoning { text, .. } => estimate_text(text),
            ContentBlock::Opaque { data, .. } => estimate_json(data),
        })
        .sum();
    MESSAGE_OVERHEAD + blocks
}

/// Estimated tokens of `messages`.
pub fn estimate_messages(messages: &[Message]) -> u64 {
    messages.iter().map(estimate_message).sum()
}

/// Estimated tokens of the tool definitions.
pub fn estimate_tools(tools: &[ToolSpec]) -> u64 {
    tools
        .iter()
        .map(|t| {
            TOOL_OVERHEAD
                + estimate_text(&t.name)
                + estimate_text(&t.description)
                + estimate_json(&t.input_schema)
        })
        .sum()
}

/// Size estimate of a request, by part.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RequestEstimate {
    /// System prompt.
    pub system: u64,
    /// Tool definitions.
    pub tools: u64,
    /// Messages.
    pub messages: u64,
}

impl RequestEstimate {
    /// Sum of the parts.
    pub fn total(&self) -> u64 {
        self.system + self.tools + self.messages
    }
}

/// Estimates `request` before it is sent.
pub fn estimate_request(request: &ChatRequest) -> RequestEstimate {
    RequestEstimate {
        system: request.system.as_deref().map_or(0, estimate_text),
        tools: estimate_tools(&request.tools),
        messages: estimate_messages(&request.messages),
    }
}

/// Context use of one node at a point in time.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ContextUsage {
    /// Estimated tokens of the next request.
    pub tokens: u64,
    /// The model's context window.
    pub window: u32,
    /// True when `tokens` is anchored on a provider-reported size.
    pub calibrated: bool,
}

impl ContextUsage {
    /// `tokens / window` (0 when the window is unknown).
    pub fn fraction(&self) -> f64 {
        if self.window == 0 {
            0.0
        } else {
            self.tokens as f64 / f64::from(self.window)
        }
    }
}

/// Tracks one node's context use across calls.
#[derive(Debug, Clone, PartialEq)]
pub struct ContextTracker {
    window: u32,
    /// `(reported tokens incl. the response, messages after the response)`.
    anchor: Option<(u64, usize)>,
    last: Option<ContextUsage>,
    peak: u64,
}

impl ContextTracker {
    /// A tracker for a model with `window` tokens of context.
    pub fn new(window: u32) -> Self {
        Self {
            window,
            anchor: None,
            last: None,
            peak: 0,
        }
    }

    /// The context window.
    pub fn window(&self) -> u32 {
        self.window
    }

    /// Records a finished call: its reported input plus output becomes the
    /// anchor for the next estimate.
    pub fn observe(&mut self, report: &CallReport) {
        self.observe_usage(&report.usage, report.messages_after);
    }

    /// Like [`ContextTracker::observe`] from raw numbers.
    /// Calls that report no input (some test providers) leave the tracker
    /// uncalibrated.
    pub fn observe_usage(&mut self, usage: &Usage, messages_after: usize) {
        if usage.total_input() == 0 {
            self.anchor = None;
            return;
        }
        let reported = usage.total_input() + usage.output_tokens;
        self.peak = self.peak.max(usage.total_input());
        self.anchor = Some((reported, messages_after));
    }

    /// Forgets the anchor (the history was rewritten).
    pub fn invalidate(&mut self) {
        self.anchor = None;
    }

    /// Estimates the next request for `system`, `tools` and `messages`.
    pub fn estimate(
        &mut self,
        system: Option<&str>,
        tools: &[ToolSpec],
        messages: &[Message],
    ) -> ContextUsage {
        let usage = match self.anchor {
            Some((reported, count)) if count <= messages.len() => ContextUsage {
                tokens: reported + estimate_messages(&messages[count..]),
                window: self.window,
                calibrated: true,
            },
            _ => ContextUsage {
                tokens: system.map_or(0, estimate_text)
                    + estimate_tools(tools)
                    + estimate_messages(messages),
                window: self.window,
                calibrated: false,
            },
        };
        self.last = Some(usage);
        usage
    }

    /// The latest estimate.
    pub fn last(&self) -> Option<ContextUsage> {
        self.last
    }

    /// Largest provider-reported input seen.
    pub fn peak(&self) -> u64 {
        self.peak
    }
}

/// Share of a call's input served from the prompt cache (provider usage),
/// `None` without input.
pub fn cache_hit_rate_of(usage: &Usage) -> Option<f64> {
    let total = usage.total_input();
    (total > 0).then(|| usage.cache_read_tokens as f64 / total as f64)
}
