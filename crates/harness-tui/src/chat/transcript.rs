//! The virtualized transcript.
//!
//! The transcript is a list of [`Entry`]s folded from run events: user
//! messages, assistant turns (streaming Markdown, reasoning, a metadata
//! line), groups of tool cards, inline notices (errors, retries, verify
//! results, memory updates) and subagent cards.
//!
//! **Virtualization.** Each entry caches its laid-out lines, keyed by width,
//! content version, settings, focus and (for live entries) the spinner
//! frame. A frame lays out only the entries it shows: following the tail it
//! fills the viewport from the last entry upwards; scrolled back it is
//! anchored at an entry and fills downwards. Scrolling walks entries from
//! the anchor, so the cost of a frame depends on the viewport and the scroll
//! distance, never on the transcript length.
//!
//! **Bottom anchoring.** Like a chat app, the newest line sits at the bottom
//! of the viewport. A transcript shorter than the viewport is padded above
//! ([`Transcript::pad`]), so a short conversation hugs the input. Scrolling up
//! pins the top line; new output then leaves the view still and raises
//! [`Transcript::has_unseen`] until the view follows the tail again. A
//! resize keeps a pinned reading position (scaled within its entry when the
//! width changes) and lets a following view stay at the bottom.
//!
//! **Copy.** User and assistant headers, code blocks and tool cards carry
//! `⧉ copy` buttons ([`TranscriptAction::Copy`]); [`Transcript::copy_text`]
//! resolves a [`CopyTarget`] to plain text. A keyboard selection mode
//! ([`Transcript::select_start`]) moves a line cursor over the rendered
//! transcript, marks a range and copies it without borders or gutters.
//!
//! **Timestamps** shown on every entry come from the event that created it
//! (`RunEvent::ts`), so a live session and a replay of its log show the same
//! times. User messages are not in the event log yet; their time is the one
//! passed to [`Transcript::push_user`].

use std::collections::HashMap;

use harness_config::ini::config::{Config, ShowThinking};
use harness_events::schema::{
    DeltaChannel, EventKind, FileChangeKind, MemoryAction, NodeId, NodeKind, NodeStatus, RunEvent,
    RunStatus, Usage,
};
use ratatui::layout::Rect;
use ratatui::style::Modifier;
use ratatui::text::{Line, Span};
use time::{OffsetDateTime, UtcOffset};

mod copy;

pub use copy::{CopyTarget, Selection};

use super::markdown::{MarkdownCache, code_blocks};
use super::text::{
    cache_pct, fmt_cost, fmt_ms, fmt_tokens, right_align, sanitize, truncate_line, wrap_line,
};
use super::timefmt::TimeFormat;
use crate::cards::{
    ApprovalNote, CardCtx, CardGroup, CardStatus, ToolCard, arg_summary, copy_button_id,
    payload_json, payload_text,
};
use crate::clipboard::COPY_LABEL;
use crate::indicators::{Spinner, node_glyph};
use crate::memory::card::{MemoryCard, undo_button_id};
use crate::theme::{Theme, Token};

/// Chat display settings from `[ui]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ViewSettings {
    /// `[ui] timestamps` and the display offset.
    pub time: TimeFormat,
    /// `[ui] show_thinking`.
    pub show_thinking: ShowThinking,
    /// `[ui] tool_preview_lines`.
    pub preview_lines: usize,
    /// `[ui] markdown`.
    pub markdown: bool,
}

impl Default for ViewSettings {
    fn default() -> Self {
        ViewSettings {
            time: TimeFormat::default(),
            show_thinking: ShowThinking::Collapsed,
            preview_lines: 8,
            markdown: true,
        }
    }
}

impl ViewSettings {
    /// Settings from `[ui]`, showing wall-clock times at `offset`.
    pub fn from_config(cfg: &Config, offset: UtcOffset) -> ViewSettings {
        ViewSettings {
            time: TimeFormat::new(cfg.ui.timestamps, offset),
            show_thinking: cfg.ui.show_thinking,
            preview_lines: cfg.ui.tool_preview_lines as usize,
            markdown: cfg.ui.markdown,
        }
    }
}

/// What clicking or pressing Enter on a transcript element does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TranscriptAction {
    /// Expand or collapse a tool card.
    ToggleCard(String),
    /// Expand or collapse an assistant turn's reasoning.
    ToggleThinking(u64),
    /// Open a subagent in the map / node detail.
    OpenNode(NodeId),
    /// Undo a memory change (the commit id).
    UndoMemory(String),
    /// Copy a message, code block, tool output or diff.
    Copy(CopyTarget),
}

/// What activating a transcript element asks the chat to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TranscriptOutput {
    /// Open this node in the map / node detail.
    OpenNode(NodeId),
    /// Revert this memory commit.
    UndoMemory(String),
    /// Copy `text` to the clipboard; `what` names it for the toast.
    Copy {
        /// What was copied, e.g. `message` or `code block`.
        what: String,
        /// The text.
        text: String,
    },
}

/// Usage and timing of a finished assistant turn.
#[derive(Debug, Clone, PartialEq)]
pub struct TurnMeta {
    /// Model id.
    pub model: String,
    /// Token usage.
    pub usage: Usage,
    /// Cost, when known.
    pub cost: Option<f64>,
    /// Wall time of the model call.
    pub duration_ms: u64,
    /// Provider stop reason.
    pub stop_reason: Option<String>,
}

impl TurnMeta {
    /// The metadata line, e.g.
    /// `sonnet · ↑12.4k (cache 96%) ↓812 · reasoning 1.2k · $0.021 · 4.2s · end_turn`.
    pub fn line(&self, theme: &Theme) -> Line<'static> {
        let muted = theme.fg(Token::Muted);
        let mut parts = vec![self.model.clone()];
        let u = self.usage;
        let mut tokens = format!("↑{}", fmt_tokens(u.input + u.cached_input));
        if let Some(p) = cache_pct(u.input, u.cached_input) {
            tokens.push_str(&format!(" (cache {p}%)"));
        }
        tokens.push_str(&format!(" ↓{}", fmt_tokens(u.output)));
        parts.push(tokens);
        if u.reasoning > 0 {
            parts.push(format!("reasoning {}", fmt_tokens(u.reasoning)));
        }
        parts.push(fmt_cost(self.cost));
        parts.push(fmt_ms(self.duration_ms));
        if let Some(s) = &self.stop_reason {
            parts.push(s.clone());
        }
        Line::from(Span::styled(parts.join(" · "), muted))
    }
}

/// One assistant model call.
#[derive(Debug, Clone)]
pub struct AssistantTurn {
    /// Model call id.
    pub call_id: String,
    /// Model id.
    pub model: String,
    /// Answer text so far.
    pub text: String,
    /// Reasoning text so far.
    pub reasoning: String,
    /// Set when the call finished.
    pub meta: Option<TurnMeta>,
    /// Reasoning expanded.
    pub thinking_expanded: bool,
    md: MarkdownCache,
}

/// A provider retry (from `harness_providers::retry::RetryEvent`).
#[derive(Debug, Clone, PartialEq)]
pub struct RetryNotice {
    /// Model call being retried.
    pub call_id: Option<String>,
    /// Why, e.g. `429 rate limited`.
    pub reason: String,
    /// Attempt number.
    pub attempt: u32,
    /// Retry limit.
    pub max: u32,
    /// Delay before the retry in seconds.
    pub delay_secs: f64,
    /// Whether the partial output already streamed is dropped.
    pub discard_partial: bool,
}

/// Severity of an inline notice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoticeLevel {
    /// Informational.
    Info,
    /// Success.
    Ok,
    /// Recoverable problem, retry.
    Warn,
    /// Error.
    Error,
}

impl NoticeLevel {
    fn style(self) -> (char, Token) {
        match self {
            NoticeLevel::Info => ('◆', Token::Accent),
            NoticeLevel::Ok => ('✔', Token::Ok),
            NoticeLevel::Warn => ('↻', Token::Warn),
            NoticeLevel::Error => ('✗', Token::Error),
        }
    }
}

/// An inline event card: error, retry, verify result, memory update.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notice {
    /// Severity.
    pub level: NoticeLevel,
    /// One-line text.
    pub text: String,
    /// Further lines.
    pub detail: Vec<String>,
}

/// A subagent's live card.
#[derive(Debug, Clone, PartialEq)]
pub struct SubagentCard {
    /// Node id.
    pub node: NodeId,
    /// Node name.
    pub name: String,
    /// Agent profile, when it differs from the name.
    pub agent: Option<String>,
    /// Model, when known.
    pub model: Option<String>,
    /// Remote host, when remote.
    pub host: Option<String>,
    /// Status.
    pub status: NodeStatus,
    /// Last action, e.g. `read src/net/mod.rs`.
    pub last_action: String,
    /// Text of the subagent's latest turn; shown as the final summary
    /// once it finishes.
    pub summary: String,
    /// Cost so far.
    pub cost: f64,
    /// Finished at.
    pub finished: Option<OffsetDateTime>,
}

/// What an entry is.
#[derive(Debug, Clone)]
pub enum EntryKind {
    /// A user message.
    User {
        /// Text.
        text: String,
    },
    /// An assistant turn.
    Assistant(Box<AssistantTurn>),
    /// Tool calls of one model turn.
    Tools {
        /// The cards.
        group: CardGroup,
        /// The model call that made them.
        turn: Option<String>,
    },
    /// An inline notice.
    Notice(Notice),
    /// A subagent card.
    Subagent(SubagentCard),
    /// An "updated memory" card with undo.
    Memory(MemoryCard),
}

#[derive(Debug, Clone)]
struct Hit {
    id: String,
    start: usize,
    len: usize,
    action: TranscriptAction,
    /// Column and width of a button; `w == 0` spans the whole row.
    col: usize,
    w: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Key {
    width: usize,
    version: u64,
    settings: ViewSettings,
    focus: Option<String>,
    live: Option<(char, i64)>,
}

#[derive(Debug, Clone)]
struct Layout {
    key: Key,
    lines: Vec<Line<'static>>,
    hits: Vec<Hit>,
}

/// One transcript entry.
#[derive(Debug, Clone)]
pub struct Entry {
    /// Stable id.
    pub id: u64,
    /// Time from the event log.
    pub ts: OffsetDateTime,
    /// Content.
    pub kind: EntryKind,
    version: u64,
    layout: Option<Layout>,
}

impl Entry {
    fn touch(&mut self) {
        self.version += 1;
    }

    fn element_ids(&self) -> Vec<String> {
        match &self.kind {
            EntryKind::Tools { group, .. } => group
                .cards
                .iter()
                .flat_map(|c| {
                    [
                        card_id(&c.call_id),
                        copy_button_id(&c.call_id, crate::cards::CardCopy::Output),
                        copy_button_id(&c.call_id, crate::cards::CardCopy::Diff),
                    ]
                })
                .collect(),
            EntryKind::User { .. } => vec![copy_message_id(self.id)],
            EntryKind::Assistant(t) => {
                let mut v = vec![thinking_id(self.id), copy_message_id(self.id)];
                v.extend((0..code_blocks(&t.text).len()).map(|n| copy_code_id(self.id, n)));
                v
            }
            EntryKind::Subagent(s) => vec![subagent_id(&s.node)],
            EntryKind::Memory(m) if m.commit().is_some() && !m.undone => {
                vec![undo_button_id(&m.memory_id)]
            }
            _ => Vec::new(),
        }
    }

    fn is_live(&self) -> bool {
        match &self.kind {
            EntryKind::Tools { group, .. } => {
                group.cards.iter().any(|c| c.status == CardStatus::Running)
            }
            EntryKind::Subagent(s) => !s.status.is_terminal(),
            _ => false,
        }
    }
}

/// Element id of a tool card.
pub fn card_id(call_id: &str) -> String {
    format!("card.{call_id}")
}

/// Element id of an assistant turn's reasoning toggle.
pub fn thinking_id(entry: u64) -> String {
    format!("thinking.{entry}")
}

/// Element id of a message's copy button.
pub fn copy_message_id(entry: u64) -> String {
    format!("copy.msg.{entry}")
}

/// Element id of code block `n`'s copy button in an assistant turn.
pub fn copy_code_id(entry: u64, n: usize) -> String {
    format!("copy.code.{entry}.{n}")
}

/// Element id of a subagent card.
pub fn subagent_id(node: &NodeId) -> String {
    format!("subagent.{}", node.as_str())
}

/// Rendering inputs that change per frame.
#[derive(Debug, Clone, Copy)]
pub struct Frame<'a> {
    /// Colours.
    pub theme: &'a Theme,
    /// Wall-clock now (relative timestamps, subagent elapsed time).
    pub now: OffsetDateTime,
    /// Spinner state.
    pub spinner: Spinner,
    /// Focused element id.
    pub focus: Option<&'a str>,
}

/// One interactive region of the rendered transcript.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranscriptHit {
    /// Element id.
    pub id: String,
    /// Screen rectangle.
    pub rect: Rect,
    /// What activating it does.
    pub action: TranscriptAction,
}

/// One rendered frame: visible lines and their hits.
#[derive(Debug, Clone, Default)]
struct Rendered {
    lines: Vec<Line<'static>>,
    hits: Vec<TranscriptHit>,
}

/// The transcript and its scroll state.
#[derive(Debug, Clone, Default)]
pub struct Transcript {
    entries: Vec<Entry>,
    next_id: u64,
    settings: ViewSettings,
    main: Option<NodeId>,
    /// User entries pushed locally on submit, not yet confirmed by a
    /// `user_message` event.
    pending_users: Vec<usize>,
    /// Done-gating of the current main turn: the last verify result and
    /// how many files it changed.
    turn_verify: Option<bool>,
    turn_changes: usize,
    turns: HashMap<String, usize>,
    cards: HashMap<String, usize>,
    subagents: HashMap<NodeId, usize>,
    /// Agent display names from `run_started` (subagent cards).
    agent_names: std::collections::BTreeMap<String, String>,
    last_turn: Option<String>,
    /// `None` follows the tail; `Some((entry, line))` pins the top line.
    anchor: Option<(usize, usize)>,
    layouts: usize,
    last_height: usize,
    last_width: usize,
    /// Blank rows above the content in the last frame.
    last_pad: usize,
    /// New output arrived while the view was pinned.
    unseen: bool,
    /// Keyboard selection mode, when active.
    selection: Option<Selection>,
}

impl Transcript {
    /// An empty transcript.
    pub fn new(settings: ViewSettings) -> Transcript {
        Transcript {
            settings,
            ..Transcript::default()
        }
    }

    /// The entries.
    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    /// Display settings.
    pub fn settings(&self) -> ViewSettings {
        self.settings
    }

    /// Changes display settings (all layouts are redone lazily).
    pub fn set_settings(&mut self, settings: ViewSettings) {
        self.settings = settings;
    }

    /// Entry layouts computed so far (cache misses). Used to check that
    /// frames lay out only what they show.
    pub fn layouts(&self) -> usize {
        self.layouts
    }

    /// Whether the view follows new output.
    pub fn is_following(&self) -> bool {
        self.anchor.is_none()
    }

    /// Blank rows above the content in the last rendered frame: the
    /// viewport height minus the line count, non-zero only when the whole
    /// transcript fits. Callers draw the lines below this padding.
    pub fn pad(&self) -> u16 {
        self.last_pad as u16
    }

    /// Whether output arrived below a pinned (scrolled-up) view since it
    /// was pinned. Cleared when the view follows the tail again.
    pub fn has_unseen(&self) -> bool {
        self.unseen
    }

    /// Pins the top line at `pos`, or follows when `pos` is at or past the
    /// tail (so a transcript that fits never leaves the bottom).
    fn pin(&mut self, pos: (usize, usize), width: usize, height: usize, f: &Frame<'_>) {
        let tail = self.tail_top(width, height, f);
        if pos >= tail {
            self.follow();
        } else {
            self.anchor = Some(pos);
        }
    }

    /// A cheap fingerprint of the bottom of the transcript.
    fn tail_mark(&self) -> (usize, u64) {
        (
            self.entries.len(),
            self.entries.last().map_or(0, |e| e.version),
        )
    }

    /// The tool card for `call_id`.
    pub fn card(&self, call_id: &str) -> Option<&ToolCard> {
        let i = *self.cards.get(call_id)?;
        match &self.entries[i].kind {
            EntryKind::Tools { group, .. } => group.card(call_id),
            _ => None,
        }
    }

    fn push(&mut self, ts: OffsetDateTime, kind: EntryKind) -> usize {
        let id = self.next_id;
        self.next_id += 1;
        self.entries.push(Entry {
            id,
            ts,
            kind,
            version: 0,
            layout: None,
        });
        if self.anchor.is_some() {
            self.unseen = true;
        }
        self.entries.len() - 1
    }

    /// Appends a user message typed in this UI. When the core records it
    /// (`user_message`), the entry takes the logged time instead of being
    /// added twice, so live view and replay show the same entry and time.
    pub fn push_user(&mut self, text: &str, ts: OffsetDateTime) {
        let i = self.push(
            ts,
            EntryKind::User {
                text: text.to_owned(),
            },
        );
        self.pending_users.push(i);
    }

    /// A `user_message` event: confirms the matching local entry, or adds
    /// one (replays, other front ends).
    fn logged_user(&mut self, text: &str, ts: OffsetDateTime) {
        let found = self.pending_users.iter().position(
            |&i| matches!(&self.entries[i].kind, EntryKind::User { text: t } if t == text),
        );
        match found {
            Some(p) => {
                let i = self.pending_users[p];
                self.pending_users.drain(..=p);
                self.entries[i].ts = ts;
                self.entries[i].touch();
            }
            None => {
                self.push(
                    ts,
                    EntryKind::User {
                        text: text.to_owned(),
                    },
                );
            }
        }
    }

    /// Appends an inline notice.
    pub fn push_notice(&mut self, notice: Notice, ts: OffsetDateTime) {
        self.push(ts, EntryKind::Notice(notice));
    }

    /// Shows a provider retry inline (`StreamEvent::Retry`). When
    /// `retry.discard_partial` is set, the partial output already streamed
    /// for `retry.call_id` is dropped.
    pub fn push_retry(&mut self, retry: &RetryNotice, ts: OffsetDateTime) {
        let RetryNotice {
            call_id,
            reason,
            attempt,
            max,
            delay_secs,
            discard_partial,
        } = retry;
        let call_id = call_id.as_deref();
        if *discard_partial
            && let Some(&i) = call_id.and_then(|c| self.turns.get(c))
            && let EntryKind::Assistant(t) = &mut self.entries[i].kind
        {
            t.text.clear();
            t.reasoning.clear();
            t.md = MarkdownCache::new();
            self.entries[i].touch();
        }
        self.push_notice(
            Notice {
                level: NoticeLevel::Warn,
                text: format!("retrying in {delay_secs:.0}s ({reason}, attempt {attempt}/{max})"),
                detail: Vec::new(),
            },
            ts,
        );
    }

    /// Appends live output of a running tool call (bash streaming).
    pub fn tool_output(&mut self, call_id: &str, chunk: &str) {
        self.with_card(call_id, |c| c.push_output(chunk));
    }

    /// Marks a card as waiting for approval, or running again with `None`.
    pub fn set_card_approval(&mut self, call_id: &str, note: Option<ApprovalNote>) {
        self.with_card(call_id, |c| c.set_approval(note));
    }

    fn with_card(&mut self, call_id: &str, f: impl FnOnce(&mut ToolCard)) -> bool {
        let Some(&i) = self.cards.get(call_id) else {
            return false;
        };
        let e = &mut self.entries[i];
        if let EntryKind::Tools { group, .. } = &mut e.kind
            && let Some(card) = group.card_mut(call_id)
        {
            f(card);
            e.touch();
            return true;
        }
        false
    }

    /// Runs a transcript action. Returns what the chat must do for
    /// [`TranscriptAction::OpenNode`] and [`TranscriptAction::UndoMemory`].
    pub fn activate(&mut self, action: &TranscriptAction) -> Option<TranscriptOutput> {
        match action {
            TranscriptAction::ToggleCard(id) => {
                self.with_card(id, ToolCard::toggle);
                None
            }
            TranscriptAction::ToggleThinking(id) => {
                if let Some(e) = self.entries.iter_mut().find(|e| e.id == *id)
                    && let EntryKind::Assistant(t) = &mut e.kind
                {
                    t.thinking_expanded = !t.thinking_expanded;
                    e.touch();
                }
                None
            }
            TranscriptAction::OpenNode(n) => Some(TranscriptOutput::OpenNode(n.clone())),
            TranscriptAction::UndoMemory(c) => Some(TranscriptOutput::UndoMemory(c.clone())),
            TranscriptAction::Copy(t) => self
                .copy_text(t)
                .map(|(what, text)| TranscriptOutput::Copy { what, text }),
        }
    }

    /// Marks the memory card of `commit` as undone.
    pub fn memory_undone(&mut self, commit: &str) {
        for e in &mut self.entries {
            if let EntryKind::Memory(m) = &mut e.kind
                && m.memory_id == commit
            {
                m.mark_undone();
                e.touch();
            }
        }
    }

    fn is_main(&mut self, ev: &RunEvent) -> bool {
        if let EventKind::NodeStarted {
            kind: NodeKind::Agent,
            ..
        } = &ev.kind
            && ev.parent_node_id.is_none()
        {
            // Each user turn may run on its own root node: the latest one is
            // the conversation's main agent.
            self.main = ev.node_id.clone();
        }
        match &ev.node_id {
            None => true,
            Some(id) => self.main.as_ref().is_none_or(|m| m == id),
        }
    }

    /// Folds one run event.
    pub fn apply(&mut self, ev: &RunEvent) {
        let before = self.tail_mark();
        self.apply_event(ev);
        if self.anchor.is_some() && self.tail_mark() != before {
            self.unseen = true;
        }
    }

    fn apply_event(&mut self, ev: &RunEvent) {
        if let EventKind::RunStarted { agent_names, .. } = &ev.kind {
            self.agent_names.clone_from(agent_names);
        }
        if !self.is_main(ev) {
            self.apply_subagent(ev);
            return;
        }
        let ts = ev.ts;
        match &ev.kind {
            EventKind::UserMessage { text } => self.logged_user(text, ts),
            EventKind::ToolOutput { call_id, text, .. } => self.tool_output(call_id, text),
            EventKind::NodeStarted {
                kind: NodeKind::Agent,
                ..
            } if ev.parent_node_id.is_none() => {
                self.turn_verify = None;
                self.turn_changes = 0;
            }
            EventKind::NodeFinished { status, .. }
                if ev.node_id.is_some()
                    && ev.node_id == self.main
                    && matches!(status, NodeStatus::Succeeded | NodeStatus::Failed) =>
            {
                if let Some(n) = self.verification_notice() {
                    self.push_notice(n, ts);
                }
            }
            EventKind::ModelCallStarted { call_id, model, .. } => {
                let thinking_expanded = self.settings.show_thinking == ShowThinking::Expanded;
                let i = self.push(
                    ts,
                    EntryKind::Assistant(Box::new(AssistantTurn {
                        call_id: call_id.clone(),
                        model: model.clone(),
                        text: String::new(),
                        reasoning: String::new(),
                        meta: None,
                        thinking_expanded,
                        md: MarkdownCache::new(),
                    })),
                );
                self.turns.insert(call_id.clone(), i);
                self.last_turn = Some(call_id.clone());
            }
            EventKind::Delta {
                call_id,
                channel,
                text,
            } => {
                if let Some(&i) = self.turns.get(call_id)
                    && let EntryKind::Assistant(t) = &mut self.entries[i].kind
                {
                    match channel {
                        DeltaChannel::Text => t.text.push_str(text),
                        DeltaChannel::Reasoning => t.reasoning.push_str(text),
                        DeltaChannel::ToolArgs => return,
                    }
                    self.entries[i].touch();
                }
            }
            EventKind::ModelCallFinished {
                call_id,
                usage,
                stop_reason,
                duration_ms,
                ..
            } => {
                if let Some(&i) = self.turns.get(call_id)
                    && let EntryKind::Assistant(t) = &mut self.entries[i].kind
                {
                    t.meta = Some(TurnMeta {
                        model: t.model.clone(),
                        usage: *usage,
                        cost: None,
                        duration_ms: *duration_ms,
                        stop_reason: stop_reason.clone(),
                    });
                    self.entries[i].touch();
                }
            }
            EventKind::Cost { usd } => {
                // Attach to the latest finished turn that has no cost yet.
                if let Some(c) = self.last_turn.clone()
                    && let Some(&i) = self.turns.get(&c)
                    && let EntryKind::Assistant(t) = &mut self.entries[i].kind
                    && let Some(m) = &mut t.meta
                {
                    m.cost = Some(m.cost.unwrap_or(0.0) + usd);
                    self.entries[i].touch();
                }
            }
            EventKind::ToolCall {
                call_id,
                tool,
                args,
            } => {
                let args = payload_json(args)
                    .unwrap_or_else(|| serde_json::Value::String(payload_text(args)));
                let card = ToolCard::new(call_id.clone(), tool.clone(), args, ts);
                let turn = self.last_turn.clone();
                let same_turn = matches!(
                    self.entries.last().map(|e| &e.kind),
                    Some(EntryKind::Tools { turn: t, .. }) if *t == turn && turn.is_some()
                );
                let i = if same_turn {
                    let i = self.entries.len() - 1;
                    if let EntryKind::Tools { group, .. } = &mut self.entries[i].kind {
                        group.cards.push(card);
                    }
                    self.entries[i].touch();
                    i
                } else {
                    self.push(
                        ts,
                        EntryKind::Tools {
                            group: CardGroup::new(card),
                            turn,
                        },
                    )
                };
                self.cards.insert(call_id.clone(), i);
            }
            EventKind::ToolResult {
                call_id,
                ok,
                output,
                duration_ms,
            } => {
                let out = payload_text(output);
                self.with_card(call_id, |c| c.finish(*ok, out, *duration_ms));
            }
            EventKind::McpCall {
                call_id,
                server,
                tool,
            } => {
                self.with_card(call_id, |c| c.set_mcp(server, tool));
            }
            EventKind::McpToolsChanged {
                server,
                added,
                removed,
                tools,
            } => {
                let mut detail = Vec::new();
                if !added.is_empty() {
                    detail.push(format!("added: {}", added.join(", ")));
                }
                if !removed.is_empty() {
                    detail.push(format!("removed: {}", removed.join(", ")));
                }
                self.push_notice(
                    Notice {
                        level: NoticeLevel::Info,
                        text: format!(
                            "MCP server {server} changed its tools ({tools} selected available)"
                        ),
                        detail,
                    },
                    ts,
                );
            }
            EventKind::FileChanged { path, change, diff } => {
                self.turn_changes += 1;
                let diff = diff.as_ref().map(payload_text);
                self.file_changed(path, *change, diff);
            }
            EventKind::NodeRetry { attempt, reason } => self.push_notice(
                Notice {
                    level: NoticeLevel::Warn,
                    text: format!("retry {attempt}: {reason}"),
                    detail: Vec::new(),
                },
                ts,
            ),
            EventKind::Error {
                message,
                recoverable,
                ..
            } => self.push_notice(
                Notice {
                    level: if *recoverable {
                        NoticeLevel::Warn
                    } else {
                        NoticeLevel::Error
                    },
                    text: message.lines().next().unwrap_or("").to_owned(),
                    detail: message.lines().skip(1).map(str::to_owned).collect(),
                },
                ts,
            ),
            EventKind::VerifyFinished {
                command,
                passed,
                exit_code,
                duration_ms,
                output,
            } => {
                self.turn_verify = Some(*passed);
                let text = if *passed {
                    format!("verify passed: {command} · {}", fmt_ms(*duration_ms))
                } else {
                    let code = exit_code
                        .map(|c| format!(" (exit {c})"))
                        .unwrap_or_default();
                    format!("verify failed{code}: {command} · {}", fmt_ms(*duration_ms))
                };
                let detail = if *passed {
                    Vec::new()
                } else {
                    let out = payload_text(output);
                    let lines: Vec<&str> = out.lines().collect();
                    lines[lines.len().saturating_sub(5)..]
                        .iter()
                        .map(|s| (*s).to_owned())
                        .collect()
                };
                self.push_notice(
                    Notice {
                        level: if *passed {
                            NoticeLevel::Ok
                        } else {
                            NoticeLevel::Error
                        },
                        text,
                        detail,
                    },
                    ts,
                );
            }
            EventKind::Memory { .. } if MemoryCard::from_event(ev).is_some() => {
                if let Some(card) = MemoryCard::from_event(ev) {
                    self.push(ts, EntryKind::Memory(card));
                }
            }
            EventKind::Memory {
                action, summary, ..
            } => {
                let verb = match action {
                    MemoryAction::Recalled => "recalled",
                    MemoryAction::Written => "written",
                    MemoryAction::Updated => "updated",
                    MemoryAction::Deleted => "deleted",
                };
                self.push_notice(
                    Notice {
                        level: NoticeLevel::Info,
                        text: format!("memory {verb}: {summary}"),
                        detail: Vec::new(),
                    },
                    ts,
                );
            }
            EventKind::RunFinished {
                status: RunStatus::Failed,
                error,
            } => self.push_notice(
                Notice {
                    level: NoticeLevel::Error,
                    text: format!(
                        "run failed: {}",
                        error.as_deref().unwrap_or("unknown error")
                    ),
                    detail: Vec::new(),
                },
                ts,
            ),
            _ => {}
        }
    }

    /// The honest final status of a finished main turn: verified, failed
    /// verification, or changes that no verify command checked.
    fn verification_notice(&self) -> Option<Notice> {
        let (level, text) = match self.turn_verify {
            Some(true) => (
                NoticeLevel::Ok,
                "verified: the verify command passed".to_owned(),
            ),
            Some(false) => (
                NoticeLevel::Error,
                "verification failed: the changes did not pass the verify command".to_owned(),
            ),
            None if self.turn_changes > 0 => (
                NoticeLevel::Warn,
                format!(
                    "unverified: {} file change{} and no verify command ran",
                    self.turn_changes,
                    if self.turn_changes == 1 { "" } else { "s" }
                ),
            ),
            None => return None,
        };
        Some(Notice {
            level,
            text,
            detail: Vec::new(),
        })
    }

    fn file_changed(&mut self, path: &str, kind: FileChangeKind, diff: Option<String>) {
        // The most recent write/edit card on this path.
        let target = self.cards.iter().filter_map(|(id, &i)| {
            let EntryKind::Tools { group, .. } = &self.entries[i].kind else {
                return None;
            };
            let c = group.card(id)?;
            let p = crate::cards::str_arg(&c.args, &["path", "file_path"])?;
            (matches!(c.tool.as_str(), "write" | "edit")
                && (path.ends_with(&p) || p.ends_with(path)))
            .then(|| (i, c.ts, id.clone()))
        });
        if let Some((_, _, id)) = target.max_by_key(|(i, ts, _)| (*i, *ts)) {
            self.with_card(&id, |c| c.set_change(kind, diff));
        }
    }

    fn apply_subagent(&mut self, ev: &RunEvent) {
        let Some(node) = ev.node_id.clone() else {
            return;
        };
        match &ev.kind {
            EventKind::NodeStarted {
                kind: NodeKind::Subagent,
                name,
                agent,
                model,
                host,
                ..
            } if ev.parent_node_id.is_none() || ev.parent_node_id == self.main => {
                let i = self.push(
                    ev.ts,
                    EntryKind::Subagent(SubagentCard {
                        node: node.clone(),
                        // Agent display names, as in the map and history.
                        name: match agent {
                            Some(a) if a == name => {
                                harness_events::names::agent_display_name(a, &self.agent_names)
                            }
                            _ => name.clone(),
                        },
                        agent: agent.as_deref().filter(|a| a != name).map(|a| {
                            harness_events::names::agent_display_name(a, &self.agent_names)
                        }),
                        summary: String::new(),
                        model: model.clone(),
                        host: host.clone(),
                        status: NodeStatus::Running,
                        last_action: "starting".into(),
                        cost: 0.0,
                        finished: None,
                    }),
                );
                self.subagents.insert(node, i);
            }
            _ => {
                let Some(&i) = self.subagents.get(&node) else {
                    return;
                };
                let e = &mut self.entries[i];
                let EntryKind::Subagent(s) = &mut e.kind else {
                    return;
                };
                match &ev.kind {
                    EventKind::ToolCall { tool, args, .. } => {
                        let summary = payload_json(args)
                            .map(|v| arg_summary(tool, &v))
                            .unwrap_or_default();
                        s.last_action = format!("{tool} {summary}").trim().to_owned();
                    }
                    EventKind::McpCall { server, tool, .. } => {
                        s.last_action = format!("mcp {server} › {tool}");
                    }
                    EventKind::ModelCallStarted { model, .. } => {
                        s.model.get_or_insert_with(|| model.clone());
                        s.last_action = "thinking".into();
                        s.summary.clear();
                    }
                    EventKind::Delta {
                        channel: DeltaChannel::Text,
                        text,
                        ..
                    } => {
                        s.summary.push_str(text);
                        if let Some(l) = s.summary.lines().rev().find(|l| !l.trim().is_empty()) {
                            s.last_action = format!("writing: {}", l.trim());
                        }
                    }
                    EventKind::Cost { usd } => s.cost += usd,
                    EventKind::NodeFinished { status, error } => {
                        s.status = *status;
                        s.finished = Some(ev.ts);
                        if let Some(err) = error {
                            s.last_action = err.clone();
                        } else {
                            s.last_action = "done".into();
                        }
                    }
                    EventKind::ApprovalRequested { action, .. } => {
                        s.status = NodeStatus::WaitingApproval;
                        s.last_action = format!("waiting for approval: {action}");
                    }
                    EventKind::ApprovalResolved { .. } => s.status = NodeStatus::Running,
                    _ => return,
                }
                e.touch();
            }
        }
    }

    fn layout_entry(&mut self, i: usize, width: usize, f: &Frame<'_>) {
        let settings = self.settings;
        let e = &self.entries[i];
        let ids = e.element_ids();
        let focus = f
            .focus
            .filter(|id| ids.iter().any(|x| x == id))
            .map(str::to_owned);
        let live = (e.is_live() || settings.time.mode == super::timefmt::Timestamps::Relative)
            .then(|| (f.spinner.glyph(), f.now.unix_timestamp()));
        let key = Key {
            width,
            version: e.version,
            settings,
            focus,
            live,
        };
        if e.layout.as_ref().is_some_and(|l| l.key == key) {
            return;
        }
        self.layouts += 1;
        let (lines, hits) = layout(&mut self.entries[i], width, &settings, f);
        self.entries[i].layout = Some(Layout { key, lines, hits });
    }

    fn height(&mut self, i: usize, width: usize, f: &Frame<'_>) -> usize {
        self.layout_entry(i, width, f);
        self.entries[i].layout.as_ref().map_or(0, |l| l.lines.len())
    }

    /// The top visible position `(entry, line)` when following the tail.
    fn tail_top(&mut self, width: usize, height: usize, f: &Frame<'_>) -> (usize, usize) {
        let mut need = height;
        let mut i = self.entries.len();
        while i > 0 {
            i -= 1;
            let h = self.height(i, width, f);
            if h >= need {
                return (i, h - need);
            }
            need -= h;
        }
        (0, 0)
    }

    /// Scrolls by `lines` (negative is up). Scrolling down to the end
    /// resumes following. Uses the size of the last rendered frame.
    pub fn scroll(&mut self, lines: i32, width: usize, f: &Frame<'_>) {
        if self.entries.is_empty() {
            return;
        }
        let height = self.last_height.max(1);
        let (mut i, mut off) = match self.anchor {
            Some(a) => a,
            None => self.tail_top(width, height, f),
        };
        if lines < 0 {
            let mut up = lines.unsigned_abs() as usize;
            while up > 0 {
                if off >= up {
                    off -= up;
                    up = 0;
                } else if i == 0 {
                    off = 0;
                    up = 0;
                } else {
                    up -= off + 1;
                    i -= 1;
                    off = self.height(i, width, f).saturating_sub(1);
                }
            }
        } else {
            let mut down = lines as usize;
            while down > 0 {
                let h = self.height(i, width, f);
                if off + down < h {
                    off += down;
                    down = 0;
                } else if i + 1 >= self.entries.len() {
                    off = h.saturating_sub(1);
                    down = 0;
                } else {
                    down -= h - off;
                    i += 1;
                    off = 0;
                }
            }
        }
        // At (or past) the tail, including a transcript that fits: follow.
        self.pin((i, off), width, height, f);
    }

    /// Jumps to the end and follows new output.
    pub fn follow(&mut self) {
        self.anchor = None;
        self.unseen = false;
    }

    /// Lays out the visible part into `area`'s size, bottom-anchored.
    /// Returns the lines (at most `area.height`) and the interactive regions
    /// in screen coordinates; the lines belong [`Transcript::pad`] rows below
    /// `area.y` (non-zero only when the whole transcript fits).
    pub fn render(
        &mut self,
        area: Rect,
        f: &Frame<'_>,
    ) -> (Vec<Line<'static>>, Vec<TranscriptHit>) {
        let width = area.width as usize;
        let height = area.height as usize;
        let resized = self.last_width != 0 && self.last_width != width;
        let old_width = self.last_width;
        self.last_height = height;
        self.last_width = width;
        self.last_pad = 0;
        if self.entries.is_empty() || height == 0 {
            return (Vec::new(), Vec::new());
        }
        if let Some((i, _)) = self.anchor
            && i >= self.entries.len()
        {
            self.follow();
        }
        // A width change rewraps every entry: keep the pinned entry and the
        // same relative position inside it.
        if resized && let Some((i, off)) = self.anchor {
            let old_h = self.entries[i]
                .layout
                .as_ref()
                .filter(|l| l.key.width == old_width)
                .map(|l| l.lines.len());
            let new_h = self.height(i, width, f);
            let off = match old_h {
                Some(old_h) if old_h > 0 => off * new_h / old_h,
                _ => off,
            };
            self.anchor = Some((i, off.min(new_h.saturating_sub(1))));
        }
        let mut r = Rendered::default();
        if let Some(top) = self.anchor {
            r = self.fill(area, top, f);
            // The content below the pin shrank or the viewport grew: the
            // tail fits, so follow it again.
            if r.lines.len() < height {
                self.follow();
            }
        }
        if self.anchor.is_none() {
            let top = self.tail_top(width, height, f);
            r = self.fill(area, top, f);
        }
        let pad = height - r.lines.len();
        if pad > 0 {
            for h in &mut r.hits {
                h.rect.y += pad as u16;
            }
        }
        self.last_pad = pad;
        (r.lines, r.hits)
    }

    /// Lays out entries from `top` downwards until `area` is full. Hit
    /// rectangles start at `area.y`.
    fn fill(&mut self, area: Rect, top: (usize, usize), f: &Frame<'_>) -> Rendered {
        let width = area.width as usize;
        let height = area.height as usize;
        let mut out = Vec::new();
        let mut hits = Vec::new();
        let (mut i, mut off) = top;
        while out.len() < height && i < self.entries.len() {
            self.layout_entry(i, width, f);
            let layout = self.entries[i].layout.as_ref().expect("laid out");
            let y0 = out.len() as isize - off as isize;
            for h in &layout.hits {
                let top = (y0 + h.start as isize).max(0);
                let bottom = (y0 + (h.start + h.len) as isize).min(height as isize);
                let (x, w) = if h.w == 0 {
                    (area.x, area.width)
                } else {
                    let col = (h.col as u16).min(area.width);
                    (area.x + col, (h.w as u16).min(area.width - col))
                };
                if bottom > top && w > 0 {
                    hits.push(TranscriptHit {
                        id: h.id.clone(),
                        rect: Rect {
                            x,
                            y: area.y + top as u16,
                            width: w,
                            height: (bottom - top) as u16,
                        },
                        action: h.action.clone(),
                    });
                }
            }
            for (n, l) in layout.lines.iter().enumerate().skip(off) {
                if out.len() >= height {
                    break;
                }
                out.push(match self.selection_style((i, n), f) {
                    Some(st) => l.clone().patch_style(st),
                    None => l.clone(),
                });
            }
            off = 0;
            i += 1;
        }
        Rendered { lines: out, hits }
    }
}

fn header(
    left: Vec<Span<'static>>,
    ts: OffsetDateTime,
    width: usize,
    settings: &ViewSettings,
    f: &Frame<'_>,
) -> Line<'static> {
    header_right(left, header_parts(ts, settings, f, None), width).0
}

/// The right part of a header: an optional copy button (`button` is its
/// element id) and the timestamp.
fn header_parts(
    ts: OffsetDateTime,
    settings: &ViewSettings,
    f: &Frame<'_>,
    button: Option<&str>,
) -> Vec<Span<'static>> {
    let muted = f.theme.fg(Token::Muted);
    let mut right = Vec::new();
    let time = settings.time.format(ts, f.now);
    if let Some(id) = button {
        let mut st = muted;
        if f.focus == Some(id) {
            st = st.patch(f.theme.focused());
        }
        right.push(Span::styled(COPY_LABEL, st));
        if time.is_some() {
            right.push(Span::styled(" · ", muted));
        }
    }
    if let Some(t) = time {
        right.push(Span::styled(t, muted));
    }
    right
}

/// Right-aligns `right` after `left`. Returns the line and the column the
/// right part starts at.
fn header_right(
    left: Vec<Span<'static>>,
    right: Vec<Span<'static>>,
    width: usize,
) -> (Line<'static>, usize) {
    let rw: usize = right.iter().map(|s| super::text::width(&s.content)).sum();
    let col = width.saturating_sub(rw);
    (
        right_align(Line::from(left), right, width, Span::raw(" ")),
        col,
    )
}

/// The hit of a header's copy button at `line`.
fn copy_hit(id: String, line: usize, col: usize, target: CopyTarget) -> Hit {
    Hit {
        id,
        start: line,
        len: 1,
        action: TranscriptAction::Copy(target),
        col,
        w: super::text::width(COPY_LABEL),
    }
}

fn layout(
    e: &mut Entry,
    width: usize,
    settings: &ViewSettings,
    f: &Frame<'_>,
) -> (Vec<Line<'static>>, Vec<Hit>) {
    let theme = f.theme;
    let mut lines = Vec::new();
    let mut hits = Vec::new();
    let ts = e.ts;
    let id = e.id;
    match &mut e.kind {
        EntryKind::User { text } => {
            let bar = Span::styled("▌ ", theme.fg(Token::User));
            let button = copy_message_id(id);
            let right = header_parts(ts, settings, f, Some(&button));
            let rw: usize = right.iter().map(|s| super::text::width(&s.content)).sum();
            let mut first = true;
            let mut head = |left: Vec<Span<'static>>, lines: &mut Vec<Line<'static>>| {
                let (l, col) = header_right(left, right.clone(), width);
                hits.push(copy_hit(
                    button.clone(),
                    lines.len(),
                    col,
                    CopyTarget::Message(id),
                ));
                lines.push(l);
            };
            for l in text.lines() {
                // Tabs (pasted code) show as four spaces.
                let body = Line::from(vec![
                    bar.clone(),
                    Span::styled(sanitize(&l.replace('\t', "    ")), theme.fg(Token::Fg)),
                ]);
                if first {
                    let wrapped = wrap_line(
                        body,
                        width.saturating_sub(rw + 1).max(10),
                        std::slice::from_ref(&bar),
                    );
                    let mut it = wrapped.into_iter();
                    if let Some(h) = it.next() {
                        head(h.spans, &mut lines);
                    }
                    lines.extend(it);
                    first = false;
                } else {
                    lines.extend(wrap_line(body, width, std::slice::from_ref(&bar)));
                }
            }
            if first {
                head(vec![bar], &mut lines);
            }
        }
        EntryKind::Assistant(t) => {
            let name = Span::styled(
                "◆ assistant",
                theme.fg(Token::Agent).add_modifier(Modifier::BOLD),
            );
            let button = copy_message_id(id);
            let (head, col) = header_right(
                vec![name],
                header_parts(ts, settings, f, Some(&button)),
                width,
            );
            hits.push(copy_hit(button, lines.len(), col, CopyTarget::Message(id)));
            lines.push(head);
            if !t.reasoning.is_empty() && settings.show_thinking != ShowThinking::Hidden {
                let n = t.reasoning.lines().count();
                let style = theme.fg(Token::Reasoning);
                let label = if t.thinking_expanded {
                    "▾ thinking".to_owned()
                } else {
                    format!("▸ thinking · {n} lines")
                };
                let mut label_style = style;
                if f.focus == Some(thinking_id(id).as_str()) {
                    label_style = label_style.patch(theme.focused());
                }
                hits.push(Hit {
                    id: thinking_id(id),
                    start: lines.len(),
                    len: 1,
                    action: TranscriptAction::ToggleThinking(id),
                    col: 0,
                    w: 0,
                });
                lines.push(Line::from(Span::styled(label, label_style)));
                if t.thinking_expanded {
                    let bar = Span::styled("┊ ", theme.fg(Token::Border));
                    for l in t.reasoning.lines() {
                        let body = Line::from(vec![bar.clone(), Span::styled(sanitize(l), style)]);
                        lines.extend(wrap_line(body, width, std::slice::from_ref(&bar)));
                    }
                }
            }
            if !t.text.is_empty() {
                let text = sanitize(&t.text);
                let base = lines.len();
                let mut md = t.md.lines(&text, width, theme, settings.markdown);
                for (n, h) in t.md.code_headers().iter().enumerate() {
                    let cid = copy_code_id(id, n);
                    if f.focus == Some(cid.as_str())
                        && let Some(last) = md.get_mut(h.line).and_then(|l| l.spans.last_mut())
                    {
                        last.style = last.style.patch(theme.focused());
                    }
                    hits.push(copy_hit(
                        cid,
                        base + h.line,
                        h.col,
                        CopyTarget::Code {
                            entry: id,
                            block: n,
                        },
                    ));
                }
                lines.extend(md);
            }
            if let Some(m) = &t.meta {
                lines.push(truncate_line(m.line(theme), width));
            }
        }
        EntryKind::Tools { group, .. } => {
            let ctx = CardCtx {
                theme,
                width,
                time: settings.time,
                now: f.now,
                preview_lines: settings.preview_lines,
                spinner: f.spinner,
                focused: false,
                copy_buttons: true,
                focus: f.focus,
            };
            let focused = f.focus.and_then(|x| x.strip_prefix("card."));
            let (l, spans) = group.render(&ctx, focused);
            for s in spans {
                hits.push(Hit {
                    id: card_id(&s.call_id),
                    start: s.start,
                    len: s.len,
                    action: TranscriptAction::ToggleCard(s.call_id.clone()),
                    col: 0,
                    w: 0,
                });
                for b in s.buttons {
                    hits.push(Hit {
                        id: copy_button_id(&s.call_id, b.kind),
                        start: b.line,
                        len: 1,
                        action: TranscriptAction::Copy(CopyTarget::Card {
                            call_id: s.call_id.clone(),
                            kind: b.kind,
                        }),
                        col: b.col,
                        w: b.width,
                    });
                }
            }
            lines.extend(l);
        }
        EntryKind::Notice(n) => {
            let (icon, token) = n.level.style();
            let left = vec![Span::styled(
                format!("{icon} {}", sanitize(&n.text)),
                theme.fg(token),
            )];
            lines.push(header(left, ts, width, settings, f));
            for d in &n.detail {
                lines.push(truncate_line(
                    Line::from(Span::styled(
                        format!("  {}", sanitize(d)),
                        theme.fg(Token::Muted),
                    )),
                    width,
                ));
            }
        }
        EntryKind::Memory(m) => {
            let id = undo_button_id(&m.memory_id);
            let (l, button) = m.lines(theme, width, f.focus == Some(id.as_str()));
            if let (Some(row), Some(commit)) = (button, m.commit()) {
                hits.push(Hit {
                    id,
                    start: row,
                    len: 1,
                    action: TranscriptAction::UndoMemory(commit.to_owned()),
                    col: 0,
                    w: 0,
                });
            }
            lines.extend(l);
        }
        EntryKind::Subagent(s) => {
            let (glyph, token) = node_glyph(s.status, &f.spinner);
            let border = if f.focus == Some(subagent_id(&s.node).as_str()) {
                theme.fg(Token::BorderFocus)
            } else {
                theme.fg(Token::Border)
            };
            let mut left = vec![
                Span::styled("╭ ", border),
                Span::styled(format!("{glyph} "), theme.fg(token)),
                Span::styled(
                    s.name.clone(),
                    theme.fg(Token::Agent).add_modifier(Modifier::BOLD),
                ),
            ];
            if let Some(a) = &s.agent {
                left.push(Span::styled(format!(" ({a})"), theme.fg(Token::Agent)));
            }
            if let Some(m) = &s.model {
                left.push(Span::styled(format!(" · {m}"), theme.fg(Token::Muted)));
            }
            if let Some(h) = &s.host {
                left.push(Span::styled(format!(" · ⧉ {h}"), theme.fg(Token::Running)));
            }
            left.push(Span::styled(" ", border));
            let end = s.finished.unwrap_or(f.now);
            let secs = (end - ts).whole_milliseconds().max(0) as u64;
            let right = vec![
                Span::styled(" ", border),
                Span::styled(fmt_ms(secs), theme.fg(Token::Number)),
                Span::styled(" · ", theme.fg(Token::Muted)),
                Span::styled(fmt_cost(Some(s.cost)), theme.fg(Token::Number)),
                Span::styled(" ╮", border),
            ];
            let w = width.max(12);
            lines.push(right_align(
                Line::from(left),
                right,
                w,
                Span::styled("─", border),
            ));
            let action_icon = if s.status.is_terminal() {
                ' '
            } else {
                f.spinner.glyph()
            };
            // Finished cards show the final summary (the last turn's text,
            // up to three lines); running cards show the live last action.
            let summary: Vec<&str> = s
                .summary
                .lines()
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .collect();
            let body_lines: Vec<String> =
                if s.status == NodeStatus::Succeeded && !summary.is_empty() {
                    let mut v: Vec<String> = summary.iter().take(3).map(|l| sanitize(l)).collect();
                    if summary.len() > 3 {
                        v.push(format!("… {} more lines", summary.len() - 3));
                    }
                    v
                } else {
                    vec![format!("{action_icon} {}", sanitize(&s.last_action))]
                };
            for text in body_lines {
                let body = Line::from(vec![
                    Span::styled("│ ", border),
                    Span::styled(text, theme.fg(Token::Fg)),
                ]);
                lines.push(right_align(
                    body,
                    vec![Span::styled(" │", border)],
                    w,
                    Span::raw(" "),
                ));
            }
            lines.push(Line::from(vec![
                Span::styled("╰", border),
                Span::styled("─".repeat(w - 2), border),
                Span::styled("╯", border),
            ]));
            hits.push(Hit {
                id: subagent_id(&s.node),
                start: 0,
                len: 3,
                action: TranscriptAction::OpenNode(s.node.clone()),
                col: 0,
                w: 0,
            });
        }
    }
    lines.push(Line::default());
    (lines, hits)
}

#[cfg(test)]
mod tests {
    use super::*;
    use harness_events::schema::{Payload, RunId};
    use serde_json::json;
    use time::macros::datetime;

    const T0: OffsetDateTime = datetime!(2026-10-22 14:32:00 UTC);

    pub(crate) fn ev(seq: u64, node: Option<&str>, kind: EventKind) -> RunEvent {
        RunEvent {
            seq,
            ts: T0 + time::Duration::seconds(seq as i64),
            run_id: RunId(uuid::Uuid::nil()),
            node_id: node.map(NodeId::from),
            parent_node_id: None,
            kind,
        }
    }

    fn frame(theme: &Theme) -> Frame<'_> {
        Frame {
            theme,
            now: T0 + time::Duration::minutes(10),
            spinner: Spinner::new(true),
            focus: None,
        }
    }

    fn texts(lines: &[Line<'_>]) -> Vec<String> {
        lines
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect()
    }

    fn model_call(seq: u64, call: &str) -> RunEvent {
        ev(
            seq,
            None,
            EventKind::ModelCallStarted {
                call_id: call.into(),
                provider: "fake".into(),
                model: "sonnet".into(),
                request: Payload::text(""),
            },
        )
    }

    fn delta(seq: u64, call: &str, channel: DeltaChannel, text: &str) -> RunEvent {
        ev(
            seq,
            None,
            EventKind::Delta {
                call_id: call.into(),
                channel,
                text: text.into(),
            },
        )
    }

    fn finished(seq: u64, call: &str) -> RunEvent {
        ev(
            seq,
            None,
            EventKind::ModelCallFinished {
                call_id: call.into(),
                usage: Usage {
                    input: 500,
                    cached_input: 11_900,
                    output: 812,
                    reasoning: 1200,
                },
                stop_reason: Some("end_turn".into()),
                response: None,
                duration_ms: 4200,
            },
        )
    }

    fn tool_call(seq: u64, id: &str, tool: &str, args: serde_json::Value) -> RunEvent {
        ev(
            seq,
            None,
            EventKind::ToolCall {
                call_id: id.into(),
                tool: tool.into(),
                args: Payload::Json(args),
            },
        )
    }

    fn render_all(t: &mut Transcript, theme: &Theme, w: u16, h: u16) -> Vec<String> {
        let (lines, _) = t.render(Rect::new(0, 0, w, h), &frame(theme));
        texts(&lines)
            .into_iter()
            .map(|l| l.trim_end().to_owned())
            .collect()
    }

    #[test]
    fn assistant_turn_streams_markdown_and_ends_with_metadata() {
        let theme = Theme::default();
        let mut t = Transcript::new(ViewSettings::default());
        t.push_user("fix the parser", T0);
        t.apply(&model_call(1, "m1"));
        t.apply(&delta(2, "m1", DeltaChannel::Text, "Here is **the** fix"));
        let l = render_all(&mut t, &theme, 60, 20);
        assert!(l[0].starts_with("▌ fix the parser"));
        assert!(l[0].ends_with("14:32:00"));
        assert!(
            l.iter()
                .any(|x| x.starts_with("◆ assistant") && x.ends_with("14:32:01"))
        );
        assert!(l.contains(&"Here is the fix".to_owned()));
        t.apply(&finished(3, "m1"));
        t.apply(&ev(4, None, EventKind::Cost { usd: 0.021 }));
        let l = render_all(&mut t, &theme, 80, 20);
        assert!(
            l.contains(
                &"sonnet · ↑12.4k (cache 96%) ↓812 · reasoning 1.2k · $0.021 · 4.2s · end_turn"
                    .to_owned()
            ),
            "{l:?}"
        );
    }

    #[test]
    fn thinking_follows_show_thinking() {
        let theme = Theme::default();
        let events = [
            model_call(1, "m"),
            delta(2, "m", DeltaChannel::Reasoning, "step one\nstep two"),
            delta(3, "m", DeltaChannel::Text, "answer"),
        ];
        let mut settings = ViewSettings::default();
        for (mode, expect_label, expect_body) in [
            (ShowThinking::Hidden, false, false),
            (ShowThinking::Collapsed, true, false),
            (ShowThinking::Expanded, true, true),
        ] {
            settings.show_thinking = mode;
            let mut t = Transcript::new(settings);
            events.iter().for_each(|e| t.apply(e));
            let l = render_all(&mut t, &theme, 60, 20);
            assert_eq!(
                l.iter().any(|x| x.contains("thinking")),
                expect_label,
                "{mode:?}"
            );
            assert_eq!(
                l.iter().any(|x| x.contains("step two")),
                expect_body,
                "{mode:?}"
            );
        }
        // Collapsed reasoning expands on activation.
        settings.show_thinking = ShowThinking::Collapsed;
        let mut t = Transcript::new(settings);
        events.iter().for_each(|e| t.apply(e));
        let (_, hits) = t.render(Rect::new(0, 0, 60, 20), &frame(&theme));
        let hit = hits.iter().find(|h| h.id.starts_with("thinking.")).unwrap();
        t.activate(&hit.action.clone());
        let l = render_all(&mut t, &theme, 60, 20);
        assert!(l.iter().any(|x| x.contains("┊ step two")));
    }

    #[test]
    fn tool_calls_render_as_cards_grouped_per_turn_and_toggle() {
        let theme = Theme::default();
        let mut t = Transcript::new(ViewSettings {
            preview_lines: 2,
            ..ViewSettings::default()
        });
        t.apply(&model_call(1, "m"));
        t.apply(&finished(2, "m"));
        t.apply(&tool_call(3, "a", "read", json!({"path": "a.rs"})));
        t.apply(&tool_call(4, "b", "bash", json!({"command": "seq 5"})));
        t.tool_output("b", "1\n2\n");
        let l = render_all(&mut t, &theme, 70, 40);
        assert!(
            l.iter().any(|x| x.starts_with("⧉ 2 parallel calls")),
            "{l:?}"
        );
        assert!(l.iter().any(|x| x.contains("│ 1 ")));
        t.apply(&ev(
            5,
            None,
            EventKind::ToolResult {
                call_id: "b".into(),
                ok: true,
                output: Payload::text("1\n2\n3\n4\n5"),
                duration_ms: 10,
            },
        ));
        let (lines, hits) = t.render(Rect::new(0, 0, 70, 40), &frame(&theme));
        assert!(texts(&lines).iter().any(|x| x.contains("… 3 more lines")));
        let hit = hits.iter().find(|h| h.id == "card.b").unwrap().clone();
        assert!(hit.rect.height >= 3);
        t.activate(&hit.action);
        let l = render_all(&mut t, &theme, 70, 40);
        assert!(l.iter().any(|x| x.contains("│ 5 ")));
        // A new model call starts a new group.
        t.apply(&model_call(6, "m2"));
        t.apply(&tool_call(7, "c", "read", json!({"path": "c.rs"})));
        assert_eq!(t.entries().len(), 4);
    }

    #[test]
    fn file_changes_attach_to_their_card() {
        let theme = Theme::default();
        let mut t = Transcript::new(ViewSettings::default());
        t.apply(&tool_call(
            1,
            "w",
            "write",
            json!({"path": "src/new.rs", "content": "fn a() {}\n"}),
        ));
        t.apply(&ev(
            2,
            None,
            EventKind::FileChanged {
                path: "/work/src/new.rs".into(),
                change: FileChangeKind::Created,
                diff: None,
            },
        ));
        let l = render_all(&mut t, &theme, 70, 10);
        assert!(l[0].contains("new file"), "{}", l[0]);
    }

    #[test]
    fn errors_and_retries_are_inline_notices() {
        let theme = Theme::default();
        let mut t = Transcript::new(ViewSettings::default());
        t.apply(&model_call(1, "m"));
        t.apply(&delta(2, "m", DeltaChannel::Text, "partial"));
        t.push_retry(
            &RetryNotice {
                call_id: Some("m".into()),
                reason: "429 rate limited".into(),
                attempt: 2,
                max: 5,
                delay_secs: 4.0,
                discard_partial: true,
            },
            T0,
        );
        t.apply(&ev(
            3,
            None,
            EventKind::NodeRetry {
                attempt: 1,
                reason: "overloaded".into(),
            },
        ));
        t.apply(&ev(
            4,
            None,
            EventKind::Error {
                message: "provider refused the request\nreduce the prompt".into(),
                recoverable: false,
                action: None,
            },
        ));
        let (lines, hits) = t.render(Rect::new(0, 0, 70, 20), &frame(&theme));
        let l = texts(&lines);
        assert!(
            !l.iter().any(|x| x.contains("partial")),
            "partial output discarded"
        );
        assert!(
            l.iter()
                .any(|x| x.starts_with("↻ retrying in 4s (429 rate limited, attempt 2/5)"))
        );
        assert!(l.iter().any(|x| x.starts_with("↻ retry 1: overloaded")));
        let err = lines
            .iter()
            .find(|x| {
                x.spans
                    .first()
                    .is_some_and(|s| s.content.starts_with("✗ provider refused"))
            })
            .unwrap();
        assert_eq!(err.spans[0].style, theme.fg(Token::Error));
        assert!(l.iter().any(|x| x.contains("reduce the prompt")));
        // Notices are plain transcript lines: nothing interactive, no modal
        // (the only elements are the assistant turns' toggles and copy buttons).
        assert!(
            hits.iter()
                .all(|h| h.id.starts_with("thinking.") || h.id.starts_with("copy.msg.")),
            "{hits:?}"
        );
    }

    #[test]
    fn subagent_cards_show_spinner_last_action_cost_and_open_node() {
        let theme = Theme::default();
        let mut t = Transcript::new(ViewSettings::default());
        t.apply(&ev(
            1,
            Some("main"),
            EventKind::NodeStarted {
                kind: NodeKind::Agent,
                name: "main".into(),
                agent: None,
                model: None,
                host: None,
                needs: vec![],
                queued: false,
            },
        ));
        let mut start = ev(
            2,
            Some("r"),
            EventKind::NodeStarted {
                kind: NodeKind::Subagent,
                name: "researcher".into(),
                agent: None,
                model: Some("haiku".into()),
                host: Some("gpu-box".into()),
                needs: vec![],
                queued: false,
            },
        );
        start.parent_node_id = Some(NodeId::from("main"));
        t.apply(&start);
        t.apply(&ev(
            3,
            Some("r"),
            EventKind::ToolCall {
                call_id: "x".into(),
                tool: "read".into(),
                args: Payload::Json(json!({"path": "src/net/mod.rs"})),
            },
        ));
        t.apply(&ev(4, Some("r"), EventKind::Cost { usd: 0.004 }));
        let (lines, hits) = t.render(Rect::new(0, 0, 70, 10), &frame(&theme));
        let l = texts(&lines);
        assert!(
            l[0].starts_with("╭ ⠋ researcher · haiku · ⧉ gpu-box"),
            "{}",
            l[0]
        );
        assert!(l[0].ends_with("$0.004 ╮"), "{}", l[0]);
        assert!(l[1].contains("⠋ read src/net/mod.rs"), "{}", l[1]);
        let hit = hits.iter().find(|h| h.id == "subagent.r").unwrap();
        assert_eq!(
            t.activate(&hit.action.clone()),
            Some(TranscriptOutput::OpenNode(NodeId::from("r")))
        );
        // Subagent tool calls are not main transcript cards.
        assert!(t.card("x").is_none());
    }

    #[test]
    fn rendering_cost_is_independent_of_length() {
        let theme = Theme::default();
        let mut cost = Vec::new();
        for n in [100usize, 10_000] {
            let mut t = Transcript::new(ViewSettings::default());
            for i in 0..n {
                t.push_user(&format!("message {i}"), T0);
            }
            let before = t.layouts();
            t.render(Rect::new(0, 0, 80, 30), &frame(&theme));
            let first = t.layouts() - before;
            let before = t.layouts();
            t.render(Rect::new(0, 0, 80, 30), &frame(&theme));
            assert_eq!(t.layouts(), before, "second frame is fully cached");
            // Scrolling back a page lays out about a page of entries.
            let before = t.layouts();
            t.scroll(-30, 80, &frame(&theme));
            t.render(Rect::new(0, 0, 80, 30), &frame(&theme));
            cost.push((first, t.layouts() - before));
        }
        assert_eq!(cost[0], cost[1], "same work for 100 and 10k entries");
        assert!(cost[1].0 <= 16, "{cost:?}");
    }

    #[test]
    fn scrolling_pins_the_view_and_resumes_following_at_the_end() {
        let theme = Theme::default();
        let mut t = Transcript::new(ViewSettings {
            time: TimeFormat::new(super::super::timefmt::Timestamps::Off, UtcOffset::UTC),
            ..ViewSettings::default()
        });
        for i in 0..20 {
            t.push_user(&format!("m{i}"), T0);
        }
        let has = |l: &[String], m: &str| l.iter().any(|x| x.starts_with(&format!("▌ {m} ")));
        let l = render_all(&mut t, &theme, 40, 4);
        assert!(has(&l, "m19"), "{l:?}");
        t.scroll(-10, 40, &frame(&theme));
        assert!(!t.is_following());
        let l = render_all(&mut t, &theme, 40, 4);
        assert!(!has(&l, "m19"), "{l:?}");
        // New output does not move a pinned view.
        t.push_user("new", T0);
        assert_eq!(render_all(&mut t, &theme, 40, 4), l);
        t.scroll(100, 40, &frame(&theme));
        assert!(t.is_following());
        assert!(has(&render_all(&mut t, &theme, 40, 4), "new"));
        // Scrolling up past the top stops at the first entry.
        t.scroll(-1000, 40, &frame(&theme));
        assert!(render_all(&mut t, &theme, 40, 4)[0].starts_with("▌ m0 "));
    }

    fn untimed() -> Transcript {
        Transcript::new(ViewSettings {
            time: TimeFormat::new(super::super::timefmt::Timestamps::Off, UtcOffset::UTC),
            ..ViewSettings::default()
        })
    }

    #[test]
    fn a_short_transcript_is_padded_above_and_hits_move_down() {
        let theme = Theme::default();
        let mut t = untimed();
        t.push_user("hi", T0);
        let (lines, hits) = t.render(Rect::new(0, 3, 40, 10), &frame(&theme));
        assert_eq!(usize::from(t.pad()) + lines.len(), 10);
        assert!(t.pad() > 0);
        let copy = hits.iter().find(|h| h.id == copy_message_id(0)).unwrap();
        // Screen row = area top + padding + line index of the header.
        let header = texts(&lines).iter().position(|l| l.contains("hi")).unwrap();
        assert_eq!(copy.rect.y, 3 + t.pad() + header as u16);
        // A full viewport has no padding.
        for i in 0..20 {
            t.push_user(&format!("m{i}"), T0);
        }
        let (lines, _) = t.render(Rect::new(0, 0, 40, 10), &frame(&theme));
        assert_eq!((t.pad(), lines.len()), (0, 10));
    }

    #[test]
    fn streamed_output_below_a_pinned_view_is_unseen_until_following() {
        let theme = Theme::default();
        let mut t = untimed();
        t.apply(&model_call(1, "c1"));
        t.apply(&delta(2, "c1", DeltaChannel::Text, "first\n\n"));
        for i in 0..20 {
            t.push_user(&format!("m{i}"), T0);
        }
        t.apply(&model_call(3, "c2"));
        render_all(&mut t, &theme, 40, 6);
        assert!(!t.has_unseen());
        t.scroll(-8, 40, &frame(&theme));
        let pinned = render_all(&mut t, &theme, 40, 6);
        assert!(!t.has_unseen(), "scrolling alone is not new output");
        // A delta into the last (off-screen) entry is new output.
        t.apply(&delta(4, "c2", DeltaChannel::Text, "streaming more"));
        assert!(t.has_unseen());
        assert_eq!(render_all(&mut t, &theme, 40, 6), pinned);
        t.follow();
        assert!(!t.has_unseen() && t.is_following());
        // Output while following is never unseen.
        t.apply(&delta(5, "c2", DeltaChannel::Text, " and more"));
        assert!(!t.has_unseen());
    }

    #[test]
    fn a_pinned_view_whose_content_now_fits_follows_again() {
        let theme = Theme::default();
        let mut t = untimed();
        for i in 0..10 {
            t.push_user(&format!("m{i}"), T0);
        }
        render_all(&mut t, &theme, 40, 4);
        t.scroll(-3, 40, &frame(&theme));
        assert!(!t.is_following());
        // Taller than the whole transcript: bottom-anchored, following.
        let (lines, _) = t.render(Rect::new(0, 0, 40, 60), &frame(&theme));
        assert!(t.is_following());
        assert_eq!(usize::from(t.pad()) + lines.len(), 60);
    }

    #[test]
    fn a_width_change_keeps_the_pinned_entry_on_top() {
        let theme = Theme::default();
        let mut t = untimed();
        let long = "word ".repeat(40);
        for i in 0..30 {
            t.push_user(&format!("m{i} {long}"), T0);
        }
        render_all(&mut t, &theme, 60, 8);
        t.scroll(-40, 60, &frame(&theme));
        let (i, off) = t.anchor.unwrap();
        let h60 = t.height(i, 60, &frame(&theme));
        render_all(&mut t, &theme, 30, 8);
        let (j, off30) = t.anchor.unwrap();
        let h30 = t.height(j, 30, &frame(&theme));
        assert_eq!(i, j, "same entry stays on top");
        // Same relative position within the entry, give or take a line.
        let rel = |o: usize, h: usize| o as f64 / h as f64;
        assert!((rel(off, h60) - rel(off30, h30)).abs() <= 1.0 / h30 as f64 + 1e-9);
        assert!(!t.is_following());
    }

    #[test]
    fn live_and_replay_show_identical_times() {
        let theme = Theme::default();
        let events = [
            model_call(1, "m"),
            delta(2, "m", DeltaChannel::Text, "hi"),
            finished(3, "m"),
            tool_call(4, "a", "read", json!({"path": "a"})),
            ev(
                5,
                None,
                EventKind::Error {
                    message: "boom".into(),
                    recoverable: true,
                    action: None,
                },
            ),
        ];
        let mut live = Transcript::new(ViewSettings::default());
        let mut replay = Transcript::new(ViewSettings::default());
        for e in &events {
            live.apply(e);
        }
        for e in events.iter() {
            replay.apply(&e.clone());
        }
        let f1 = frame(&theme);
        let f2 = Frame {
            now: f1.now + time::Duration::hours(30),
            ..f1
        };
        let (a, _) = live.render(Rect::new(0, 0, 70, 20), &f1);
        let (b, _) = replay.render(Rect::new(0, 0, 70, 20), &f2);
        assert_eq!(texts(&a), texts(&b));
        assert!(texts(&a).iter().any(|x| x.ends_with("14:32:01")));
        assert!(texts(&a).iter().any(|x| x.ends_with("14:32:04 ╮")));
        assert!(texts(&a).iter().any(|x| x.ends_with("14:32:05")));
    }

    #[test]
    fn timestamp_modes() {
        let theme = Theme::default();
        for (mode, suffix) in [
            (super::super::timefmt::Timestamps::Time, Some("14:32:00")),
            (super::super::timefmt::Timestamps::Relative, Some("10m ago")),
            (super::super::timefmt::Timestamps::Off, None),
        ] {
            let mut t = Transcript::new(ViewSettings {
                time: TimeFormat::new(mode, UtcOffset::UTC),
                ..ViewSettings::default()
            });
            t.push_user("hello", T0);
            let l = render_all(&mut t, &theme, 40, 3);
            match suffix {
                Some(s) => assert!(l[0].ends_with(s), "{mode:?}: {}", l[0]),
                // No time: just the copy button on the right.
                None => {
                    assert!(l[0].starts_with("▌ hello "), "{}", l[0]);
                    assert!(l[0].ends_with(COPY_LABEL), "{}", l[0]);
                }
            }
        }
    }

    fn agent_started(seq: u64, node: &str) -> RunEvent {
        ev(
            seq,
            Some(node),
            EventKind::NodeStarted {
                kind: NodeKind::Agent,
                name: "coder".into(),
                agent: Some("coder".into()),
                model: None,
                host: None,
                needs: vec![],
                queued: false,
            },
        )
    }

    fn node_done(seq: u64, node: &str) -> RunEvent {
        ev(
            seq,
            Some(node),
            EventKind::NodeFinished {
                status: NodeStatus::Succeeded,
                error: None,
            },
        )
    }

    fn user_texts(t: &Transcript) -> Vec<(String, OffsetDateTime)> {
        t.entries()
            .iter()
            .filter_map(|e| match &e.kind {
                EntryKind::User { text } => Some((text.clone(), e.ts)),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn user_messages_take_their_time_from_the_log_live_and_in_replay() {
        let log = [
            agent_started(1, "turn1"),
            ev(
                2,
                Some("turn1"),
                EventKind::UserMessage {
                    text: "fix it".into(),
                },
            ),
            node_done(3, "turn1"),
        ];
        // Live: typed locally first (wall clock), then confirmed by the log.
        let mut live = Transcript::new(ViewSettings::default());
        live.push_user("/cost", T0 - time::Duration::minutes(1));
        live.push_user("fix it", T0 - time::Duration::minutes(5));
        for e in &log {
            live.apply(e);
        }
        // Replay: only the log.
        let mut replay = Transcript::new(ViewSettings::default());
        for e in &log {
            replay.apply(e);
        }
        let logged = T0 + time::Duration::seconds(2);
        assert_eq!(user_texts(&replay), vec![("fix it".to_owned(), logged)]);
        let live_users = user_texts(&live);
        assert_eq!(live_users.len(), 2, "no duplicate entry: {live_users:?}");
        assert_eq!(live_users[1], ("fix it".to_owned(), logged));
    }

    #[test]
    fn every_root_agent_turn_is_shown_as_the_main_conversation() {
        let mut t = Transcript::new(ViewSettings::default());
        for (i, node) in ["turn1", "turn2"].iter().enumerate() {
            let base = (i as u64) * 10;
            t.apply(&agent_started(base, node));
            let mut e = model_call(base + 1, &format!("c{i}"));
            e.node_id = Some(NodeId::from(*node));
            t.apply(&e);
            let mut d = delta(
                base + 2,
                &format!("c{i}"),
                DeltaChannel::Text,
                &format!("answer {i}"),
            );
            d.node_id = Some(NodeId::from(*node));
            t.apply(&d);
        }
        let answers: Vec<&str> = t
            .entries()
            .iter()
            .filter_map(|e| match &e.kind {
                EntryKind::Assistant(a) => Some(a.text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(answers, ["answer 0", "answer 1"]);
    }

    fn notices(t: &Transcript) -> Vec<String> {
        t.entries()
            .iter()
            .filter_map(|e| match &e.kind {
                EntryKind::Notice(n) => Some(n.text.clone()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn finished_turns_report_verified_unverified_or_failed() {
        let changed = |seq| {
            ev(
                seq,
                Some("n"),
                EventKind::FileChanged {
                    path: "a.rs".into(),
                    change: FileChangeKind::Modified,
                    diff: None,
                },
            )
        };
        let verify = |seq, passed| {
            ev(
                seq,
                Some("n"),
                EventKind::VerifyFinished {
                    command: "cargo test".into(),
                    passed,
                    exit_code: Some(if passed { 0 } else { 1 }),
                    duration_ms: 5,
                    output: Payload::text("out"),
                },
            )
        };
        let run = |events: Vec<RunEvent>| {
            let mut t = Transcript::new(ViewSettings::default());
            t.apply(&agent_started(0, "n"));
            for e in &events {
                t.apply(e);
            }
            t.apply(&node_done(99, "n"));
            notices(&t)
        };
        let n = run(vec![changed(1)]);
        assert!(
            n.last().unwrap().starts_with("unverified: 1 file change"),
            "{n:?}"
        );
        let n = run(vec![changed(1), verify(2, true)]);
        assert!(n.last().unwrap().starts_with("verified"), "{n:?}");
        let n = run(vec![changed(1), verify(2, false)]);
        assert!(
            n.last().unwrap().starts_with("verification failed"),
            "{n:?}"
        );
        // Nothing changed and nothing verified: nothing to report.
        assert!(run(vec![]).is_empty());
    }

    #[test]
    fn memory_changes_render_as_cards_with_undo() {
        const SHA: &str = "0123456789abcdef0123456789abcdef01234567";
        let mut t = Transcript::new(ViewSettings::default());
        t.apply(&ev(
            1,
            None,
            EventKind::Memory {
                action: MemoryAction::Written,
                memory_id: SHA.into(),
                summary: "saved in projects/app/build: use cargo nextest".into(),
            },
        ));
        let theme = Theme::default();
        let (lines, hits) = t.render(Rect::new(0, 0, 70, 20), &frame(&theme));
        let text = texts(&lines).join("\n");
        assert!(text.contains("Updated memory"), "{text}");
        assert!(text.contains("+ use cargo nextest"), "{text}");
        let undo = hits
            .iter()
            .find(|h| h.id == undo_button_id(SHA))
            .expect("undo button region");
        // Hit rows are screen rows: the lines start below the padding.
        let row = (undo.rect.y - t.pad()) as usize;
        assert!(texts(&lines)[row].contains("[ undo ]"));
        assert_eq!(
            t.activate(&undo.action),
            Some(TranscriptOutput::UndoMemory(SHA.into()))
        );
        t.memory_undone(SHA);
        let (lines, hits) = t.render(Rect::new(0, 0, 70, 20), &frame(&theme));
        assert!(texts(&lines).join("\n").contains("undone"));
        assert!(hits.iter().all(|h| h.id != undo_button_id(SHA)));
    }
}
