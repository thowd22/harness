//! Working indicators: spinners, the activity line, stall detection and the
//! terminal title.
//!
//! Something is always visibly happening, or visibly not:
//!
//! - [`Activity`] follows the main agent through its [`Phase`]s (sending,
//!   waiting for the first token, thinking, writing, running a tool,
//!   retrying, waiting for approval, verifying, compacting, syncing) from run
//!   events, keeps phase and turn timers and live token and cost counts, and
//!   renders the activity line shown above the input.
//! - When no provider data or tool output arrives for `[ui] stall_after`,
//!   [`Activity::stall`] reports it and the line turns `stalled` (orange).
//! - [`Spinner`] is the shared braille spinner. With `[ui] animations =
//!   false` it is a static `◐` and never needs ticks.
//! - [`node_glyph`] gives map nodes, goal-bar tasks and subagent cards their
//!   status icon; [`running_agents`] and [`active_remotes`] feed the status
//!   line counts.
//! - [`TerminalTitle`] produces the OSC title sequence when `[ui]
//!   terminal_title` is on, only when the text changes.
//!
//! All timing takes an explicit `now: Instant`, so tests are deterministic.

use std::time::{Duration, Instant};

use harness_events::reducer::RunState;
use harness_events::schema::{
    DeltaChannel, EventKind, NodeId, NodeKind, NodeStatus, RunEvent, Usage,
};
use ratatui::text::{Line, Span};

use crate::cards::arg_summary;
use crate::chat::text::{fmt_cost, fmt_ms, fmt_tokens, sanitize, truncate};
use crate::theme::{Theme, Token};

/// Braille spinner frames, advanced every animation tick (~80 ms).
pub const SPINNER_FRAMES: [char; 10] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
/// Static icon used instead of the spinner when animations are off.
pub const STATIC_ICON: char = '◐';
/// Icon of the waiting-for-approval phase.
pub const PAUSE_ICON: char = '⏸';
/// Icon of a stalled indicator.
pub const STALL_ICON: char = '⚠';

/// The shared spinner.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Spinner {
    frame: usize,
    animated: bool,
}

impl Spinner {
    /// A spinner; `animated = false` always shows [`STATIC_ICON`].
    pub fn new(animated: bool) -> Spinner {
        Spinner { frame: 0, animated }
    }

    /// Whether the spinner animates (and so needs ticks while visible).
    pub fn is_animated(&self) -> bool {
        self.animated
    }

    /// Advances one frame. A no-op for a static spinner.
    pub fn tick(&mut self) {
        if self.animated {
            self.frame = (self.frame + 1) % SPINNER_FRAMES.len();
        }
    }

    /// The current glyph.
    pub fn glyph(&self) -> char {
        if self.animated {
            SPINNER_FRAMES[self.frame]
        } else {
            STATIC_ICON
        }
    }

    /// Whether a tick is needed: only when animated, something is running
    /// and an indicator is on screen.
    pub fn needs_tick(&self, running: bool, visible: bool) -> bool {
        self.animated && running && visible
    }
}

/// What a silent indicator is waiting on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StallSource {
    /// The model's response stream.
    Provider,
    /// A running tool's output.
    Tool,
}

impl StallSource {
    /// Display name, e.g. `provider stream`.
    pub fn label(self) -> &'static str {
        match self {
            StallSource::Provider => "provider stream",
            StallSource::Tool => "tool output",
        }
    }
}

/// A detected stall.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stall {
    /// What went silent.
    pub source: StallSource,
    /// How long it has been silent.
    pub silent: Duration,
}

/// What the main agent is doing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Phase {
    /// Nothing is running.
    Idle,
    /// Sending a request to the provider.
    Sending,
    /// Request sent, no token yet.
    WaitingFirstToken,
    /// Receiving reasoning.
    Thinking,
    /// Receiving answer text or tool arguments.
    Writing,
    /// Running a tool.
    RunningTool {
        /// Tool name.
        tool: String,
        /// Argument summary, e.g. `cargo test parse`.
        summary: String,
    },
    /// Waiting before a retry.
    Retrying {
        /// Why, e.g. `429 rate limited`.
        reason: String,
        /// Attempt number (1-based).
        attempt: u32,
        /// Retry limit, 0 when unknown.
        max: u32,
        /// Delay before the retry, counted down from the phase start.
        delay: Duration,
    },
    /// Waiting for the user to approve an action.
    WaitingApproval {
        /// The action, e.g. `git push origin main`.
        action: String,
    },
    /// Running the verify command.
    Verifying {
        /// The command.
        command: String,
    },
    /// Compacting the context.
    Compacting,
    /// Syncing the workspace to a remote host.
    Syncing {
        /// Host name.
        host: String,
    },
}

impl Phase {
    /// Whether this phase is work in progress.
    pub fn is_active(&self) -> bool {
        !matches!(self, Phase::Idle)
    }

    /// What must keep sending data for the phase not to be stalled.
    pub fn stall_source(&self) -> Option<StallSource> {
        match self {
            Phase::Sending | Phase::WaitingFirstToken | Phase::Thinking | Phase::Writing => {
                Some(StallSource::Provider)
            }
            Phase::RunningTool { .. } => Some(StallSource::Tool),
            _ => None,
        }
    }

    /// Short label, e.g. `thinking` or `running bash: cargo test`.
    /// `elapsed` is the time spent in the phase (used for the retry countdown).
    pub fn label(&self, elapsed: Duration) -> String {
        match self {
            Phase::Idle => "idle".into(),
            Phase::Sending => "sending".into(),
            Phase::WaitingFirstToken => "waiting for first token".into(),
            Phase::Thinking => "thinking".into(),
            Phase::Writing => "writing".into(),
            Phase::RunningTool { tool, summary } if summary.is_empty() => format!("running {tool}"),
            Phase::RunningTool { tool, summary } => format!("running {tool}: {summary}"),
            Phase::Retrying {
                reason,
                attempt,
                max,
                delay,
            } => {
                let left = delay.saturating_sub(elapsed).as_secs_f64().ceil() as u64;
                let of = if *max > 0 {
                    format!("attempt {attempt}/{max}")
                } else {
                    format!("attempt {attempt}")
                };
                format!("retrying in {left}s ({reason}, {of})")
            }
            Phase::WaitingApproval { action } => format!("waiting for approval: {action}"),
            Phase::Verifying { command } => format!("verifying: {command}"),
            Phase::Compacting => "compacting context".into(),
            Phase::Syncing { host } => format!("syncing to {host}"),
        }
    }
}

/// Live state of the main agent's work, folded from run events.
#[derive(Debug, Clone)]
pub struct Activity {
    phase: Phase,
    phase_since: Instant,
    resume: Option<(Phase, Instant)>,
    turn: u32,
    turn_since: Option<Instant>,
    last_data: Instant,
    usage: Usage,
    cost: Option<f64>,
    cost_unknown: bool,
    tool_lines: usize,
    main: Option<NodeId>,
}

impl Activity {
    /// An idle activity.
    pub fn new(now: Instant) -> Activity {
        Activity {
            phase: Phase::Idle,
            phase_since: now,
            resume: None,
            turn: 0,
            turn_since: None,
            last_data: now,
            usage: Usage::default(),
            cost: None,
            cost_unknown: false,
            tool_lines: 0,
            main: None,
        }
    }

    /// The current phase.
    pub fn phase(&self) -> &Phase {
        &self.phase
    }

    /// The turn number (1-based; 0 before the first turn).
    pub fn turn(&self) -> u32 {
        self.turn
    }

    /// Tokens used in this turn.
    pub fn usage(&self) -> Usage {
        self.usage
    }

    /// Cost of this turn; `None` until a cost is known.
    pub fn cost(&self) -> Option<f64> {
        self.cost
    }

    /// Whether work is in progress.
    pub fn is_active(&self) -> bool {
        self.phase.is_active()
    }

    /// Time in the current phase.
    pub fn phase_elapsed(&self, now: Instant) -> Duration {
        now.saturating_duration_since(self.phase_since)
    }

    /// Time since the turn started.
    pub fn turn_elapsed(&self, now: Instant) -> Duration {
        self.turn_since
            .map_or(Duration::ZERO, |t| now.saturating_duration_since(t))
    }

    /// The node treated as the main agent (the first root agent node).
    pub fn main_node(&self) -> Option<&NodeId> {
        self.main.as_ref()
    }

    /// Enters `phase`.
    pub fn set_phase(&mut self, phase: Phase, now: Instant) {
        if phase != self.phase {
            self.phase = phase;
            self.phase_since = now;
            self.last_data = now;
            if self.phase.is_active() && self.turn_since.is_none() {
                self.turn += 1;
                self.turn_since = Some(now);
            }
        }
    }

    /// Starts a new turn (the user sent a message): resets turn counters and
    /// enters [`Phase::Sending`].
    pub fn start_turn(&mut self, now: Instant) {
        self.turn += 1;
        self.turn_since = Some(now);
        self.usage = Usage::default();
        self.cost = None;
        self.cost_unknown = false;
        self.phase = Phase::Sending;
        self.phase_since = now;
        self.last_data = now;
    }

    /// Ends the turn and goes idle.
    pub fn finish(&mut self, now: Instant) {
        self.phase = Phase::Idle;
        self.phase_since = now;
        self.turn_since = None;
        self.resume = None;
    }

    /// Records that data arrived (provider bytes or tool output).
    pub fn data(&mut self, now: Instant) {
        self.last_data = now;
    }

    /// Records `lines` of streamed tool output.
    pub fn tool_output(&mut self, lines: usize, now: Instant) {
        self.tool_lines += lines;
        self.last_data = now;
    }

    /// Enters the retry phase for a provider retry (`StreamEvent::Retry`).
    pub fn retrying(
        &mut self,
        reason: &str,
        attempt: u32,
        max: u32,
        delay: Duration,
        now: Instant,
    ) {
        self.set_phase(
            Phase::Retrying {
                reason: reason.to_owned(),
                attempt,
                max,
                delay,
            },
            now,
        );
    }

    fn is_main(&mut self, ev: &RunEvent) -> bool {
        if let EventKind::NodeStarted {
            kind: NodeKind::Agent,
            ..
        } = &ev.kind
            && self.main.is_none()
            && ev.parent_node_id.is_none()
        {
            self.main = ev.node_id.clone();
        }
        match &ev.node_id {
            None => true,
            Some(id) => self.main.as_ref().is_none_or(|m| m == id),
        }
    }

    /// Folds one run event. Only the main agent's events change the phase.
    pub fn apply(&mut self, ev: &RunEvent, now: Instant) {
        if !self.is_main(ev) {
            return;
        }
        match &ev.kind {
            EventKind::NodeStarted { .. } | EventKind::NodeRunning => {
                if !self.is_active() {
                    self.set_phase(Phase::Sending, now);
                }
            }
            EventKind::ModelCallStarted { .. } => self.set_phase(Phase::WaitingFirstToken, now),
            EventKind::Delta { channel, .. } => {
                let next = match channel {
                    DeltaChannel::Reasoning => Phase::Thinking,
                    DeltaChannel::Text | DeltaChannel::ToolArgs => Phase::Writing,
                };
                self.set_phase(next, now);
                self.data(now);
            }
            EventKind::ModelCallFinished { usage, .. } => {
                self.usage.add(usage);
                self.data(now);
            }
            EventKind::ToolCall { tool, args, .. } => {
                self.tool_lines = 0;
                let summary = crate::cards::payload_json(args)
                    .map(|v| arg_summary(tool, &v))
                    .map(|s| s.strip_prefix("$ ").map(str::to_owned).unwrap_or(s))
                    .unwrap_or_default();
                self.set_phase(
                    Phase::RunningTool {
                        tool: tool.clone(),
                        summary,
                    },
                    now,
                );
            }
            EventKind::ToolResult { .. } => self.set_phase(Phase::Sending, now),
            EventKind::ApprovalRequested { action, .. } => {
                if !matches!(self.phase, Phase::WaitingApproval { .. }) {
                    self.resume = Some((self.phase.clone(), self.phase_since));
                }
                self.set_phase(
                    Phase::WaitingApproval {
                        action: action.clone(),
                    },
                    now,
                );
            }
            EventKind::ApprovalResolved { .. } => {
                if let Some((phase, _)) = self.resume.take() {
                    self.set_phase(phase, now);
                }
            }
            EventKind::VerifyStarted { command } => self.set_phase(
                Phase::Verifying {
                    command: command.clone(),
                },
                now,
            ),
            EventKind::VerifyFinished { .. } => self.set_phase(Phase::Sending, now),
            EventKind::NodeRetry { attempt, reason } => {
                self.retrying(reason, *attempt, 0, Duration::ZERO, now)
            }
            EventKind::Cost { usd } => {
                self.cost = Some(self.cost.unwrap_or(0.0) + usd);
            }
            EventKind::NodeFinished { .. } | EventKind::RunFinished { .. } => self.finish(now),
            _ => {}
        }
    }

    /// The current stall, if the phase expects data and none arrived for
    /// `after` (`[ui] stall_after`).
    pub fn stall(&self, now: Instant, after: Duration) -> Option<Stall> {
        let source = self.phase.stall_source()?;
        let silent = now.saturating_duration_since(self.last_data);
        (!after.is_zero() && silent >= after).then_some(Stall { source, silent })
    }

    /// Plain-text status for the terminal title, e.g. `⠹ thinking · 12.3s`.
    pub fn title(&self, now: Instant, spinner: &Spinner, stall_after: Duration) -> String {
        if !self.is_active() {
            return "harness".to_owned();
        }
        let label = match self.stall(now, stall_after) {
            Some(s) => format!("{STALL_ICON} no data for {}s", s.silent.as_secs()),
            None => format!(
                "{} {}",
                self.icon(spinner),
                self.phase.label(self.phase_elapsed(now))
            ),
        };
        format!(
            "{} · {} — harness",
            truncate(&label, 60),
            fmt_ms(self.turn_elapsed(now).as_millis() as u64)
        )
    }

    fn icon(&self, spinner: &Spinner) -> char {
        match self.phase {
            Phase::WaitingApproval { .. } => PAUSE_ICON,
            _ => spinner.glyph(),
        }
    }

    /// The activity line, e.g.
    /// `⠹ thinking 3.1s · turn 4 · 12.3s · ↑12.4k (cache 11.9k) ↓812 · $0.021 · ctrl+c to interrupt`.
    /// `interrupt_hint` is the interrupt key, e.g. `ctrl+c`.
    pub fn line(
        &self,
        now: Instant,
        spinner: &Spinner,
        stall_after: Duration,
        theme: &Theme,
        interrupt_hint: &str,
    ) -> Line<'static> {
        if !self.is_active() {
            return Line::default();
        }
        let muted = theme.fg(Token::Muted);
        let num = theme.fg(Token::Number);
        let sep = || Span::styled(" · ", theme.fg(Token::Muted));
        let phase_ms = self.phase_elapsed(now).as_millis() as u64;
        let mut spans = Vec::new();
        match self.stall(now, stall_after) {
            Some(stall) => {
                spans.push(Span::styled(
                    format!(
                        "{STALL_ICON} no data for {}s ({})",
                        stall.silent.as_secs(),
                        stall.source.label()
                    ),
                    theme.fg(Token::Stalled),
                ));
                spans.push(sep());
                spans.push(Span::styled(
                    sanitize(&self.phase.label(self.phase_elapsed(now))),
                    muted,
                ));
            }
            None => {
                let token = match self.phase {
                    Phase::WaitingApproval { .. } => Token::Approval,
                    Phase::Retrying { .. } => Token::Warn,
                    _ => Token::Running,
                };
                spans.push(Span::styled(
                    format!(
                        "{} {}",
                        self.icon(spinner),
                        sanitize(&self.phase.label(self.phase_elapsed(now)))
                    ),
                    theme.fg(token),
                ));
            }
        }
        spans.push(Span::raw(" "));
        spans.push(Span::styled(fmt_ms(phase_ms), num));
        if matches!(self.phase, Phase::RunningTool { .. }) && self.tool_lines > 0 {
            spans.push(sep());
            spans.push(Span::styled(
                format!("streaming {} lines", self.tool_lines),
                muted,
            ));
        }
        spans.push(sep());
        spans.push(Span::styled(format!("turn {}", self.turn), muted));
        spans.push(sep());
        spans.push(Span::styled(
            fmt_ms(self.turn_elapsed(now).as_millis() as u64),
            num,
        ));
        let u = self.usage;
        if u.input + u.cached_input + u.output > 0 {
            spans.push(sep());
            spans.push(Span::styled(
                format!("↑{}", fmt_tokens(u.input + u.cached_input)),
                num,
            ));
            if u.cached_input > 0 {
                spans.push(Span::styled(
                    format!(" (cache {})", fmt_tokens(u.cached_input)),
                    muted,
                ));
            }
            spans.push(Span::styled(format!(" ↓{}", fmt_tokens(u.output)), num));
        }
        if self.cost.is_some() {
            spans.push(sep());
            spans.push(Span::styled(fmt_cost(self.cost), num));
        }
        if !interrupt_hint.is_empty() {
            spans.push(sep());
            spans.push(Span::styled(
                format!("{interrupt_hint} to interrupt"),
                muted,
            ));
        }
        Line::from(spans)
    }
}

/// Status icon and colour for a node: the spinner while running, `⏸` while
/// waiting for approval, `✔`/`✗`/`⊘` when finished, `○` when queued.
pub fn node_glyph(status: NodeStatus, spinner: &Spinner) -> (char, Token) {
    match status {
        NodeStatus::Running => (spinner.glyph(), Token::Running),
        NodeStatus::WaitingApproval => (PAUSE_ICON, Token::Approval),
        NodeStatus::Queued => ('○', Token::Muted),
        NodeStatus::Succeeded => ('✔', Token::Ok),
        NodeStatus::Failed => ('✗', Token::Error),
        NodeStatus::Skipped | NodeStatus::Cancelled => ('⊘', Token::Muted),
    }
}

/// Number of agent and subagent nodes currently running.
pub fn running_agents(run: &RunState) -> usize {
    run.nodes
        .iter()
        .filter(|n| {
            matches!(n.kind, Some(NodeKind::Agent | NodeKind::Subagent))
                && matches!(n.status, NodeStatus::Running | NodeStatus::WaitingApproval)
        })
        .count()
}

/// Distinct remote hosts with running nodes, in first-seen order.
pub fn active_remotes(run: &RunState) -> Vec<String> {
    let mut hosts: Vec<String> = Vec::new();
    for n in &run.nodes {
        if matches!(n.status, NodeStatus::Running | NodeStatus::WaitingApproval)
            && let Some(h) = &n.host
            && !hosts.contains(h)
        {
            hosts.push(h.clone());
        }
    }
    hosts
}

/// Builds the OSC 0 sequence that sets the terminal title to `text`
/// (control characters are removed).
pub fn title_sequence(text: &str) -> String {
    let clean: String = text.chars().filter(|c| !c.is_control()).collect();
    format!("\x1b]0;{clean}\x07")
}

/// The optional terminal title (`[ui] terminal_title`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TerminalTitle {
    enabled: bool,
    last: Option<String>,
}

impl TerminalTitle {
    /// A title writer; disabled writers never emit anything.
    pub fn new(enabled: bool) -> TerminalTitle {
        TerminalTitle {
            enabled,
            last: None,
        }
    }

    /// The escape sequence to write for `text`, or `None` when disabled or
    /// unchanged since the last call.
    pub fn update(&mut self, text: &str) -> Option<String> {
        if !self.enabled || self.last.as_deref() == Some(text) {
            return None;
        }
        self.last = Some(text.to_owned());
        Some(title_sequence(text))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use harness_events::schema::{Payload, RunId};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use time::OffsetDateTime;

    fn ev(seq: u64, node: Option<&str>, kind: EventKind) -> RunEvent {
        RunEvent {
            seq,
            ts: OffsetDateTime::UNIX_EPOCH,
            run_id: RunId(uuid::Uuid::nil()),
            node_id: node.map(NodeId::from),
            parent_node_id: None,
            kind,
        }
    }

    fn started(node: &str, kind: NodeKind, host: Option<&str>) -> EventKind {
        EventKind::NodeStarted {
            kind,
            name: node.into(),
            agent: None,
            model: None,
            host: host.map(Into::into),
            needs: vec![],
            queued: false,
        }
    }

    fn text(line: &Line<'_>) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn phases_follow_main_agent_events() {
        let t0 = Instant::now();
        let mut a = Activity::new(t0);
        assert_eq!(a.phase(), &Phase::Idle);
        a.start_turn(t0);
        assert_eq!(a.phase(), &Phase::Sending);
        a.apply(
            &ev(1, Some("main"), started("main", NodeKind::Agent, None)),
            t0,
        );
        let call = |kind| ev(2, Some("main"), kind);
        a.apply(
            &call(EventKind::ModelCallStarted {
                call_id: "c".into(),
                provider: "fake".into(),
                model: "m".into(),
                request: Payload::text(""),
            }),
            t0,
        );
        assert_eq!(a.phase(), &Phase::WaitingFirstToken);
        a.apply(
            &call(EventKind::Delta {
                call_id: "c".into(),
                channel: DeltaChannel::Reasoning,
                text: "hm".into(),
            }),
            t0,
        );
        assert_eq!(a.phase(), &Phase::Thinking);
        a.apply(
            &call(EventKind::Delta {
                call_id: "c".into(),
                channel: DeltaChannel::Text,
                text: "ok".into(),
            }),
            t0,
        );
        assert_eq!(a.phase(), &Phase::Writing);
        a.apply(
            &call(EventKind::ToolCall {
                call_id: "t".into(),
                tool: "bash".into(),
                args: Payload::Json(serde_json::json!({"command": "cargo test parse"})),
            }),
            t0,
        );
        assert_eq!(
            a.phase().label(Duration::ZERO),
            "running bash: cargo test parse"
        );
        a.apply(
            &call(EventKind::ApprovalRequested {
                approval_id: "a".into(),
                action: "git push origin main".into(),
                preview: None,
            }),
            t0,
        );
        assert_eq!(
            a.phase().label(Duration::ZERO),
            "waiting for approval: git push origin main"
        );
        a.apply(
            &call(EventKind::ApprovalResolved {
                approval_id: "a".into(),
                decision: harness_events::schema::ApprovalDecision::Approved,
                by: "user".into(),
            }),
            t0,
        );
        assert!(matches!(a.phase(), Phase::RunningTool { .. }));
        a.apply(
            &call(EventKind::VerifyStarted {
                command: "cargo test".into(),
            }),
            t0,
        );
        assert_eq!(a.phase().label(Duration::ZERO), "verifying: cargo test");
        a.retrying("429 rate limited", 2, 5, Duration::from_secs(4), t0);
        assert_eq!(
            a.phase().label(Duration::from_millis(500)),
            "retrying in 4s (429 rate limited, attempt 2/5)"
        );
        assert_eq!(
            a.phase().label(Duration::from_millis(1500)),
            "retrying in 3s (429 rate limited, attempt 2/5)"
        );
        a.set_phase(Phase::Compacting, t0);
        assert_eq!(a.phase().label(Duration::ZERO), "compacting context");
        a.set_phase(
            Phase::Syncing {
                host: "gpu-box".into(),
            },
            t0,
        );
        assert_eq!(a.phase().label(Duration::ZERO), "syncing to gpu-box");
        a.apply(
            &call(EventKind::NodeFinished {
                status: NodeStatus::Succeeded,
                error: None,
            }),
            t0,
        );
        assert!(!a.is_active());
    }

    #[test]
    fn subagent_events_do_not_change_the_main_phase() {
        let t0 = Instant::now();
        let mut a = Activity::new(t0);
        a.apply(
            &ev(1, Some("main"), started("main", NodeKind::Agent, None)),
            t0,
        );
        a.set_phase(Phase::Writing, t0);
        let mut sub = ev(2, Some("sub"), started("sub", NodeKind::Subagent, None));
        sub.parent_node_id = Some(NodeId::from("main"));
        a.apply(&sub, t0);
        a.apply(
            &ev(
                3,
                Some("sub"),
                EventKind::NodeFinished {
                    status: NodeStatus::Succeeded,
                    error: None,
                },
            ),
            t0,
        );
        assert_eq!(a.phase(), &Phase::Writing);
    }

    #[test]
    fn activity_line_shows_spinner_phase_times_tokens_cost_and_hint() {
        let theme = Theme::default();
        let t0 = Instant::now();
        let mut a = Activity::new(t0);
        a.start_turn(t0);
        a.set_phase(Phase::Thinking, t0 + Duration::from_secs(9));
        a.apply(
            &ev(
                1,
                None,
                EventKind::ModelCallFinished {
                    call_id: "c".into(),
                    usage: Usage {
                        input: 500,
                        cached_input: 11_900,
                        output: 812,
                        reasoning: 0,
                    },
                    stop_reason: None,
                    response: None,
                    duration_ms: 1,
                },
            ),
            t0 + Duration::from_secs(12),
        );
        a.apply(&ev(2, None, EventKind::Cost { usd: 0.021 }), t0);
        let spinner = Spinner::new(true);
        let now = t0 + Duration::from_millis(12_300);
        let l = text(&a.line(now, &spinner, Duration::from_secs(20), &theme, "ctrl+c"));
        assert_eq!(
            l,
            "⠋ thinking 3.3s · turn 1 · 12.3s · ↑12.4k (cache 11.9k) ↓812 · $0.021 · ctrl+c to interrupt"
        );
        let line = a.line(now, &spinner, Duration::from_secs(20), &theme, "ctrl+c");
        assert_eq!(line.spans[0].style, theme.fg(Token::Running));
    }

    #[test]
    fn stall_turns_the_indicator_orange_with_the_reason() {
        let theme = Theme::default();
        let t0 = Instant::now();
        let mut a = Activity::new(t0);
        a.start_turn(t0);
        a.set_phase(Phase::WaitingFirstToken, t0);
        let after = Duration::from_secs(20);
        assert_eq!(a.stall(t0 + Duration::from_secs(19), after), None);
        let stall = a.stall(t0 + Duration::from_secs(21), after).unwrap();
        assert_eq!(stall.source, StallSource::Provider);
        let line = a.line(
            t0 + Duration::from_secs(21),
            &Spinner::new(true),
            after,
            &theme,
            "",
        );
        assert!(text(&line).starts_with("⚠ no data for 21s (provider stream)"));
        assert_eq!(line.spans[0].style, theme.fg(Token::Stalled));
        // Data resets the clock.
        a.data(t0 + Duration::from_secs(21));
        assert_eq!(a.stall(t0 + Duration::from_secs(22), after), None);
        // Tool phases stall on missing tool output.
        a.set_phase(
            Phase::RunningTool {
                tool: "bash".into(),
                summary: String::new(),
            },
            t0,
        );
        assert_eq!(
            a.stall(t0 + Duration::from_secs(30), after).unwrap().source,
            StallSource::Tool
        );
        a.tool_output(3, t0 + Duration::from_secs(29));
        assert_eq!(a.stall(t0 + Duration::from_secs(30), after), None);
        // Waiting on the user is never a stall.
        a.set_phase(Phase::WaitingApproval { action: "x".into() }, t0);
        assert_eq!(a.stall(t0 + Duration::from_secs(300), after), None);
    }

    #[test]
    fn spinner_static_without_animations_and_ticks_only_when_needed() {
        let mut s = Spinner::new(false);
        s.tick();
        assert_eq!(s.glyph(), STATIC_ICON);
        assert!(!s.needs_tick(true, true));
        let mut s = Spinner::new(true);
        let g = s.glyph();
        s.tick();
        assert_ne!(s.glyph(), g);
        assert!(s.needs_tick(true, true));
        assert!(!s.needs_tick(false, true));
        assert!(!s.needs_tick(true, false));
    }

    #[test]
    fn spinner_tick_redraws_only_the_spinner_cell() {
        let theme = Theme::default();
        let t0 = Instant::now();
        let mut a = Activity::new(t0);
        a.start_turn(t0);
        a.set_phase(Phase::Writing, t0);
        let mut spinner = Spinner::new(true);
        let mut term = Terminal::new(TestBackend::new(80, 1)).unwrap();
        let draw = |term: &mut Terminal<TestBackend>, s: &Spinner| {
            term.draw(|f| {
                let l = a.line(t0, s, Duration::from_secs(20), &theme, "");
                f.render_widget(ratatui::widgets::Paragraph::new(l), f.area());
            })
            .unwrap();
            term.backend().buffer().clone()
        };
        let before = draw(&mut term, &spinner);
        spinner.tick();
        let after = draw(&mut term, &spinner);
        let changed = before.diff(&after);
        assert_eq!(changed.len(), 1, "only the spinner cell changes");
        assert_eq!((changed[0].0, changed[0].1), (0, 0));
    }

    #[test]
    fn node_glyphs_and_counts() {
        let spinner = Spinner::new(true);
        assert_eq!(
            node_glyph(NodeStatus::Running, &spinner),
            ('⠋', Token::Running)
        );
        assert_eq!(node_glyph(NodeStatus::Succeeded, &spinner).0, '✔');
        assert_eq!(
            node_glyph(NodeStatus::Running, &Spinner::new(false)).0,
            STATIC_ICON
        );
        let events = vec![
            ev(1, Some("main"), started("main", NodeKind::Agent, None)),
            ev(2, Some("main"), EventKind::NodeRunning),
            ev(
                3,
                Some("a"),
                started("a", NodeKind::Subagent, Some("gpu-box")),
            ),
            ev(4, Some("a"), EventKind::NodeRunning),
            ev(
                5,
                Some("b"),
                started("b", NodeKind::Subagent, Some("gpu-box")),
            ),
            ev(6, Some("b"), EventKind::NodeRunning),
            ev(7, Some("c"), started("c", NodeKind::Subagent, None)),
            ev(8, Some("c"), EventKind::NodeRunning),
            ev(
                9,
                Some("c"),
                EventKind::NodeFinished {
                    status: NodeStatus::Succeeded,
                    error: None,
                },
            ),
        ];
        let run = RunState::from_events(events.iter());
        assert_eq!(running_agents(&run), 3);
        assert_eq!(active_remotes(&run), vec!["gpu-box".to_owned()]);
    }

    #[test]
    fn terminal_title_only_when_enabled_and_changed() {
        let mut off = TerminalTitle::new(false);
        assert_eq!(off.update("x"), None);
        let mut on = TerminalTitle::new(true);
        assert_eq!(
            on.update("⠋ thinking"),
            Some("\x1b]0;⠋ thinking\x07".into())
        );
        assert_eq!(on.update("⠋ thinking"), None);
        assert_eq!(on.update("a\x07b"), Some("\x1b]0;ab\x07".into()));
        let t0 = Instant::now();
        let mut a = Activity::new(t0);
        assert_eq!(
            a.title(t0, &Spinner::new(true), Duration::from_secs(20)),
            "harness"
        );
        a.start_turn(t0);
        a.set_phase(Phase::Thinking, t0);
        assert_eq!(
            a.title(
                t0 + Duration::from_secs(2),
                &Spinner::new(true),
                Duration::from_secs(20)
            ),
            "⠋ thinking · 2.0s — harness"
        );
    }
}
