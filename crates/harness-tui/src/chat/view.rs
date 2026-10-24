//! The chat screen: every chat component composed and wired to input.
//!
//! ```text
//! ┌ transcript (virtualized) ─────────────────────── toasts ┐
//! │ …                                                        │
//! │ ⠹ thinking 3.1s · turn 4 · 12.3s · … · ctrl+c to interrupt  ← activity line
//! │ ╭ ⏸ approve bash  $ git push origin main ───────── … ╮     ← inline approvals
//! │ /review [branch]  Review the diff           project      ← slash completion
//! │ › draft                                                  ← input
//! │ ~/CODE/harness  main │ sonnet · auto-edit │ ctx … │ $0.84  ← status line
//! └──────────────────────────────────────────────────────────┘
//!   palette / notification history: modal overlays
//! ```
//!
//! [`ChatView`] owns its focus model (an [`Interaction`]) and turns terminal
//! events into [`ChatOutput`]s for the app and the core. Key routing: global
//! keys (quit, interrupt, palette) first, then an open modal (palette,
//! history), then a pending approval (`y` `s` `a` `n` `Y` `N` Esc, Up/Down),
//! then a focused transcript element (Tab/arrows move, Enter toggles, Esc
//! returns to the input), then the input.
//!
//! The transcript is bottom-anchored like a chat app: the newest message
//! sits just above the input and a short conversation hugs the bottom of
//! the pane. Scrolling up (wheel, PgUp) pins the view; new output then shows
//! a `↓ new messages` button on the bottom row. Clicking it, Enter on it,
//! End (from an empty input or with the cursor at the end of its line, or
//! from a focused transcript element), Ctrl+End, the palette's follow entry
//! or scrolling back down returns to the bottom and resumes following.
//!
//! Copy: the `copy` key copies the focused element (or the last assistant
//! message), the `select` key starts keyboard selection (see
//! [`super::copy`]), and copy buttons in the transcript are clickable. Every
//! copy leaves the chat as a [`ChatOutput::Copy`]; the app writes it to the
//! clipboard and confirms with a toast. Bracketed pastes go to the palette
//! filter or argument prompt when the palette is open, else to the input
//! (never to approval keys or keybindings).

use std::time::{Duration, Instant};

use crossterm::event::{
    Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use harness_config::ini::config::{Config, ShowThinking};
use harness_events::schema::{NodeId, RunEvent};
use harness_policy::approvals::{ModeState, PromptEnvelope};
use harness_policy::rules::{Action as PolicyAction, Mode};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::text::Span;
use ratatui::widgets::{Block, Paragraph};
use time::{OffsetDateTime, UtcOffset};

use super::copy::{self as copy_ui, SelectAction, ids as copy_ids};
use super::text::soft_join;
use super::timefmt::{TimeFormat, Timestamps};
use super::transcript::{self, CopyTarget, Transcript, TranscriptAction, ViewSettings};
use crate::app::{AppCommand, View};
use crate::approval::{ApprovalAction, ApprovalOutcome, ApprovalQueue};
use crate::cards::{ApprovalNote, CardCopy};
use crate::indicators::{Activity, Spinner, TerminalTitle};
use crate::input::{INPUT_ID, Input, InputAction, InputOutcome, SlashCommand};
use crate::interaction::{Dispatch, ElementId, Interaction, PaneId};
use crate::keys::{Action, Keymap};
use crate::notify::{Level, Notifications, NotifyAction};
use crate::palette::{
    Invocation, Palette, PaletteAction, PaletteEntry, PaletteOutcome, builtin_entries, ids,
};
use crate::selection;
use crate::status::StatusLine;
use crate::theme::Theme;

/// Scroll pane id of the transcript.
pub const CHAT_PANE: &str = "chat";

/// Element id of the `↓ new messages` button shown while scrolled up.
pub const NEW_MESSAGES_ID: &str = "chat.new-messages";

/// Label of the `↓ new messages` button.
const NEW_MESSAGES_LABEL: &str = " ↓ new messages · end ";

/// Every clickable element of the chat screen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChatAction {
    /// A transcript element.
    Transcript(TranscriptAction),
    /// The input or its popups.
    Input(InputAction),
    /// A palette row.
    Palette(PaletteAction),
    /// An approval button.
    Approval(ApprovalAction),
    /// A toast or history element.
    Notify(NotifyAction),
    /// A selection-mode bar button.
    Select(SelectAction),
    /// The `↓ new messages` button: jump to the bottom and follow.
    Follow,
    /// The status line's MCP indicator: open `/mcp`.
    Mcp,
    /// The status line's background-jobs indicator: open `/jobs`.
    Jobs,
    /// The status line's `+N more` button: open the full status.
    StatusMore,
}

/// What the chat screen asks the app or core to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChatOutput {
    /// Send this message to the agent.
    Submit(String),
    /// Interrupt the running agent.
    Interrupt,
    /// Esc with nothing to back out of.
    Cancel,
    /// Quit.
    Quit,
    /// An app command (mouse, help, view).
    Command(AppCommand),
    /// A palette entry the chat does not handle itself (workflow, skill,
    /// agent, run, remote).
    Run(Invocation),
    /// Open a node in the map / node detail.
    OpenNode(NodeId),
    /// The session autonomy mode changed (from the palette).
    ModeChanged(Mode),
    /// Write this OSC sequence to set the terminal title.
    SetTitle(String),
    /// Revert this memory commit (the "updated memory" card's undo).
    UndoMemory(String),
    /// Copy `text` to the clipboard; `what` names it for the toast
    /// (`message`, `code block`, `tool output`, `diff`, `selection`).
    Copy {
        /// What is copied.
        what: String,
        /// The text.
        text: String,
    },
}

/// The built-in palette entries plus the copy actions.
fn chat_entries(keymap: &Keymap) -> Vec<PaletteEntry> {
    let mut v = builtin_entries(keymap);
    v.extend(copy_ui::palette_entries(keymap));
    v
}

/// Chat settings.
#[derive(Debug, Clone)]
pub struct ChatConfig {
    /// Colours.
    pub theme: Theme,
    /// Key bindings.
    pub keymap: Keymap,
    /// Transcript settings.
    pub settings: ViewSettings,
    /// `[ui] stall_after`.
    pub stall_after: Duration,
    /// `[ui] animations`.
    pub animations: bool,
    /// `[ui] terminal_title`.
    pub terminal_title: bool,
    /// `[general] mode`.
    pub mode: Mode,
}

impl Default for ChatConfig {
    fn default() -> Self {
        ChatConfig {
            theme: Theme::default(),
            keymap: Keymap::defaults(),
            settings: ViewSettings::default(),
            stall_after: Duration::from_secs(20),
            animations: true,
            terminal_title: false,
            mode: Mode::AutoEdit,
        }
    }
}

impl ChatConfig {
    /// Settings from `harness.ini` plus the already resolved theme and keys.
    pub fn from_config(
        cfg: &Config,
        theme: Theme,
        keymap: Keymap,
        offset: UtcOffset,
    ) -> ChatConfig {
        ChatConfig {
            theme,
            keymap,
            settings: ViewSettings::from_config(cfg, offset),
            stall_after: cfg.ui.stall_after,
            animations: cfg.ui.animations,
            terminal_title: cfg.ui.terminal_title,
            mode: cfg.general.mode.into(),
        }
    }
}

/// Node (and agent) name of a background cache pre-warm call
/// (`harness_agent::prewarm::PREWARM_NODE`, `[experimental] cache_prewarm`).
pub const PREWARM_NODE: &str = "cache-prewarm";

/// The composed chat screen.
#[derive(Debug)]
pub struct ChatView {
    cfg: ChatConfig,
    transcript: Transcript,
    activity: Activity,
    status: StatusLine,
    input: Input,
    palette: Palette,
    extra_entries: Vec<PaletteEntry>,
    approvals: ApprovalQueue,
    notify: Notifications,
    spinner: Spinner,
    /// Rows the activity line took this turn (see `render`).
    activity_rows: u16,
    title: TerminalTitle,
    interaction: Interaction<ChatAction>,
    mode: ModeState,
    width: u16,
    wall: OffsetDateTime,
    /// Focus when the palette opened (palette copy actions act on it).
    palette_focus: Option<ElementId>,
    /// The transcript area of the last frame.
    body: Rect,
    /// A screen (node detail, diff) is shown over the chat: "Copy focused"
    /// and "Copy diff" go to it.
    screen_open: bool,
    /// The chat is not shown (the map or a screen is): "Select text" is
    /// left to the app's selection mode.
    covered: bool,
    /// Toasts drawn over another view or screen ([`ChatView::render_toast_overlay`]).
    overlay_toasts: Vec<(Rect, NotifyAction)>,
    /// A press on a card's body: the card and what a click does, run on
    /// the release when no drag (a text selection) happened in between.
    body_press: Option<(ElementId, ChatAction)>,
    /// Background cache pre-warm nodes ([`PREWARM_NODE`]): shown in the map only,
    /// never as a chat turn (see [`ChatView::apply_run_event`]).
    prewarm_nodes: std::collections::HashSet<NodeId>,
}

impl ChatView {
    /// A chat screen. `status` carries cwd and branch.
    pub fn new(cfg: ChatConfig, mut status: StatusLine, now: Instant) -> ChatView {
        let mut interaction = Interaction::new();
        interaction.set_focus(INPUT_ID);
        status.set_mode(cfg.mode);
        let palette = Palette::new(chat_entries(&cfg.keymap));
        ChatView {
            transcript: Transcript::new(cfg.settings),
            activity: Activity::new(now),
            status,
            input: Input::new(),
            palette,
            extra_entries: Vec::new(),
            approvals: ApprovalQueue::new(),
            notify: Notifications::new(),
            spinner: Spinner::new(cfg.animations),
            activity_rows: 0,
            title: TerminalTitle::new(cfg.terminal_title),
            interaction,
            mode: ModeState::new(cfg.mode),
            width: 80,
            wall: OffsetDateTime::UNIX_EPOCH,
            palette_focus: None,
            body: Rect::default(),
            screen_open: false,
            covered: false,
            overlay_toasts: Vec::new(),
            body_press: None,
            prewarm_nodes: std::collections::HashSet::new(),
            cfg,
        }
    }

    /// The transcript.
    pub fn transcript(&self) -> &Transcript {
        &self.transcript
    }

    /// The transcript, for direct updates (tool output streaming, retries).
    pub fn transcript_mut(&mut self) -> &mut Transcript {
        &mut self.transcript
    }

    /// The activity tracker (for phases not in the event log: compacting,
    /// syncing, provider retries).
    pub fn activity_mut(&mut self) -> &mut Activity {
        &mut self.activity
    }

    /// The status line (model, branch, context window).
    pub fn status_mut(&mut self) -> &mut StatusLine {
        &mut self.status
    }

    /// The input editor.
    pub fn input(&self) -> &Input {
        &self.input
    }

    /// The input editor, for direct updates.
    pub fn input_mut(&mut self) -> &mut Input {
        &mut self.input
    }

    /// The palette.
    pub fn palette(&self) -> &Palette {
        &self.palette
    }

    /// The notifications.
    pub fn notifications_mut(&mut self) -> &mut Notifications {
        &mut self.notify
    }

    /// Pending approvals.
    pub fn approvals(&self) -> &ApprovalQueue {
        &self.approvals
    }

    /// The effective session mode.
    pub fn mode(&self) -> Mode {
        self.mode.session()
    }

    /// The focused element.
    pub fn focused(&self) -> Option<&ElementId> {
        self.interaction.focused()
    }

    /// Adds palette entries (catalog, runs, remotes) after the built-ins.
    pub fn set_palette_entries(&mut self, extra: Vec<PaletteEntry>) {
        self.extra_entries = extra;
        let mut all = chat_entries(&self.cfg.keymap);
        all.extend(self.extra_entries.iter().cloned());
        self.palette.set_entries(all);
    }

    /// Tells the chat whether a screen is shown over it; then the "Copy
    /// focused" and "Copy diff" palette entries are left to the app
    /// ([`ChatOutput::Run`]).
    pub fn set_screen_open(&mut self, open: bool) {
        self.screen_open = open;
    }

    /// Tells the chat whether it is hidden behind the map or a screen;
    /// then the "Select text" palette entry is left to the app
    /// ([`ChatOutput::Run`]), which selects in the view that is shown.
    pub fn set_covered(&mut self, covered: bool) {
        self.covered = covered;
    }

    /// Draws the toasts alone over `area` (when the map or a screen is
    /// shown), so copy results and other notifications stay visible.
    pub fn render_toast_overlay(&mut self, frame: &mut Frame<'_>, area: Rect) {
        let theme = self.cfg.theme.clone();
        let mut hits: crate::interaction::HitMap<NotifyAction> = crate::interaction::HitMap::new();
        self.notify
            .render_toasts(frame, area, &theme, &mut hits, &|a| a);
        self.overlay_toasts = hits.focusable().map(|r| (r.rect, r.action)).collect();
    }

    /// A click at `(x, y)` on a toast drawn by
    /// [`ChatView::render_toast_overlay`] dismisses it. Returns whether it
    /// hit one.
    pub fn click_overlay_toast(&mut self, x: u16, y: u16) -> bool {
        let p = ratatui::layout::Position::new(x, y);
        match self.overlay_toasts.iter().find(|(r, _)| r.contains(p)) {
            Some((_, a)) => {
                let a = *a;
                self.notify.activate(a);
                self.overlay_toasts.clear();
                true
            }
            None => false,
        }
    }

    /// Opens the palette asking for `entry`'s arguments.
    pub fn prompt_args(&mut self, entry: PaletteEntry) {
        self.notify.close_history();
        self.palette.prompt_entry(entry);
    }

    /// Sets the slash commands offered by input completion.
    pub fn set_slash_commands(&mut self, commands: Vec<SlashCommand>) {
        self.input.set_commands(commands);
    }

    /// Shows a notification.
    pub fn notify(&mut self, level: Level, title: &str, body: Option<String>, now: Instant) {
        self.notify.push(level, title, body, self.wall, now);
    }

    /// Folds a run event into every component.
    ///
    /// A cache pre-warm (`[experimental] cache_prewarm`) runs in the
    /// background while the user types: its node adds no transcript entry,
    /// does not move the activity line and raises no notification, so it
    /// neither looks like a turn nor ends one. Its usage and cost still
    /// reach the status line; the map shows the node.
    pub fn apply_run_event(&mut self, ev: &RunEvent, now: Instant) {
        self.wall = self.wall.max(ev.ts);
        if let Some(node) = &ev.node_id {
            if let harness_events::schema::EventKind::NodeStarted { name, agent, .. } = &ev.kind
                && (name == PREWARM_NODE || agent.as_deref() == Some(PREWARM_NODE))
            {
                self.prewarm_nodes.insert(node.clone());
            }
            if self.prewarm_nodes.contains(node) {
                if matches!(
                    ev.kind,
                    harness_events::schema::EventKind::ModelCallFinished { .. }
                        | harness_events::schema::EventKind::Cost { .. }
                ) {
                    self.status.apply(ev);
                }
                return;
            }
        }
        if let harness_events::schema::EventKind::ToolOutput { text, .. } = &ev.kind {
            self.activity.tool_output(text.lines().count().max(1), now);
        }
        self.transcript.apply(ev);
        self.activity.apply(ev, now);
        self.status.apply(ev);
        self.notify.push_event(ev, now);
    }

    /// Queues an approval prompt from `ChannelUi`. Its cards turn into
    /// approval cards until answered.
    pub fn push_approval(&mut self, env: PromptEnvelope) {
        for r in &env.prompt.requests {
            let rule = &r.verdict.rule;
            self.transcript.set_card_approval(
                &r.call_id,
                Some(ApprovalNote {
                    rule: if rule.rule.is_empty() {
                        rule.origin.clone()
                    } else {
                        rule.rule.clone()
                    },
                    reason: if r.verdict.outward {
                        "outward action".into()
                    } else {
                        rule.reason.clone()
                    },
                }),
            );
        }
        let action = env
            .prompt
            .requests
            .first()
            .map(|r| match &r.action {
                PolicyAction::Bash { command } => {
                    r.verdict.command.clone().unwrap_or_else(|| command.clone())
                }
                other => other.summary(),
            })
            .unwrap_or_default();
        self.activity.enter_approval(action, Instant::now());
        self.approvals.push_envelope(env);
    }

    fn approvals_answered(&mut self, out: &ApprovalOutcome, now: Instant) {
        if let ApprovalOutcome::Answered { .. } = out {
            // Clear approval tint on every card that is still waiting.
            let ids: Vec<String> = self
                .transcript
                .entries()
                .iter()
                .filter_map(|e| match &e.kind {
                    transcript::EntryKind::Tools { group, .. } => Some(group),
                    _ => None,
                })
                .flat_map(|g| g.cards.iter())
                .filter(|c| c.status == crate::cards::CardStatus::Approval)
                .map(|c| c.call_id.clone())
                .collect();
            let still: Vec<String> = self
                .approvals
                .current()
                .map(|p| {
                    p.prompt()
                        .requests
                        .iter()
                        .map(|r| r.call_id.clone())
                        .collect()
                })
                .unwrap_or_default();
            for id in ids.into_iter().filter(|i| !still.contains(i)) {
                self.transcript.set_card_approval(&id, None);
            }
            if self.approvals.is_empty() {
                // Resumes the phase the prompt interrupted, e.g. the tool
                // that was waiting for this approval.
                self.activity.approval_answered(now);
            }
        }
    }

    /// Whether the chat wants animation ticks. `visible` is whether the chat
    /// is on screen.
    pub fn needs_tick(&self, visible: bool) -> bool {
        let running = self.activity.is_active()
            || self
                .transcript
                .entries()
                .iter()
                .rev()
                .take(32)
                .any(|e| match &e.kind {
                    transcript::EntryKind::Tools { group, .. } => group
                        .cards
                        .iter()
                        .any(|c| c.status == crate::cards::CardStatus::Running),
                    transcript::EntryKind::Subagent(s) => !s.status.is_terminal(),
                    _ => false,
                });
        self.spinner.needs_tick(running, visible) || self.notify.next_expiry().is_some()
    }

    /// One animation tick: advances the spinner, expires toasts and updates
    /// the terminal title. Returns outputs (title changes).
    pub fn tick(&mut self, now: Instant) -> Vec<ChatOutput> {
        self.spinner.tick();
        self.notify.expire(now);
        self.title_output(now).into_iter().collect()
    }

    fn title_output(&mut self, now: Instant) -> Option<ChatOutput> {
        let text = self
            .activity
            .title(now, &self.spinner, self.cfg.stall_after);
        self.title.update(&text).map(ChatOutput::SetTitle)
    }

    fn frame_ctx(&self) -> (Theme, OffsetDateTime, Spinner, Option<String>) {
        (
            self.cfg.theme.clone(),
            self.wall,
            self.spinner,
            self.interaction.focused().map(|f| f.0.clone()),
        )
    }

    fn scroll(&mut self, lines: i32) {
        let (theme, now, spinner, focus) = self.frame_ctx();
        let f = transcript::Frame {
            theme: &theme,
            now,
            spinner,
            focus: focus.as_deref(),
        };
        self.transcript.scroll(lines, self.width as usize, &f);
    }

    /// Handles a terminal event. `wall` is the current wall-clock time
    /// (used for user messages, which are not in the event log yet).
    pub fn handle(&mut self, ev: &Event, now: Instant, wall: OffsetDateTime) -> Vec<ChatOutput> {
        self.wall = self.wall.max(wall);
        let mut out = Vec::new();
        match ev {
            Event::Key(k) => self.handle_key(k, now, &mut out),
            Event::Mouse(m) => self.handle_mouse(m, now, &mut out),
            Event::Paste(text) => {
                if self.palette.is_open() {
                    self.palette.paste(text);
                } else {
                    self.transcript.select_stop();
                    self.interaction.set_focus(INPUT_ID);
                    self.input.paste(text);
                }
            }
            _ => {}
        }
        if let Some(t) = self.title_output(now) {
            out.push(t);
        }
        out
    }

    fn handle_key(&mut self, k: &KeyEvent, now: Instant, out: &mut Vec<ChatOutput>) {
        if k.kind == KeyEventKind::Release {
            return;
        }
        let action = self.cfg.keymap.lookup(k);
        match action {
            Some(Action::Quit) => return out.push(ChatOutput::Quit),
            Some(Action::Interrupt) => return out.push(ChatOutput::Interrupt),
            Some(Action::Palette) => {
                if self.palette.is_open() {
                    self.palette.close();
                } else {
                    self.palette_focus = self.interaction.focused().cloned();
                    self.notify.close_history();
                    self.palette.open();
                }
                return;
            }
            _ => {}
        }
        if self.notify.is_history_open() {
            if !self.notify.handle_key(k)
                && let Some(d) = self.interaction.handle_key(k)
            {
                self.dispatch(d, now, out);
            }
            return;
        }
        if self.palette.is_open() {
            let o = self.palette.handle_key(k);
            self.palette_outcome(o, now, out);
            return;
        }
        if self.transcript.selection().is_some() {
            return self.selection_key(k, action, now, out);
        }
        match action {
            Some(Action::Copy) => {
                let focus = self.interaction.focused().cloned();
                return self.copy_focused(focus, now, out);
            }
            Some(Action::Select) => return self.start_selection(now),
            _ => {}
        }
        if !self.approvals.is_empty() {
            let o = self.approvals.handle_key(k);
            if o != ApprovalOutcome::Ignored {
                self.approvals_answered(&o, now);
                return;
            }
        }
        let input_focused = self.interaction.focused().is_none_or(|f| f.0 == INPUT_ID);
        if self.is_follow_key(k, input_focused) {
            self.transcript.follow();
            self.interaction.set_focus(INPUT_ID);
            return;
        }
        if !input_focused {
            if let Some(d) = self.interaction.handle_key(k) {
                return self.dispatch(d, now, out);
            }
            // Typing while a card is focused goes back to the input.
            self.interaction.set_focus(INPUT_ID);
        }
        match self.input.handle_key(k, action) {
            InputOutcome::Submit(text) => self.submit(text, now, out),
            InputOutcome::Changed => {}
            InputOutcome::Ignored => {
                if k.code == KeyCode::Esc || action == Some(Action::Cancel) {
                    return out.push(ChatOutput::Cancel);
                }
                // Like the app: the switch-view key (Tab) switches view from
                // the input; Shift+Tab and paging reach the focus layer.
                if action == Some(Action::SwitchView) {
                    return out.push(ChatOutput::Command(AppCommand::SwitchView(View::Map)));
                }
                if let Some(d) = self.interaction.handle_key(k)
                    && !matches!(d, Dispatch::Back | Dispatch::Activate(_))
                {
                    self.dispatch(d, now, out);
                }
            }
        }
    }

    /// Whether `k` jumps to the bottom: Ctrl+End always while scrolled up;
    /// plain End too unless the input would use it to move its cursor.
    fn is_follow_key(&self, k: &KeyEvent, input_focused: bool) -> bool {
        if self.transcript.is_following() || k.code != KeyCode::End {
            return false;
        }
        if k.modifiers == KeyModifiers::CONTROL {
            return true;
        }
        if !k.modifiers.is_empty() {
            return false;
        }
        let raw = self.input.raw();
        let at_line_end = raw
            .get(self.input.cursor()..)
            .is_some_and(|rest| rest.is_empty() || rest.starts_with('\n'));
        !input_focused || self.input.is_empty() || at_line_end
    }

    fn submit(&mut self, text: String, now: Instant, out: &mut Vec<ChatOutput>) {
        self.transcript.push_user(&text, self.wall);
        self.transcript.follow();
        // Slash commands may never reach the model; a turn they do start
        // (skills, workflows) enters an active phase through its events.
        if !text.trim_start().starts_with('/') {
            self.activity.start_turn(now);
        }
        out.push(ChatOutput::Submit(text));
    }

    fn handle_mouse(&mut self, m: &MouseEvent, now: Instant, out: &mut Vec<ChatOutput>) {
        let b = self.body;
        if self.transcript.selection().is_some()
            && !self.palette.is_open()
            && m.kind == MouseEventKind::Down(MouseButton::Left)
            && m.column >= b.x
            && m.column < b.right()
            && m.row >= b.y
            && m.row < b.bottom()
        {
            let (theme, wall, spinner, focus) = self.frame_ctx();
            let f = transcript::Frame {
                theme: &theme,
                now: wall,
                spinner,
                focus: focus.as_deref(),
            };
            self.transcript.select_row((m.row - b.y) as usize, &f);
            return;
        }
        // A click on the body of a card (below its header row, including
        // its expand hint) toggles or opens it on the release: the press
        // only focuses it, so a drag over its output selects text instead
        // (the app's selection layer keeps the release of a drag).
        if m.kind == MouseEventKind::Down(MouseButton::Left) {
            self.body_press = None;
        }
        if m.kind == MouseEventKind::Up(MouseButton::Left) {
            if let Some((id, action)) = self.body_press.take()
                && !self.palette.is_open()
                && self
                    .interaction
                    .hits()
                    .hit(m.column, m.row)
                    .is_some_and(|r| r.id == id)
            {
                self.activate(action, now, out);
            }
            return;
        }
        if m.kind == MouseEventKind::Down(MouseButton::Left)
            && !self.palette.is_open()
            && !self.notify.is_history_open()
            && let Some(r) = self.interaction.hits().hit(m.column, m.row)
            && r.rect.height > 1
            && m.row > r.rect.y
            && matches!(
                r.action,
                ChatAction::Transcript(
                    TranscriptAction::ToggleCard(_) | TranscriptAction::OpenNode(_)
                )
            )
        {
            let (id, action) = (r.id.clone(), r.action.clone());
            self.interaction.set_focus(id.clone());
            self.body_press = Some((id, action));
            return;
        }
        if let Some(d) = self.interaction.handle_mouse(m) {
            self.dispatch(d, now, out);
        }
    }

    fn dispatch(&mut self, d: Dispatch<ChatAction>, now: Instant, out: &mut Vec<ChatOutput>) {
        match d {
            Dispatch::Activate(a) => self.activate(a, now, out),
            Dispatch::Scroll { pane, lines } => {
                if pane == PaneId::from(CHAT_PANE) {
                    self.scroll(lines);
                } else if pane == PaneId::from(crate::notify::HISTORY_PANE) {
                    self.notify.scroll(lines);
                } else if pane == PaneId::from(crate::approval::APPROVAL_PANE) {
                    self.approvals.scroll(lines);
                }
            }
            Dispatch::Back => {
                if self.palette.is_open() {
                    let o = self.palette.back();
                    self.palette_outcome(o, now, out);
                } else if self.notify.is_history_open() {
                    self.notify.close_history();
                } else {
                    self.interaction.set_focus(INPUT_ID);
                }
            }
            Dispatch::Focus(_) => {}
        }
    }

    fn activate(&mut self, a: ChatAction, now: Instant, out: &mut Vec<ChatOutput>) {
        match a {
            ChatAction::Transcript(t) => match self.transcript.activate(&t) {
                Some(transcript::TranscriptOutput::OpenNode(node)) => {
                    out.push(ChatOutput::OpenNode(node))
                }
                Some(transcript::TranscriptOutput::UndoMemory(commit)) => {
                    out.push(ChatOutput::UndoMemory(commit))
                }
                Some(transcript::TranscriptOutput::Copy { what, text }) => {
                    out.push(ChatOutput::Copy { what, text })
                }
                None => {}
            },
            ChatAction::Input(i) => {
                self.input.activate(&i);
                self.interaction.set_focus(INPUT_ID);
            }
            ChatAction::Palette(p) => {
                let o = self.palette.activate(&p);
                self.palette_outcome(o, now, out);
            }
            ChatAction::Approval(b) => {
                let o = self.approvals.activate(b);
                self.approvals_answered(&o, now);
                self.interaction.set_focus(INPUT_ID);
            }
            ChatAction::Notify(n) => {
                self.notify.activate(n);
            }
            ChatAction::Follow => {
                self.transcript.follow();
                self.interaction.set_focus(INPUT_ID);
            }
            ChatAction::Mcp => {
                out.push(ChatOutput::Submit("/mcp".into()));
                self.interaction.set_focus(INPUT_ID);
            }
            ChatAction::Jobs => {
                out.push(ChatOutput::Submit("/jobs".into()));
                self.interaction.set_focus(INPUT_ID);
            }
            ChatAction::StatusMore => {
                out.push(ChatOutput::Command(AppCommand::ShowStatus));
                self.interaction.set_focus(INPUT_ID);
            }
            ChatAction::Select(a) => match a {
                SelectAction::Copy => self.copy_selection(now, out),
                SelectAction::Mark => self.transcript.select_mark(),
                SelectAction::Done => self.stop_selection(),
            },
        }
    }

    // ---- copy ----

    /// Emits the copy of `target`, or warns when there is nothing.
    fn emit_copy(&mut self, target: Option<CopyTarget>, now: Instant, out: &mut Vec<ChatOutput>) {
        match target.and_then(|t| self.transcript.copy_text(&t)) {
            Some((what, text)) => out.push(ChatOutput::Copy { what, text }),
            None => self.notify(Level::Warning, "nothing to copy", None, now),
        }
    }

    /// Copies what `focus` points at, else the last assistant message.
    fn copy_focused(&mut self, focus: Option<ElementId>, now: Instant, out: &mut Vec<ChatOutput>) {
        let target = focus
            .and_then(|f| self.transcript.target_for(&f.0))
            .or_else(|| self.transcript.last_message());
        self.emit_copy(target, now, out);
    }

    /// The tool call behind a focused card or card button.
    fn focused_call(focus: Option<&ElementId>) -> Option<String> {
        let id = &focus?.0;
        ["card.", "copy.out.", "copy.diff."]
            .iter()
            .find_map(|p| id.strip_prefix(p))
            .map(str::to_owned)
    }

    /// Runs a copy palette entry. Returns false for other ids.
    fn copy_palette(
        &mut self,
        id: &str,
        focus: Option<ElementId>,
        now: Instant,
        out: &mut Vec<ChatOutput>,
    ) -> bool {
        if self.screen_open && matches!(id, copy_ids::FOCUSED | copy_ids::DIFF) {
            return false;
        }
        if self.covered && id == copy_ids::SELECT {
            return false;
        }
        let call = Self::focused_call(focus.as_ref());
        let card = |kind| {
            call.clone()
                .map(|call_id| CopyTarget::Card { call_id, kind })
        };
        let target = match id {
            copy_ids::FOCUSED => {
                self.copy_focused(focus, now, out);
                return true;
            }
            copy_ids::SELECT => {
                self.start_selection(now);
                return true;
            }
            copy_ids::MESSAGE => self.transcript.last_message(),
            copy_ids::CODE => focus
                .and_then(|f| self.transcript.target_for(&f.0))
                .filter(|t| matches!(t, CopyTarget::Code { .. }))
                .or_else(|| self.transcript.last_code()),
            copy_ids::OUTPUT => {
                card(CardCopy::Output).or_else(|| self.transcript.last_card(CardCopy::Output))
            }
            copy_ids::OUTPUT_NUMBERED => call
                .clone()
                .or_else(|| match self.transcript.last_card(CardCopy::Output) {
                    Some(CopyTarget::Card { call_id, .. }) => Some(call_id),
                    _ => None,
                })
                .map(CopyTarget::NumberedOutput),
            copy_ids::DIFF => {
                card(CardCopy::Diff).or_else(|| self.transcript.last_card(CardCopy::Diff))
            }
            _ => return false,
        };
        self.emit_copy(target, now, out);
        true
    }

    fn start_selection(&mut self, now: Instant) {
        let (theme, wall, spinner, focus) = self.frame_ctx();
        let f = transcript::Frame {
            theme: &theme,
            now: wall,
            spinner,
            focus: focus.as_deref(),
        };
        if self.transcript.select_start(&f) {
            self.interaction.set_focus(INPUT_ID);
        } else {
            self.notify(Level::Warning, "nothing to select", None, now);
        }
    }

    fn stop_selection(&mut self) {
        self.transcript.select_stop();
        self.interaction.set_focus(INPUT_ID);
    }

    fn copy_selection(&mut self, now: Instant, out: &mut Vec<ChatOutput>) {
        let (theme, wall, spinner, focus) = self.frame_ctx();
        let f = transcript::Frame {
            theme: &theme,
            now: wall,
            spinner,
            focus: focus.as_deref(),
        };
        match self.transcript.selected_text(&f) {
            Some((text, _)) if !text.trim().is_empty() => out.push(ChatOutput::Copy {
                what: "selection".into(),
                text,
            }),
            _ => self.notify(Level::Warning, "nothing to copy", None, now),
        }
        self.stop_selection();
    }

    /// Keys in selection mode.
    fn selection_key(
        &mut self,
        k: &KeyEvent,
        action: Option<Action>,
        now: Instant,
        out: &mut Vec<ChatOutput>,
    ) {
        let (theme, wall, spinner, focus) = self.frame_ctx();
        let f = transcript::Frame {
            theme: &theme,
            now: wall,
            spinner,
            focus: focus.as_deref(),
        };
        let on_button = focus.as_deref().is_some_and(|id| id.starts_with("select."));
        let t = &mut self.transcript;
        match k.code {
            KeyCode::Tab | KeyCode::BackTab => {
                if let Some(d) = self.interaction.handle_key(k) {
                    self.dispatch(d, now, out);
                }
            }
            KeyCode::Enter if on_button => {
                if let Some(d) = self.interaction.handle_key(k) {
                    self.dispatch(d, now, out);
                }
            }
            KeyCode::Up | KeyCode::Down if k.modifiers.contains(KeyModifiers::SHIFT) => {
                // Shift extends: the mark stays where the cursor was.
                if t.selection().is_some_and(|s| s.mark.is_none()) {
                    t.select_mark();
                }
                t.select_move(if k.code == KeyCode::Up { -1 } else { 1 }, &f);
            }
            KeyCode::Up | KeyCode::Char('k') => t.select_move(-1, &f),
            KeyCode::Down | KeyCode::Char('j') => t.select_move(1, &f),
            KeyCode::PageUp => t.select_page(false, &f),
            KeyCode::PageDown => t.select_page(true, &f),
            KeyCode::Home | KeyCode::Char('g') => t.select_edge(false, &f),
            KeyCode::End | KeyCode::Char('G') => t.select_edge(true, &f),
            KeyCode::Char(' ') | KeyCode::Char('v') => t.select_mark(),
            KeyCode::Char('y') | KeyCode::Enter => self.copy_selection(now, out),
            KeyCode::Esc | KeyCode::Char('q') => self.stop_selection(),
            _ if action == Some(Action::Copy) => self.copy_selection(now, out),
            _ if matches!(action, Some(Action::Select | Action::Cancel)) => self.stop_selection(),
            _ => {}
        }
    }

    fn palette_outcome(&mut self, o: PaletteOutcome, now: Instant, out: &mut Vec<ChatOutput>) {
        let PaletteOutcome::Run(inv) = o else {
            return;
        };
        let focus = self.palette_focus.take();
        self.interaction.set_focus(INPUT_ID);
        if self.copy_palette(&inv.id, focus, now, out) {
            return;
        }
        match inv.id.as_str() {
            ids::TOGGLE_MOUSE => out.push(ChatOutput::Command(AppCommand::ToggleMouse)),
            ids::SHOW_KEYS => out.push(ChatOutput::Command(AppCommand::ShowHelp)),
            ids::SWITCH_VIEW => out.push(ChatOutput::Command(AppCommand::SwitchView(View::Map))),
            ids::QUIT => out.push(ChatOutput::Quit),
            ids::INTERRUPT => out.push(ChatOutput::Interrupt),
            ids::NOTIFICATIONS => self.notify.open_history(),
            ids::FOLLOW => self.transcript.follow(),
            ids::TIMESTAMPS => {
                let mut s = self.transcript.settings();
                s.time = TimeFormat {
                    mode: match s.time.mode {
                        Timestamps::Time => Timestamps::Relative,
                        Timestamps::Relative => Timestamps::Off,
                        Timestamps::Off => Timestamps::Time,
                    },
                    ..s.time
                };
                self.transcript.set_settings(s);
            }
            ids::SHOW_THINKING => {
                let mut s = self.transcript.settings();
                s.show_thinking = match s.show_thinking {
                    ShowThinking::Collapsed => ShowThinking::Expanded,
                    ShowThinking::Expanded => ShowThinking::Hidden,
                    ShowThinking::Hidden => ShowThinking::Collapsed,
                };
                self.transcript.set_settings(s);
            }
            id => match self.mode.apply_palette_command(id) {
                Some(m) => {
                    self.status.set_mode(m);
                    out.push(ChatOutput::ModeChanged(m));
                }
                None => out.push(ChatOutput::Run(inv)),
            },
        }
    }

    /// The full status line, one item per line (the `+N more` button and
    /// the "Show status" palette entry open it in a panel).
    pub fn status_details(&self) -> Vec<String> {
        self.status.details(&self.cfg.theme, &self.spinner)
    }

    /// Draws the chat screen into `area`.
    pub fn render(&mut self, frame: &mut Frame<'_>, area: Rect, now: Instant) {
        let theme = self.cfg.theme.clone();
        self.width = area.width;
        frame.render_widget(Block::new().style(theme.base()), area);
        let width = area.width;
        // The activity line and the status line wrap onto more rows rather
        // than cutting text; the status line takes at most a sixth of the
        // screen (up to 3 rows) and offers `+N more` for the rest.
        let activity_line = self.activity.is_active().then(|| {
            let hint = self.cfg.keymap.describe(Action::Interrupt);
            self.activity
                .line(now, &self.spinner, self.cfg.stall_after, &theme, &hint)
        });
        let activity_lines = activity_line.clone().map_or_else(Vec::new, |line| {
            crate::chat::text::wrap_line(line, width as usize, &[Span::raw("  ")])
        });
        // Timers change the line's length every frame. The height comes from
        // the line with every number at a fixed width, and only grows during
        // a turn, so the transcript above does not jump as time passes.
        self.activity_rows = if activity_lines.is_empty() {
            0
        } else {
            let probe = activity_line
                .as_ref()
                .map_or(0, |l| activity_probe_rows(l, width as usize));
            self.activity_rows
                .max(probe)
                .max(activity_lines.len() as u16)
        };
        let activity_h = self
            .activity_rows
            .min(area.height / 4)
            .max(u16::from(!activity_lines.is_empty()));
        let status_rows = (area.height / 6).clamp(1, 3);
        let status_layout = self.status.layout(
            &theme,
            &self.spinner,
            width as usize,
            usize::from(status_rows),
        );
        let status_h = status_layout.lines.len().max(1) as u16;
        let approval_h = self
            .approvals
            .current()
            .map_or(0, |p| p.height(width, &theme).min(area.height / 2));
        let selecting = self.transcript.selection().is_some();
        let input_h = if selecting {
            copy_ui::bar_height(width)
        } else {
            self.input.height(width)
        };
        let [body, activity, approvals, input, status] = Layout::vertical([
            Constraint::Min(1),
            Constraint::Length(activity_h),
            Constraint::Length(approval_h),
            Constraint::Length(input_h),
            Constraint::Length(status_h),
        ])
        .areas(area);

        let mut hits = self.interaction.begin_frame();
        let focus = self.interaction.focused().map(|f| f.0.clone());
        // Transcript.
        self.body = body;
        hits.register_pane(CHAT_PANE, body);
        let f = transcript::Frame {
            theme: &theme,
            now: self.wall,
            spinner: self.spinner,
            focus: focus.as_deref(),
        };
        let (lines, thits) = self.transcript.render(body, &f);
        if lines.is_empty() {
            // Bottom-anchored like the transcript: the hint sits above the
            // input, wrapped on narrow screens.
            let hint = crate::chat::text::wrap_line(
                ratatui::text::Line::from(
                    "Type a message and press Enter. Ctrl+P opens the command palette.",
                ),
                body.width as usize,
                &[],
            );
            let h = (hint.len() as u16).min(body.height);
            let row = Rect::new(body.x, body.bottom().saturating_sub(h), body.width, h);
            frame.render_widget(
                Paragraph::new(hint).style(theme.fg(crate::theme::Token::Muted)),
                row.intersection(body),
            );
        } else {
            let pad = self.transcript.pad().min(body.height);
            let r = Rect::new(body.x, body.y + pad, body.width, body.height - pad);
            // Text selection: content positions, soft wraps and chrome.
            let rows: Vec<selection::Row> = lines
                .iter()
                .zip(self.transcript.visible_ords())
                .map(|(l, &ord)| selection::Row {
                    ord,
                    join: soft_join(l),
                })
                .collect();
            selection::update(CHAT_PANE, |region| {
                region.rect = Rect {
                    height: rows.len() as u16,
                    ..r
                };
                region.rows = rows;
                region.scrolls = true;
                region.chrome = selection::Chrome::Smart;
            });
            frame.render_widget(Paragraph::new(lines), r);
        }
        for h in thits {
            let button = matches!(
                h.action,
                TranscriptAction::Copy(_) | TranscriptAction::UndoMemory(_)
            );
            if button {
                hits.register_button(h.id, h.rect, ChatAction::Transcript(h.action));
            } else {
                hits.register(h.id, h.rect, ChatAction::Transcript(h.action));
            }
        }
        // New output below a scrolled-up view.
        if self.transcript.has_unseen() && !self.transcript.is_following() && body.height > 0 {
            let label = NEW_MESSAGES_LABEL;
            let w = (unicode_width::UnicodeWidthStr::width(label) as u16).min(body.width);
            let r = Rect::new(
                body.right().saturating_sub(w + 1).max(body.x),
                body.bottom() - 1,
                w,
                1,
            );
            let focused = focus.as_deref() == Some(NEW_MESSAGES_ID);
            let style = if focused {
                theme.focused().patch(theme.fg(crate::theme::Token::Accent))
            } else {
                theme.fg_on(crate::theme::Token::Accent, crate::theme::Token::Surface)
            };
            frame.render_widget(Paragraph::new(label).style(style), r);
            hits.register_button(NEW_MESSAGES_ID, r, ChatAction::Follow);
        }
        // Activity line.
        if activity_h > 0 {
            frame.render_widget(Paragraph::new(activity_lines), activity);
        }
        // Approvals.
        if let Some(p) = self.approvals.current() {
            p.render(frame, approvals, &theme, &mut hits, &ChatAction::Approval);
        }
        // Slash completion over the bottom of the transcript.
        let ch = self.input.completion_height_at(body.width).min(body.height);
        if ch > 0 {
            let r = Rect::new(body.x, body.bottom() - ch, body.width, ch);
            self.input
                .render_completion(frame, r, &theme, &mut hits, &ChatAction::Input);
        }
        // Input, or the selection bar.
        let modal = self.palette.is_open() || self.notify.is_history_open();
        let input_focused = !modal && focus.as_deref().is_none_or(|f| f == INPUT_ID);
        if selecting {
            let lines = self.transcript.selected_text(&f).map_or(0, |(_, n)| n);
            let marked = self
                .transcript
                .selection()
                .is_some_and(|s| s.mark.is_some());
            copy_ui::render_bar(
                frame,
                input,
                &theme,
                lines,
                marked,
                &mut hits,
                &ChatAction::Select,
            );
        } else {
            self.input.render(
                frame,
                input,
                &theme,
                input_focused,
                &mut hits,
                &ChatAction::Input,
            );
        }
        // Status, with the MCP and background-jobs indicators and the
        // `+N more` button as buttons when they show.
        let mut status_lines = status_layout.lines;
        for b in &status_layout.buttons {
            let action = match b.id {
                crate::status::MCP_INDICATOR_ID => ChatAction::Mcp,
                crate::status::JOBS_INDICATOR_ID => ChatAction::Jobs,
                _ => ChatAction::StatusMore,
            };
            if focus.as_deref() == Some(b.id)
                && let Some(line) = status_lines.get_mut(usize::from(b.row))
            {
                // Highlight the button's spans on this row.
                let mut x = 0u16;
                for span in &mut line.spans {
                    let w = unicode_width::UnicodeWidthStr::width(span.content.as_ref()) as u16;
                    if x >= b.col && x + w <= b.col + b.width && w > 0 {
                        span.style = span.style.patch(theme.focused());
                    }
                    x += w;
                }
            }
            let r = Rect::new(status.x + b.col, status.y + b.row, b.width, 1).intersection(status);
            if !r.is_empty() {
                hits.register(b.id, r, action);
            }
        }
        frame.render_widget(Paragraph::new(status_lines), status);
        // Overlays.
        hits.push_layer();
        self.notify
            .render_toasts(frame, body, &theme, &mut hits, &ChatAction::Notify);
        let time = self.transcript.settings().time;
        let wall = self.wall;
        self.notify.render_history(
            frame,
            area,
            &theme,
            &|ts| time.format(ts, wall),
            &mut hits,
            &ChatAction::Notify,
        );
        self.palette
            .render(frame, area, &theme, &mut hits, &ChatAction::Palette);
        if self.interaction.end_frame(hits) && self.interaction.focused().is_none() {
            self.interaction.set_focus(INPUT_ID);
        }
    }
}

/// Rows the activity line needs with every run of digits four cells wide
/// (`12.3s`, `2m05s`, `12.4k` keep their width), so ticking timers do not
/// change its height.
fn activity_probe_rows(line: &ratatui::text::Line<'static>, width: usize) -> u16 {
    let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
    let mut probe = String::new();
    let mut in_digits = false;
    for c in text.chars() {
        if c.is_ascii_digit() || (in_digits && c == '.') {
            if !in_digits {
                probe.push_str("0000");
            }
            in_digits = true;
        } else {
            in_digits = false;
            probe.push(c);
        }
    }
    crate::chat::text::wrap_line(ratatui::text::Line::from(probe), width, &[Span::raw("  ")]).len()
        as u16
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interaction::testing::{buffer_text, click, find_text, key, type_text};
    use harness_events::schema::{EventKind, NodeKind, Payload, RunId};
    use harness_policy::approvals::{ApprovalPrompt, ApprovalRequest, ApprovalUi, ChannelUi};
    use harness_policy::rules::{Decision, Layer, MatchedRule, Verdict};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use serde_json::json;
    use std::path::Path;
    use time::macros::datetime;

    const T0: OffsetDateTime = datetime!(2026-10-22 14:32:00 UTC);

    struct Ui {
        view: ChatView,
        term: Terminal<TestBackend>,
        now: Instant,
        out: Vec<ChatOutput>,
    }

    impl Ui {
        fn new() -> Ui {
            Ui::with(ChatConfig::default())
        }

        fn with(cfg: ChatConfig) -> Ui {
            let now = Instant::now();
            let mut status = StatusLine::new(
                Path::new("/home/u/CODE/harness"),
                Some(Path::new("/home/u")),
                cfg.mode,
            );
            status.set_branch(Some("main".into()));
            status.set_model("sonnet", Some(200_000));
            let mut ui = Ui {
                view: ChatView::new(cfg, status, now),
                term: Terminal::new(TestBackend::new(100, 30)).unwrap(),
                now,
                out: Vec::new(),
            };
            ui.draw();
            ui
        }

        fn sized(w: u16, h: u16) -> Ui {
            let mut ui = Ui::new();
            ui.term = Terminal::new(TestBackend::new(w, h)).unwrap();
            ui.draw();
            ui
        }

        fn draw(&mut self) {
            let now = self.now;
            let view = &mut self.view;
            self.term.draw(|f| view.render(f, f.area(), now)).unwrap();
        }

        fn send(&mut self, events: Vec<Event>) {
            for e in events {
                let o = self.view.handle(&e, self.now, T0);
                self.out.extend(o);
                self.draw();
            }
        }

        fn text(&self) -> String {
            buffer_text(self.term.backend().buffer())
        }

        fn find(&self, s: &str) -> (u16, u16) {
            find_text(self.term.backend().buffer(), s)
                .unwrap_or_else(|| panic!("{s:?} not on screen:\n{}", self.text()))
        }

        fn click_text(&mut self, s: &str) {
            let (x, y) = self.find(s);
            self.send(vec![click(x, y)]);
        }

        fn event(&mut self, seq: u64, node: Option<&str>, kind: EventKind) {
            let ev = RunEvent {
                seq,
                ts: T0 + time::Duration::seconds(seq as i64),
                run_id: RunId(uuid::Uuid::nil()),
                node_id: node.map(NodeId::from),
                parent_node_id: None,
                kind,
            };
            self.view.apply_run_event(&ev, self.now);
            self.draw();
        }

        fn take(&mut self) -> Vec<ChatOutput> {
            std::mem::take(&mut self.out)
        }
    }

    #[test]
    fn ctrl_p_opens_palette_filters_and_runs_by_keyboard_and_mouse() {
        let mut ui = Ui::new();
        ui.send(vec![key("ctrl+p")]);
        assert!(ui.view.palette().is_open());
        assert!(ui.text().contains("Command palette"));
        ui.send(type_text("mouse"));
        ui.send(vec![key("enter")]);
        assert_eq!(
            ui.take(),
            vec![ChatOutput::Command(AppCommand::ToggleMouse)]
        );
        assert!(!ui.view.palette().is_open());
        // Mouse: open, click a row.
        ui.send(vec![key("ctrl+p")]);
        ui.click_text("Show keys");
        assert_eq!(ui.take(), vec![ChatOutput::Command(AppCommand::ShowHelp)]);
        // Esc closes; a click outside closes too.
        ui.send(vec![key("ctrl+p"), key("esc")]);
        assert!(!ui.view.palette().is_open());
        ui.send(vec![key("ctrl+p")]);
        ui.send(vec![click(0, 29)]);
        assert!(!ui.view.palette().is_open());
        // Catalog entries are run by the core, with prompted arguments.
        ui.view.set_palette_entries(vec![
            PaletteEntry::new(
                "workflow.review",
                "/review",
                "Review",
                crate::palette::EntryKind::Workflow,
            )
            .with_args(vec![crate::palette::ArgSpec::required("branch")]),
        ]);
        ui.send(vec![key("ctrl+p")]);
        ui.send(type_text("/review"));
        ui.send(vec![key("enter")]);
        assert!(ui.text().contains("/review › branch:"));
        ui.send(type_text("main"));
        ui.send(vec![key("enter")]);
        assert_eq!(
            ui.take(),
            vec![ChatOutput::Run(Invocation {
                id: "workflow.review".into(),
                args: vec!["main".into()]
            })]
        );
    }

    #[test]
    fn palette_mode_changes_update_the_footer() {
        let mut ui = Ui::new();
        assert!(ui.text().contains("sonnet · auto-edit"));
        ui.send(vec![key("ctrl+p")]);
        ui.send(type_text("mode auto act"));
        ui.send(vec![key("enter")]);
        assert_eq!(ui.take(), vec![ChatOutput::ModeChanged(Mode::Auto)]);
        assert!(ui.text().contains("sonnet · auto "), "{}", ui.text());
        assert_eq!(ui.view.mode(), Mode::Auto);
    }

    #[test]
    fn notification_history_from_palette_and_toasts_keep_focus() {
        let mut ui = Ui::new();
        let now = ui.now;
        ui.view.notify(Level::Warning, "Budget 80% used", None, now);
        ui.draw();
        assert!(ui.text().contains("⚠ Budget 80% used"));
        assert_eq!(ui.view.focused(), Some(&ElementId::from(INPUT_ID)));
        ui.send(type_text("hi"));
        assert_eq!(
            ui.view.input_mut().text(),
            "hi",
            "typing still goes to the input"
        );
        ui.view
            .notifications_mut()
            .expire(now + crate::notify::TOAST_TTL);
        ui.draw();
        assert!(!ui.text().contains("Budget 80% used"));
        ui.send(vec![key("ctrl+p")]);
        ui.send(type_text("notifications"));
        ui.send(vec![key("enter")]);
        assert!(
            ui.text().contains("Budget 80% used"),
            "history shows expired toasts"
        );
        ui.send(vec![key("esc")]);
        assert!(!ui.text().contains("Budget 80% used"));
    }

    fn request(n: usize) -> ApprovalRequest {
        ApprovalRequest {
            approval_id: format!("approval-1-{n}"),
            call_id: format!("call-{n}"),
            node_id: None,
            action: PolicyAction::Bash {
                command: "git push origin main".into(),
            },
            verdict: Verdict {
                decision: Decision::Ask,
                rule: MatchedRule {
                    layer: Layer::BuiltIn,
                    origin: "outward guard".into(),
                    rule: "git push*".into(),
                    reason: "outward action".into(),
                },
                command: Some("git push origin main".into()),
                outward: true,
            },
            preview: None,
            suggested_pattern: Some("git push *".into()),
        }
    }

    #[tokio::test]
    async fn approval_appears_inline_above_input_and_answers_by_key_or_click() {
        let mut ui = Ui::new();
        ui.event(
            1,
            None,
            EventKind::ToolCall {
                call_id: "call-0".into(),
                tool: "bash".into(),
                args: Payload::Json(json!({"command": "git push origin main"})),
            },
        );
        let (chan, mut rx) = ChannelUi::new(2);
        let c = chan.clone();
        let task = tokio::spawn(async move {
            c.ask(ApprovalPrompt {
                batch: 1,
                requests: vec![request(0)],
            })
            .await
        });
        ui.view.push_approval(rx.recv().await.unwrap());
        ui.draw();
        let (_, btn_y) = ui.find("[y] once");
        let (_, input_y) = ui.find("› ");
        assert!(btn_y < input_y, "approval sits above the input");
        assert!(
            ui.text()
                .contains("⏸ waiting for approval: git push origin main")
        );
        assert!(
            ui.text().contains("╭ ⏸ bash"),
            "the card shows the approval state"
        );
        ui.send(vec![key("y")]);
        assert!(!ui.text().contains("[y] once"));
        assert_eq!(
            task.await.unwrap()[0].1,
            harness_policy::approvals::ApprovalAnswer::Approve(
                harness_policy::approvals::ApprovalScope::Once
            )
        );
        // Mouse.
        let c = chan.clone();
        let task = tokio::spawn(async move {
            c.ask(ApprovalPrompt {
                batch: 2,
                requests: vec![request(0)],
            })
            .await
        });
        ui.view.push_approval(rx.recv().await.unwrap());
        ui.draw();
        ui.click_text("[n] deny");
        assert!(matches!(
            task.await.unwrap()[0].1,
            harness_policy::approvals::ApprovalAnswer::Deny { .. }
        ));
    }

    /// Regression: a bash call approved from the prompt resumes as the
    /// running tool, so its progress shows elapsed/timeout and CPU instead
    /// of the model-silence warning.
    #[tokio::test]
    async fn approved_tool_resumes_running_with_progress_not_model_silence() {
        let mut ui = Ui::new();
        start_main(&mut ui);
        ui.event(
            2,
            Some("main"),
            EventKind::ToolCall {
                call_id: "call-0".into(),
                tool: "bash".into(),
                args: Payload::Json(json!({"command": "git push origin main"})),
            },
        );
        let (chan, mut rx) = ChannelUi::new(2);
        let c = chan.clone();
        let task = tokio::spawn(async move {
            c.ask(ApprovalPrompt {
                batch: 1,
                requests: vec![request(0)],
            })
            .await
        });
        // The UI's prompt arrives before the approval_requested event.
        ui.view.push_approval(rx.recv().await.unwrap());
        ui.event(
            3,
            Some("main"),
            EventKind::ApprovalRequested {
                approval_id: "approval-1-0".into(),
                action: "git push origin main".into(),
                preview: None,
            },
        );
        ui.now += Duration::from_secs(1);
        ui.send(vec![key("y")]);
        assert!(matches!(
            task.await.unwrap()[0].1,
            harness_policy::approvals::ApprovalAnswer::Approve(_)
        ));
        ui.event(
            4,
            Some("main"),
            EventKind::ApprovalResolved {
                approval_id: "approval-1-0".into(),
                decision: harness_events::schema::ApprovalDecision::Approved,
                by: "user".into(),
            },
        );
        assert!(matches!(
            ui.view.activity.phase(),
            crate::indicators::Phase::RunningTool { .. }
        ));
        let start = ui.now;
        for i in 0..=8u64 {
            ui.now = start + Duration::from_secs(i * 3);
            ui.event(
                5 + i,
                Some("main"),
                EventKind::ToolProgress {
                    call_id: "call-0".into(),
                    timeout_ms: (i == 0).then_some(600_000),
                    liveness: (i > 0).then_some(harness_events::schema::ProcessLiveness {
                        cpu_ms: i * 2990,
                        procs: 1,
                        active: 1,
                    }),
                },
            );
        }
        ui.now = start + Duration::from_secs(25);
        ui.draw();
        let t = ui.text();
        assert!(
            t.contains("running bash: git push origin main 25.0s of 10m · cpu 23.9s (1 process)"),
            "{t}"
        );
        assert!(
            !t.contains("waiting on the model") && !t.contains('⚠'),
            "{t}"
        );
    }

    fn start_main(ui: &mut Ui) {
        ui.event(
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
        );
    }

    /// The events of one background cache pre-warm (`cache-prewarm` node
    /// under parent `p`, as the recorder writes them), from `seq` on.
    fn prewarm_events(seq: u64) -> Vec<RunEvent> {
        use harness_events::schema::Usage as EventUsage;
        let kinds = vec![
            EventKind::NodeStarted {
                kind: NodeKind::Agent,
                name: PREWARM_NODE.into(),
                agent: Some(PREWARM_NODE.into()),
                model: Some("sonnet".into()),
                host: None,
                needs: vec![],
                queued: false,
            },
            EventKind::ModelCallStarted {
                call_id: "call_warm".into(),
                provider: "anthropic".into(),
                model: "sonnet".into(),
                request: Payload::Json(json!({"purpose": "cache-prewarm"})),
            },
            EventKind::ModelCallFinished {
                call_id: "call_warm".into(),
                usage: EventUsage {
                    input: 1_900,
                    cache_write: 1_900,
                    ..EventUsage::default()
                },
                stop_reason: Some("max_tokens".into()),
                response: None,
                duration_ms: 400,
            },
            EventKind::Cost { usd: 0.25 },
            EventKind::NodeFinished {
                status: harness_events::schema::NodeStatus::Succeeded,
                error: None,
            },
        ];
        kinds
            .into_iter()
            .zip(seq..)
            .map(|(kind, seq)| RunEvent {
                seq,
                ts: T0 + time::Duration::seconds(seq as i64),
                run_id: RunId(uuid::Uuid::nil()),
                node_id: Some(NodeId::from("warm")),
                parent_node_id: Some(NodeId::from("p")),
                kind,
            })
            .collect()
    }

    fn apply_all(ui: &mut Ui, events: &[RunEvent]) {
        for e in events {
            ui.view.apply_run_event(e, ui.now);
            ui.draw();
        }
    }

    #[test]
    fn the_prewarm_node_name_matches_the_agent_crate() {
        assert_eq!(PREWARM_NODE, harness_agent::prewarm::PREWARM_NODE);
    }

    #[test]
    fn a_session_start_prewarm_is_not_a_turn() {
        let mut ui = Ui::new();
        let events = prewarm_events(1);
        // Mid-call: no assistant turn, no activity.
        apply_all(&mut ui, &events[..2]);
        assert!(ui.view.transcript().entries().is_empty());
        assert_eq!(ui.view.activity.phase(), &crate::indicators::Phase::Idle);
        assert!(!ui.text().contains("waiting"), "{}", ui.text());
        apply_all(&mut ui, &events[2..]);
        assert!(ui.view.transcript().entries().is_empty());
        assert_eq!(ui.view.activity.phase(), &crate::indicators::Phase::Idle);
        assert_eq!(ui.view.activity.turn(), 0);
        let t = ui.text();
        assert!(
            !t.contains("assistant") && !t.contains("ctrl+c to interrupt"),
            "{t}"
        );
        assert!(ui.view.notify.history().next().is_none());
        // Its spend is real: the status line counts it.
        assert!(t.contains("$0.25"), "{t}");
    }

    #[test]
    fn a_prewarm_finishing_during_a_turn_does_not_end_it() {
        let mut ui = Ui::new();
        let events = prewarm_events(10);
        apply_all(&mut ui, &events[..2]);
        // The user submits while the pre-warm is in flight.
        ui.send(type_text("fix the parser"));
        ui.send(vec![key("enter")]);
        assert!(ui.view.activity.is_active());
        apply_all(&mut ui, &events[2..]);
        assert!(ui.view.activity.is_active(), "the turn is still sending");
        // The real turn then runs as usual and is the only assistant entry.
        start_main(&mut ui);
        ui.event(
            20,
            Some("main"),
            EventKind::ModelCallStarted {
                call_id: "m".into(),
                provider: "fake".into(),
                model: "sonnet".into(),
                request: Payload::text(""),
            },
        );
        assert!(
            ui.text().contains("waiting for first token"),
            "{}",
            ui.text()
        );
        let assistants = ui
            .view
            .transcript()
            .entries()
            .iter()
            .filter(|e| matches!(e.kind, transcript::EntryKind::Assistant(_)))
            .count();
        assert_eq!(assistants, 1);
        // A later pre-warm (after /compact) leaves the main agent alone too.
        apply_all(&mut ui, &prewarm_events(30));
        assert!(
            ui.text().contains("waiting for first token"),
            "{}",
            ui.text()
        );
    }

    #[test]
    fn slash_command_does_not_start_a_turn() {
        let mut ui = Ui::new();
        ui.send(type_text("/model"));
        ui.send(vec![key("enter")]);
        assert_eq!(ui.take(), vec![ChatOutput::Submit("/model".into())]);
        assert!(!ui.view.activity.is_active(), "no request was sent");
        assert_eq!(ui.view.activity.turn(), 0);
        assert!(!ui.text().contains("ctrl+c to interrupt"));
    }

    #[test]
    fn submit_shows_user_message_and_activity_line_above_input() {
        let mut ui = Ui::new();
        ui.send(type_text("fix the parser"));
        ui.send(vec![key("enter")]);
        assert_eq!(ui.take(), vec![ChatOutput::Submit("fix the parser".into())]);
        assert!(ui.text().contains("▌ fix the parser"));
        start_main(&mut ui);
        ui.event(
            2,
            Some("main"),
            EventKind::ModelCallStarted {
                call_id: "m".into(),
                provider: "fake".into(),
                model: "sonnet".into(),
                request: Payload::text(""),
            },
        );
        let (_, act_y) = ui.find("waiting for first token");
        let (_, input_y) = ui.find("› ");
        assert_eq!(act_y + 1, input_y);
        assert!(ui.text().contains("ctrl+c to interrupt"));
        assert!(ui.view.needs_tick(true));
        assert!(!ui.view.needs_tick(false), "no ticks while not visible");
        ui.event(
            3,
            Some("main"),
            EventKind::NodeFinished {
                status: harness_events::schema::NodeStatus::Succeeded,
                error: None,
            },
        );
        assert!(!ui.text().contains("to interrupt"));
        assert!(!ui.view.needs_tick(true), "idle: no ticks");
        // Static icons without animations.
        let mut ui = Ui::with(ChatConfig {
            animations: false,
            ..ChatConfig::default()
        });
        ui.send(type_text("x"));
        ui.send(vec![key("enter")]);
        assert!(ui.text().contains("◐ sending"));
        assert!(!ui.view.needs_tick(true));
    }

    #[test]
    fn cards_toggle_by_click_and_by_keyboard() {
        let mut ui = Ui::with(ChatConfig {
            settings: ViewSettings {
                preview_lines: 2,
                ..ViewSettings::default()
            },
            ..ChatConfig::default()
        });
        ui.event(
            1,
            None,
            EventKind::ToolCall {
                call_id: "b".into(),
                tool: "bash".into(),
                args: Payload::Json(json!({"command": "seq 6"})),
            },
        );
        ui.event(
            2,
            None,
            EventKind::ToolResult {
                call_id: "b".into(),
                ok: true,
                output: Payload::text("1\n2\n3\n4\n5\n6"),
                duration_ms: 3,
            },
        );
        assert!(ui.text().contains("… 4 more lines"));
        ui.click_text("$ seq 6");
        assert!(ui.text().contains("Enter to collapse"));
        // Keyboard: Shift+Tab from the input reaches the card's copy button,
        // then the card; Enter toggles, Esc returns.
        ui.send(vec![key("esc")]);
        ui.view.interaction.set_focus(INPUT_ID);
        ui.send(vec![key("shift+tab")]);
        assert_eq!(ui.view.focused(), Some(&ElementId::from("copy.out.b")));
        ui.send(vec![key("shift+tab")]);
        assert_eq!(ui.view.focused(), Some(&ElementId::from("card.b")));
        ui.send(vec![key("enter")]);
        assert!(ui.text().contains("… 4 more lines"));
        ui.send(vec![key("esc")]);
        assert_eq!(ui.view.focused(), Some(&ElementId::from(INPUT_ID)));
    }

    #[test]
    fn mcp_cards_show_server_tool_duration_and_result_and_toggle_by_click_and_keyboard() {
        let mut ui = Ui::with(ChatConfig {
            settings: ViewSettings {
                preview_lines: 2,
                ..ViewSettings::default()
            },
            ..ChatConfig::default()
        });
        ui.event(
            1,
            None,
            EventKind::ToolCall {
                call_id: "m".into(),
                tool: "mcp_tools".into(),
                args: Payload::Json(
                    json!({"call": "github/create_issue", "arguments": {"title": "Crash on start"}}),
                ),
            },
        );
        ui.event(
            2,
            None,
            EventKind::McpCall {
                call_id: "m".into(),
                server: "github".into(),
                tool: "create_issue".into(),
            },
        );
        ui.event(
            3,
            None,
            EventKind::ToolResult {
                call_id: "m".into(),
                ok: true,
                output: Payload::text(
                    "<tool_result tool=\"mcp_tools\" call_id=\"m\" trust=\"untrusted\" origin=\"mcp server\">\nissue #1\nurl a\nurl b\nurl c\n</tool_result>",
                ),
                duration_ms: 1234,
            },
        );
        let text = ui.text();
        assert!(
            text.contains("╭ ✔ mcp github › create_issue  title=Crash on start"),
            "{text}"
        );
        assert!(text.contains("1.2s"), "{text}");
        // The body is the server's output, without the provenance marker.
        assert!(text.contains("issue #1"), "{text}");
        assert!(!text.contains("<tool_result"), "{text}");
        assert!(text.contains("… 2 more lines"), "{text}");
        ui.click_text("mcp github › create_issue");
        assert!(ui.text().contains("Enter to collapse"));
        ui.send(vec![key("esc")]);
        ui.view.interaction.set_focus(INPUT_ID);
        ui.send(vec![key("shift+tab")]);
        assert_eq!(ui.view.focused(), Some(&ElementId::from("copy.out.m")));
        ui.send(vec![key("shift+tab")]);
        assert_eq!(ui.view.focused(), Some(&ElementId::from("card.m")));
        ui.send(vec![key("enter")]);
        assert!(ui.text().contains("… 2 more lines"));
        ui.send(vec![key("esc")]);
        assert_eq!(ui.view.focused(), Some(&ElementId::from(INPUT_ID)));
        // A tool-list change shows as a notice.
        ui.event(
            4,
            None,
            EventKind::McpToolsChanged {
                server: "github".into(),
                added: vec!["create_pr".into()],
                removed: vec![],
                tools: 5,
            },
        );
        assert!(
            ui.text()
                .contains("MCP server github changed its tools (5 selected available)"),
            "{}",
            ui.text()
        );
    }

    #[test]
    fn global_keys_quit_interrupt_switch_view_and_cancel() {
        let mut ui = Ui::new();
        ui.send(vec![key("tab"), key("ctrl+c"), key("esc"), key("ctrl+q")]);
        assert_eq!(
            ui.take(),
            vec![
                ChatOutput::Command(AppCommand::SwitchView(View::Map)),
                ChatOutput::Interrupt,
                ChatOutput::Cancel,
                ChatOutput::Quit,
            ]
        );
    }

    // ---- bottom-anchored transcript (TASK-143) ----

    impl Ui {
        fn rows(&self) -> Vec<String> {
            self.text().lines().map(str::to_owned).collect()
        }

        /// The input row.
        fn input_row(&self) -> usize {
            self.find("› ").1 as usize
        }

        /// The transcript rows (everything above the input).
        fn body_rows(&self) -> Vec<String> {
            let r = self.rows();
            r[..self.input_row()].to_vec()
        }

        fn push(&mut self, text: &str) {
            self.view.transcript_mut().push_user(text, T0);
            self.draw();
        }

        fn fill(&mut self, n: usize) {
            for i in 0..n {
                self.view
                    .transcript_mut()
                    .push_user(&format!("message {i}"), T0);
            }
            self.draw();
        }

        fn resize(&mut self, w: u16, h: u16) {
            self.term.backend_mut().resize(w, h);
            self.draw();
        }

        fn snapshot(&self) -> String {
            crate::interaction::testing::styled_snapshot(self.term.backend().buffer())
        }
    }

    /// Rows between the last non-blank transcript row and the input.
    fn gap_above_input(ui: &Ui) -> usize {
        let body = ui.body_rows();
        let last = body
            .iter()
            .rposition(|r| !r.trim().is_empty())
            .expect("transcript has content");
        body.len() - 1 - last
    }

    #[test]
    fn short_transcript_sits_directly_above_the_input() {
        let mut ui = Ui::sized(60, 14);
        ui.push("hello");
        ui.push("second");
        let body = ui.body_rows();
        // The newest message is right above the input (one separator row)…
        assert_eq!(gap_above_input(&ui), 1, "{}", ui.text());
        assert!(body[body.len() - 2].contains("second"), "{}", ui.text());
        // …and the empty space is above the conversation.
        let first = body.iter().position(|r| r.contains("hello")).unwrap();
        assert!(first >= 5, "{}", ui.text());
        assert!(body[..first].iter().all(|r| r.trim().is_empty()));
        assert!(ui.view.transcript().is_following());
        insta::assert_snapshot!("chat_short", ui.snapshot());
    }

    #[test]
    fn empty_chat_hint_sits_above_the_input() {
        let ui = Ui::sized(80, 10);
        let body = ui.body_rows();
        assert!(
            body.last()
                .unwrap()
                .contains("Type a message and press Enter")
        );
        assert!(body[..body.len() - 1].iter().all(|r| r.trim().is_empty()));
    }

    #[test]
    fn short_transcript_copy_buttons_are_clickable_where_drawn() {
        // Hits move down with the padding: clicking a drawn button works.
        let mut ui = Ui::sized(60, 14);
        ui.push("hello");
        ui.click_text("⧉ copy");
        assert_eq!(copies(ui.take()), vec![("message".into(), "hello".into())]);
    }

    #[test]
    fn new_output_appears_at_the_bottom_and_is_followed() {
        let mut ui = Ui::sized(60, 14);
        ui.fill(40);
        assert!(ui.body_rows()[ui.input_row() - 2].contains("message 39"));
        ui.push("newest");
        let body = ui.body_rows();
        assert!(body[body.len() - 2].contains("newest"), "{}", ui.text());
        assert_eq!(gap_above_input(&ui), 1);
        assert!(ui.view.transcript().is_following());
        assert!(!ui.text().contains("new messages"));
        insta::assert_snapshot!("chat_long", ui.snapshot());
    }

    /// Scrolls up, adds output, and checks that the viewed lines stayed put
    /// and the indicator appeared on the bottom transcript row.
    fn scroll_up_then_output(ui: &mut Ui, up: Vec<Event>) -> (u16, u16) {
        ui.fill(40);
        ui.send(up);
        assert!(!ui.view.transcript().is_following());
        assert!(!ui.text().contains("new messages"), "no output yet");
        let before = ui.body_rows();
        ui.push("arrived while reading");
        let after = ui.body_rows();
        // Every row but the bottom one (the indicator) is unchanged.
        assert_eq!(before[..before.len() - 1], after[..after.len() - 1]);
        assert!(after.last().unwrap().contains("↓ new messages · end"));
        assert!(!ui.text().contains("arrived while reading"));
        assert!(ui.view.transcript().has_unseen());
        ui.find("↓ new messages")
    }

    fn assert_at_bottom(ui: &Ui) {
        assert!(ui.view.transcript().is_following());
        assert!(!ui.view.transcript().has_unseen());
        assert!(!ui.text().contains("new messages"), "{}", ui.text());
        let body = ui.body_rows();
        assert!(
            body[body.len() - 2].contains("arrived while reading"),
            "{}",
            ui.text()
        );
    }

    #[test]
    fn wheel_up_pauses_and_clicking_the_indicator_resumes() {
        let mut ui = Ui::sized(60, 14);
        let wheel = crate::interaction::testing::wheel_up(5, 5);
        let (x, y) = scroll_up_then_output(&mut ui, vec![wheel.clone(), wheel]);
        insta::assert_snapshot!("chat_scrolled_up", ui.snapshot());
        ui.send(vec![click(x, y)]);
        assert_at_bottom(&ui);
        assert_eq!(ui.view.focused().map(|f| f.0.as_str()), Some(INPUT_ID));
    }

    #[test]
    fn page_up_pauses_and_end_resumes() {
        let mut ui = Ui::sized(60, 14);
        scroll_up_then_output(&mut ui, vec![key("pageup")]);
        ui.send(vec![key("end")]);
        assert_at_bottom(&ui);
    }

    #[test]
    fn end_moves_the_input_cursor_first_and_ctrl_end_always_follows() {
        let mut ui = Ui::sized(60, 14);
        scroll_up_then_output(&mut ui, vec![key("pageup")]);
        ui.send(type_text("draft"));
        ui.send(vec![key("home")]);
        // Mid-line, End moves the cursor to the end of the line.
        ui.send(vec![key("end")]);
        assert!(!ui.view.transcript().is_following());
        assert_eq!(ui.view.input().cursor(), "draft".len());
        // At the end of the line, End jumps to the bottom.
        ui.send(vec![key("end")]);
        assert_at_bottom(&ui);
        assert_eq!(ui.view.input().raw(), "draft");
        ui.send(vec![key("pageup")]);
        ui.send(vec![key("home"), key("ctrl+end")]);
        assert!(ui.view.transcript().is_following());
    }

    #[test]
    fn indicator_is_keyboard_focusable_and_enter_resumes() {
        let mut ui = Ui::sized(60, 14);
        scroll_up_then_output(&mut ui, vec![key("pageup")]);
        // The indicator is the last element above the input.
        ui.send(vec![key("shift+tab")]);
        assert_eq!(
            ui.view.focused().map(|f| f.0.as_str()),
            Some(NEW_MESSAGES_ID)
        );
        // Focus is drawn with the focus style.
        let (x, y) = ui.find("↓ new messages");
        let cell = &ui.term.backend().buffer()[(x, y)];
        assert_eq!(cell.style().bg, ui.view.cfg.theme.focused().bg);
        ui.send(vec![key("enter")]);
        assert_at_bottom(&ui);
    }

    #[test]
    fn mcp_indicator_shows_failed_servers_and_opens_mcp_by_click_and_keyboard() {
        let mut ui = Ui::new();
        assert!(!ui.text().contains("⚠ mcp"), "hidden while nothing failed");
        ui.view.status_mut().set_mcp_failed(vec!["docs".into()]);
        ui.draw();
        let (x, y) = ui.find("⚠ mcp docs");
        assert_eq!(y, 29, "on the status line");
        let cell = &ui.term.backend().buffer()[(x + 2, y)];
        assert_eq!(
            cell.style().fg,
            ui.view.cfg.theme.fg(crate::theme::Token::Error).fg
        );
        // Mouse.
        ui.click_text("⚠ mcp docs");
        assert_eq!(ui.take(), vec![ChatOutput::Submit("/mcp".into())]);
        // Keyboard: Shift+Tab from the input (Tab switches the view there)
        // cycles the focus layer and reaches it; Enter activates it.
        ui.send(vec![key("shift+tab")]);
        assert_eq!(
            ui.view.focused().map(|f| f.0.as_str()),
            Some(crate::status::MCP_INDICATOR_ID)
        );
        let (x, y) = ui.find("⚠ mcp docs");
        assert_eq!(
            ui.term.backend().buffer()[(x, y)].style().bg,
            ui.view.cfg.theme.focused().bg
        );
        ui.send(vec![key("enter")]);
        assert_eq!(ui.take(), vec![ChatOutput::Submit("/mcp".into())]);
        // Several failures are counted; none hides it again.
        ui.view
            .status_mut()
            .set_mcp_failed(vec!["a".into(), "b".into()]);
        ui.draw();
        assert!(ui.text().contains("⚠ mcp 2 failed"));
        ui.view.status_mut().set_mcp_failed(Vec::new());
        ui.draw();
        assert!(!ui.text().contains("⚠ mcp"));
    }

    #[test]
    fn jobs_indicator_opens_jobs_by_click_and_keyboard() {
        let mut ui = Ui::new();
        assert!(!ui.text().contains("⚙"), "hidden while no job runs");
        let job = |state: &str| RunEvent {
            seq: 0,
            ts: time::OffsetDateTime::UNIX_EPOCH,
            run_id: harness_events::schema::RunId(uuid::Uuid::nil()),
            node_id: Some(NodeId::from("n1")),
            parent_node_id: None,
            kind: EventKind::BashJob {
                call_id: "c1".into(),
                job_id: "j1".into(),
                command: "cargo test".into(),
                state: state.into(),
                exit_code: None,
                duration_ms: None,
                output: None,
            },
        };
        ui.view.status_mut().apply(&job("running"));
        ui.draw();
        let (x, y) = ui.find("⚙ 1 job");
        assert_eq!(y, 29, "on the status line");
        assert_eq!(
            ui.term.backend().buffer()[(x, y)].style().fg,
            ui.view.cfg.theme.fg(crate::theme::Token::Running).fg
        );
        // Mouse.
        ui.click_text("⚙ 1 job");
        assert_eq!(ui.take(), vec![ChatOutput::Submit("/jobs".into())]);
        // Keyboard: Shift+Tab from the input reaches it; Enter activates it.
        ui.send(vec![key("shift+tab")]);
        assert_eq!(
            ui.view.focused().map(|f| f.0.as_str()),
            Some(crate::status::JOBS_INDICATOR_ID)
        );
        let (x, y) = ui.find("⚙ 1 job");
        assert_eq!(
            ui.term.backend().buffer()[(x, y)].style().bg,
            ui.view.cfg.theme.focused().bg
        );
        ui.send(vec![key("enter")]);
        assert_eq!(ui.take(), vec![ChatOutput::Submit("/jobs".into())]);
        // Esc backs out to the input.
        ui.send(vec![key("shift+tab")]);
        ui.send(vec![key("esc")]);
        assert_ne!(
            ui.view.focused().map(|f| f.0.as_str()),
            Some(crate::status::JOBS_INDICATOR_ID)
        );
        // The job ends: the indicator goes away.
        ui.view.status_mut().apply(&job("exited"));
        ui.draw();
        assert!(!ui.text().contains("⚙"));
    }

    #[test]
    fn scrolling_back_down_resumes_following() {
        let mut ui = Ui::sized(60, 14);
        scroll_up_then_output(&mut ui, vec![key("pageup")]);
        ui.send(vec![key("pagedown"); 3]);
        assert_at_bottom(&ui);
    }

    #[test]
    fn scrolling_a_short_transcript_keeps_it_at_the_bottom() {
        let mut ui = Ui::sized(60, 14);
        ui.push("hello");
        let before = ui.rows();
        ui.send(vec![
            key("pageup"),
            crate::interaction::testing::wheel_up(5, 5),
        ]);
        assert!(ui.view.transcript().is_following());
        assert_eq!(ui.rows(), before);
    }

    #[test]
    fn resizing_keeps_the_bottom_anchored() {
        let mut ui = Ui::sized(60, 14);
        ui.fill(40);
        for (w, h) in [(40, 10), (90, 20), (60, 14)] {
            ui.resize(w, h);
            assert!(ui.view.transcript().is_following());
            let body = ui.body_rows();
            assert!(
                body[body.len() - 2].contains("message 39"),
                "{w}x{h}\n{}",
                ui.text()
            );
            assert_eq!(gap_above_input(&ui), 1);
        }
        // A short transcript stays at the bottom of a taller pane.
        let mut ui = Ui::sized(60, 10);
        ui.push("hello");
        ui.resize(60, 20);
        assert_eq!(gap_above_input(&ui), 1, "{}", ui.text());
        insta::assert_snapshot!("chat_resized", ui.snapshot());
    }

    #[test]
    fn resizing_a_scrolled_up_view_keeps_the_reading_position() {
        let mut ui = Ui::sized(60, 14);
        ui.fill(40);
        ui.send(vec![key("pageup"), key("pageup")]);
        // The top text row (the row above it may be the gap between two
        // messages, depending on how many rows the status line takes).
        let top = ui
            .body_rows()
            .into_iter()
            .find(|r| !r.trim().is_empty())
            .unwrap_or_default();
        assert!(top.contains("message"), "{top}");
        let first = |ui: &Ui| {
            ui.body_rows()
                .into_iter()
                .find(|r| r.contains("message"))
                .unwrap()
                .split_whitespace()
                .take(3)
                .collect::<Vec<_>>()
                .join(" ")
        };
        let reading = first(&ui);
        for (w, h) in [(90, 14), (40, 14), (60, 20), (60, 14)] {
            ui.resize(w, h);
            assert!(!ui.view.transcript().is_following(), "{w}x{h}");
            assert_eq!(first(&ui), reading, "{w}x{h}\n{}", ui.text());
        }
        // Growing past the tail follows again instead of leaving a gap.
        ui.resize(60, 200);
        assert!(ui.view.transcript().is_following());
        assert_eq!(gap_above_input(&ui), 1);
    }

    #[test]
    fn long_transcript_frames_stay_within_budget() {
        // The slowest frame of draw / page up / new output / follow on fresh
        // state with 10k entries.
        let sequence = || {
            let mut ui = Ui::new();
            for i in 0..10_000 {
                ui.view
                    .transcript_mut()
                    .push_user(&format!("message {i} with some **markdown**"), T0);
            }
            let mut worst = std::time::Duration::ZERO;
            let mut frame = |ui: &mut Ui, events: Vec<Event>| {
                let t = Instant::now();
                ui.send(events);
                if ui.out.is_empty() {
                    ui.draw();
                }
                worst = worst.max(t.elapsed());
            };
            frame(&mut ui, vec![]);
            frame(&mut ui, vec![key("pageup")]);
            ui.view.transcript_mut().push_user("late", T0);
            frame(&mut ui, vec![]);
            frame(&mut ui, vec![key("end")]);
            assert!(ui.view.transcript().is_following());
            worst
        };
        // Best of five: a slow implementation is slow every run, a CI
        // scheduler hiccup only hits some of them.
        let worst = (0..5).map(|_| sequence()).min().unwrap();
        assert!(
            worst < std::time::Duration::from_millis(16),
            "worst frame {worst:?}"
        );
    }

    #[test]
    fn page_keys_and_wheel_scroll_the_transcript() {
        let mut ui = Ui::new();
        for i in 0..60 {
            ui.view
                .transcript_mut()
                .push_user(&format!("message {i}"), T0);
        }
        ui.draw();
        assert!(ui.text().contains("message 59"));
        ui.send(vec![key("pageup")]);
        assert!(!ui.text().contains("message 59"));
        assert!(!ui.view.transcript().is_following());
        ui.send(vec![crate::interaction::testing::wheel_down(5, 5); 20]);
        assert!(ui.view.transcript().is_following());
    }

    #[test]
    fn terminal_title_follows_activity_when_enabled() {
        let mut ui = Ui::with(ChatConfig {
            terminal_title: true,
            ..ChatConfig::default()
        });
        ui.send(type_text("go"));
        ui.send(vec![key("enter")]);
        let titles: Vec<ChatOutput> = ui
            .take()
            .into_iter()
            .filter(|o| matches!(o, ChatOutput::SetTitle(_)))
            .collect();
        // The idle title, then the working one; unchanged titles are not resent.
        assert_eq!(titles.len(), 2);
        assert_eq!(titles[0], ChatOutput::SetTitle("\x1b]0;harness\x07".into()));
        let ChatOutput::SetTitle(t) = &titles[1] else {
            unreachable!()
        };
        assert!(t.starts_with("\x1b]0;⠋ sending"), "{t:?}");
        let mut off = Ui::new();
        off.send(type_text("go"));
        off.send(vec![key("enter")]);
        assert!(
            !off.take()
                .iter()
                .any(|o| matches!(o, ChatOutput::SetTitle(_)))
        );
    }

    #[test]
    fn slash_completion_from_the_input() {
        let mut ui = Ui::new();
        ui.view
            .set_slash_commands(vec![SlashCommand::new("compact", "Compact the context")]);
        ui.send(type_text("/co"));
        assert!(ui.text().contains("/compact"));
        assert!(ui.text().contains("Compact the context"));
        ui.click_text("Compact the context");
        assert_eq!(ui.view.input_mut().text(), "/compact ");
    }

    const EDIT_DIFF: &str =
        "--- a/src/a.rs\n+++ b/src/a.rs\n@@ -1 +1 @@\n-let a = 1;\n+let a = 2;\n";
    const ANSWER: &str = "Run this:\n\n```sh\ncargo test -p parse\n```\n\nDone.";

    /// An assistant answer with a code block and an edit with a diff.
    fn copy_session(ui: &mut Ui) {
        use harness_events::schema::{DeltaChannel, FileChangeKind, Usage};
        ui.event(
            1,
            None,
            EventKind::ModelCallStarted {
                call_id: "m1".into(),
                provider: "fake".into(),
                model: "sonnet".into(),
                request: Payload::text(""),
            },
        );
        ui.event(
            2,
            None,
            EventKind::Delta {
                call_id: "m1".into(),
                channel: DeltaChannel::Text,
                text: ANSWER.into(),
            },
        );
        ui.event(
            3,
            None,
            EventKind::ModelCallFinished {
                call_id: "m1".into(),
                usage: Usage::default(),
                stop_reason: Some("end_turn".into()),
                response: None,
                duration_ms: 10,
            },
        );
        ui.event(
            4,
            None,
            EventKind::ToolCall {
                call_id: "e".into(),
                tool: "edit".into(),
                args: Payload::Json(
                    json!({"path": "src/a.rs", "old_string": "let a = 1;", "new_string": "let a = 2;"}),
                ),
            },
        );
        ui.event(
            5,
            None,
            EventKind::ToolResult {
                call_id: "e".into(),
                ok: true,
                output: Payload::text("\u{1b}[32medited\u{1b}[0m src/a.rs"),
                duration_ms: 1,
            },
        );
        ui.event(
            6,
            None,
            EventKind::FileChanged {
                path: "src/a.rs".into(),
                change: FileChangeKind::Modified,
                diff: Some(Payload::text(EDIT_DIFF)),
                generated: false,
            },
        );
    }

    fn copies(out: Vec<ChatOutput>) -> Vec<(String, String)> {
        out.into_iter()
            .filter_map(|o| match o {
                ChatOutput::Copy { what, text } => Some((what, text)),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn copy_buttons_copy_message_code_output_and_diff_by_click() {
        let mut ui = Ui::new();
        copy_session(&mut ui);
        let text = ui.text();
        assert!(text.contains("◆ assistant"), "{text}");
        assert!(text.contains("sh  ⧉ copy"), "{text}");
        assert!(
            text.contains("⧉ copy output") && text.contains("⧉ copy diff"),
            "{text}"
        );
        // The assistant header's button.
        let (_, hy) = ui.find("◆ assistant");
        let (bx, by) = ui.find("⧉ copy ·");
        assert_eq!(by, hy, "the message button sits in the header");
        ui.send(vec![click(bx, by)]);
        // The code block's button.
        let (cx, cy) = ui.find("sh  ⧉ copy");
        ui.send(vec![click(cx + 4, cy)]);
        ui.click_text("⧉ copy output");
        ui.click_text("⧉ copy diff");
        assert_eq!(
            copies(ui.take()),
            vec![
                ("message".to_owned(), ANSWER.to_owned()),
                ("code block".to_owned(), "cargo test -p parse".to_owned()),
                (
                    "tool output".to_owned(),
                    "\u{1b}[32medited\u{1b}[0m src/a.rs".to_owned()
                ),
                ("diff".to_owned(), EDIT_DIFF.to_owned()),
            ],
            "ANSI codes are stripped later by the clipboard"
        );
        // Clicking the card elsewhere still toggles it, not copies.
        ui.click_text("src/a.rs  +1");
        assert!(copies(ui.take()).is_empty());
    }

    #[test]
    fn copy_key_copies_the_focused_element_or_the_last_message() {
        let mut ui = Ui::new();
        copy_session(&mut ui);
        // Nothing focused but the input: the last assistant message.
        ui.send(vec![key("ctrl+y")]);
        assert_eq!(copies(ui.take()), vec![("message".into(), ANSWER.into())]);
        // Tab to the code block's button, Enter copies it.
        ui.view.interaction.set_focus("copy.code.0.0");
        ui.send(vec![key("enter")]);
        assert_eq!(copies(ui.take())[0].1, "cargo test -p parse");
        // Shift+Tab from the input walks back over the card's buttons.
        ui.view.interaction.set_focus(INPUT_ID);
        ui.send(vec![key("shift+tab")]);
        assert_eq!(ui.view.focused(), Some(&ElementId::from("copy.diff.e")));
        ui.send(vec![key("ctrl+y")]);
        assert_eq!(copies(ui.take()), vec![("diff".into(), EDIT_DIFF.into())]);
        ui.send(vec![key("shift+tab")]);
        assert_eq!(ui.view.focused(), Some(&ElementId::from("copy.out.e")));
        ui.send(vec![key("enter")]);
        assert_eq!(copies(ui.take())[0].0, "tool output");
        // A focused card copies its diff.
        ui.send(vec![key("shift+tab")]);
        assert_eq!(ui.view.focused(), Some(&ElementId::from("card.e")));
        ui.send(vec![key("ctrl+y")]);
        assert_eq!(copies(ui.take())[0].1, EDIT_DIFF);
        // An empty transcript has nothing to copy: a warning, no output.
        let mut empty = Ui::new();
        empty.send(vec![key("ctrl+y")]);
        assert!(copies(empty.take()).is_empty());
        assert!(empty.text().contains("nothing to copy"));
    }

    #[test]
    fn palette_copy_entries() {
        let mut ui = Ui::new();
        copy_session(&mut ui);
        let run = |ui: &mut Ui, q: &str| {
            ui.send(vec![key("ctrl+p")]);
            ui.send(type_text(q));
            ui.send(vec![key("enter")]);
            copies(ui.take())
        };
        assert_eq!(run(&mut ui, "copy last message")[0].1, ANSWER);
        assert_eq!(
            run(&mut ui, "copy last code block")[0].1,
            "cargo test -p parse"
        );
        assert_eq!(run(&mut ui, "copy diff")[0].1, EDIT_DIFF);
        assert_eq!(run(&mut ui, "copy tool output")[0].0, "tool output");
        // Mouse: click the entry.
        ui.send(vec![key("ctrl+p")]);
        ui.send(type_text("copy last code"));
        ui.click_text("Copy last code block");
        assert_eq!(copies(ui.take())[0].1, "cargo test -p parse");
        // With a screen open, Copy focused and Copy diff go to the app.
        ui.view.set_screen_open(true);
        ui.send(vec![key("ctrl+p")]);
        ui.send(type_text("copy focused"));
        ui.send(vec![key("enter")]);
        let out = ui.take();
        assert!(
            matches!(&out[..], [ChatOutput::Run(inv)] if inv.id == copy_ids::FOCUSED),
            "{out:?}"
        );
    }

    #[test]
    fn selection_mode_selects_a_range_by_keyboard_and_copies_plain_text() {
        let mut ui = Ui::new();
        copy_session(&mut ui);
        ui.send(vec![key("ctrl+t")]);
        assert!(ui.view.transcript().selection().is_some());
        assert!(ui.text().contains("SELECT"), "{}", ui.text());
        assert!(!ui.text().contains("› "), "the bar replaces the input");
        // Move to the top, mark, walk down over the code block.
        ui.send(vec![key("home"), key("space")]);
        for _ in 0..5 {
            ui.send(vec![key("down")]);
        }
        assert!(ui.text().contains("6 lines"), "{}", ui.text());
        ui.send(vec![key("y")]);
        let c = copies(ui.take());
        assert_eq!(c.len(), 1, "{}", ui.text());
        assert_eq!(c[0].0, "selection");
        let lines: Vec<&str> = c[0].1.lines().collect();
        assert!(lines[0].starts_with("◆ assistant"), "{lines:?}");
        assert!(!c[0].1.contains("⧉"), "no buttons: {:?}", c[0].1);
        assert!(lines.contains(&"Run this:"), "{lines:?}");
        assert!(lines.contains(&"  sh"), "{lines:?}");
        assert!(
            lines.contains(&"cargo test -p parse"),
            "no code gutter: {lines:?}"
        );
        assert!(
            ui.view.transcript().selection().is_none(),
            "copy leaves the mode"
        );
        // The diff card selected: no borders, no line numbers.
        ui.send(vec![key("ctrl+t"), key("end")]);
        ui.send(vec![key("space")]);
        for _ in 0..8 {
            ui.send(vec![key("up")]);
        }
        ui.send(vec![key("enter")]);
        let c = copies(ui.take());
        let text = &c[0].1;
        assert!(
            text.contains("-let a = 1;") && text.contains("+let a = 2;"),
            "{text}"
        );
        assert!(
            !text.contains('│') && !text.contains('╰') && !text.contains('╭'),
            "{text}"
        );
        // Esc leaves without copying; the selection bar is clickable.
        ui.send(vec![key("ctrl+t"), key("esc")]);
        assert!(ui.view.transcript().selection().is_none());
        assert!(copies(ui.take()).is_empty());
        ui.send(vec![key("ctrl+t")]);
        ui.click_text("[mark]");
        assert!(ui.view.transcript().selection().unwrap().mark.is_some());
        ui.send(vec![key("up"), key("up"), key("up")]);
        ui.click_text("[copy]");
        assert_eq!(copies(ui.take()).len(), 1);
        ui.send(vec![key("ctrl+t")]);
        ui.click_text("[done]");
        assert!(ui.view.transcript().selection().is_none());
        // Tab reaches the bar's buttons; Enter on [done] activates it.
        ui.send(vec![key("ctrl+t"), key("tab")]);
        let f = ui.view.focused().cloned().unwrap();
        assert!(
            f.0.starts_with("select.") || f.0.starts_with("copy.") || f.0.starts_with("card."),
            "{f:?}"
        );
        ui.view.interaction.set_focus(copy_ui::SELECT_DONE_ID);
        ui.send(vec![key("enter")]);
        assert!(ui.view.transcript().selection().is_none());
        assert!(copies(ui.take()).is_empty());
    }

    #[test]
    fn selection_mode_click_moves_the_cursor() {
        let mut ui = Ui::new();
        copy_session(&mut ui);
        ui.send(vec![key("ctrl+t"), key("space")]);
        let (x, y) = ui.find("Run this:");
        ui.send(vec![click(x, y)]);
        ui.send(vec![key("y")]);
        let c = copies(ui.take());
        assert!(c[0].1.starts_with("Run this:"), "{:?}", c[0].1);
        assert!(
            c[0].1.contains("+let a = 2;"),
            "range runs to the bottom: {:?}",
            c[0].1
        );
    }

    #[tokio::test]
    async fn pastes_reach_the_input_not_approval_keys_and_the_palette_fields() {
        let mut ui = Ui::new();
        ui.event(
            1,
            None,
            EventKind::ToolCall {
                call_id: "call-0".into(),
                tool: "bash".into(),
                args: Payload::Json(json!({"command": "git push origin main"})),
            },
        );
        let (chan, mut rx) = ChannelUi::new(2);
        let c = chan.clone();
        let _task = tokio::spawn(async move {
            c.ask(ApprovalPrompt {
                batch: 1,
                requests: vec![request(0)],
            })
            .await
        });
        ui.view.push_approval(rx.recv().await.unwrap());
        ui.draw();
        // A paste full of approval keys and newlines while a prompt waits.
        let text = "y\nn\tY ü\r\nN\n";
        ui.send(vec![Event::Paste(text.into())]);
        assert!(!ui.view.approvals().is_empty(), "no approval key fired");
        assert_eq!(ui.view.input().text(), "y\nn\tY ü\nN\n");
        assert!(ui.take().is_empty(), "nothing was sent");
        // A paste while a card is focused goes to the input and focuses it.
        ui.view.interaction.set_focus("card.call-0");
        ui.send(vec![Event::Paste("more".into())]);
        assert!(ui.view.input().text().ends_with("more"));
        assert_eq!(ui.view.focused(), Some(&ElementId::from(INPUT_ID)));
        // Palette filter: one line.
        ui.send(vec![key("ctrl+p"), Event::Paste("show\nkeys".into())]);
        assert_eq!(ui.view.palette().query(), "show keys");
        ui.send(vec![key("enter")]);
        assert_eq!(ui.take(), vec![ChatOutput::Command(AppCommand::ShowHelp)]);
        // Palette argument prompt.
        ui.view.set_palette_entries(vec![
            PaletteEntry::new(
                "workflow.review",
                "/review",
                "Review",
                crate::palette::EntryKind::Workflow,
            )
            .with_args(vec![crate::palette::ArgSpec::required("branch")]),
        ]);
        ui.send(vec![key("ctrl+p")]);
        ui.send(type_text("/review"));
        ui.send(vec![
            key("enter"),
            Event::Paste("feature/ü-x\n".into()),
            key("enter"),
        ]);
        match &ui.take()[..] {
            [ChatOutput::Run(inv)] => assert_eq!(inv.args, vec!["feature/ü-x ".to_owned()]),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn long_status_wraps_and_more_opens_the_full_status_by_click_and_keyboard() {
        let long = "/private/var/folders/n6/hn37q6pj7sj_9dm0000gn/T/.tmpXyZ/project/status_end";
        // Tall enough for three status rows: the path wraps, nothing is cut.
        let mut ui = Ui::sized(60, 30);
        ui.view.status_mut().cwd = long.into();
        ui.draw();
        assert!(
            crate::layout_matrix::shows(&ui.text(), long),
            "{}",
            ui.text()
        );
        assert!(!ui.text().contains('…'), "{}", ui.text());
        // Short: two status rows, the rest behind `+N more`.
        let mut ui = Ui::sized(40, 12);
        ui.view.status_mut().cwd = long.into();
        ui.view.status_mut().set_mcp_failed(vec!["docs".into()]);
        ui.draw();
        let (x, y) = ui.find("more");
        assert!(y >= 10, "on the status line: {}", ui.text());
        let label_style = ui.term.backend().buffer()[(x, y)].style();
        assert_eq!(
            label_style.fg,
            ui.view.cfg.theme.fg(crate::theme::Token::Link).fg
        );
        // Mouse.
        ui.click_text("more");
        assert_eq!(ui.take(), vec![ChatOutput::Command(AppCommand::ShowStatus)]);
        // Keyboard: Shift+Tab from the input reaches the last status
        // button; Enter activates it.
        ui.send(vec![key("shift+tab")]);
        assert_eq!(
            ui.view.focused().map(|f| f.0.as_str()),
            Some(crate::status::STATUS_MORE_ID)
        );
        ui.send(vec![key("enter")]);
        assert_eq!(ui.take(), vec![ChatOutput::Command(AppCommand::ShowStatus)]);
        let details = ui.view.status_details().join("\n");
        assert!(details.contains(long), "{details}");
    }
}
