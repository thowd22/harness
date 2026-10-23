//! Run history: find past runs, open them in the map, replay them and
//! print them for `harness runs show`.
//!
//! [`load_runs`] folds every run under `<data_dir>/runs` with the shared
//! reducer into a [`RunSummary`] (status, workflow or agent, start,
//! duration, cost, host). [`HistoryView`] lists them newest first; the
//! palette filters the list ([`palette_entries`], [`HistoryView::run_palette`])
//! and Enter or a click opens a run ([`HistoryOutput::Open`]).
//! [`replay`] steps through a run's events, and [`show`] renders the text
//! tree and JSON of `harness runs show`.

pub mod replay;
pub mod show;

use std::path::Path;

use crossterm::event::{Event, KeyCode, KeyEventKind, MouseEventKind};
use harness_events::names;
use harness_events::reducer::RunState;
use harness_events::schema::{NodeKind, RunEvent, RunId, RunStatus};
use harness_events::store::{list_runs, read_run, run_dir};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Modifier;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use time::{OffsetDateTime, UtcOffset};

use crate::chat::text::{fmt_cost, fmt_ms, truncate, width};
use crate::interaction::{Dispatch, ElementId, HitMap, Interaction};
use crate::palette::{ArgSpec, EntryKind as PaletteKind, Invocation, PaletteEntry};
use crate::theme::{Theme, Token};

/// Palette ids of the history view.
pub mod ids {
    /// Filter the run list (argument: query).
    pub const FILTER: &str = "history.filter";
    /// Clear the filter.
    pub const CLEAR: &str = "history.clear_filter";
    /// Replay the selected run.
    pub const REPLAY: &str = "history.replay";
}

/// Palette entries offered while the history is shown.
pub fn palette_entries() -> Vec<PaletteEntry> {
    vec![
        PaletteEntry::new(
            ids::FILTER,
            "Filter runs",
            "Show runs matching a query",
            PaletteKind::Builtin,
        )
        .with_args(vec![ArgSpec::required("query")]),
        PaletteEntry::new(
            ids::CLEAR,
            "Clear run filter",
            "Show every run",
            PaletteKind::Builtin,
        ),
        PaletteEntry::new(
            ids::REPLAY,
            "Replay run",
            "Step through the selected run",
            PaletteKind::Builtin,
        ),
    ]
}

/// Longest title, in characters, inside a [`RunSummary::label`].
pub const LABEL_TITLE_MAX: usize = 24;

/// Width of the history list's run column.
const RUN_COL: usize = 44;

/// One row of the run history.
#[derive(Debug, Clone, PartialEq)]
pub struct RunSummary {
    /// Run id.
    pub run_id: RunId,
    /// Run name.
    pub name: String,
    /// Workflow, if the run was one.
    pub workflow: Option<String>,
    /// Agent of the first top-level agent node.
    pub agent: Option<String>,
    /// What ran, for people: the workflow name or the agent's display
    /// name ([`RunState::who`]).
    pub who: String,
    /// Short title from the first prompt or the workflow inputs.
    pub title: Option<String>,
    /// Files changed across the run.
    pub files_changed: u32,
    /// Run error, if it failed.
    pub error: Option<String>,
    /// Status.
    pub status: RunStatus,
    /// Start time.
    pub started_at: Option<OffsetDateTime>,
    /// Duration (until now for running runs).
    pub duration_ms: Option<u64>,
    /// Cost in USD.
    pub cost_usd: f64,
    /// Host (run host or the first remote node host).
    pub host: Option<String>,
    /// Number of nodes.
    pub nodes: usize,
    /// The log ended in a torn line (live or crashed run).
    pub truncated: bool,
}

impl RunSummary {
    /// Summarises a folded run.
    pub fn from_state(run: &RunState, now: OffsetDateTime) -> Option<RunSummary> {
        let run_id = run.run_id?;
        let started = run
            .started_at
            .or_else(|| run.nodes.iter().filter_map(|n| n.started_at).min());
        let agent = run
            .nodes
            .iter()
            .find(|n| n.parent.is_none() && matches!(n.kind, Some(NodeKind::Agent)))
            .and_then(|n| n.agent.clone().or_else(|| Some(n.name.clone())));
        Some(RunSummary {
            run_id,
            name: run
                .name
                .clone()
                .or_else(|| run.nodes.first().map(|n| n.name.clone()))
                .unwrap_or_else(|| "run".into()),
            workflow: run.workflow.clone(),
            agent,
            who: run.who(),
            title: run.title.clone(),
            files_changed: run.nodes.iter().map(|n| n.files_changed).sum(),
            error: run
                .error
                .clone()
                .or_else(|| run.nodes.iter().rev().find_map(|n| n.error.clone())),
            status: run.status,
            started_at: started,
            duration_ms: crate::map::metrics::run_elapsed_ms(run, now),
            cost_usd: run.cost_usd,
            host: run
                .host
                .clone()
                .or_else(|| run.nodes.iter().find_map(|n| n.host.clone())),
            nodes: run.nodes.len(),
            truncated: false,
        })
    }

    /// What ran and what it was about: `Coder · fix the header parser`
    /// (just the agent or workflow before a title is known).
    pub fn what(&self) -> String {
        match &self.title {
            Some(t) => format!("{} · {t}", self.who),
            None => self.who.clone(),
        }
    }

    /// The one-line label used in lists such as the palette:
    /// `Coder · fix the header parser · 14:32 Oct 23 · ✔`. Times are shown
    /// in `offset`; the id is never part of it.
    pub fn label(&self, offset: UtcOffset, now: OffsetDateTime) -> String {
        format!("{}{}", self.label_head(), self.label_tail(offset, now))
    }

    /// The part of [`RunSummary::label`] that may be cut in narrow lists:
    /// `Coder · fix the header parser` with the title shortened.
    pub fn label_head(&self) -> String {
        match &self.title {
            Some(t) => format!("{} · {}", self.who, names::shorten(t, LABEL_TITLE_MAX)),
            None => self.who.clone(),
        }
    }

    /// The part of [`RunSummary::label`] that lists keep visible:
    /// ` · 14:32 Oct 23 · ✔`.
    pub fn label_tail(&self, offset: UtcOffset, now: OffsetDateTime) -> String {
        let mut s = String::new();
        if let Some(t) = self.started_at {
            s.push_str(" · ");
            s.push_str(&names::when_label(t, offset, now));
        }
        s.push_str(" · ");
        s.push(names::status_glyph(self.status));
        s
    }

    /// A one-line summary of how the run went: `Succeeded in 2m14s ·
    /// $0.12 · 3 files changed`, `Failed after 40s: <error>`, ...
    pub fn outcome(&self) -> String {
        let took = self.duration_ms.map(fmt_ms);
        let mut s = match (self.status, &took) {
            (RunStatus::Running, Some(d)) => format!("Running for {d}"),
            (RunStatus::Running, None) => "Running".to_owned(),
            (RunStatus::Succeeded, Some(d)) => format!("Succeeded in {d}"),
            (RunStatus::Succeeded, None) => "Succeeded".to_owned(),
            (RunStatus::Failed, Some(d)) => format!("Failed after {d}"),
            (RunStatus::Failed, None) => "Failed".to_owned(),
            (RunStatus::Cancelled, Some(d)) => format!("Cancelled after {d}"),
            (RunStatus::Cancelled, None) => "Cancelled".to_owned(),
        };
        if self.status == RunStatus::Failed
            && let Some(e) = self.error.as_deref().and_then(|e| e.lines().next())
        {
            s.push_str(": ");
            s.push_str(&truncate(&names::scrub_ids(e.trim()), 60));
        }
        if self.cost_usd > 0.0 {
            s.push_str(&format!(" · {}", fmt_cost(Some(self.cost_usd))));
        }
        match self.files_changed {
            0 => {}
            1 => s.push_str(" · 1 file changed"),
            n => s.push_str(&format!(" · {n} files changed")),
        }
        if let Some(h) = &self.host {
            s.push_str(&format!(" · on {h}"));
        }
        s
    }

    /// Whether the row matches a lower-case query (title, name, workflow,
    /// agent, status, host, id prefix or tag).
    pub fn matches(&self, q: &str) -> bool {
        let status = format!("{:?}", self.status).to_lowercase();
        if names::matches_tag(&self.run_id.to_string(), q) {
            return true;
        }
        [
            self.title.as_ref().map(|s| s.to_lowercase()),
            Some(self.who.to_lowercase()),
            Some(self.name.to_lowercase()),
            self.workflow.as_ref().map(|s| s.to_lowercase()),
            self.agent.as_ref().map(|s| s.to_lowercase()),
            Some(status),
            self.host.as_ref().map(|s| s.to_lowercase()),
            Some(self.run_id.to_string()),
        ]
        .iter()
        .flatten()
        .any(|f| f.contains(q))
    }
}

/// Loads a run's events from `<data_dir>/runs/<id>` (live runs included:
/// a torn last line is skipped).
pub fn load_run(
    data_dir: &Path,
    id: RunId,
) -> Result<(Vec<RunEvent>, bool), harness_events::store::StoreError> {
    let r = read_run(&run_dir(data_dir, id))?;
    Ok((r.events, r.truncated))
}

/// Summaries of every readable run under `data_dir`, newest first.
/// Unreadable runs are skipped.
pub fn load_runs(data_dir: &Path, now: OffsetDateTime) -> Vec<RunSummary> {
    let mut out: Vec<RunSummary> = list_runs(data_dir)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|id| {
            let (events, truncated) = load_run(data_dir, id).ok()?;
            let state = RunState::from_events(&events);
            let mut s = RunSummary::from_state(&state, now)?;
            s.truncated = truncated;
            Some(s)
        })
        .collect();
    out.sort_by(|a, b| {
        b.started_at
            .cmp(&a.started_at)
            .then(b.run_id.0.cmp(&a.run_id.0))
    });
    out
}

/// What the history view asks the app to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HistoryOutput {
    /// Open this run in the map.
    Open(RunId),
    /// Replay this run.
    Replay(RunId),
    /// Esc.
    Back,
}

/// Element id of a history row.
pub fn row_id(id: &RunId) -> String {
    format!("history.run.{id}")
}

/// The run history browser.
pub struct HistoryView {
    runs: Vec<RunSummary>,
    filter: String,
    theme: Theme,
    scroll: usize,
    offset: UtcOffset,
    interaction: Interaction<RunId>,
}

impl HistoryView {
    /// A view of `runs` (newest first).
    pub fn new(runs: Vec<RunSummary>, theme: Theme) -> HistoryView {
        HistoryView {
            runs,
            filter: String::new(),
            theme,
            scroll: 0,
            offset: UtcOffset::UTC,
            interaction: Interaction::new(),
        }
    }

    /// Shows start times in `offset` (the local offset; UTC by default).
    pub fn with_offset(mut self, offset: UtcOffset) -> HistoryView {
        self.offset = offset;
        self
    }

    /// Replaces the list.
    pub fn set_runs(&mut self, runs: Vec<RunSummary>) {
        self.runs = runs;
    }

    /// Sets the filter query.
    pub fn set_filter(&mut self, q: &str) {
        self.filter = q.trim().to_lowercase();
        self.scroll = 0;
    }

    /// The filter query.
    pub fn filter(&self) -> &str {
        &self.filter
    }

    /// Rows matching the filter.
    pub fn visible(&self) -> Vec<&RunSummary> {
        self.runs
            .iter()
            .filter(|r| self.filter.is_empty() || r.matches(&self.filter))
            .collect()
    }

    fn selected(&self) -> Option<RunId> {
        let f = self.interaction.focused()?;
        self.runs
            .iter()
            .find(|r| row_id(&r.run_id) == f.0)
            .map(|r| r.run_id)
    }

    /// Runs a palette entry from [`palette_entries`].
    pub fn run_palette(&mut self, inv: &Invocation) -> Option<HistoryOutput> {
        match inv.id.as_str() {
            ids::FILTER => {
                self.set_filter(inv.args.first().map(String::as_str).unwrap_or(""));
                None
            }
            ids::CLEAR => {
                self.set_filter("");
                None
            }
            ids::REPLAY => self
                .selected()
                .or_else(|| self.visible().first().map(|r| r.run_id))
                .map(HistoryOutput::Replay),
            _ => None,
        }
    }

    /// Handles input: Up/Down/Tab move over rows, Enter or click opens,
    /// `r` replays the focused run, Esc backs out.
    pub fn handle(&mut self, ev: &Event) -> Option<HistoryOutput> {
        let d = match ev {
            Event::Key(k) if k.kind != KeyEventKind::Release => {
                if k.code == KeyCode::Char('r') {
                    return self.selected().map(HistoryOutput::Replay);
                }
                self.interaction.handle_key(k)
            }
            Event::Mouse(m) => match m.kind {
                MouseEventKind::ScrollUp => {
                    self.scroll = self.scroll.saturating_sub(3);
                    None
                }
                MouseEventKind::ScrollDown => {
                    self.scroll += 3;
                    None
                }
                _ => self.interaction.handle_mouse(m),
            },
            _ => None,
        }?;
        match d {
            Dispatch::Activate(id) => Some(HistoryOutput::Open(id)),
            Dispatch::Back => Some(HistoryOutput::Back),
            _ => None,
        }
    }

    /// Renders the list.
    pub fn render(&mut self, frame: &mut Frame<'_>, area: Rect, now: OffsetDateTime) {
        let t = self.theme.clone();
        let mut hits: HitMap<RunId> = self.interaction.begin_frame();
        hits.register_pane("history", area);
        let muted = t.fg(Token::Muted);
        let mut lines = Vec::new();
        let head = if self.filter.is_empty() {
            format!(" Runs ({})", self.runs.len())
        } else {
            format!(
                " Runs matching “{}” ({} of {})",
                self.filter,
                self.visible().len(),
                self.runs.len()
            )
        };
        lines.push(Line::from(Span::styled(
            head,
            t.fg(Token::Fg).add_modifier(Modifier::BOLD),
        )));
        lines.push(Line::from(Span::styled(
            format!(
                "   {:<RUN_COL$} {:<17} {:>8} {:>8}  {}",
                "run", "started", "duration", "cost", "host"
            ),
            muted,
        )));
        let visible: Vec<RunSummary> = self.visible().into_iter().cloned().collect();
        let body_h = usize::from(area.height).saturating_sub(2);
        if let Some(sel) = self
            .selected()
            .and_then(|s| visible.iter().position(|r| r.run_id == s))
        {
            if sel < self.scroll {
                self.scroll = sel;
            } else if sel >= self.scroll + body_h {
                self.scroll = sel + 1 - body_h;
            }
        }
        self.scroll = self.scroll.min(visible.len().saturating_sub(body_h));
        // Rows with the same label and start minute get a short tag.
        let keys: Vec<(String, Option<String>)> = visible
            .iter()
            .map(|r| {
                let when = r.started_at.map(|s| names::when_label(s, self.offset, now));
                (r.what(), when)
            })
            .collect();
        // The tag goes after the cut so colliding rows stay distinct.
        let labels: Vec<String> = names::collisions(&keys)
            .into_iter()
            .zip(&visible)
            .map(|(dup, r)| {
                if dup {
                    let tag = names::short_tag(&r.run_id.to_string());
                    let room = RUN_COL.saturating_sub(width(&tag) + 1);
                    format!("{} {tag}", truncate(&r.what(), room))
                } else {
                    r.what()
                }
            })
            .collect();
        for (row, r) in visible.iter().skip(self.scroll).take(body_h).enumerate() {
            let (icon, token) = match r.status {
                RunStatus::Running => ('◐', Token::Running),
                RunStatus::Succeeded => ('✔', Token::Ok),
                RunStatus::Failed => ('✗', Token::Error),
                RunStatus::Cancelled => ('⊘', Token::Muted),
            };
            let id = row_id(&r.run_id);
            let focused = hits.is_focused(&ElementId::from(id.as_str()));
            let start = r
                .started_at
                .map(|s| names::when_label(s, self.offset, now))
                .unwrap_or_else(|| "?".into());
            let mut line = Line::from(vec![
                Span::styled(format!(" {icon} "), t.fg(token)),
                Span::styled(
                    format!(
                        "{:<RUN_COL$}",
                        truncate(&labels[row + self.scroll], RUN_COL)
                    ),
                    t.fg(Token::Agent),
                ),
                Span::styled(format!(" {start:<17}"), muted),
                Span::styled(
                    format!(" {:>8}", r.duration_ms.map(fmt_ms).unwrap_or_default()),
                    t.fg(Token::Number),
                ),
                Span::styled(
                    format!(" {:>8}", fmt_cost(Some(r.cost_usd))),
                    t.fg(Token::Number),
                ),
                Span::styled(
                    format!(
                        "  {}",
                        r.host
                            .as_ref()
                            .map(|h| format!("⧉ {h}"))
                            .unwrap_or_else(|| "local".into())
                    ),
                    t.fg(Token::Running),
                ),
            ]);
            if focused {
                line = line.patch_style(t.focused());
            }
            let y = area.y + 2 + row as u16;
            hits.register(id, Rect::new(area.x, y, area.width, 1), r.run_id);
            lines.push(line);
        }
        if visible.is_empty() {
            lines.push(Line::from(Span::styled("   no runs", muted)));
        }
        frame.render_widget(Paragraph::new(lines), area);
        self.interaction.end_frame(hits);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::demo::events::{EventBuilder, NodeSpec};
    use crate::interaction::testing::{buffer_text, click, find_text, key};
    use harness_events::redact::Redactor;
    use harness_events::schema::NodeStatus;
    use harness_events::store::{EventDraft, RecorderOptions, RunRecorder};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use std::sync::Arc;

    async fn record(dir: &Path, events: &[RunEvent]) -> RunId {
        let id = RunId::new();
        let rec = RunRecorder::create(
            dir,
            id,
            Arc::new(Redactor::default()),
            RecorderOptions::default(),
        )
        .await
        .unwrap();
        for e in events {
            let draft = EventDraft {
                node_id: e.node_id.clone(),
                parent_node_id: e.parent_node_id.clone(),
                kind: e.kind.clone(),
            };
            rec.emit(draft).unwrap();
        }
        rec.finish().await.unwrap();
        id
    }

    fn workflow_run() -> Vec<RunEvent> {
        let mut b = EventBuilder::new();
        b.run_started("fix-bug", Some("fix-bug"));
        b.start(NodeSpec::step("fix").agent("implementer").host("gpu-box"));
        b.cost("fix", 0.42);
        b.finish("fix", NodeStatus::Succeeded, None);
        b.run_finished(RunStatus::Succeeded);
        b.into_events()
    }

    fn agent_run() -> Vec<RunEvent> {
        let mut b = EventBuilder::new();
        b.run_started("chat", None);
        b.start(NodeSpec::step("main").agent("coder"));
        b.user_message("main", "Fix the header parser\nIt drops values");
        b.cost("main", 0.05);
        b.into_events()
    }

    #[tokio::test]
    async fn lists_runs_with_status_agent_start_duration_cost_and_host() {
        let dir = tempfile::tempdir().unwrap();
        record(dir.path(), &workflow_run()).await;
        record(dir.path(), &agent_run()).await;
        let now = OffsetDateTime::now_utc();
        let runs = load_runs(dir.path(), now);
        assert_eq!(runs.len(), 2);
        let wf = runs.iter().find(|r| r.workflow.is_some()).unwrap();
        assert_eq!(wf.status, RunStatus::Succeeded);
        assert_eq!(wf.host.as_deref(), Some("gpu-box"));
        assert!((wf.cost_usd - 0.42).abs() < 1e-9);
        let chat = runs.iter().find(|r| r.workflow.is_none()).unwrap();
        assert_eq!(chat.status, RunStatus::Running);
        assert_eq!(chat.what(), "Coder · Fix the header parser");
        assert_eq!(wf.what(), "fix-bug");

        let mut v = HistoryView::new(runs, Theme::default());
        let mut term = Terminal::new(TestBackend::new(100, 10)).unwrap();
        term.draw(|f| v.render(f, f.area(), now)).unwrap();
        let text = buffer_text(term.backend().buffer());
        assert!(text.contains("✔ fix-bug"), "{text}");
        assert!(text.contains("◐ Coder · Fix the header parser"), "{text}");
        assert!(names::find_raw_id(&text).is_none(), "{text}");
        assert!(text.contains("$0.42"), "{text}");
        assert!(text.contains("⧉ gpu-box"), "{text}");
        assert!(text.contains("local"), "{text}");
    }

    #[tokio::test]
    async fn palette_filters_and_rows_open_by_click_and_keyboard() {
        let dir = tempfile::tempdir().unwrap();
        let wf = record(dir.path(), &workflow_run()).await;
        let chat = record(dir.path(), &agent_run()).await;
        let now = OffsetDateTime::now_utc();
        let mut v = HistoryView::new(load_runs(dir.path(), now), Theme::default());
        assert!(palette_entries().iter().any(|e| e.id == ids::FILTER));
        v.run_palette(&Invocation {
            id: ids::FILTER.into(),
            args: vec!["gpu".into()],
        });
        assert_eq!(v.visible().len(), 1);
        let mut term = Terminal::new(TestBackend::new(100, 10)).unwrap();
        term.draw(|f| v.render(f, f.area(), now)).unwrap();
        let (x, y) = find_text(term.backend().buffer(), "fix-bug").unwrap();
        assert_eq!(v.handle(&click(x, y)), Some(HistoryOutput::Open(wf)));
        v.run_palette(&Invocation {
            id: ids::CLEAR.into(),
            args: vec![],
        });
        term.draw(|f| v.render(f, f.area(), now)).unwrap();
        // Keyboard: move to the chat row and open it.
        let mut opened = None;
        for _ in 0..4 {
            v.handle(&key("down"));
            term.draw(|f| v.render(f, f.area(), now)).unwrap();
            if v.selected() == Some(chat) {
                opened = v.handle(&key("enter"));
                break;
            }
        }
        assert_eq!(opened, Some(HistoryOutput::Open(chat)));
        assert_eq!(v.handle(&key("r")), Some(HistoryOutput::Replay(chat)));
        assert_eq!(v.handle(&key("esc")), Some(HistoryOutput::Back));
        // An opened run folds into the map.
        let (events, _) = load_run(dir.path(), wf).unwrap();
        let mut map = crate::map::MapView::new(crate::map::MapConfig::default());
        map.set_state(&events);
        assert_eq!(map.run_state().name.as_deref(), Some("fix-bug"));
    }
}
