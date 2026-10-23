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
//! - When no provider data arrives for `[ui] stall_after`, [`Activity::stall`]
//!   reports it and the line turns `stalled` (orange): waiting on the model.
//! - A running tool is shown as working, with its elapsed time against its
//!   timeout and, when the executor samples it, its process group's CPU
//!   time (`tool_progress` events). Only a tool with no output, no CPU
//!   progress and nothing runnable for `stall_after` (at least
//!   [`QUIET_FLOOR`]) gets a soft "quiet" hint naming it and the cancel key;
//!   a tool whose liveness is unknown is never flagged.
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
    DeltaChannel, EventKind, NodeId, NodeKind, NodeStatus, ProcessLiveness, RunEvent, Usage,
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
/// Shortest quiet time before a sampled tool is called quiet, whatever
/// `stall_after` says: two liveness samples (taken every 3 s), so a busy
/// tool never flickers between samples.
pub const QUIET_FLOOR: Duration = Duration::from_secs(6);

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
    /// A running tool with no output and no CPU progress.
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

/// The latest liveness of the running tool's process group.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ToolLiveness {
    /// CPU time used so far by the group.
    pub cpu: Duration,
    /// Live processes in the group.
    pub procs: u32,
    /// Processes running or runnable.
    pub active: u32,
    /// When a sample last showed progress (CPU advanced, a runnable
    /// process, or the process count changed).
    pub progressed: Instant,
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
    tool_call: Option<String>,
    tool_timeout: Option<Duration>,
    tool_live: Option<ToolLiveness>,
    /// Calls of the current batch that have no result yet, in call order:
    /// `(call_id, phase)`. The agent loop records every call of a response
    /// before running any, so the last `ToolCall` is not necessarily the
    /// one running.
    in_flight: Vec<(String, Phase)>,
    main: Option<NodeId>,
    /// The main node finished, so the next root agent node (the next turn)
    /// becomes the main node.
    main_done: bool,
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
            tool_call: None,
            tool_timeout: None,
            tool_live: None,
            in_flight: Vec::new(),
            main: None,
            main_done: false,
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

    /// The node treated as the main agent: the current turn's root agent node.
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
        self.in_flight.clear();
    }

    /// Enters [`Phase::WaitingApproval`] for `action`, remembering the phase
    /// it interrupts (e.g. the running tool) so
    /// [`approval_answered`](Self::approval_answered) can resume it. Used
    /// both for the `approval_requested` event and for the UI's approval
    /// prompt, whichever arrives first.
    pub fn enter_approval(&mut self, action: String, now: Instant) {
        if !matches!(self.phase, Phase::WaitingApproval { .. }) {
            self.resume = Some((self.phase.clone(), self.phase_since));
        }
        self.set_phase(Phase::WaitingApproval { action }, now);
    }

    /// The pending approval was answered: resumes the phase it interrupted
    /// (a running tool keeps its call, timeout and liveness), or
    /// [`Phase::Sending`] when nothing was saved. No-op unless waiting for
    /// approval.
    pub fn approval_answered(&mut self, now: Instant) {
        if !matches!(self.phase, Phase::WaitingApproval { .. }) {
            return;
        }
        let phase = self.resume.take().map_or(Phase::Sending, |(p, _)| p);
        self.set_phase(phase, now);
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

    /// The running tool's timeout, once it reported one.
    pub fn tool_timeout(&self) -> Option<Duration> {
        self.tool_timeout
    }

    /// The running tool's latest liveness; `None` while unknown.
    pub fn tool_liveness(&self) -> Option<ToolLiveness> {
        self.tool_live
    }

    /// Records a `tool_progress` event of `call_id` at `now`. Ignored
    /// unless a tool is running and `call_id` is a call of the current batch
    /// without a result. Progress from another call of the batch than the
    /// one shown means that call is the one running now (bash reports its
    /// timeout when it starts): it becomes the running tool, with a fresh
    /// elapsed time, timeout and liveness.
    pub fn tool_progress(
        &mut self,
        call_id: &str,
        timeout: Option<Duration>,
        liveness: Option<ProcessLiveness>,
        now: Instant,
    ) {
        if !matches!(self.phase, Phase::RunningTool { .. }) {
            return;
        }
        if self.tool_call.as_deref() != Some(call_id) {
            let Some((_, phase)) = self.in_flight.iter().find(|(id, _)| id == call_id) else {
                return;
            };
            self.phase = phase.clone();
            self.phase_since = now;
            self.last_data = now;
            self.tool_call = Some(call_id.to_owned());
            self.tool_lines = 0;
            self.tool_timeout = None;
            self.tool_live = None;
        }
        if timeout.is_some() {
            self.tool_timeout = timeout;
        }
        let Some(l) = liveness else { return };
        let cpu = Duration::from_millis(l.cpu_ms);
        let advanced = match self.tool_live {
            None => !cpu.is_zero() || l.active > 0,
            Some(p) => cpu != p.cpu || l.active > 0 || l.procs != p.procs,
        };
        let progressed = if advanced {
            now
        } else {
            self.tool_live.map_or(self.phase_since, |p| p.progressed)
        };
        self.tool_live = Some(ToolLiveness {
            cpu,
            procs: l.procs,
            active: l.active,
            progressed,
        });
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
            && (self.main.is_none() || self.main_done)
            && ev.parent_node_id.is_none()
        {
            // Each turn runs a new root agent node; follow it once the
            // previous turn's node has finished.
            self.main = ev.node_id.clone();
            self.main_done = false;
        }
        let main = match &ev.node_id {
            None => true,
            Some(id) => self.main.as_ref().is_none_or(|m| m == id),
        };
        if main && ev.node_id.is_some() && matches!(ev.kind, EventKind::NodeFinished { .. }) {
            self.main_done = true;
        }
        main
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
            EventKind::ModelCallStarted { .. } => {
                self.in_flight.clear();
                self.set_phase(Phase::WaitingFirstToken, now);
            }
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
            EventKind::ToolCall {
                call_id,
                tool,
                args,
            } => {
                self.tool_lines = 0;
                self.tool_call = Some(call_id.clone());
                self.tool_timeout = None;
                self.tool_live = None;
                let summary = crate::cards::payload_json(args)
                    .map(|v| arg_summary(tool, &v))
                    .map(|s| s.strip_prefix("$ ").map(str::to_owned).unwrap_or(s))
                    .unwrap_or_default();
                let phase = Phase::RunningTool {
                    tool: tool.clone(),
                    summary,
                };
                self.in_flight.push((call_id.clone(), phase.clone()));
                if matches!(self.phase, Phase::WaitingApproval { .. }) {
                    // The approval prompt overtook this event: the call runs
                    // once it is answered.
                    self.resume = Some((phase, now));
                } else {
                    self.set_phase(phase, now);
                }
            }
            EventKind::ToolProgress {
                call_id,
                timeout_ms,
                liveness,
            } => self.tool_progress(
                call_id,
                timeout_ms.map(Duration::from_millis),
                *liveness,
                now,
            ),
            EventKind::ToolResult { call_id, .. } => {
                self.in_flight.retain(|(id, _)| id != call_id);
                self.set_phase(Phase::Sending, now);
            }
            EventKind::ApprovalRequested { action, .. } => {
                self.enter_approval(action.clone(), now);
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

    /// The current stall, if any (`after` is `[ui] stall_after`; zero
    /// disables detection):
    ///
    /// - provider phases: no stream data for `after`;
    /// - a running tool: only when its liveness is known and it produced no
    ///   output, used no CPU and had nothing runnable for `after` (at least
    ///   [`QUIET_FLOOR`]). `silent` is then the quiet time. A tool whose
    ///   liveness is unknown never stalls.
    pub fn stall(&self, now: Instant, after: Duration) -> Option<Stall> {
        if after.is_zero() {
            return None;
        }
        let source = self.phase.stall_source()?;
        let (since, after) = match source {
            StallSource::Provider => (self.last_data, after),
            StallSource::Tool => {
                let live = self.tool_live?;
                (self.last_data.max(live.progressed), after.max(QUIET_FLOOR))
            }
        };
        let silent = now.saturating_duration_since(since);
        (silent >= after).then_some(Stall { source, silent })
    }

    /// How long a running tool has gone without output, when its liveness
    /// is unknown and that is at least `after`: shown as a neutral note.
    fn unsampled_silence(&self, now: Instant, after: Duration) -> Option<Duration> {
        if after.is_zero()
            || self.tool_live.is_some()
            || !matches!(self.phase, Phase::RunningTool { .. })
        {
            return None;
        }
        let silent = now.saturating_duration_since(self.last_data);
        (silent >= after).then_some(silent)
    }

    fn tool_name(&self) -> &str {
        match &self.phase {
            Phase::RunningTool { tool, .. } => tool,
            _ => "tool",
        }
    }

    /// Plain-text status for the terminal title, e.g. `⠹ thinking · 12.3s`.
    pub fn title(&self, now: Instant, spinner: &Spinner, stall_after: Duration) -> String {
        if !self.is_active() {
            return "harness".to_owned();
        }
        let label = match self.stall(now, stall_after) {
            Some(s) if s.source == StallSource::Provider => {
                format!("{STALL_ICON} waiting on the model {}s", s.silent.as_secs())
            }
            Some(s) => format!(
                "{} {} quiet {}s",
                self.icon(spinner),
                self.tool_name(),
                s.silent.as_secs()
            ),
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
        let stall = self.stall(now, stall_after);
        match stall {
            Some(stall) if stall.source == StallSource::Provider => {
                spans.push(Span::styled(
                    format!(
                        "{STALL_ICON} waiting on the model: no data for {}s",
                        stall.silent.as_secs()
                    ),
                    theme.fg(Token::Stalled),
                ));
                spans.push(sep());
                spans.push(Span::styled(
                    sanitize(&self.phase.label(self.phase_elapsed(now))),
                    muted,
                ));
            }
            _ => {
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
        let running_tool = matches!(self.phase, Phase::RunningTool { .. });
        let mut cancel_shown = false;
        if running_tool {
            if let Some(t) = self.tool_timeout {
                spans.push(Span::styled(format!(" of {}", fmt_limit(t)), muted));
            }
            if let Some(l) = self.tool_live {
                spans.push(sep());
                let n = l.procs;
                let procs = if n == 1 { "process" } else { "processes" };
                spans.push(Span::styled(
                    format!("cpu {} ({n} {procs})", fmt_ms(l.cpu.as_millis() as u64)),
                    muted,
                ));
            }
            if self.tool_lines > 0 {
                spans.push(sep());
                spans.push(Span::styled(
                    format!("streaming {} lines", self.tool_lines),
                    muted,
                ));
            }
            if let Some(stall) = stall.filter(|s| s.source == StallSource::Tool) {
                // Soft: a quiet, idle tool may be hung, or just waiting
                // (`sleep`, a network call). Say what is known.
                spans.push(sep());
                spans.push(Span::styled(
                    format!(
                        "{} quiet for {}s (no output, no CPU)",
                        sanitize(self.tool_name()),
                        stall.silent.as_secs()
                    ),
                    theme.fg(Token::Warn),
                ));
                if !interrupt_hint.is_empty() {
                    spans.push(sep());
                    spans.push(Span::styled(
                        format!("{interrupt_hint} to cancel"),
                        theme.fg(Token::Warn),
                    ));
                    cancel_shown = true;
                }
            } else if let Some(silent) = self.unsampled_silence(now, stall_after) {
                spans.push(sep());
                spans.push(Span::styled(
                    format!("no output for {}s", silent.as_secs()),
                    muted,
                ));
            }
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
        if !interrupt_hint.is_empty() && !cancel_shown {
            spans.push(sep());
            spans.push(Span::styled(
                format!("{interrupt_hint} to interrupt"),
                muted,
            ));
        }
        Line::from(spans)
    }
}

/// A limit such as a timeout: whole minutes or hours without seconds
/// (`10m`, `1h`), anything else like [`fmt_ms`].
fn fmt_limit(d: Duration) -> String {
    let secs = d.as_secs();
    if secs >= 3600 && secs.is_multiple_of(3600) {
        format!("{}h", secs / 3600)
    } else if secs >= 60 && secs.is_multiple_of(60) {
        format!("{}m", secs / 60)
    } else {
        fmt_ms(d.as_millis() as u64)
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
    fn every_turn_start_and_finish_drives_the_activity_line() {
        let t0 = Instant::now();
        let mut a = Activity::new(t0);
        let finished = |seq, node: &str, status| {
            ev(
                seq,
                Some(node),
                EventKind::NodeFinished {
                    status,
                    error: None,
                },
            )
        };
        let turns = [
            ("One.", NodeStatus::Succeeded),
            ("Two.", NodeStatus::Succeeded),
            ("Three.", NodeStatus::Succeeded),
            ("Four.", NodeStatus::Cancelled),
            ("Five.", NodeStatus::Failed),
        ];
        let mut seq = 0;
        for (i, (node, status)) in turns.into_iter().enumerate() {
            seq += 10;
            a.start_turn(t0);
            assert!(a.is_active(), "{node} starts the activity line");
            a.apply(
                &ev(seq, Some(node), started(node, NodeKind::Agent, None)),
                t0,
            );
            assert_eq!(a.main_node(), Some(&NodeId::from(node)));
            a.apply(
                &ev(
                    seq + 1,
                    Some(node),
                    EventKind::Delta {
                        call_id: "c".into(),
                        channel: DeltaChannel::Text,
                        text: "ok".into(),
                    },
                ),
                t0,
            );
            assert_eq!(a.phase(), &Phase::Writing, "{node}");
            let mut sub = ev(
                seq + 2,
                Some("sub"),
                started("sub", NodeKind::Subagent, None),
            );
            sub.parent_node_id = Some(NodeId::from(node));
            a.apply(&sub, t0);
            let mut sub_done = finished(seq + 3, "sub", NodeStatus::Succeeded);
            sub_done.parent_node_id = Some(NodeId::from(node));
            a.apply(&sub_done, t0);
            assert!(a.is_active(), "{node}: a subagent does not end the turn");
            assert_eq!(a.main_node(), Some(&NodeId::from(node)));
            a.apply(&finished(seq + 4, node, status), t0);
            assert!(
                !a.is_active(),
                "{node} ({status:?}) clears the activity line"
            );
            assert_eq!(a.turn(), i as u32 + 1);
        }
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
                        cache_write: 0,
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
        assert!(
            text(&line).starts_with("⚠ waiting on the model: no data for 21s"),
            "{}",
            text(&line)
        );
        assert_eq!(line.spans[0].style, theme.fg(Token::Stalled));
        // Data resets the clock.
        a.data(t0 + Duration::from_secs(21));
        assert_eq!(a.stall(t0 + Duration::from_secs(22), after), None);
        // `stall_after = 0` disables detection.
        assert_eq!(a.stall(t0 + Duration::from_secs(600), Duration::ZERO), None);
        // A tool whose liveness is unknown is never a stall.
        a.set_phase(
            Phase::RunningTool {
                tool: "bash".into(),
                summary: String::new(),
            },
            t0,
        );
        assert_eq!(a.stall(t0 + Duration::from_secs(600), after), None);
        // Waiting on the user is never a stall.
        a.set_phase(Phase::WaitingApproval { action: "x".into() }, t0);
        assert_eq!(a.stall(t0 + Duration::from_secs(300), after), None);
    }

    fn tool_call(seq: u64, call_id: &str, command: &str) -> RunEvent {
        ev(
            seq,
            Some("main"),
            EventKind::ToolCall {
                call_id: call_id.into(),
                tool: "bash".into(),
                args: Payload::Json(serde_json::json!({ "command": command })),
            },
        )
    }

    fn progress(call_id: &str, timeout_s: Option<u64>, live: Option<(u64, u32, u32)>) -> RunEvent {
        ev(
            9,
            Some("main"),
            EventKind::ToolProgress {
                call_id: call_id.into(),
                timeout_ms: timeout_s.map(|s| s * 1000),
                liveness: live.map(|(cpu_ms, procs, active)| ProcessLiveness {
                    cpu_ms,
                    procs,
                    active,
                }),
            },
        )
    }

    /// An activity running `command` as call `t1` since `t0`, with a 10m
    /// timeout reported.
    fn running(t0: Instant, command: &str) -> Activity {
        let mut a = Activity::new(t0);
        a.start_turn(t0);
        a.apply(
            &ev(1, Some("main"), started("main", NodeKind::Agent, None)),
            t0,
        );
        a.apply(&tool_call(2, "t1", command), t0);
        a.apply(&progress("t1", Some(600), None), t0);
        a
    }

    #[test]
    fn busy_quiet_tool_shows_elapsed_of_timeout_and_cpu_without_warning() {
        let theme = Theme::default();
        let t0 = Instant::now();
        let after = Duration::from_secs(20);
        let mut a = running(t0, "cargo test --workspace");
        assert_eq!(a.tool_timeout(), Some(Duration::from_secs(600)));
        // Eight minutes of silence on stdout while the group burns CPU,
        // sampled every 3s.
        let mut cpu = 0;
        for i in 1..=170u64 {
            cpu += 10_500;
            a.apply(
                &progress("t1", Some(600), Some((cpu, 9, 3))),
                t0 + Duration::from_secs(i * 3),
            );
        }
        let now = t0 + Duration::from_millis(510_500);
        assert_eq!(a.stall(now, after), None, "busy is not stalled");
        let line = a.line(now, &Spinner::new(true), after, &theme, "ctrl+c");
        let l = text(&line);
        assert!(
            l.starts_with(
                "⠋ running bash: cargo test --workspace 8m30s of 10m · cpu 29m45s (9 processes)"
            ),
            "{l}"
        );
        assert!(
            !l.contains('⚠') && !l.contains("no data") && !l.contains("quiet"),
            "{l}"
        );
        assert!(l.ends_with("ctrl+c to interrupt"), "{l}");
        assert_eq!(line.spans[0].style, theme.fg(Token::Running));
        // A runnable process with no measurable CPU yet is alive too.
        let mut b = running(t0, "make");
        for i in 1..=20u64 {
            b.apply(
                &progress("t1", None, Some((0, 2, 1))),
                t0 + Duration::from_secs(i * 3),
            );
        }
        assert_eq!(b.stall(t0 + Duration::from_secs(61), after), None);
        assert_eq!(b.tool_timeout(), Some(Duration::from_secs(600)), "kept");
        assert_eq!(b.tool_liveness().map(|l| l.active), Some(1));
    }

    #[test]
    fn idle_tool_gets_a_soft_quiet_hint_naming_it_and_the_cancel_key() {
        let theme = Theme::default();
        let t0 = Instant::now();
        let after = Duration::from_secs(20);
        let mut a = running(t0, "sleep 600");
        // Shell start-up CPU in the first sample, then nothing.
        for i in 1..=20u64 {
            a.apply(
                &progress("t1", Some(600), Some((4, 2, 0))),
                t0 + Duration::from_secs(i * 3),
            );
        }
        // Quiet since the first sample (3s).
        assert_eq!(a.stall(t0 + Duration::from_secs(22), after), None);
        let stall = a.stall(t0 + Duration::from_secs(24), after).unwrap();
        assert_eq!(stall.source, StallSource::Tool);
        assert_eq!(stall.silent, Duration::from_secs(21));
        let now = t0 + Duration::from_secs(62);
        let line = a.line(now, &Spinner::new(true), after, &theme, "ctrl+c");
        let l = text(&line);
        assert!(
            l.starts_with("⠋ running bash: sleep 600 1m02s of 10m · cpu 4ms (2 processes) · bash quiet for 59s (no output, no CPU) · ctrl+c to cancel · turn 1"),
            "{l}"
        );
        // Soft, not an alarm: no warning icon, never claims a hang, the
        // spinner keeps its running colour and the hint is `warn`.
        assert!(
            !l.contains('⚠') && !l.contains("hung") && !l.contains("stalled"),
            "{l}"
        );
        assert!(!l.contains("to interrupt"), "one cancel hint: {l}");
        assert_eq!(line.spans[0].style, theme.fg(Token::Running));
        let hint = line
            .spans
            .iter()
            .find(|s| s.content.contains("quiet for"))
            .unwrap();
        assert_eq!(hint.style, theme.fg(Token::Warn));
        assert_eq!(
            a.title(now, &Spinner::new(true), after),
            "⠋ bash quiet 59s · 1m02s — harness"
        );
        // Output resets the quiet clock.
        a.tool_output(1, now);
        assert_eq!(a.stall(now + Duration::from_secs(19), after), None);
        // CPU progress resets it too.
        let later = now + Duration::from_secs(30);
        assert!(a.stall(later, after).is_some());
        a.apply(&progress("t1", None, Some((90, 2, 0))), later);
        assert_eq!(a.stall(later + Duration::from_secs(19), after), None);
        // So does a change in the process count (a child started).
        let later = later + Duration::from_secs(40);
        assert!(a.stall(later, after).is_some());
        a.apply(&progress("t1", None, Some((90, 3, 0))), later);
        assert_eq!(a.stall(later + Duration::from_secs(1), after), None);
    }

    #[test]
    fn quiet_needs_two_samples_whatever_stall_after_says() {
        let t0 = Instant::now();
        let mut a = running(t0, "sleep 9");
        a.apply(
            &progress("t1", None, Some((0, 2, 0))),
            t0 + Duration::from_secs(3),
        );
        let after = Duration::from_secs(1);
        assert_eq!(a.stall(t0 + Duration::from_secs(5), after), None);
        assert!(a.stall(t0 + Duration::from_secs(6), after).is_some());
    }

    #[test]
    fn unknown_liveness_falls_back_to_elapsed_and_timeout() {
        let theme = Theme::default();
        let t0 = Instant::now();
        let after = Duration::from_secs(20);
        let mut a = running(t0, "ssh-only-host-command");
        // Progress for another call is ignored.
        a.apply(&progress("other", Some(5), Some((0, 1, 0))), t0);
        assert_eq!(a.tool_timeout(), Some(Duration::from_secs(600)));
        assert_eq!(a.tool_liveness(), None);
        let now = t0 + Duration::from_secs(252);
        assert_eq!(a.stall(now, after), None);
        let l = text(&a.line(now, &Spinner::new(true), after, &theme, "ctrl+c"));
        assert!(
            l.starts_with(
                "⠋ running bash: ssh-only-host-command 4m12s of 10m · no output for 252s · turn 1"
            ),
            "{l}"
        );
        assert!(!l.contains('⚠') && !l.contains("quiet"), "{l}");
        // Before stall_after there is no note at all, and with no timeout
        // reported only the elapsed time shows.
        let mut b = Activity::new(t0);
        b.start_turn(t0);
        b.apply(&tool_call(2, "t2", "true"), t0);
        let l = text(&b.line(
            t0 + Duration::from_secs(5),
            &Spinner::new(true),
            after,
            &theme,
            "",
        ));
        assert_eq!(l, "⠋ running bash: true 5.0s · turn 1 · 5.0s");
        // A new call forgets the previous call's timeout and liveness.
        let mut c = running(t0, "a");
        c.apply(&progress("t1", None, Some((5, 1, 0))), t0);
        c.apply(
            &ev(
                3,
                Some("main"),
                EventKind::ToolResult {
                    call_id: "t1".into(),
                    ok: true,
                    output: Payload::text(""),
                    duration_ms: 1,
                },
            ),
            t0,
        );
        c.apply(&tool_call(4, "t3", "b"), t0);
        assert_eq!((c.tool_timeout(), c.tool_liveness()), (None, None));
    }

    fn read_call(seq: u64, call_id: &str, path: &str) -> RunEvent {
        ev(
            seq,
            Some("main"),
            EventKind::ToolCall {
                call_id: call_id.into(),
                tool: "read".into(),
                args: Payload::Json(serde_json::json!({ "path": path })),
            },
        )
    }

    #[test]
    fn progress_from_an_earlier_call_of_the_batch_makes_it_the_running_tool() {
        let theme = Theme::default();
        let t0 = Instant::now();
        let after = Duration::from_secs(20);
        let mut a = Activity::new(t0);
        a.start_turn(t0);
        a.apply(
            &ev(1, Some("main"), started("main", NodeKind::Agent, None)),
            t0,
        );
        // The loop records every call of the response before running any.
        a.apply(&tool_call(2, "t1", "cargo test --workspace"), t0);
        a.apply(&read_call(3, "t2", "Cargo.toml"), t0);
        // bash reports its timeout when it starts, then samples.
        let t1 = t0 + Duration::from_secs(1);
        a.apply(&progress("t1", Some(600), None), t1);
        let mut cpu = 0;
        for i in 1..=170u64 {
            cpu += 10_500;
            a.apply(
                &progress("t1", None, Some((cpu, 9, 3))),
                t1 + Duration::from_secs(i * 3),
            );
        }
        let now = t1 + Duration::from_millis(510_500);
        assert_eq!(a.stall(now, after), None);
        let l = text(&a.line(now, &Spinner::new(true), after, &theme, "ctrl+c"));
        assert!(
            l.starts_with(
                "⠋ running bash: cargo test --workspace 8m30s of 10m · cpu 29m45s (9 processes)"
            ),
            "{l}"
        );
        assert!(!l.contains("read") && !l.contains("no output"), "{l}");
        // An idle earlier call is flagged too.
        let mut b = Activity::new(t0);
        b.start_turn(t0);
        b.apply(&tool_call(2, "t1", "sleep 600"), t0);
        b.apply(&tool_call(3, "t2", "git status"), t0);
        b.apply(&progress("t1", Some(600), Some((4, 2, 0))), t0);
        for i in 1..=10u64 {
            b.apply(
                &progress("t1", None, Some((4, 2, 0))),
                t0 + Duration::from_secs(i * 3),
            );
        }
        let stall = b.stall(t0 + Duration::from_secs(30), after).unwrap();
        assert_eq!(stall.source, StallSource::Tool);
        let l = text(&b.line(
            t0 + Duration::from_secs(30),
            &Spinner::new(true),
            after,
            &theme,
            "ctrl+c",
        ));
        assert!(
            l.starts_with("⠋ running bash: sleep 600 30.0s of 10m"),
            "{l}"
        );
        assert!(l.contains("bash quiet for"), "{l}");
        // Once a call has its result, its late progress is ignored.
        let mut c = Activity::new(t0);
        c.start_turn(t0);
        c.apply(&tool_call(2, "t1", "a"), t0);
        c.apply(&tool_call(3, "t2", "b"), t0);
        c.apply(&progress("t2", Some(600), None), t0);
        c.apply(
            &ev(
                4,
                Some("main"),
                EventKind::ToolResult {
                    call_id: "t1".into(),
                    ok: true,
                    output: Payload::text(""),
                    duration_ms: 1,
                },
            ),
            t0,
        );
        c.apply(&tool_call(5, "t3", "c"), t0);
        c.apply(&progress("t1", Some(5), Some((9, 1, 1))), t0);
        assert_eq!((c.tool_timeout(), c.tool_liveness()), (None, None));
        assert_eq!(c.phase().label(Duration::ZERO), "running bash: c");
    }

    #[test]
    fn approval_resumes_the_interrupted_tool_with_its_progress() {
        let theme = Theme::default();
        let t0 = Instant::now();
        let after = Duration::from_secs(8);
        let mut a = running(t0, "spin");
        // The UI's prompt, then the event (which must not overwrite the
        // saved phase).
        a.enter_approval("spin".into(), t0);
        a.apply(
            &ev(
                3,
                Some("main"),
                EventKind::ApprovalRequested {
                    approval_id: "a1".into(),
                    action: "spin".into(),
                    preview: None,
                },
            ),
            t0,
        );
        assert_eq!(a.stall(t0 + Duration::from_secs(60), after), None);
        let t1 = t0 + Duration::from_secs(2);
        a.approval_answered(t1);
        assert_eq!(a.phase().label(Duration::ZERO), "running bash: spin");
        assert_eq!(a.tool_timeout(), Some(Duration::from_secs(600)), "kept");
        for i in 1..=5u64 {
            a.apply(
                &progress("t1", None, Some((i * 2990, 1, 1))),
                t1 + Duration::from_secs(i * 3),
            );
        }
        let now = t1 + Duration::from_secs(15);
        assert_eq!(a.stall(now, after), None);
        let l = text(&a.line(now, &Spinner::new(true), after, &theme, "ctrl+c"));
        assert!(
            l.starts_with("⠋ running bash: spin 15.0s of 10m · cpu 14.9s (1 process)"),
            "{l}"
        );
        assert!(!l.contains("waiting on the model"), "{l}");
        // A late approval_resolved changes nothing.
        a.apply(
            &ev(
                4,
                Some("main"),
                EventKind::ApprovalResolved {
                    approval_id: "a1".into(),
                    decision: harness_events::schema::ApprovalDecision::Approved,
                    by: "user".into(),
                },
            ),
            now,
        );
        assert_eq!(a.phase().label(Duration::ZERO), "running bash: spin");
        // Answering with nothing saved falls back to sending; answering
        // when not waiting is a no-op.
        let mut b = Activity::new(t0);
        b.start_turn(t0);
        b.set_phase(Phase::WaitingApproval { action: "x".into() }, t0);
        b.approval_answered(t0);
        assert_eq!(b.phase(), &Phase::Sending);
        let mut c = running(t0, "z");
        c.approval_answered(t0);
        assert_eq!(c.phase().label(Duration::ZERO), "running bash: z");
        // A prompt that overtakes its tool_call event resumes that call.
        let mut d = Activity::new(t0);
        d.start_turn(t0);
        d.enter_approval("w".into(), t0);
        d.apply(&tool_call(2, "t9", "w"), t0);
        assert!(matches!(d.phase(), Phase::WaitingApproval { .. }));
        d.approval_answered(t0);
        d.apply(&progress("t9", Some(600), None), t0);
        assert_eq!(d.phase().label(Duration::ZERO), "running bash: w");
        assert_eq!(d.tool_timeout(), Some(Duration::from_secs(600)));
    }

    #[test]
    fn limits_print_whole_units() {
        assert_eq!(fmt_limit(Duration::from_secs(600)), "10m");
        assert_eq!(fmt_limit(Duration::from_secs(7200)), "2h");
        assert_eq!(fmt_limit(Duration::from_secs(90)), "1m30s");
        assert_eq!(fmt_limit(Duration::from_secs(45)), "45.0s");
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
