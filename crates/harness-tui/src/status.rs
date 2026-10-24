//! The status line (footer).
//!
//! ```text
//! ~/CODE/harness  main │ sonnet · auto-edit │ ctx ▓▓▓▓░░░░ 41% │ ↑182k ↓14k cache 93% │ $0.84 │ verify ✔ │ ◐ 3 agents │ ⧉ gpu-box
//! ```
//!
//! [`StatusLine`] keeps the session totals and is updated by folding run
//! events with [`StatusLine::apply`]; it never polls. The working directory,
//! git branch, context window and autonomy mode are set by the owner when
//! they change. When the line is too narrow, the least important segments
//! are dropped first (remotes, path, tokens, verify, agents).
//!
//! When an MCP server has failed, a compact indicator (`⚠ mcp docs`, or
//! `⚠ mcp 2 failed`) shows near the front; the chat registers it as a
//! button that opens `/mcp` ([`MCP_INDICATOR_ID`]).
//!
//! While background bash jobs run (folded from `bash_job` events), a
//! `⚙ 2 jobs` indicator shows; the chat registers it as a button that opens
//! `/jobs` ([`JOBS_INDICATOR_ID`]).

use std::collections::BTreeMap;
use std::path::Path;

use harness_events::schema::{EventKind, NodeId, NodeKind, RunEvent, Usage};
use harness_policy::rules::Mode;
use ratatui::text::{Line, Span};

use crate::chat::text::{cache_pct, fmt_cost, fmt_tokens, truncate, width};
use crate::indicators::Spinner;
use crate::theme::{Theme, Token};

/// State of the verify command.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum VerifyState {
    /// Not run yet.
    #[default]
    None,
    /// Running.
    Running,
    /// Last run passed.
    Passed,
    /// Last run failed.
    Failed,
}

/// Interaction id of the MCP indicator (clicking it opens `/mcp`).
pub const MCP_INDICATOR_ID: &str = "status.mcp";

/// Interaction id of the background-jobs indicator (clicking it opens `/jobs`).
pub const JOBS_INDICATOR_ID: &str = "status.jobs";

/// Width of the context usage bar in cells.
pub const CONTEXT_BAR_CELLS: usize = 8;

/// Session totals and state shown in the footer.
#[derive(Debug, Clone, PartialEq)]
pub struct StatusLine {
    /// Working directory as shown (home shortened to `~`).
    pub cwd: String,
    /// Git branch, if any.
    pub branch: Option<String>,
    /// Model of the main agent.
    pub model: Option<String>,
    /// Reasoning effort of the main agent (`effort low`, `no reasoning`),
    /// shown after the model; `None` when no effort is set.
    pub effort: Option<String>,
    /// Autonomy mode.
    pub mode: Mode,
    /// Context window of the model, in tokens.
    pub context_window: Option<u64>,
    /// Tokens in the last request (prompt + output).
    pub context_used: u64,
    /// Session token totals.
    pub tokens: Usage,
    /// Session cost; `None` until a cost is known.
    pub cost: Option<f64>,
    /// Verify state.
    pub verify: VerifyState,
    /// Goal-bar progress, shown when the bar is collapsed
    /// (`GoalBar::indicator`).
    pub goal: Option<Vec<Span<'static>>>,
    /// MCP servers that failed (from the core), by name.
    pub mcp_failed: Vec<String>,
    running: BTreeMap<NodeId, Option<String>>,
    /// Running background jobs by (starting call, job id): job ids of
    /// separate managers (another run) may repeat.
    jobs: std::collections::BTreeSet<(String, String)>,
}

impl StatusLine {
    /// A status line for `cwd` in `mode`. `home` shortens the path to `~`.
    pub fn new(cwd: &Path, home: Option<&Path>, mode: Mode) -> StatusLine {
        StatusLine {
            cwd: display_path(cwd, home),
            branch: None,
            model: None,
            effort: None,
            mode,
            context_window: None,
            context_used: 0,
            tokens: Usage::default(),
            cost: None,
            verify: VerifyState::None,
            goal: None,
            mcp_failed: Vec::new(),
            running: BTreeMap::new(),
            jobs: std::collections::BTreeSet::new(),
        }
    }

    /// Sets the MCP servers that failed (empty hides the indicator).
    pub fn set_mcp_failed(&mut self, failed: Vec<String>) {
        self.mcp_failed = failed;
    }

    /// The MCP indicator text, when a server has failed: `⚠ mcp <name>`
    /// for one server, `⚠ mcp <n> failed` for several.
    pub fn mcp_label(&self) -> Option<String> {
        match self.mcp_failed.as_slice() {
            [] => None,
            [one] => Some(format!("⚠ mcp {}", truncate(one, 16))),
            many => Some(format!("⚠ mcp {} failed", many.len())),
        }
    }

    /// Number of running background bash jobs.
    pub fn running_jobs(&self) -> usize {
        self.jobs.len()
    }

    /// The background-jobs indicator text while jobs run: `⚙ 1 job`,
    /// `⚙ 3 jobs`.
    pub fn jobs_label(&self) -> Option<String> {
        match self.jobs.len() {
            0 => None,
            1 => Some("⚙ 1 job".to_owned()),
            n => Some(format!("⚙ {n} jobs")),
        }
    }

    /// Shows (or hides) the goal-bar progress indicator.
    pub fn set_goal_indicator(&mut self, goal: Option<Vec<Span<'static>>>) {
        self.goal = goal;
    }

    /// Sets the git branch.
    pub fn set_branch(&mut self, branch: Option<String>) {
        self.branch = branch;
    }

    /// Sets the autonomy mode (after a palette change).
    pub fn set_mode(&mut self, mode: Mode) {
        self.mode = mode;
    }

    /// Sets the model and its context window (from the model registry).
    pub fn set_model(&mut self, model: impl Into<String>, context_window: Option<u64>) {
        self.model = Some(model.into());
        self.context_window = context_window;
    }

    /// Sets the reasoning-effort label shown after the model (`None`
    /// hides it).
    pub fn set_effort(&mut self, effort: Option<String>) {
        self.effort = effort;
    }

    /// Number of running agents.
    pub fn running_agents(&self) -> usize {
        self.running.len()
    }

    /// Distinct remote hosts with running agents.
    pub fn active_remotes(&self) -> Vec<String> {
        let mut hosts: Vec<String> = Vec::new();
        for h in self.running.values().flatten() {
            if !hosts.contains(h) {
                hosts.push(h.clone());
            }
        }
        hosts
    }

    /// Context usage in percent, when the window is known.
    pub fn context_pct(&self) -> Option<u64> {
        let w = self.context_window.filter(|w| *w > 0)?;
        Some((self.context_used * 100 / w).min(100))
    }

    /// Folds one run event.
    pub fn apply(&mut self, ev: &RunEvent) {
        match &ev.kind {
            EventKind::NodeStarted { kind, host, .. } => {
                if matches!(kind, NodeKind::Agent | NodeKind::Subagent)
                    && let Some(id) = &ev.node_id
                {
                    self.running.insert(id.clone(), host.clone());
                }
            }
            EventKind::NodeFinished { .. } => {
                if let Some(id) = &ev.node_id {
                    self.running.remove(id);
                }
            }
            EventKind::ModelCallStarted { model, .. } => {
                if ev.parent_node_id.is_none() && self.model.is_none() {
                    self.model = Some(model.clone());
                }
            }
            EventKind::ModelCallFinished { usage, .. } => {
                self.tokens.add(usage);
                if ev.parent_node_id.is_none() {
                    self.context_used = usage.input + usage.cached_input + usage.output;
                }
            }
            EventKind::Cost { usd } => self.cost = Some(self.cost.unwrap_or(0.0) + usd),
            EventKind::BashJob {
                call_id,
                job_id,
                state,
                ..
            } => {
                let key = (call_id.clone(), job_id.clone());
                if state == "running" {
                    self.jobs.insert(key);
                } else {
                    self.jobs.remove(&key);
                }
            }
            EventKind::VerifyStarted { .. } => self.verify = VerifyState::Running,
            EventKind::VerifyFinished { passed, .. } => {
                self.verify = if *passed {
                    VerifyState::Passed
                } else {
                    VerifyState::Failed
                };
            }
            _ => {}
        }
    }

    /// The segments in display order, with their priority (lower is
    /// dropped first) and the button id of the clickable ones.
    fn segments(&self, theme: &Theme, spinner: &Spinner) -> Vec<Segment> {
        // (priority: lower is dropped first, spans)
        let muted = theme.fg(Token::Muted);
        let num = theme.fg(Token::Number);
        let mut segs: Vec<(u8, Vec<Span<'static>>)> = Vec::new();
        let mut loc = vec![Span::styled(self.cwd.clone(), theme.fg(Token::Path))];
        if let Some(b) = &self.branch {
            loc.push(Span::styled(format!("  {b}"), theme.fg(Token::Keyword)));
        }
        segs.push((3, loc));
        let mut model = vec![Span::styled(
            self.model.clone().unwrap_or_else(|| "no model".into()),
            theme.fg(Token::Agent),
        )];
        if let Some(effort) = &self.effort {
            model.push(Span::styled(" · ", muted));
            model.push(Span::styled(effort.clone(), theme.fg(Token::Keyword)));
        }
        model.push(Span::styled(" · ", muted));
        model.push(Span::styled(
            self.mode.as_str().to_owned(),
            theme.fg(Token::Accent),
        ));
        segs.push((9, model));
        let mcp_at = segs.len();
        if let Some(label) = self.mcp_label() {
            segs.push((9, vec![Span::styled(label, theme.fg(Token::Error))]));
        }
        if let Some(pct) = self.context_pct() {
            let filled = (pct as usize * CONTEXT_BAR_CELLS)
                .div_ceil(100)
                .min(CONTEXT_BAR_CELLS);
            let token = if pct >= 85 {
                Token::Error
            } else if pct >= 60 {
                Token::Warn
            } else {
                Token::Ok
            };
            segs.push((
                8,
                vec![
                    Span::styled("ctx ", muted),
                    Span::styled("▓".repeat(filled), theme.fg(token)),
                    Span::styled("░".repeat(CONTEXT_BAR_CELLS - filled), muted),
                    Span::styled(format!(" {pct}%"), num),
                ],
            ));
        }
        let u = self.tokens;
        let mut tok = vec![
            Span::styled(format!("↑{}", fmt_tokens(u.input + u.cached_input)), num),
            Span::styled(format!(" ↓{}", fmt_tokens(u.output)), num),
        ];
        if let Some(p) = cache_pct(u.input, u.cached_input) {
            tok.push(Span::styled(format!(" cache {p}%"), muted));
        }
        if u.cache_write > 0 {
            tok.push(Span::styled(
                format!(" wrote {}", fmt_tokens(u.cache_write)),
                muted,
            ));
        }
        segs.push((4, tok));
        segs.push((10, vec![Span::styled(fmt_cost(self.cost), num)]));
        let verify = match self.verify {
            VerifyState::None => None,
            VerifyState::Running => Some((spinner.glyph(), Token::Running)),
            VerifyState::Passed => Some(('✔', Token::Ok)),
            VerifyState::Failed => Some(('✗', Token::Error)),
        };
        if let Some((icon, token)) = verify {
            segs.push((
                5,
                vec![
                    Span::styled("verify ", muted),
                    Span::styled(icon.to_string(), theme.fg(token)),
                ],
            ));
        }
        let goal_at = segs.len();
        if let Some(goal) = &self.goal {
            segs.push((7, goal.clone()));
        }
        let agents = self.running_agents();
        if agents > 0 {
            let s = if agents == 1 { "" } else { "s" };
            segs.push((
                6,
                vec![Span::styled(
                    format!("{} {agents} agent{s}", spinner.glyph()),
                    theme.fg(Token::Running),
                )],
            ));
        }
        let jobs_at = segs.len();
        if let Some(label) = self.jobs_label() {
            segs.push((8, vec![Span::styled(label, theme.fg(Token::Running))]));
        }
        let remotes = self.active_remotes();
        if !remotes.is_empty() {
            segs.push((
                2,
                vec![Span::styled(
                    format!("⧉ {}", remotes.join(", ")),
                    theme.fg(Token::Running),
                )],
            ));
        }
        segs.into_iter()
            .enumerate()
            .map(|(i, (prio, spans))| Segment {
                prio,
                spans,
                button: if i == mcp_at && self.mcp_label().is_some() {
                    Some(MCP_INDICATOR_ID)
                } else if i == jobs_at && self.jobs_label().is_some() {
                    Some(JOBS_INDICATOR_ID)
                } else if i == goal_at && self.goal.is_some() {
                    Some(GOAL_INDICATOR_ID)
                } else {
                    None
                },
            })
            .collect()
    }

    /// Renders the footer on one row of `width` cells, dropping the least
    /// important segments that do not fit. The chat uses [`Self::layout`],
    /// which wraps onto more rows instead.
    pub fn line(&self, theme: &Theme, spinner: &Spinner, width: usize) -> Line<'static> {
        let mut segs: Vec<(u8, Vec<Span<'static>>)> = self
            .segments(theme, spinner)
            .into_iter()
            .map(|s| (s.prio, s.spans))
            .collect();
        let sep_w = 3;
        let total = |segs: &[(u8, Vec<Span<'static>>)]| -> usize {
            segs.iter()
                .map(|(_, s)| s.iter().map(|x| self::width(&x.content)).sum::<usize>())
                .sum::<usize>()
                + sep_w * segs.len().saturating_sub(1)
        };
        while total(&segs) > width && segs.len() > 1 {
            let (idx, _) = segs
                .iter()
                .enumerate()
                .min_by_key(|(_, (p, _))| *p)
                .expect("non-empty");
            segs.remove(idx);
        }
        let mut spans = Vec::new();
        for (i, (_, s)) in segs.into_iter().enumerate() {
            if i > 0 {
                spans.push(Span::styled(" │ ", theme.fg(Token::Border)));
            }
            spans.extend(s);
        }
        let line = Line::from(spans);
        if crate::chat::text::line_width(&line) > width {
            crate::chat::text::truncate_line(line, width)
        } else {
            line
        }
    }
}

impl StatusLine {
    /// Lays the status line out at `width` on at most `max_rows` rows:
    /// segments wrap onto more rows rather than being cut. When they still
    /// do not fit, the least important ones are left out and a `+N more`
    /// button ([`STATUS_MORE_ID`]) ends the last row; it opens the full
    /// status ([`Self::details`]).
    pub fn layout(
        &self,
        theme: &Theme,
        spinner: &Spinner,
        width: usize,
        max_rows: usize,
    ) -> StatusLayout {
        let max_rows = max_rows.max(1);
        let mut segs = self.segments(theme, spinner);
        let all = segs.len();
        loop {
            let hidden = all - segs.len();
            let mut shown: Vec<Segment> = Vec::new();
            for s in &segs {
                shown.push(Segment {
                    prio: s.prio,
                    spans: s.spans.clone(),
                    button: s.button,
                });
            }
            if hidden > 0 {
                shown.push(Segment {
                    prio: u8::MAX,
                    spans: vec![Span::styled(
                        format!("+{hidden} more"),
                        theme.fg(Token::Link),
                    )],
                    button: Some(STATUS_MORE_ID),
                });
            }
            let (lines, buttons) = pack(&shown, width, theme);
            if lines.len() <= max_rows || segs.is_empty() {
                let mut lines = lines;
                lines.truncate(max_rows);
                let buttons = buttons
                    .into_iter()
                    .filter(|b| usize::from(b.row) < max_rows)
                    .collect();
                return StatusLayout {
                    lines,
                    buttons,
                    hidden,
                };
            }
            let (idx, _) = segs
                .iter()
                .enumerate()
                .min_by_key(|(_, s)| s.prio)
                .expect("non-empty");
            segs.remove(idx);
        }
    }

    /// The full status, one item per line, for the details panel the
    /// "more" button opens.
    pub fn details(&self, theme: &Theme, spinner: &Spinner) -> Vec<String> {
        let text = |spans: &[Span<'static>]| -> String {
            spans.iter().map(|s| s.content.as_ref()).collect()
        };
        let mut out = vec![
            format!("Directory: {}", self.cwd),
            format!("Branch: {}", self.branch.as_deref().unwrap_or("none")),
        ];
        for s in self.segments(theme, spinner).iter().skip(1) {
            out.push(text(&s.spans));
        }
        if !self.mcp_failed.is_empty() {
            out.push(format!(
                "Failed MCP servers: {}",
                self.mcp_failed.join(", ")
            ));
        }
        out
    }
}

/// `path` with `home` replaced by `~`. Long paths are kept whole: the
/// status line wraps them.
pub fn display_path(path: &Path, home: Option<&Path>) -> String {
    match home.and_then(|h| path.strip_prefix(h).ok()) {
        Some(rest) if rest.as_os_str().is_empty() => "~".to_owned(),
        Some(rest) => format!("~/{}", rest.display()),
        None => path.display().to_string(),
    }
}

/// One status segment.
struct Segment {
    prio: u8,
    spans: Vec<Span<'static>>,
    button: Option<&'static str>,
}

/// Interaction id of the goal-progress indicator (shown while the goal bar
/// is collapsed); clicking it opens the status details with the goal and
/// every task.
pub const GOAL_INDICATOR_ID: &str = "status.goal";

/// Interaction id of the status line's "more" button, shown when segments
/// did not fit; it opens the status details panel.
pub const STATUS_MORE_ID: &str = "status.more";

/// A clickable part of the status line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusButton {
    /// Interaction id ([`MCP_INDICATOR_ID`], [`JOBS_INDICATOR_ID`],
    /// [`GOAL_INDICATOR_ID`], [`STATUS_MORE_ID`]).
    pub id: &'static str,
    /// Row in [`StatusLayout::lines`].
    pub row: u16,
    /// First column.
    pub col: u16,
    /// Width in cells.
    pub width: u16,
}

/// The status line laid out on one or more rows ([`StatusLine::layout`]).
#[derive(Debug, Clone, PartialEq)]
pub struct StatusLayout {
    /// The rows.
    pub lines: Vec<Line<'static>>,
    /// Where the buttons are (a wrapped button has one entry per row).
    pub buttons: Vec<StatusButton>,
    /// Segments left out for lack of rows; the full status is then one
    /// click (or Tab + Enter) away on the "more" button.
    pub hidden: usize,
}

/// Packs segments into rows of `width`: segments are separated by ` │ `,
/// move to the next row when they do not fit and wrap when they are wider
/// than a row.
fn pack(segs: &[Segment], width: usize, theme: &Theme) -> (Vec<Line<'static>>, Vec<StatusButton>) {
    let width = width.max(1);
    let mut rows: Vec<Vec<Span<'static>>> = vec![Vec::new()];
    let mut buttons = Vec::new();
    let mut w = 0usize;
    for seg in segs {
        let sw: usize = seg.spans.iter().map(|s| self::width(&s.content)).sum();
        let sep = if w > 0 { 3 } else { 0 };
        if w + sep + sw <= width {
            if sep > 0 {
                rows.last_mut()
                    .expect("a row")
                    .push(Span::styled(" │ ", theme.fg(Token::Border)));
            }
            if let Some(id) = seg.button {
                buttons.push(StatusButton {
                    id,
                    row: (rows.len() - 1) as u16,
                    col: (w + sep) as u16,
                    width: sw as u16,
                });
            }
            rows.last_mut()
                .expect("a row")
                .extend(seg.spans.iter().cloned());
            w += sep + sw;
            continue;
        }
        if w > 0 {
            rows.push(Vec::new());
        }
        let pieces = crate::chat::text::wrap_line(Line::from(seg.spans.clone()), width, &[]);
        let n = pieces.len();
        for (i, p) in pieces.into_iter().enumerate() {
            let pw = crate::chat::text::line_width(&p);
            if let Some(id) = seg.button {
                buttons.push(StatusButton {
                    id,
                    row: (rows.len() - 1) as u16,
                    col: 0,
                    width: pw as u16,
                });
            }
            rows.last_mut().expect("a row").extend(p.spans);
            w = pw;
            if i + 1 < n {
                rows.push(Vec::new());
            }
        }
    }
    (rows.into_iter().map(Line::from).collect(), buttons)
}

#[cfg(test)]
mod tests {
    use super::*;
    use harness_events::schema::{NodeStatus, Payload, RunId};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::widgets::Paragraph;
    use time::OffsetDateTime;

    fn ev(node: Option<&str>, kind: EventKind) -> RunEvent {
        RunEvent {
            seq: 0,
            ts: OffsetDateTime::UNIX_EPOCH,
            run_id: RunId(uuid::Uuid::nil()),
            node_id: node.map(NodeId::from),
            parent_node_id: None,
            kind,
        }
    }

    fn node(id: &str, kind: NodeKind, host: Option<&str>) -> RunEvent {
        ev(
            Some(id),
            EventKind::NodeStarted {
                kind,
                name: id.into(),
                agent: None,
                model: None,
                host: host.map(Into::into),
                needs: vec![],
                queued: false,
            },
        )
    }

    fn text(l: &Line<'_>) -> String {
        l.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    fn sample() -> StatusLine {
        let mut s = StatusLine::new(
            Path::new("/home/u/CODE/harness"),
            Some(Path::new("/home/u")),
            Mode::AutoEdit,
        );
        s.set_branch(Some("main".into()));
        s.set_model("sonnet", Some(200_000));
        for e in [
            node("main", NodeKind::Agent, None),
            node("a", NodeKind::Subagent, Some("gpu-box")),
            node("b", NodeKind::Subagent, None),
            ev(
                None,
                EventKind::ModelCallFinished {
                    call_id: "c".into(),
                    usage: Usage {
                        input: 12_740,
                        cached_input: 169_260,
                        output: 14_000,
                        reasoning: 0,
                        cache_write: 0,
                    },
                    stop_reason: None,
                    response: None,
                    duration_ms: 1,
                },
            ),
            ev(None, EventKind::Cost { usd: 0.84 }),
            ev(
                None,
                EventKind::VerifyStarted {
                    command: "cargo test".into(),
                },
            ),
            ev(
                None,
                EventKind::VerifyFinished {
                    command: "cargo test".into(),
                    passed: true,
                    exit_code: Some(0),
                    duration_ms: 1,
                    output: Payload::text(""),
                },
            ),
        ] {
            s.apply(&e);
        }
        s
    }

    #[test]
    fn footer_shows_cache_writes() {
        let theme = Theme::default();
        let mut s = sample();
        let l = text(&s.line(&theme, &Spinner::new(false), 200));
        assert!(!l.contains("wrote"), "{l}");
        s.apply(&ev(
            None,
            EventKind::ModelCallFinished {
                call_id: "c2".into(),
                usage: Usage {
                    input: 2_600,
                    cache_write: 2_600,
                    ..Usage::default()
                },
                stop_reason: None,
                response: None,
                duration_ms: 1,
            },
        ));
        let l = text(&s.line(&theme, &Spinner::new(false), 200));
        assert!(l.contains("cache 92% wrote 2.6k │"), "{l}");
    }

    #[test]
    fn footer_matches_the_design() {
        let theme = Theme::default();
        let s = sample();
        let l = text(&s.line(&theme, &Spinner::new(false), 200));
        assert_eq!(
            l,
            "~/CODE/harness  main │ sonnet · auto-edit │ ctx ▓▓▓▓▓▓▓▓ 98% │ ↑182k ↓14k cache 93% │ $0.84 │ verify ✔ │ ◐ 3 agents │ ⧉ gpu-box"
        );
    }

    #[test]
    fn footer_shows_the_reasoning_effort_after_the_model() {
        let theme = Theme::default();
        let mut s = sample();
        s.set_effort(Some("effort low".into()));
        let l = text(&s.line(&theme, &Spinner::new(false), 200));
        assert!(l.contains("│ sonnet · effort low · auto-edit │"), "{l}");
        s.set_effort(None);
        let l = text(&s.line(&theme, &Spinner::new(false), 200));
        assert!(l.contains("│ sonnet · auto-edit │"), "{l}");
    }

    #[test]
    fn updates_live_from_events() {
        let mut s = sample();
        assert_eq!(s.running_agents(), 3);
        s.apply(&ev(
            Some("a"),
            EventKind::NodeFinished {
                status: NodeStatus::Succeeded,
                error: None,
            },
        ));
        assert_eq!(s.running_agents(), 2);
        assert!(s.active_remotes().is_empty());
        s.apply(&ev(
            None,
            EventKind::VerifyStarted {
                command: "x".into(),
            },
        ));
        assert_eq!(s.verify, VerifyState::Running);
        s.apply(&ev(
            None,
            EventKind::VerifyFinished {
                command: "x".into(),
                passed: false,
                exit_code: Some(1),
                duration_ms: 1,
                output: Payload::text(""),
            },
        ));
        assert_eq!(s.verify, VerifyState::Failed);
        s.apply(&ev(None, EventKind::Cost { usd: 0.16 }));
        assert!((s.cost.unwrap() - 1.0).abs() < 1e-9);
        let mut fresh = StatusLine::new(Path::new("/x"), None, Mode::Ask);
        assert!(text(&fresh.line(&Theme::default(), &Spinner::new(true), 200)).contains("$?"));
        fresh.apply(&ev(
            None,
            EventKind::ModelCallStarted {
                call_id: "c".into(),
                provider: "fake".into(),
                model: "haiku".into(),
                request: Payload::text(""),
            },
        ));
        assert_eq!(fresh.model.as_deref(), Some("haiku"));
    }

    #[test]
    fn uses_theme_colors_and_context_bar_levels() {
        let theme = Theme::default();
        let mut s = sample();
        let line = s.line(&theme, &Spinner::new(false), 200);
        let bar = line
            .spans
            .iter()
            .find(|x| x.content.starts_with('▓'))
            .unwrap();
        assert_eq!(bar.style, theme.fg(Token::Error));
        let cost = line.spans.iter().find(|x| x.content == "$0.84").unwrap();
        assert_eq!(cost.style, theme.fg(Token::Number));
        let cwd = line
            .spans
            .iter()
            .find(|x| x.content == "~/CODE/harness")
            .unwrap();
        assert_eq!(cwd.style, theme.fg(Token::Path));
        s.context_used = 40_000;
        let line = s.line(&theme, &Spinner::new(false), 200);
        let bar = line
            .spans
            .iter()
            .find(|x| x.content.starts_with('▓'))
            .unwrap();
        assert_eq!(bar.style, theme.fg(Token::Ok));
        assert!(text(&line).contains("ctx ▓▓░░░░░░ 20%"));
    }

    #[test]
    fn mcp_indicator_is_compact_red_and_survives_narrow_terminals() {
        let theme = Theme::default();
        let mut s = sample();
        assert!(!text(&s.line(&theme, &Spinner::new(false), 200)).contains("mcp"));
        s.set_mcp_failed(vec!["a-very-long-server-name-indeed".into()]);
        let label = s.mcp_label().unwrap();
        assert!(width(&label) <= 22, "{label}");
        let line = s.line(&theme, &Spinner::new(false), 60);
        let span = line.spans.iter().find(|x| x.content == label).unwrap();
        assert_eq!(span.style, theme.fg(Token::Error));
        assert!(text(&line).contains(&label), "kept at 60 cells");
        s.set_mcp_failed(vec!["a".into(), "b".into(), "c".into()]);
        assert_eq!(s.mcp_label().as_deref(), Some("⚠ mcp 3 failed"));
    }

    fn job(id: &str, state: &str) -> RunEvent {
        job_of("c1", id, state)
    }

    fn job_of(call: &str, id: &str, state: &str) -> RunEvent {
        ev(
            Some("main"),
            EventKind::BashJob {
                call_id: call.into(),
                job_id: id.into(),
                command: "cargo test".into(),
                state: state.into(),
                exit_code: None,
                duration_ms: None,
                output: None,
            },
        )
    }

    #[test]
    fn jobs_indicator_counts_running_background_jobs() {
        let theme = Theme::default();
        let mut s = sample();
        assert_eq!(s.jobs_label(), None);
        s.apply(&job("j1", "running"));
        assert_eq!(s.jobs_label().as_deref(), Some("⚙ 1 job"));
        s.apply(&job("j2", "running"));
        assert_eq!(s.running_jobs(), 2);
        let line = s.line(&theme, &Spinner::new(false), 70);
        let span = line.spans.iter().find(|x| x.content == "⚙ 2 jobs").unwrap();
        assert_eq!(span.style, theme.fg(Token::Running));
        assert!(text(&line).contains("⚙ 2 jobs"), "kept at 70 cells");
        s.apply(&job("j1", "exited"));
        s.apply(&job("j2", "stopped"));
        assert_eq!(s.jobs_label(), None);
        assert!(!text(&s.line(&theme, &Spinner::new(false), 200)).contains('⚙'));
    }

    #[test]
    fn jobs_indicator_keys_jobs_by_call_and_id() {
        let mut s = sample();
        // Two runs (separate managers) may both have a `j1`.
        s.apply(&job_of("c1", "j1", "running"));
        s.apply(&job_of("c9", "j1", "running"));
        assert_eq!(s.running_jobs(), 2);
        s.apply(&job_of("c9", "j1", "stopped"));
        assert_eq!(s.jobs_label().as_deref(), Some("⚙ 1 job"));
        s.apply(&job_of("c1", "j1", "exited"));
        assert_eq!(s.jobs_label(), None);
    }

    #[test]
    fn narrow_terminals_drop_low_priority_segments() {
        let theme = Theme::default();
        let s = sample();
        let l = text(&s.line(&theme, &Spinner::new(false), 60));
        assert!(width(&l) <= 60, "{l}");
        assert!(l.contains("sonnet · auto-edit"));
        assert!(l.contains("$0.84"));
        assert!(!l.contains("gpu-box"));
        let mut term = Terminal::new(TestBackend::new(30, 1)).unwrap();
        term.draw(|f| {
            f.render_widget(
                Paragraph::new(s.line(&theme, &Spinner::new(false), 30)),
                f.area(),
            )
        })
        .unwrap();
    }

    #[test]
    fn paths_are_shortened() {
        assert_eq!(
            display_path(Path::new("/home/u"), Some(Path::new("/home/u"))),
            "~"
        );
        assert_eq!(
            display_path(Path::new("/srv/x"), Some(Path::new("/home/u"))),
            "/srv/x"
        );
    }

    fn rows(l: &StatusLayout) -> Vec<String> {
        l.lines.iter().map(text).collect()
    }

    #[test]
    fn layout_wraps_long_values_onto_more_rows_instead_of_cutting() {
        let theme = Theme::default();
        let long = "/private/var/folders/n6/hn37q6pj7sj_9dm0000gn/T/.tmpXyZ/project/src_end";
        let mut s = sample();
        s.cwd = long.into();
        let l = s.layout(&theme, &Spinner::new(false), 40, 6);
        assert_eq!(l.hidden, 0, "{:?}", rows(&l));
        assert!(l.lines.len() > 1);
        for r in rows(&l) {
            assert!(width(&r) <= 40, "{r}");
            assert!(!r.contains('…'), "nothing is cut: {r}");
        }
        let joined: String = rows(&l).concat();
        assert!(joined.contains(long), "{joined}");
        assert!(joined.contains("gpu-box"), "remotes are kept: {joined}");
        // Wide enough, it is one row like `line`.
        let wide = s.layout(&theme, &Spinner::new(false), 400, 3);
        assert_eq!(wide.lines.len(), 1);
    }

    #[test]
    fn layout_offers_more_when_rows_run_out_and_details_have_everything() {
        let theme = Theme::default();
        let mut s = sample();
        s.cwd = "/a/very/long/path/that/needs/rows/of/its/own/to/show/in/full_end".into();
        s.set_mcp_failed(vec!["docs".into()]);
        s.apply(&job("j1", "running"));
        let l = s.layout(&theme, &Spinner::new(false), 30, 2);
        assert_eq!(l.lines.len(), 2, "{:?}", rows(&l));
        assert!(l.hidden > 0);
        let more = format!("+{} more", l.hidden);
        assert!(rows(&l).concat().contains(&more), "{:?}", rows(&l));
        let b = l
            .buttons
            .iter()
            .find(|b| b.id == STATUS_MORE_ID)
            .expect("more button");
        let row = &rows(&l)[usize::from(b.row)];
        let cut: String = row
            .chars()
            .skip(usize::from(b.col))
            .take(more.chars().count())
            .collect();
        assert_eq!(cut, more, "button covers its label");
        // The MCP and jobs indicators keep their buttons where they show.
        for b in &l.buttons {
            assert!(usize::from(b.row) < l.lines.len());
        }
        let details = s.details(&theme, &Spinner::new(false));
        let all = details.join("\n");
        for want in [
            "/a/very/long/path/that/needs/rows/of/its/own/to/show/in/full_end",
            "Branch: main",
            "sonnet · auto-edit",
            "$0.84",
            "gpu-box",
            "⚙ 1 job",
            "Failed MCP servers: docs",
        ] {
            assert!(all.contains(want), "{want}: {all}");
        }
    }
}
