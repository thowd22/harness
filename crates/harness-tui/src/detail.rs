//! Node detail: a GitHub-Actions-style log of one agent or step.
//!
//! Left, a **sidebar** lists the node's turns, tool calls, verify runs and
//! the hygiene report; activating an item scrolls the log to it and
//! expands it. Right, the **log** has one collapsible group per model turn
//! (header with duration, tokens in / cached / out / reasoning and cost),
//! holding the turn's reasoning, text, tool call cards ([`ToolCard`], the
//! same cards as the chat) and the chat's per-turn metadata line
//! ([`TurnMeta::line`]). Verify runs, errors and the hygiene report are
//! groups of their own. Failed tool calls, failed verify runs and the turns
//! that contain them expand automatically.
//!
//! The current turn streams live: reasoning is expanded while it streams
//! and collapses per `[ui] show_thinking` once the turn finishes; text and
//! tool calls appear as they are produced, bash output streams through
//! [`DetailView::tool_output`]. When a provider hides its reasoning the
//! turn says so. **Follow mode** (on while the node runs, `f` toggles)
//! keeps the log tailed.
//!
//! Every group header, card, reasoning toggle, sidebar item and button is
//! registered in the interaction layer: click it, or move focus with
//! Tab / arrows and press Enter. Esc backs out. PgUp/PgDn and the wheel
//! scroll the log.
//!
//! Palette actions ([`palette_entries`], [`DetailView::run_palette`]):
//! search the log, copy the focused entry, open the diff review and open
//! the request inspector for the focused turn.
//!
//! **Copy**: `y` (or the `copy` key, or the `[⧉ copy entry]` button in the
//! header, or the palette) copies the focused log entry as plain text: a
//! turn's reasoning, text and tool outputs, a card's output, a verify run.
//!
//! **Virtualization**: each group caches its laid-out lines keyed by its
//! content version, width, focus and spinner frame, so a frame only lays
//! out groups that changed; 10k-entry logs stay within the frame budget.

use std::collections::HashMap;

use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseEventKind};
use harness_config::ini::config::ShowThinking;
use harness_events::reducer::RunState;
use harness_events::schema::{
    DeltaChannel, EventKind, FileChangeKind, NodeId, NodeStatus, RunEvent,
};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use time::OffsetDateTime;

use crate::cards::{CardCtx, CardStatus, ToolCard, payload_json, payload_text, str_arg};
use crate::chat::text::{
    fmt_cost, fmt_ms, fmt_tokens, right_align, sanitize, truncate, truncate_line, wrap_line,
};
use crate::chat::transcript::{TurnMeta, ViewSettings};
use crate::indicators::{Spinner, node_glyph};
use crate::interaction::{Dispatch, ElementId, HitMap, Interaction};
use crate::map::live::hidden_reasoning;
use crate::map::metrics;
use crate::palette::{ArgSpec, EntryKind as PaletteKind, Invocation, PaletteEntry};
use crate::theme::{Theme, Token};

/// Width of the sidebar.
pub const SIDEBAR_WIDTH: u16 = 30;
/// Element id of the header's copy button.
pub const COPY_BUTTON_ID: &str = "detail.copy_button";
/// Label of the header's copy button.
pub const COPY_BUTTON_LABEL: &str = "[⧉ copy entry]";

/// Palette entry ids of the detail view.
pub mod ids {
    /// Search the log (argument: query).
    pub const SEARCH: &str = "detail.search";
    /// Copy the focused entry.
    pub const COPY: &str = "detail.copy";
    /// Open the diff review for the node.
    pub const DIFF: &str = "detail.diff";
    /// Open the request inspector for the focused turn.
    pub const INSPECT: &str = "detail.inspect";
    /// Toggle follow mode.
    pub const FOLLOW: &str = "detail.follow";
}

/// Palette entries offered while node detail is shown.
pub fn palette_entries() -> Vec<PaletteEntry> {
    vec![
        PaletteEntry::new(
            ids::SEARCH,
            "Search log",
            "Find text in the node log",
            PaletteKind::Builtin,
        )
        .with_args(vec![ArgSpec::required("query")]),
        PaletteEntry::new(
            ids::COPY,
            "Copy entry",
            "Copy the focused log entry",
            PaletteKind::Builtin,
        ),
        PaletteEntry::new(
            ids::DIFF,
            "Open diff",
            "Review the node's file changes",
            PaletteKind::Builtin,
        ),
        PaletteEntry::new(
            ids::INSPECT,
            "Open request inspector",
            "Exact request and response of the focused turn",
            PaletteKind::Builtin,
        ),
        PaletteEntry::new(
            ids::FOLLOW,
            "Follow log",
            "Tail the running node",
            PaletteKind::Builtin,
        ),
    ]
}

/// What a log group is.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum GroupKey {
    /// Model turn `n` (0-based).
    Turn(usize),
    /// Verify run `n`.
    Verify(usize),
    /// Notice `n` (errors, retries).
    Notice(usize),
    /// The hygiene report.
    Hygiene,
    /// Artifacts the node's step wrote.
    Artifacts,
}

/// What activating a detail element does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DetailAction {
    /// Expand or collapse a group.
    Toggle(GroupKey),
    /// Expand or collapse a tool card.
    ToggleCard(String),
    /// Expand or collapse a turn's reasoning.
    ToggleThinking(usize),
    /// Scroll to a group (sidebar), expanding it.
    Jump(GroupKey),
    /// Scroll to a tool card (sidebar), expanding its turn.
    JumpCard(String),
    /// Hygiene: open the diff review.
    ReviewDiff,
    /// Hygiene: remove temporary files.
    RemoveTemp,
    /// Open artifact `n`.
    OpenArtifact(usize),
    /// Copy the focused (or last focused) entry.
    Copy,
}

/// What the detail view asks the app to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DetailOutput {
    /// Esc: back to the map.
    Back,
    /// Copy this text to the clipboard (OSC 52 or similar).
    Copy(String),
    /// Open the diff review for the node.
    OpenDiff(NodeId),
    /// Open the request inspector on a model call.
    OpenInspector {
        /// Node.
        node: NodeId,
        /// Model call id.
        call_id: String,
    },
    /// Remove the hygiene report's temp/debug files
    /// (`harness_hygiene::report::remove_temp_files`).
    RemoveTempFiles(NodeId),
    /// Show the content of one of the node's artifacts.
    OpenArtifact {
        /// Node.
        node: NodeId,
        /// Artifact name.
        name: String,
    },
}

#[derive(Debug, Clone)]
struct Turn {
    call_id: String,
    model: String,
    ts: OffsetDateTime,
    text: String,
    reasoning: String,
    meta: Option<TurnMeta>,
    cards: Vec<ToolCard>,
    hidden: Option<String>,
    thinking: Option<bool>,
}

impl Turn {
    fn failed(&self) -> bool {
        self.cards.iter().any(|c| c.status == CardStatus::Error)
    }
    fn streaming(&self) -> bool {
        self.meta.is_none()
    }
}

#[derive(Debug, Clone)]
struct Verify {
    command: String,
    ts: OffsetDateTime,
    passed: Option<bool>,
    exit_code: Option<i32>,
    duration_ms: u64,
    output: String,
}

#[derive(Debug, Clone)]
struct NoticeEntry {
    ts: OffsetDateTime,
    text: String,
    error: bool,
}

#[derive(Debug, Clone, PartialEq)]
struct CacheKey {
    version: u64,
    width: usize,
    focus: Option<String>,
    spinner: char,
    expanded: bool,
    search: Option<String>,
}

#[derive(Debug, Clone)]
struct Rendered {
    key: CacheKey,
    lines: Vec<Line<'static>>,
    hits: Vec<Hit>,
}

/// A clickable element inside a group: id, line, column range (`width`
/// 0 = the whole line) and action.
#[derive(Debug, Clone)]
struct Hit {
    id: String,
    line: usize,
    x: u16,
    width: u16,
    action: DetailAction,
}

/// Element id of artifact `i` in the Artifacts section.
pub fn artifact_id(i: usize) -> String {
    format!("detail.artifact.{i}")
}

fn hit(id: impl Into<String>, line: usize, action: DetailAction) -> Hit {
    Hit {
        id: id.into(),
        line,
        x: 0,
        width: 0,
        action,
    }
}

#[derive(Debug, Clone)]
struct Group {
    key: GroupKey,
    ts: OffsetDateTime,
    version: u64,
    expanded: Option<bool>,
    cache: Option<Rendered>,
}

/// Element id of a group header.
pub fn group_id(key: &GroupKey) -> String {
    match key {
        GroupKey::Turn(i) => format!("detail.turn.{i}"),
        GroupKey::Verify(i) => format!("detail.verify.{i}"),
        GroupKey::Notice(i) => format!("detail.notice.{i}"),
        GroupKey::Hygiene => "detail.hygiene".into(),
        GroupKey::Artifacts => "detail.artifacts".into(),
    }
}

/// Element id of a sidebar item.
pub fn sidebar_id(key: &str) -> String {
    format!("detail.side.{key}")
}

/// The node detail view.
pub struct DetailView {
    node: NodeId,
    theme: Theme,
    settings: ViewSettings,
    spinner: Spinner,
    run: RunState,
    turns: Vec<Turn>,
    verifies: Vec<Verify>,
    notices: Vec<NoticeEntry>,
    groups: Vec<Group>,
    cards: HashMap<String, usize>,
    reasoning_seen: HashMap<String, bool>,
    hygiene: Vec<String>,
    hygiene_temp: usize,
    artifacts: Vec<String>,
    follow: bool,
    scroll: usize,
    last_height: usize,
    search: Option<String>,
    interaction: Interaction<DetailAction>,
    layouts: u64,
    pending_jump: Option<usize>,
    focus_scroll: Option<ElementId>,
    side_scroll: usize,
    /// The last focused log entry (the copy button takes focus when clicked).
    copy_focus: Option<ElementId>,
}

impl DetailView {
    /// Detail for `node`.
    pub fn new(node: NodeId, theme: Theme, settings: ViewSettings, animations: bool) -> DetailView {
        DetailView {
            node,
            theme,
            settings,
            spinner: Spinner::new(animations),
            run: RunState::new(),
            turns: Vec::new(),
            verifies: Vec::new(),
            notices: Vec::new(),
            groups: Vec::new(),
            cards: HashMap::new(),
            reasoning_seen: HashMap::new(),
            hygiene: Vec::new(),
            hygiene_temp: 0,
            artifacts: Vec::new(),
            follow: true,
            scroll: 0,
            last_height: 0,
            search: None,
            interaction: Interaction::new(),
            layouts: 0,
            pending_jump: None,
            focus_scroll: None,
            side_scroll: 0,
            copy_focus: None,
        }
    }

    /// Detail for `node` folded from a run's events.
    pub fn from_events<'a>(
        node: NodeId,
        theme: Theme,
        settings: ViewSettings,
        animations: bool,
        events: impl IntoIterator<Item = &'a RunEvent>,
    ) -> DetailView {
        let mut d = DetailView::new(node, theme, settings, animations);
        for e in events {
            d.apply_run_event(e);
        }
        d
    }

    /// The focused element.
    pub fn focused(&self) -> Option<&crate::interaction::ElementId> {
        self.interaction.focused()
    }

    /// The node shown.
    pub fn node(&self) -> &NodeId {
        &self.node
    }

    /// Whether follow mode is on.
    pub fn is_following(&self) -> bool {
        self.follow
    }

    /// Group layouts computed so far (for tests and benchmarks).
    pub fn layouts(&self) -> u64 {
        self.layouts
    }

    /// Number of log groups.
    pub fn group_count(&self) -> usize {
        self.groups.len()
    }

    /// Advances the spinner.
    pub fn tick(&mut self) {
        self.spinner.tick();
    }

    /// Whether the node is still running.
    pub fn is_running(&self) -> bool {
        self.run
            .node(&self.node)
            .is_some_and(|n| !n.status.is_terminal())
    }

    fn touch(&mut self, key: &GroupKey) {
        if let Some(g) = self.groups.iter_mut().rev().find(|g| &g.key == key) {
            g.version += 1;
        }
    }

    fn push_group(&mut self, key: GroupKey, ts: OffsetDateTime) {
        self.groups.push(Group {
            key,
            ts,
            version: 0,
            expanded: None,
            cache: None,
        });
    }

    /// Folds one run event (events of other nodes only update run totals).
    pub fn apply_run_event(&mut self, ev: &RunEvent) {
        self.run.apply(ev);
        if ev.node_id.as_ref() != Some(&self.node) {
            return;
        }
        let ts = ev.ts;
        match &ev.kind {
            EventKind::ToolOutput { call_id, text, .. } => self.tool_output(call_id, text),
            EventKind::ModelCallStarted { call_id, model, .. } => {
                let i = self.turns.len();
                self.turns.push(Turn {
                    call_id: call_id.clone(),
                    model: model.clone(),
                    ts,
                    text: String::new(),
                    reasoning: String::new(),
                    meta: None,
                    cards: Vec::new(),
                    hidden: None,
                    thinking: None,
                });
                self.push_group(GroupKey::Turn(i), ts);
            }
            EventKind::Delta {
                call_id,
                channel,
                text,
            } => {
                if let Some(i) = self.turns.iter().rposition(|t| &t.call_id == call_id) {
                    match channel {
                        DeltaChannel::Text => self.turns[i].text.push_str(text),
                        DeltaChannel::Reasoning => {
                            self.reasoning_seen.insert(call_id.clone(), true);
                            self.turns[i].reasoning.push_str(text)
                        }
                        DeltaChannel::ToolArgs => return,
                    }
                    self.touch(&GroupKey::Turn(i));
                }
            }
            EventKind::ModelCallFinished {
                call_id,
                usage,
                stop_reason,
                duration_ms,
                ..
            } => {
                if let Some(i) = self.turns.iter().rposition(|t| &t.call_id == call_id) {
                    let saw = self.reasoning_seen.remove(call_id).unwrap_or(false);
                    let t = &mut self.turns[i];
                    t.hidden = hidden_reasoning(&ev.kind, saw);
                    t.meta = Some(TurnMeta {
                        model: t.model.clone(),
                        usage: *usage,
                        cost: None,
                        duration_ms: *duration_ms,
                        stop_reason: stop_reason.clone(),
                    });
                    self.touch(&GroupKey::Turn(i));
                }
            }
            EventKind::Cost { usd } => {
                if let Some(i) = self.turns.iter().rposition(|t| t.meta.is_some()) {
                    if let Some(m) = &mut self.turns[i].meta {
                        m.cost = Some(m.cost.unwrap_or(0.0) + usd);
                    }
                    self.touch(&GroupKey::Turn(i));
                }
            }
            EventKind::ToolCall {
                call_id,
                tool,
                args,
            } => {
                let args = payload_json(args)
                    .unwrap_or_else(|| serde_json::Value::String(payload_text(args)));
                if self.turns.is_empty() {
                    self.turns.push(Turn {
                        call_id: String::new(),
                        model: String::new(),
                        ts,
                        text: String::new(),
                        reasoning: String::new(),
                        meta: None,
                        cards: Vec::new(),
                        hidden: None,
                        thinking: None,
                    });
                    self.push_group(GroupKey::Turn(0), ts);
                }
                let i = self.turns.len() - 1;
                self.turns[i]
                    .cards
                    .push(ToolCard::new(call_id.clone(), tool.clone(), args, ts));
                self.cards.insert(call_id.clone(), i);
                self.touch(&GroupKey::Turn(i));
            }
            EventKind::ToolResult {
                call_id,
                ok,
                output,
                duration_ms,
            } => {
                let out = payload_text(output);
                self.with_card(call_id, |c| {
                    c.finish(*ok, out, *duration_ms);
                    if !*ok {
                        // Failures auto-expand.
                        c.expanded = true;
                    }
                });
            }
            EventKind::McpCall {
                call_id,
                server,
                tool,
            } => self.with_card(call_id, |c| c.set_mcp(server, tool)),
            EventKind::FileChanged { path, change, diff } => {
                let diff = diff.as_ref().map(payload_text);
                self.file_changed(path, *change, diff);
            }
            EventKind::VerifyStarted { command } => {
                let i = self.verifies.len();
                self.verifies.push(Verify {
                    command: command.clone(),
                    ts,
                    passed: None,
                    exit_code: None,
                    duration_ms: 0,
                    output: String::new(),
                });
                self.push_group(GroupKey::Verify(i), ts);
            }
            EventKind::VerifyFinished {
                command,
                passed,
                exit_code,
                duration_ms,
                output,
            } => {
                let i = match self
                    .verifies
                    .iter()
                    .rposition(|v| &v.command == command && v.passed.is_none())
                {
                    Some(i) => i,
                    None => {
                        let i = self.verifies.len();
                        self.verifies.push(Verify {
                            command: command.clone(),
                            ts,
                            passed: None,
                            exit_code: None,
                            duration_ms: 0,
                            output: String::new(),
                        });
                        self.push_group(GroupKey::Verify(i), ts);
                        i
                    }
                };
                let v = &mut self.verifies[i];
                v.passed = Some(*passed);
                v.exit_code = *exit_code;
                v.duration_ms = *duration_ms;
                v.output = payload_text(output);
                self.touch(&GroupKey::Verify(i));
            }
            EventKind::NodeRetry { attempt, reason } => {
                self.notice(ts, format!("retry {attempt}: {reason}"), false)
            }
            EventKind::Error {
                message,
                recoverable,
                ..
            } => self.notice(ts, message.clone(), !recoverable),
            EventKind::NodeFinished {
                status,
                error: Some(e),
            } if *status != NodeStatus::Succeeded => self.notice(ts, e.clone(), true),
            _ => {}
        }
        if let EventKind::NodeFinished { .. } = ev.kind {
            for t in 0..self.turns.len() {
                self.touch(&GroupKey::Turn(t));
            }
        }
    }

    fn notice(&mut self, ts: OffsetDateTime, text: String, error: bool) {
        let i = self.notices.len();
        self.notices.push(NoticeEntry { ts, text, error });
        self.push_group(GroupKey::Notice(i), ts);
    }

    fn with_card(&mut self, call_id: &str, f: impl FnOnce(&mut ToolCard)) {
        if let Some(&i) = self.cards.get(call_id)
            && let Some(c) = self.turns[i]
                .cards
                .iter_mut()
                .find(|c| c.call_id == call_id)
        {
            f(c);
            self.touch(&GroupKey::Turn(i));
        }
    }

    fn file_changed(&mut self, path: &str, kind: FileChangeKind, diff: Option<String>) {
        let mut target = None;
        for (ti, t) in self.turns.iter().enumerate() {
            for c in &t.cards {
                if matches!(c.tool.as_str(), "write" | "edit")
                    && let Some(p) = str_arg(&c.args, &["path", "file_path"])
                    && (path.ends_with(&p) || p.ends_with(path))
                {
                    target = Some((ti, c.call_id.clone()));
                }
            }
        }
        if let Some((_, id)) = target {
            self.with_card(&id, |c| c.set_change(kind, diff));
        }
    }

    /// Appends live tool output (bash streaming) to a call.
    pub fn tool_output(&mut self, call_id: &str, chunk: &str) {
        self.with_card(call_id, |c| c.push_output(chunk));
    }

    /// Sets the hygiene section: the change ledger summary
    /// (`harness_hygiene::ledger::summary_lines`) and the report
    /// (`HygieneReport::lines`), plus how many temp/debug files it found.
    pub fn set_hygiene(&mut self, ledger: Vec<String>, report: Vec<String>, temp_files: usize) {
        let mut lines = ledger;
        lines.extend(report);
        self.hygiene = lines;
        self.hygiene_temp = temp_files;
        match self.groups.iter_mut().find(|g| g.key == GroupKey::Hygiene) {
            Some(g) => g.version += 1,
            None => {
                let ts = self
                    .groups
                    .last()
                    .map_or(OffsetDateTime::UNIX_EPOCH, |g| g.ts);
                self.push_group(GroupKey::Hygiene, ts);
            }
        }
    }

    /// Lists the artifacts the node's step wrote (names); each opens with
    /// a click or Enter ([`DetailOutput::OpenArtifact`]).
    pub fn set_artifacts(&mut self, names: Vec<String>) {
        if names == self.artifacts {
            return;
        }
        self.artifacts = names;
        match self
            .groups
            .iter_mut()
            .find(|g| g.key == GroupKey::Artifacts)
        {
            Some(g) => g.version += 1,
            None if !self.artifacts.is_empty() => {
                let ts = self
                    .groups
                    .last()
                    .map_or(OffsetDateTime::UNIX_EPOCH, |g| g.ts);
                self.push_group(GroupKey::Artifacts, ts);
            }
            None => {}
        }
    }

    /// Sets the search query: matching lines are highlighted and the log
    /// scrolls to the first match. Returns the number of matching lines.
    pub fn search(&mut self, query: &str) -> usize {
        let q = query.trim().to_lowercase();
        self.search = (!q.is_empty()).then(|| q.clone());
        if q.is_empty() {
            return 0;
        }
        let hits: Vec<usize> = (0..self.groups.len())
            .filter(|&i| self.group_plain(i).to_lowercase().contains(&q))
            .collect();
        // Expand matching groups and jump to the first.
        for &i in &hits {
            self.groups[i].expanded = Some(true);
        }
        if let Some(&first) = hits.first() {
            self.follow = false;
            self.pending_jump = Some(first);
        }
        hits.len()
    }

    /// The plain text of a group (search, copy).
    fn group_plain(&self, gi: usize) -> String {
        match &self.groups[gi].key {
            GroupKey::Turn(i) => {
                let t = &self.turns[*i];
                let mut s = format!("{}\n{}\n", t.reasoning, t.text);
                for c in &t.cards {
                    s.push_str(&format!(
                        "{} {}\n{}\n",
                        c.display_name(),
                        c.summary(),
                        c.output_text(false)
                    ));
                }
                s
            }
            GroupKey::Verify(i) => {
                let v = &self.verifies[*i];
                format!("verify {}\n{}", v.command, v.output)
            }
            GroupKey::Notice(i) => self.notices[*i].text.clone(),
            GroupKey::Hygiene => self.hygiene.join("\n"),
            GroupKey::Artifacts => self.artifacts.join("\n"),
        }
    }

    fn default_expanded(&self, g: &Group) -> bool {
        match &g.key {
            GroupKey::Turn(i) => {
                let t = &self.turns[*i];
                t.failed() || t.streaming() || *i + 1 == self.turns.len()
            }
            GroupKey::Verify(i) => self.verifies[*i].passed == Some(false),
            GroupKey::Notice(_) | GroupKey::Hygiene | GroupKey::Artifacts => true,
        }
    }

    fn is_expanded(&self, gi: usize) -> bool {
        let g = &self.groups[gi];
        g.expanded.unwrap_or_else(|| self.default_expanded(g))
    }

    fn thinking_expanded(&self, t: &Turn) -> bool {
        t.thinking.unwrap_or_else(|| {
            t.streaming() || self.settings.show_thinking == ShowThinking::Expanded
        })
    }

    fn group_index(&self, key: &GroupKey) -> Option<usize> {
        self.groups.iter().position(|g| &g.key == key)
    }

    fn activate(&mut self, a: DetailAction) -> Option<DetailOutput> {
        match a {
            DetailAction::Toggle(k) => {
                if let Some(i) = self.group_index(&k) {
                    let e = !self.is_expanded(i);
                    self.groups[i].expanded = Some(e);
                }
            }
            DetailAction::ToggleCard(id) => self.with_card(&id, |c| c.toggle()),
            DetailAction::ToggleThinking(t) => {
                let e = !self.thinking_expanded(&self.turns[t]);
                self.turns[t].thinking = Some(e);
                self.touch(&GroupKey::Turn(t));
            }
            DetailAction::Jump(k) => self.jump(&k),
            DetailAction::JumpCard(id) => {
                if let Some(&t) = self.cards.get(&id) {
                    self.jump(&GroupKey::Turn(t));
                    self.interaction
                        .set_focus(crate::chat::transcript::card_id(&id));
                }
            }
            DetailAction::ReviewDiff => return Some(DetailOutput::OpenDiff(self.node.clone())),
            DetailAction::RemoveTemp => {
                return Some(DetailOutput::RemoveTempFiles(self.node.clone()));
            }
            DetailAction::OpenArtifact(i) => {
                return self
                    .artifacts
                    .get(i)
                    .map(|name| DetailOutput::OpenArtifact {
                        node: self.node.clone(),
                        name: name.clone(),
                    });
            }
            DetailAction::Copy => return self.copy(),
        }
        None
    }

    /// Copies the focused entry ([`DetailOutput::Copy`]); `None` when no
    /// entry has been focused.
    pub fn copy(&self) -> Option<DetailOutput> {
        self.focused_text().map(DetailOutput::Copy)
    }

    fn jump(&mut self, k: &GroupKey) {
        if let Some(i) = self.group_index(k) {
            self.groups[i].expanded = Some(true);
            self.follow = false;
            self.pending_jump = Some(i);
            self.interaction.set_focus(group_id(k));
        }
    }

    /// The focused entry's text (for copy).
    fn focused_text(&self) -> Option<String> {
        let f = match self.interaction.focused() {
            Some(f) if f.0 != COPY_BUTTON_ID => f.0.clone(),
            _ => self.copy_focus.clone()?.0,
        };
        // A sidebar item copies the entry it points at.
        let f = match f.strip_prefix("detail.side.") {
            Some(rest) if rest.starts_with("card.") => rest.to_owned(),
            Some(rest) => format!("detail.{rest}"),
            None => f,
        };
        if let Some(id) = f.strip_prefix("card.") {
            let t = *self.cards.get(id)?;
            let c = self.turns[t].cards.iter().find(|c| c.call_id == id)?;
            return Some(format!(
                "{} {}\n{}",
                c.display_name(),
                c.summary(),
                c.output_text(false)
            ));
        }
        let gi = self.groups.iter().position(|g| group_id(&g.key) == f)?;
        Some(self.group_plain(gi))
    }

    fn focused_turn(&self) -> Option<usize> {
        let f = &self.interaction.focused()?.0;
        if let Some(id) = f.strip_prefix("card.") {
            return self.cards.get(id).copied();
        }
        f.strip_prefix("detail.turn.")
            .or_else(|| f.strip_prefix("detail.thinking."))
            .and_then(|n| n.parse().ok())
    }

    /// Runs a palette entry from [`palette_entries`].
    pub fn run_palette(&mut self, inv: &Invocation) -> Option<DetailOutput> {
        match inv.id.as_str() {
            ids::SEARCH => {
                let q = inv.args.first().cloned().unwrap_or_default();
                self.search(&q);
                None
            }
            ids::COPY => self.focused_text().map(DetailOutput::Copy),
            ids::DIFF => Some(DetailOutput::OpenDiff(self.node.clone())),
            ids::INSPECT => {
                let t = self
                    .focused_turn()
                    .or_else(|| self.turns.len().checked_sub(1))?;
                Some(DetailOutput::OpenInspector {
                    node: self.node.clone(),
                    call_id: self.turns[t].call_id.clone(),
                })
            }
            ids::FOLLOW => {
                self.follow = true;
                None
            }
            _ => None,
        }
    }

    /// Handles terminal input.
    pub fn handle(&mut self, ev: &Event) -> Option<DetailOutput> {
        match ev {
            Event::Key(k) => self.handle_key(k),
            Event::Mouse(m) => match m.kind {
                MouseEventKind::ScrollUp => {
                    self.scroll_by(-crate::interaction::WHEEL_LINES);
                    None
                }
                MouseEventKind::ScrollDown => {
                    self.scroll_by(crate::interaction::WHEEL_LINES);
                    None
                }
                _ => match self.interaction.handle_mouse(m)? {
                    Dispatch::Activate(a) => self.activate(a),
                    Dispatch::Back => Some(DetailOutput::Back),
                    _ => None,
                },
            },
            _ => None,
        }
    }

    fn scroll_by(&mut self, lines: i32) {
        let s = self.scroll as i64 + i64::from(lines);
        self.scroll = s.max(0) as usize;
        if lines < 0 {
            self.follow = false;
        }
    }

    /// Handles a key.
    pub fn handle_key(&mut self, k: &KeyEvent) -> Option<DetailOutput> {
        if k.kind == KeyEventKind::Release {
            return None;
        }
        let plain = k.modifiers.difference(KeyModifiers::SHIFT).is_empty();
        if plain {
            match k.code {
                KeyCode::Char('f') => {
                    self.follow = !self.follow;
                    return None;
                }
                KeyCode::Char('y') => return self.copy(),
                KeyCode::PageUp | KeyCode::PageDown => {
                    let page = self.last_height.saturating_sub(1).max(1) as i32;
                    self.scroll_by(if k.code == KeyCode::PageUp {
                        -page
                    } else {
                        page
                    });
                    return None;
                }
                KeyCode::Esc => return Some(DetailOutput::Back),
                _ => {}
            }
        }
        match self.interaction.handle_key(k)? {
            Dispatch::Activate(a) => self.activate(a),
            Dispatch::Back => Some(DetailOutput::Back),
            Dispatch::Focus(id) => {
                self.focus_scroll = Some(id);
                None
            }
            Dispatch::Scroll { lines, .. } => {
                self.scroll_by(lines);
                None
            }
        }
    }
}

struct Ctx<'a> {
    theme: &'a Theme,
    width: usize,
    now: OffsetDateTime,
    focus: Option<&'a str>,
}

impl DetailView {
    fn turn_header(&self, i: usize, expanded: bool, c: &Ctx<'_>) -> Line<'static> {
        let t = &self.turns[i];
        let th = c.theme;
        let muted = th.fg(Token::Muted);
        let num = th.fg(Token::Number);
        let arrow = if expanded { "▾ " } else { "▸ " };
        let (icon, token) = if t.streaming() && self.is_running() {
            (self.spinner.glyph(), Token::Running)
        } else if t.failed() {
            ('✗', Token::Error)
        } else {
            ('✔', Token::Ok)
        };
        let mut left = vec![
            Span::styled(arrow, th.fg(Token::Accent)),
            Span::styled(format!("{icon} "), th.fg(token)),
            Span::styled(
                format!("Turn {}", i + 1),
                th.fg(Token::Fg).add_modifier(Modifier::BOLD),
            ),
            Span::styled(format!("  {}", t.model), th.fg(Token::Agent)),
        ];
        if let Some(m) = &t.meta {
            let u = m.usage;
            left.push(Span::styled("  in ", muted));
            left.push(Span::styled(fmt_tokens(u.input + u.cached_input), num));
            left.push(Span::styled(" (cached ", muted));
            left.push(Span::styled(fmt_tokens(u.cached_input), num));
            if u.cache_write > 0 {
                left.push(Span::styled(", written ", muted));
                left.push(Span::styled(fmt_tokens(u.cache_write), num));
            }
            left.push(Span::styled(") out ", muted));
            left.push(Span::styled(fmt_tokens(u.output), num));
            if u.reasoning > 0 {
                left.push(Span::styled(" reasoning ", muted));
                left.push(Span::styled(fmt_tokens(u.reasoning), num));
            }
            left.push(Span::styled("  ", muted));
            left.push(Span::styled(fmt_cost(m.cost), num));
            left.push(Span::styled("  ", muted));
            left.push(Span::styled(fmt_ms(m.duration_ms), num));
        } else {
            left.push(Span::styled("  streaming…", th.fg(Token::Running)));
        }
        if !t.cards.is_empty() {
            left.push(Span::styled(format!("  {} tools", t.cards.len()), muted));
        }
        let right = self
            .settings
            .time
            .format(t.ts, c.now)
            .map(|s| vec![Span::styled(s, muted)])
            .unwrap_or_default();
        right_align(Line::from(left), right, c.width, Span::raw(" "))
    }

    fn layout_group(&self, gi: usize, c: &Ctx<'_>) -> (Vec<Line<'static>>, Vec<Hit>) {
        let th = c.theme;
        let g = &self.groups[gi];
        let expanded = self.is_expanded(gi);
        let mut lines: Vec<Line<'static>> = Vec::new();
        let mut hits = Vec::new();
        let header_style = |id: &str| {
            if c.focus == Some(id) {
                th.focused()
            } else {
                Style::default()
            }
        };
        let gid = group_id(&g.key);
        let indent = || Span::raw("  ");
        match &g.key {
            GroupKey::Turn(i) => {
                let t = &self.turns[*i];
                lines.push(
                    self.turn_header(*i, expanded, c)
                        .patch_style(header_style(&gid)),
                );
                hits.push(hit(gid.clone(), 0, DetailAction::Toggle(g.key.clone())));
                if expanded {
                    let w = c.width.saturating_sub(2).max(10);
                    if !t.reasoning.is_empty()
                        && self.settings.show_thinking != ShowThinking::Hidden
                    {
                        let open = self.thinking_expanded(t);
                        let tid = format!("detail.thinking.{i}");
                        let label = if open {
                            "▾ thinking".to_owned()
                        } else {
                            format!(
                                "▸ thinking ({} lines, collapsed)",
                                t.reasoning.lines().count()
                            )
                        };
                        hits.push(hit(
                            tid.clone(),
                            lines.len(),
                            DetailAction::ToggleThinking(*i),
                        ));
                        lines.push(
                            Line::from(vec![
                                indent(),
                                Span::styled(label, th.fg(Token::Reasoning)),
                            ])
                            .patch_style(header_style(&tid)),
                        );
                        if open {
                            for l in t.reasoning.lines() {
                                let line =
                                    Line::from(Span::styled(sanitize(l), th.fg(Token::Reasoning)));
                                for wl in wrap_line(line, w.saturating_sub(2), &[]) {
                                    lines.push(prefixed(
                                        vec![indent(), Span::styled("┊ ", th.fg(Token::Muted))],
                                        wl,
                                    ));
                                }
                            }
                        }
                    }
                    if let Some(h) = &t.hidden {
                        lines.push(Line::from(vec![
                            indent(),
                            Span::styled(
                                format!("ⓘ {h}"),
                                th.fg(Token::Warn).add_modifier(Modifier::ITALIC),
                            ),
                        ]));
                    }
                    for l in t.text.lines() {
                        let line = Line::from(Span::styled(sanitize(l), th.fg(Token::Fg)));
                        for wl in wrap_line(line, w, &[]) {
                            lines.push(prefixed(vec![indent()], wl));
                        }
                    }
                    for card in &t.cards {
                        let cid = crate::chat::transcript::card_id(&card.call_id);
                        let ctx = CardCtx {
                            theme: th,
                            width: w,
                            time: self.settings.time,
                            now: c.now,
                            preview_lines: self.settings.preview_lines,
                            spinner: self.spinner,
                            focused: c.focus == Some(cid.as_str()),
                            copy_buttons: false,
                            focus: None,
                        };
                        hits.push(hit(
                            cid,
                            lines.len(),
                            DetailAction::ToggleCard(card.call_id.clone()),
                        ));
                        for l in card.render(&ctx) {
                            let mut spans = vec![indent()];
                            spans.extend(l.spans);
                            lines.push(Line::from(spans).style(l.style));
                        }
                    }
                    if let Some(m) = &t.meta {
                        let mut spans = vec![indent()];
                        spans.extend(m.line(th).spans);
                        lines.push(truncate_line(Line::from(spans), c.width));
                    }
                }
            }
            GroupKey::Verify(i) => {
                let v = &self.verifies[*i];
                let (icon, token) = match v.passed {
                    None => (self.spinner.glyph(), Token::Running),
                    Some(true) => ('✔', Token::Ok),
                    Some(false) => ('✗', Token::Error),
                };
                let arrow = if expanded { "▾ " } else { "▸ " };
                let mut right = Vec::new();
                if let Some(code) = v.exit_code.filter(|_| v.passed == Some(false)) {
                    right.push(Span::styled(format!("exit {code} · "), th.fg(Token::Error)));
                }
                if v.passed.is_some() {
                    right.push(Span::styled(fmt_ms(v.duration_ms), th.fg(Token::Number)));
                }
                if let Some(ts) = self.settings.time.format(v.ts, c.now) {
                    right.push(Span::styled(format!(" · {ts}"), th.fg(Token::Muted)));
                }
                let left = Line::from(vec![
                    Span::styled(arrow, th.fg(Token::Accent)),
                    Span::styled(format!("{icon} "), th.fg(token)),
                    Span::styled("verify ", th.fg(Token::Accent)),
                    Span::styled(sanitize(&v.command), th.fg(Token::Fg)),
                ]);
                lines.push(
                    right_align(left, right, c.width, Span::raw(" "))
                        .patch_style(header_style(&gid)),
                );
                hits.push(hit(gid.clone(), 0, DetailAction::Toggle(g.key.clone())));
                if expanded {
                    let out: Vec<&str> = v.output.lines().collect();
                    for l in &out[out.len().saturating_sub(20)..] {
                        lines.push(Line::from(vec![
                            indent(),
                            Span::styled(
                                truncate(&sanitize(l), c.width.saturating_sub(2)),
                                th.fg(Token::Muted),
                            ),
                        ]));
                    }
                }
            }
            GroupKey::Notice(i) => {
                let n = &self.notices[*i];
                let (icon, token) = if n.error {
                    ('✗', Token::Error)
                } else {
                    ('⚠', Token::Warn)
                };
                for (k, l) in n.text.lines().enumerate() {
                    let prefix = if k == 0 {
                        format!("{icon} ")
                    } else {
                        "  ".into()
                    };
                    let line = Line::from(Span::styled(
                        format!("{prefix}{}", sanitize(l)),
                        th.fg(token),
                    ));
                    let right = if k == 0 {
                        self.settings
                            .time
                            .format(n.ts, c.now)
                            .map(|s| vec![Span::styled(s, th.fg(Token::Muted))])
                            .unwrap_or_default()
                    } else {
                        Vec::new()
                    };
                    lines.push(right_align(line, right, c.width, Span::raw(" ")));
                }
                if lines.is_empty() {
                    lines.push(Line::from(Span::styled(format!("{icon}"), th.fg(token))));
                }
            }
            GroupKey::Hygiene => {
                let arrow = if expanded { "▾ " } else { "▸ " };
                lines.push(
                    Line::from(vec![
                        Span::styled(arrow, th.fg(Token::Accent)),
                        Span::styled(
                            "Hygiene report",
                            th.fg(Token::Fg).add_modifier(Modifier::BOLD),
                        ),
                    ])
                    .patch_style(header_style(&gid)),
                );
                hits.push(hit(gid.clone(), 0, DetailAction::Toggle(g.key.clone())));
                if expanded {
                    for l in &self.hygiene {
                        lines.push(truncate_line(
                            Line::from(vec![indent(), Span::styled(sanitize(l), th.fg(Token::Fg))]),
                            c.width,
                        ));
                    }
                    let diff_id = "detail.hygiene.diff".to_owned();
                    let temp_id = "detail.hygiene.temp".to_owned();
                    let mut spans = vec![
                        indent(),
                        Span::styled(
                            "[review diff]",
                            th.fg(Token::Accent).patch(header_style(&diff_id)),
                        ),
                    ];
                    hits.push(Hit {
                        x: 2,
                        width: 13,
                        ..hit(diff_id, lines.len(), DetailAction::ReviewDiff)
                    });
                    if self.hygiene_temp > 0 {
                        spans.push(Span::raw("  "));
                        let label = format!("[remove {} temp files]", self.hygiene_temp);
                        let w = label.chars().count() as u16;
                        spans.push(Span::styled(
                            label,
                            th.fg(Token::Warn).patch(header_style(&temp_id)),
                        ));
                        hits.push(Hit {
                            x: 17,
                            width: w,
                            ..hit(temp_id, lines.len(), DetailAction::RemoveTemp)
                        });
                    }
                    lines.push(Line::from(spans));
                }
            }
            GroupKey::Artifacts => {
                let arrow = if expanded { "▾ " } else { "▸ " };
                lines.push(
                    Line::from(vec![
                        Span::styled(arrow, th.fg(Token::Accent)),
                        Span::styled(
                            format!("Artifacts ({})", self.artifacts.len()),
                            th.fg(Token::Fg).add_modifier(Modifier::BOLD),
                        ),
                    ])
                    .patch_style(header_style(&gid)),
                );
                hits.push(hit(gid.clone(), 0, DetailAction::Toggle(g.key.clone())));
                if expanded {
                    for (i, name) in self.artifacts.iter().enumerate() {
                        let id = artifact_id(i);
                        hits.push(hit(id.clone(), lines.len(), DetailAction::OpenArtifact(i)));
                        lines.push(Line::from(vec![
                            indent(),
                            Span::styled(
                                format!("[{}]", sanitize(name)),
                                th.fg(Token::Link).patch(header_style(&id)),
                            ),
                        ]));
                    }
                }
            }
        }
        if let Some(q) = &self.search {
            for l in &mut lines {
                let plain: String = l.spans.iter().map(|s| s.content.as_ref()).collect();
                if plain.to_lowercase().contains(q.as_str()) {
                    let hl = th.fg(Token::Highlight).add_modifier(Modifier::BOLD);
                    for span in &mut l.spans {
                        span.style = span.style.patch(hl);
                    }
                }
            }
        }
        (lines, hits)
    }

    fn ensure_layout(&mut self, gi: usize, c: &Ctx<'_>) {
        let g = &self.groups[gi];
        let ids: Vec<String> = g
            .cache
            .as_ref()
            .map(|r| r.hits.iter().map(|h| h.id.clone()).collect())
            .unwrap_or_default();
        let focus = c
            .focus
            .filter(|f| ids.iter().any(|i| i == f) || *f == group_id(&g.key))
            .map(str::to_owned);
        let live = match &g.key {
            GroupKey::Turn(i) => {
                self.turns[*i].streaming()
                    || self.turns[*i]
                        .cards
                        .iter()
                        .any(|c| c.status == CardStatus::Running)
            }
            GroupKey::Verify(i) => self.verifies[*i].passed.is_none(),
            _ => false,
        };
        let key = CacheKey {
            version: g.version,
            width: c.width,
            focus,
            spinner: if live { self.spinner.glyph() } else { ' ' },
            expanded: self.is_expanded(gi),
            search: self.search.clone(),
        };
        if g.cache.as_ref().is_some_and(|r| r.key == key) {
            return;
        }
        let (lines, hits) = self.layout_group(gi, c);
        self.layouts += 1;
        self.groups[gi].cache = Some(Rendered { key, lines, hits });
    }

    fn sidebar_items(&self) -> Vec<(String, Line<'static>, DetailAction)> {
        let th = &self.theme;
        let mut out = Vec::new();
        for g in &self.groups {
            match &g.key {
                GroupKey::Turn(i) => {
                    let t = &self.turns[*i];
                    let (icon, token) = if t.streaming() && self.is_running() {
                        (self.spinner.glyph(), Token::Running)
                    } else if t.failed() {
                        ('✗', Token::Error)
                    } else {
                        ('✔', Token::Ok)
                    };
                    let mut spans = vec![
                        Span::styled(format!("{icon} "), th.fg(token)),
                        Span::styled(format!("Turn {}", i + 1), th.fg(Token::Fg)),
                    ];
                    if let Some(m) = &t.meta {
                        spans.push(Span::styled(
                            format!(" {}", fmt_ms(m.duration_ms)),
                            th.fg(Token::Number),
                        ));
                    }
                    out.push((
                        sidebar_id(&format!("turn.{i}")),
                        Line::from(spans),
                        DetailAction::Jump(g.key.clone()),
                    ));
                    for card in &t.cards {
                        let icon = card.status.icon(&self.spinner);
                        out.push((
                            sidebar_id(&format!("card.{}", card.call_id)),
                            Line::from(vec![
                                Span::raw("  "),
                                Span::styled(format!("{icon} "), th.fg(card.status.token())),
                                Span::styled(card.display_name(), th.fg(Token::Accent)),
                                Span::styled(format!(" {}", card.summary()), th.fg(Token::Muted)),
                            ]),
                            DetailAction::JumpCard(card.call_id.clone()),
                        ));
                    }
                }
                GroupKey::Verify(i) => {
                    let v = &self.verifies[*i];
                    let (icon, token) = match v.passed {
                        None => (self.spinner.glyph(), Token::Running),
                        Some(true) => ('✔', Token::Ok),
                        Some(false) => ('✗', Token::Error),
                    };
                    out.push((
                        sidebar_id(&format!("verify.{i}")),
                        Line::from(vec![
                            Span::styled(format!("{icon} "), th.fg(token)),
                            Span::styled("verify ", th.fg(Token::Accent)),
                            Span::styled(v.command.clone(), th.fg(Token::Muted)),
                        ]),
                        DetailAction::Jump(g.key.clone()),
                    ));
                }
                GroupKey::Hygiene => out.push((
                    sidebar_id("hygiene"),
                    Line::from(Span::styled("◆ Hygiene report", th.fg(Token::Fg))),
                    DetailAction::Jump(GroupKey::Hygiene),
                )),
                GroupKey::Artifacts => out.push((
                    sidebar_id("artifacts"),
                    Line::from(Span::styled(
                        format!("◆ Artifacts ({})", self.artifacts.len()),
                        th.fg(Token::Fg),
                    )),
                    DetailAction::Jump(GroupKey::Artifacts),
                )),
                GroupKey::Notice(_) => {}
            }
        }
        out
    }

    fn header(&self, now: OffsetDateTime, width: u16) -> Line<'static> {
        let th = &self.theme;
        let muted = th.fg(Token::Muted);
        let num = th.fg(Token::Number);
        let Some(n) = self.run.node(&self.node) else {
            return Line::from(Span::styled(
                format!(" {}", harness_events::names::scrub_ids(self.node.as_str())),
                th.fg(Token::Agent),
            ));
        };
        let (glyph, token) = node_glyph(n.status, &self.spinner);
        let mut spans = vec![
            Span::styled(format!(" {glyph} "), th.fg(token)),
            Span::styled(
                self.run.node_label(n),
                th.fg(Token::Agent).add_modifier(Modifier::BOLD),
            ),
        ];
        for part in [self.run.node_agent_detail(n), n.model.clone()]
            .into_iter()
            .flatten()
        {
            spans.push(Span::styled(format!(" · {part}"), muted));
        }
        if let Some(h) = &n.host {
            spans.push(Span::styled(format!(" · ⧉{h}"), th.fg(Token::Running)));
        }
        if let Some(ms) = metrics::elapsed_ms(n, now) {
            spans.push(Span::styled("  ", muted));
            spans.push(Span::styled(fmt_ms(ms), num));
        }
        spans.push(Span::styled("  ", muted));
        spans.push(Span::styled(fmt_cost(Some(n.cost_usd)), num));
        let u = n.tokens;
        spans.push(Span::styled(
            format!(
                "  in {} (cached {}) out {} reasoning {}",
                fmt_tokens(u.input + u.cached_input),
                fmt_tokens(u.cached_input),
                fmt_tokens(u.output),
                fmt_tokens(u.reasoning)
            ),
            num,
        ));
        if n.retries > 0 {
            spans.push(Span::styled(
                format!("  ↻{}", n.retries),
                th.fg(Token::Warn),
            ));
        }
        let follow = if self.follow {
            "  following"
        } else {
            "  f to follow"
        };
        spans.push(Span::styled(follow, muted));
        truncate_line(Line::from(spans), usize::from(width))
    }

    /// Renders the view into `area`.
    pub fn render(&mut self, frame: &mut Frame<'_>, area: Rect, now: OffsetDateTime) {
        let theme = self.theme.clone();
        let mut hits = self.interaction.begin_frame();
        let [head, body] =
            Layout::vertical([Constraint::Length(1), Constraint::Min(1)]).areas(area);
        if let Some(f) = self.interaction.focused()
            && f.0 != COPY_BUTTON_ID
        {
            self.copy_focus = Some(f.clone());
        }
        let bw = crate::chat::text::width(COPY_BUTTON_LABEL) as u16;
        let (head, button) = if head.width > bw + 20 {
            let [h, b] =
                Layout::horizontal([Constraint::Min(1), Constraint::Length(bw)]).areas(head);
            (h, Some(b))
        } else {
            (head, None)
        };
        frame.render_widget(Paragraph::new(self.header(now, head.width)), head);
        let (side, log) = if body.width >= SIDEBAR_WIDTH + 40 {
            let [s, l] =
                Layout::horizontal([Constraint::Length(SIDEBAR_WIDTH), Constraint::Min(1)])
                    .areas(body);
            (Some(s), l)
        } else {
            (None, body)
        };
        let focus = self.interaction.focused().map(|f| f.0.clone());
        if let Some(s) = side {
            self.render_sidebar(frame, s, &theme, focus.as_deref(), &mut hits);
        }
        self.render_log(frame, log, &theme, now, focus.as_deref(), &mut hits);
        if let Some(b) = button {
            let id = ElementId::from(COPY_BUTTON_ID);
            let mut st = theme.fg(Token::Accent);
            if hits.is_focused(&id) {
                st = st.patch(theme.focused());
            }
            frame.render_widget(Paragraph::new(Span::styled(COPY_BUTTON_LABEL, st)), b);
            hits.register_button(id, b, DetailAction::Copy);
        }
        self.interaction.end_frame(hits);
    }

    fn render_sidebar(
        &mut self,
        frame: &mut Frame<'_>,
        area: Rect,
        theme: &Theme,
        focus: Option<&str>,
        hits: &mut HitMap<DetailAction>,
    ) {
        let block = Block::new()
            .borders(Borders::RIGHT)
            .border_style(theme.fg(Token::Border));
        let inner = block.inner(area);
        frame.render_widget(block, area);
        hits.register_pane("detail.sidebar", inner);
        let items = self.sidebar_items();
        let h = usize::from(inner.height);
        if let Some(fi) = focus.and_then(|f| items.iter().position(|i| i.0 == f)) {
            if fi < self.side_scroll {
                self.side_scroll = fi;
            } else if fi >= self.side_scroll + h {
                self.side_scroll = fi + 1 - h;
            }
        }
        self.side_scroll = self.side_scroll.min(items.len().saturating_sub(h));
        let first = self.side_scroll;
        crate::selection::update("detail.sidebar", |r| r.set_scroll(first));
        let mut lines = Vec::new();
        for (row, (id, line, action)) in
            items.into_iter().skip(self.side_scroll).take(h).enumerate()
        {
            let rect = Rect::new(inner.x, inner.y + row as u16, inner.width, 1);
            let style = if focus == Some(id.as_str()) {
                theme.focused()
            } else {
                Style::default()
            };
            lines.push(truncate_line(line, usize::from(inner.width)).patch_style(style));
            hits.register(id, rect, action);
        }
        frame.render_widget(Paragraph::new(lines), inner);
    }

    fn render_log(
        &mut self,
        frame: &mut Frame<'_>,
        area: Rect,
        theme: &Theme,
        now: OffsetDateTime,
        focus: Option<&str>,
        hits: &mut HitMap<DetailAction>,
    ) {
        hits.register_pane("detail.log", area);
        let width = usize::from(area.width);
        let height = usize::from(area.height);
        self.last_height = height;
        let c = Ctx {
            theme,
            width,
            now,
            focus,
        };
        let mut offsets = Vec::with_capacity(self.groups.len());
        let mut total = 0usize;
        for gi in 0..self.groups.len() {
            self.ensure_layout(gi, &c);
            offsets.push(total);
            total += self.groups[gi].cache.as_ref().map_or(0, |r| r.lines.len());
        }
        let max = total.saturating_sub(height);
        if let Some(gi) = self.pending_jump.take() {
            self.scroll = offsets.get(gi).copied().unwrap_or(0);
        }
        if let Some(id) = self.focus_scroll.take() {
            for (gi, g) in self.groups.iter().enumerate() {
                if let Some(r) = &g.cache
                    && let Some(h) = r.hits.iter().find(|h| h.id == id.0)
                {
                    let line = offsets[gi] + h.line;
                    if line < self.scroll {
                        self.scroll = line;
                    } else if line >= self.scroll + height {
                        self.scroll = line + 1 - height;
                    }
                    self.follow = false;
                }
            }
        }
        if self.follow {
            self.scroll = max;
        }
        self.scroll = self.scroll.min(max);
        if self.scroll == max && self.is_running() {
            self.follow = true;
        }
        let start = self.scroll;
        let end = (start + height).min(total);
        let mut lines = Vec::with_capacity(height);
        // First group intersecting the viewport (binary search on offsets).
        let first = offsets.partition_point(|&o| o <= start).saturating_sub(1);
        for (gi, &off) in offsets.iter().enumerate().skip(first) {
            if off >= end {
                break;
            }
            let Some(r) = &self.groups[gi].cache else {
                continue;
            };
            for (k, l) in r.lines.iter().enumerate() {
                let line_no = off + k;
                if line_no >= start && line_no < end {
                    lines.push(l.clone());
                }
            }
            for h in &r.hits {
                let line_no = off + h.line;
                if line_no >= start && line_no < end {
                    let y = area.y + (line_no - start) as u16;
                    let (x, w) = if h.width == 0 {
                        (area.x, area.width)
                    } else {
                        (area.x + h.x, h.width.min(area.width.saturating_sub(h.x)))
                    };
                    let rect = Rect::new(x, y, w, 1);
                    if h.width == 0 {
                        hits.register(h.id.clone(), rect, h.action.clone());
                    } else {
                        hits.register_button(h.id.clone(), rect, h.action.clone());
                    }
                }
            }
        }
        if lines.is_empty() {
            lines.push(Line::from(Span::styled(
                "No activity yet.",
                theme.fg(Token::Muted),
            )));
        }
        crate::selection::update("detail.log", |r| {
            r.set_scroll(start);
            r.set_joins(lines.iter().map(crate::chat::text::soft_join));
            r.chrome = crate::selection::Chrome::Smart;
        });
        frame.render_widget(Paragraph::new(lines), area);
    }
}

/// `prefix` then the spans of `wl`, keeping the soft-wrap marker of a
/// continuation line first so text selection still rejoins it
/// ([`crate::chat::text::soft_join`]).
fn prefixed(prefix: Vec<Span<'static>>, wl: Line<'static>) -> Line<'static> {
    let style = wl.style;
    let continues = crate::chat::text::soft_join(&wl) != crate::selection::Join::Hard;
    let mut spans = wl.spans.into_iter();
    let mut out: Vec<Span<'static>> = Vec::new();
    if continues {
        out.extend(spans.next());
    }
    out.extend(prefix);
    out.extend(spans);
    Line::from(out).style(style)
}

#[cfg(test)]
mod tests;
