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
}

impl StatusLine {
    /// A status line for `cwd` in `mode`. `home` shortens the path to `~`.
    pub fn new(cwd: &Path, home: Option<&Path>, mode: Mode) -> StatusLine {
        StatusLine {
            cwd: display_path(cwd, home),
            branch: None,
            model: None,
            mode,
            context_window: None,
            context_used: 0,
            tokens: Usage::default(),
            cost: None,
            verify: VerifyState::None,
            goal: None,
            mcp_failed: Vec::new(),
            running: BTreeMap::new(),
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

    /// Renders the footer at `width` cells.
    pub fn line(&self, theme: &Theme, spinner: &Spinner, width: usize) -> Line<'static> {
        // (priority: lower is dropped first, spans)
        let muted = theme.fg(Token::Muted);
        let num = theme.fg(Token::Number);
        let mut segs: Vec<(u8, Vec<Span<'static>>)> = Vec::new();
        let mut loc = vec![Span::styled(self.cwd.clone(), theme.fg(Token::Path))];
        if let Some(b) = &self.branch {
            loc.push(Span::styled(format!("  {b}"), theme.fg(Token::Keyword)));
        }
        segs.push((3, loc));
        segs.push((
            9,
            vec![
                Span::styled(
                    self.model.clone().unwrap_or_else(|| "no model".into()),
                    theme.fg(Token::Agent),
                ),
                Span::styled(" · ", muted),
                Span::styled(self.mode.as_str().to_owned(), theme.fg(Token::Accent)),
            ],
        ));
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

/// `path` with `home` replaced by `~`, truncated to 40 cells.
pub fn display_path(path: &Path, home: Option<&Path>) -> String {
    let s = match home.and_then(|h| path.strip_prefix(h).ok()) {
        Some(rest) if rest.as_os_str().is_empty() => "~".to_owned(),
        Some(rest) => format!("~/{}", rest.display()),
        None => path.display().to_string(),
    };
    truncate(&s, 40)
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
}
