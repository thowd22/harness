//! Live output and reasoning of every agent, buffered per node.
//!
//! [`LiveStreams`] folds the run's `delta`, `tool_call`, `tool_result` and
//! `model_call_finished` events into one bounded [`NodeStream`] per node.
//! The map shows each running node's latest line ([`LiveStreams::preview`])
//! and the [`LivePane`] streams the selected node token by token.
//!
//! Buffering is per node and O(1) per event (text is appended to the last
//! segment; old segments are dropped past [`MAX_STREAM_BYTES`]). Redraws
//! are throttled by [`FrameThrottle`], so ten or more agents streaming at
//! once cost at most one frame per [`MIN_FRAME`].
//!
//! Provider honesty: when a model call finishes with reasoning tokens but
//! no reasoning text, or the provider attached a `reasoning_hidden`
//! notice, the stream gets an explicit [`Segment::Note`]
//! ("reasoning not exposed by provider") instead of showing nothing.

use std::collections::{HashMap, HashSet, VecDeque};
use std::time::{Duration, Instant};

use harness_events::schema::{DeltaChannel, EventKind, NodeId, Payload, RunEvent};
use harness_providers::model::Message;
use harness_providers::openai::notice::{HiddenReason, Notice};
use ratatui::layout::Rect;
use ratatui::style::Modifier;
use ratatui::text::{Line, Span};

use crate::cards::{arg_summary, payload_json, payload_text};
use crate::chat::text::{sanitize, truncate, truncate_line, wrap_line};
use crate::theme::{Theme, Token};

/// Bytes of stream text kept per node; older segments are dropped.
pub const MAX_STREAM_BYTES: usize = 64 * 1024;

/// Minimum time between two frames caused by stream updates (~60 fps).
pub const MIN_FRAME: Duration = Duration::from_millis(16);

/// Text shown when a provider does not expose reasoning.
pub const REASONING_HIDDEN: &str = "reasoning not exposed by provider";

/// One piece of a node's live stream.
#[derive(Debug, Clone, PartialEq)]
pub enum Segment {
    /// Assistant output text.
    Text(String),
    /// Reasoning text.
    Reasoning(String),
    /// A tool call and (once finished) its result.
    Tool {
        /// Call id.
        call_id: String,
        /// Tool name (`mcp <server> › <tool>` for MCP calls).
        tool: String,
        /// Argument summary.
        summary: String,
        /// Live or final output.
        output: String,
        /// `Some(ok)` once finished.
        ok: Option<bool>,
        /// Wall time once finished.
        duration_ms: Option<u64>,
    },
    /// A harness note, e.g. hidden reasoning.
    Note(String),
}

impl Segment {
    fn len(&self) -> usize {
        match self {
            Segment::Text(s) | Segment::Reasoning(s) | Segment::Note(s) => s.len(),
            Segment::Tool {
                summary, output, ..
            } => summary.len() + output.len(),
        }
    }
}

/// The live stream of one node.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct NodeStream {
    /// Segments, oldest first.
    pub segments: VecDeque<Segment>,
    bytes: usize,
    version: u64,
    reasoning_calls: HashSet<String>,
}

impl NodeStream {
    /// Changes whenever the stream changes.
    pub fn version(&self) -> u64 {
        self.version
    }

    fn push_text(&mut self, reasoning: bool, text: &str) {
        match (self.segments.back_mut(), reasoning) {
            (Some(Segment::Text(s)), false) | (Some(Segment::Reasoning(s)), true) => {
                s.push_str(text)
            }
            _ => self.segments.push_back(if reasoning {
                Segment::Reasoning(text.to_owned())
            } else {
                Segment::Text(text.to_owned())
            }),
        }
        self.bytes += text.len();
        self.trim();
    }

    fn push(&mut self, seg: Segment) {
        self.bytes += seg.len();
        self.segments.push_back(seg);
        self.trim();
    }

    fn trim(&mut self) {
        self.version += 1;
        while self.bytes > MAX_STREAM_BYTES && self.segments.len() > 1 {
            if let Some(s) = self.segments.pop_front() {
                self.bytes -= s.len();
            }
        }
    }

    fn tool_mut(&mut self, call_id: &str) -> Option<&mut Segment> {
        self.segments
            .iter_mut()
            .rev()
            .find(|s| matches!(s, Segment::Tool { call_id: c, .. } if c == call_id))
    }

    /// The latest non-empty line and whether it is reasoning.
    pub fn last_line(&self) -> Option<(String, bool)> {
        for s in self.segments.iter().rev() {
            let (text, reasoning) = match s {
                Segment::Text(t) => (t.as_str(), false),
                Segment::Reasoning(t) => (t.as_str(), true),
                Segment::Note(t) => (t.as_str(), true),
                Segment::Tool {
                    tool,
                    summary,
                    output,
                    ..
                } => {
                    if let Some(l) = output.lines().rev().find(|l| !l.trim().is_empty()) {
                        return Some((sanitize(l.trim()), false));
                    }
                    return Some((format!("{tool} {summary}").trim().to_owned(), false));
                }
            };
            if let Some(l) = text.lines().rev().find(|l| !l.trim().is_empty()) {
                return Some((sanitize(l.trim()), reasoning));
            }
        }
        None
    }

    /// All text of the stream as `(kind, text)` pairs, for comparisons.
    pub fn transcript(&self) -> String {
        let mut out = String::new();
        for s in &self.segments {
            match s {
                Segment::Text(t) => out.push_str(&format!("[text]{t}\n")),
                Segment::Reasoning(t) => out.push_str(&format!("[reasoning]{t}\n")),
                Segment::Note(t) => out.push_str(&format!("[note]{t}\n")),
                Segment::Tool {
                    tool,
                    summary,
                    output,
                    ok,
                    ..
                } => out.push_str(&format!("[tool {tool} {summary} {ok:?}]{output}\n")),
            }
        }
        out
    }
}

/// Why a finished model call shows no reasoning, if it reasoned.
///
/// `saw_reasoning` says whether reasoning deltas arrived for the call.
pub fn hidden_reasoning(kind: &EventKind, saw_reasoning: bool) -> Option<String> {
    let EventKind::ModelCallFinished {
        usage, response, ..
    } = kind
    else {
        return None;
    };
    let notice = response
        .as_ref()
        .and_then(|p| match p {
            Payload::Json(v) => serde_json::from_value::<Message>(v.clone()).ok(),
            _ => None,
        })
        .and_then(|m| {
            m.content.iter().find_map(|b| match Notice::from_block(b) {
                Some(Notice::ReasoningHidden { provider, reason }) => Some((provider, reason)),
                _ => None,
            })
        });
    match notice {
        Some((provider, reason)) => {
            let why = match reason {
                HiddenReason::EncryptedOnly => "encrypted",
                HiddenReason::NotProvided => "not provided",
            };
            Some(format!("{REASONING_HIDDEN} ({provider}: {why})"))
        }
        None if usage.reasoning > 0 && !saw_reasoning => Some(format!(
            "{REASONING_HIDDEN} ({} reasoning tokens)",
            usage.reasoning
        )),
        None => None,
    }
}

/// Live streams of every node in a run.
#[derive(Debug, Clone, Default)]
pub struct LiveStreams {
    nodes: HashMap<NodeId, NodeStream>,
    changed: bool,
}

impl LiveStreams {
    /// No streams.
    pub fn new() -> Self {
        Self::default()
    }

    /// Folds one event. Returns whether a stream changed.
    pub fn apply(&mut self, ev: &RunEvent) -> bool {
        let Some(node) = &ev.node_id else {
            return false;
        };
        let changed = match &ev.kind {
            EventKind::Delta {
                call_id,
                channel,
                text,
            } => {
                let s = self.nodes.entry(node.clone()).or_default();
                match channel {
                    DeltaChannel::Text => s.push_text(false, text),
                    DeltaChannel::Reasoning => {
                        s.reasoning_calls.insert(call_id.clone());
                        s.push_text(true, text)
                    }
                    DeltaChannel::ToolArgs => return false,
                }
                true
            }
            EventKind::ToolCall {
                call_id,
                tool,
                args,
            } => {
                let summary = payload_json(args)
                    .map(|v| arg_summary(tool, &v))
                    .unwrap_or_default();
                self.nodes
                    .entry(node.clone())
                    .or_default()
                    .push(Segment::Tool {
                        call_id: call_id.clone(),
                        tool: tool.clone(),
                        summary,
                        output: String::new(),
                        ok: None,
                        duration_ms: None,
                    });
                true
            }
            EventKind::McpCall {
                call_id,
                server,
                tool: name,
            } => {
                let s = self.nodes.entry(node.clone()).or_default();
                if let Some(Segment::Tool { tool, .. }) = s.tool_mut(call_id) {
                    *tool = format!("mcp {server} › {name}");
                    true
                } else {
                    false
                }
            }
            EventKind::ToolResult {
                call_id,
                ok,
                output,
                duration_ms,
            } => {
                let s = self.nodes.entry(node.clone()).or_default();
                let text = payload_text(output);
                if let Some(Segment::Tool {
                    output,
                    ok: o,
                    duration_ms: d,
                    ..
                }) = s.tool_mut(call_id)
                {
                    let old = output.len();
                    *output = text;
                    *o = Some(*ok);
                    *d = Some(*duration_ms);
                    let new = output.len();
                    s.bytes = s.bytes.saturating_sub(old) + new;
                    s.trim();
                    true
                } else {
                    false
                }
            }
            EventKind::ModelCallFinished { call_id, .. } => {
                let s = self.nodes.entry(node.clone()).or_default();
                let saw = s.reasoning_calls.remove(call_id);
                match hidden_reasoning(&ev.kind, saw) {
                    Some(note) => {
                        s.push(Segment::Note(note));
                        true
                    }
                    None => false,
                }
            }
            _ => false,
        };
        self.changed |= changed;
        changed
    }

    /// Appends live tool output (bash streaming) to a node's call.
    pub fn tool_output(&mut self, node: &NodeId, call_id: &str, chunk: &str) {
        let s = self.nodes.entry(node.clone()).or_default();
        if let Some(Segment::Tool { output, .. }) = s.tool_mut(call_id) {
            output.push_str(chunk);
            s.bytes += chunk.len();
            s.trim();
            self.changed = true;
        }
    }

    /// The stream of `node`.
    pub fn stream(&self, node: &NodeId) -> Option<&NodeStream> {
        self.nodes.get(node)
    }

    /// The latest line of `node` and whether it is reasoning.
    pub fn preview(&self, node: &NodeId) -> Option<(String, bool)> {
        self.nodes.get(node)?.last_line()
    }

    /// Whether any stream changed since the last call; resets the flag.
    pub fn take_changed(&mut self) -> bool {
        std::mem::take(&mut self.changed)
    }

    /// Number of nodes with a stream.
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    /// Whether no node has a stream.
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// Drops every stream.
    pub fn clear(&mut self) {
        self.nodes.clear();
        self.changed = true;
    }
}

/// Limits redraws caused by stream updates to one per [`MIN_FRAME`].
#[derive(Debug, Clone, Copy)]
pub struct FrameThrottle {
    min: Duration,
    last: Option<Instant>,
    pending: bool,
}

impl Default for FrameThrottle {
    fn default() -> Self {
        FrameThrottle::new(MIN_FRAME)
    }
}

impl FrameThrottle {
    /// A throttle allowing one frame per `min`.
    pub fn new(min: Duration) -> Self {
        FrameThrottle {
            min,
            last: None,
            pending: false,
        }
    }

    /// Records that something changed.
    pub fn changed(&mut self) {
        self.pending = true;
    }

    /// Whether a frame should be drawn at `now`. Marks it drawn when true.
    pub fn should_draw(&mut self, now: Instant) -> bool {
        if !self.pending {
            return false;
        }
        if self.last.is_some_and(|l| now.duration_since(l) < self.min) {
            return false;
        }
        self.pending = false;
        self.last = Some(now);
        true
    }

    /// When the pending change may be drawn, if one is pending.
    pub fn next_draw(&self) -> Option<Instant> {
        if !self.pending {
            return None;
        }
        Some(self.last.map_or_else(Instant::now, |l| l + self.min))
    }
}

/// Renders one node's stream: reasoning (dimmed, italic), text and tool
/// calls with their output tail.
#[derive(Debug, Clone, Copy)]
pub struct LivePane<'a> {
    /// Colours.
    pub theme: &'a Theme,
    /// Lines of tool output shown per call.
    pub tool_lines: usize,
}

impl LivePane<'_> {
    /// All lines of `stream` at `width`.
    pub fn lines(&self, stream: &NodeStream, width: usize) -> Vec<Line<'static>> {
        let t = self.theme;
        let mut out = Vec::new();
        let reasoning = t.fg(Token::Reasoning);
        for seg in &stream.segments {
            match seg {
                Segment::Reasoning(text) | Segment::Text(text) => {
                    let is_r = matches!(seg, Segment::Reasoning(_));
                    let style = if is_r { reasoning } else { t.fg(Token::Fg) };
                    for l in text.lines() {
                        let mut spans = Vec::new();
                        if is_r {
                            spans.push(Span::styled("┊ ", t.fg(Token::Muted)));
                        }
                        spans.push(Span::styled(sanitize(l), style));
                        out.extend(wrap_line(Line::from(spans), width, &[]));
                    }
                }
                Segment::Note(n) => out.push(Line::from(Span::styled(
                    truncate(&format!("ⓘ {n}"), width),
                    t.fg(Token::Warn).add_modifier(Modifier::ITALIC),
                ))),
                Segment::Tool {
                    tool,
                    summary,
                    output,
                    ok,
                    duration_ms,
                    ..
                } => {
                    let (icon, token) = match ok {
                        None => ('→', Token::Running),
                        Some(true) => ('✔', Token::Ok),
                        Some(false) => ('✗', Token::Error),
                    };
                    let mut spans = vec![
                        Span::styled(format!("{icon} "), t.fg(token)),
                        Span::styled(tool.clone(), t.fg(Token::Accent)),
                        Span::styled(
                            format!(" {}", truncate(&sanitize(summary), width.saturating_sub(4))),
                            t.fg(Token::Muted),
                        ),
                    ];
                    if let Some(ms) = duration_ms.filter(|_| tool.starts_with("mcp ")) {
                        spans.push(Span::styled(
                            format!(" · {}", crate::chat::text::fmt_ms(ms)),
                            t.fg(Token::Number),
                        ));
                    }
                    out.push(truncate_line(Line::from(spans), width));
                    let lines: Vec<&str> = output.lines().collect();
                    for l in &lines[lines.len().saturating_sub(self.tool_lines)..] {
                        out.push(Line::from(Span::styled(
                            truncate(&format!("  {}", sanitize(l)), width),
                            t.fg(Token::Muted),
                        )));
                    }
                }
            }
        }
        out
    }

    /// The last `area.height` lines (tail), skipping `back` lines from the
    /// end when scrolled up.
    pub fn tail(&self, stream: &NodeStream, area: Rect, back: usize) -> Vec<Line<'static>> {
        let lines = self.lines(stream, usize::from(area.width.max(1)));
        let h = usize::from(area.height);
        let end = lines.len().saturating_sub(back);
        let start = end.saturating_sub(h);
        lines[start..end].to_vec()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use harness_events::schema::{RunId, Usage};
    use serde_json::json;
    use time::macros::datetime;

    fn ev(seq: u64, node: &str, kind: EventKind) -> RunEvent {
        RunEvent {
            seq,
            ts: datetime!(2026-10-22 12:00:00 UTC),
            run_id: RunId(uuid::Uuid::nil()),
            node_id: Some(NodeId::from(node)),
            parent_node_id: None,
            kind,
        }
    }

    fn delta(seq: u64, node: &str, ch: DeltaChannel, text: &str) -> RunEvent {
        ev(
            seq,
            node,
            EventKind::Delta {
                call_id: format!("{node}-c1"),
                channel: ch,
                text: text.into(),
            },
        )
    }

    fn finished(seq: u64, node: &str, reasoning: u64, response: Option<Payload>) -> RunEvent {
        ev(
            seq,
            node,
            EventKind::ModelCallFinished {
                call_id: format!("{node}-c1"),
                usage: Usage {
                    input: 10,
                    cached_input: 0,
                    output: 5,
                    reasoning,
                    cache_write: 0,
                },
                stop_reason: None,
                response,
                duration_ms: 3,
            },
        )
    }

    #[test]
    fn preview_is_the_latest_line_and_marks_reasoning() {
        let mut s = LiveStreams::new();
        let n = NodeId::from("a");
        s.apply(&delta(
            1,
            "a",
            DeltaChannel::Reasoning,
            "thinking about\nthe parser",
        ));
        assert_eq!(s.preview(&n), Some(("the parser".into(), true)));
        s.apply(&delta(2, "a", DeltaChannel::Text, "I'll fix "));
        s.apply(&delta(3, "a", DeltaChannel::Text, "parse_header"));
        assert_eq!(s.preview(&n), Some(("I'll fix parse_header".into(), false)));
        s.apply(&ev(
            4,
            "a",
            EventKind::ToolCall {
                call_id: "t1".into(),
                tool: "bash".into(),
                args: Payload::Json(json!({"command": "cargo test"})),
            },
        ));
        s.tool_output(&n, "t1", "running 3 tests\n");
        assert_eq!(s.preview(&n), Some(("running 3 tests".into(), false)));
        assert!(s.take_changed());
        assert!(!s.take_changed());
    }

    #[test]
    fn hidden_reasoning_is_stated() {
        let mut s = LiveStreams::new();
        s.apply(&finished(1, "a", 812, None));
        let st = s.stream(&NodeId::from("a")).unwrap();
        assert!(st.transcript().contains(REASONING_HIDDEN));

        let notice = Notice::ReasoningHidden {
            provider: "openai".into(),
            reason: HiddenReason::EncryptedOnly,
        };
        let msg = json!({"role": "assistant", "content": [
            serde_json::to_value(notice.to_block()).unwrap()
        ]});
        s.apply(&finished(2, "b", 0, Some(Payload::Json(msg))));
        let t = s.stream(&NodeId::from("b")).unwrap().transcript();
        assert!(
            t.contains("reasoning not exposed by provider (openai: encrypted)"),
            "{t}"
        );

        // Visible reasoning: no note.
        s.apply(&delta(3, "c", DeltaChannel::Reasoning, "hmm"));
        s.apply(&finished(4, "c", 100, None));
        let t = s.stream(&NodeId::from("c")).unwrap().transcript();
        assert!(!t.contains(REASONING_HIDDEN), "{t}");
    }

    #[test]
    fn streams_are_bounded() {
        let mut s = LiveStreams::new();
        let chunk = "x".repeat(1000);
        for i in 0..200 {
            let ch = if i % 2 == 0 {
                DeltaChannel::Text
            } else {
                DeltaChannel::Reasoning
            };
            s.apply(&delta(i, "a", ch, &chunk));
        }
        let st = s.stream(&NodeId::from("a")).unwrap();
        assert!(st.bytes <= MAX_STREAM_BYTES + 1000);
    }

    #[test]
    fn throttle_limits_frames() {
        let mut t = FrameThrottle::new(Duration::from_millis(16));
        let t0 = Instant::now();
        assert!(!t.should_draw(t0), "nothing pending");
        t.changed();
        assert!(t.should_draw(t0));
        t.changed();
        assert!(!t.should_draw(t0 + Duration::from_millis(5)));
        assert!(t.should_draw(t0 + Duration::from_millis(17)));
    }

    #[test]
    fn pane_renders_reasoning_dimmed_and_tools() {
        let theme = Theme::default();
        let mut s = LiveStreams::new();
        s.apply(&delta(1, "a", DeltaChannel::Reasoning, "plan"));
        s.apply(&delta(2, "a", DeltaChannel::Text, "done"));
        let st = s.stream(&NodeId::from("a")).unwrap();
        let pane = LivePane {
            theme: &theme,
            tool_lines: 3,
        };
        let lines = pane.tail(st, Rect::new(0, 0, 40, 5), 0);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].spans[1].style, theme.fg(Token::Reasoning));
        assert!(
            theme
                .fg(Token::Reasoning)
                .add_modifier
                .contains(Modifier::ITALIC)
        );
    }
}
