//! The goal bar: a right-hand pane with the run's goal and its tasks.
//!
//! [`Goals`] collects the goal and tasks from two sources: the `todo` tool
//! (shared by all agents in a run; each call carries the full list, see
//! [`Goals::apply`]) and the optional `goal` / `tasks` of a workflow
//! ([`Goals::set_goal`], [`Goals::set_tasks`]). The owner of a task is the
//! node that last moved it, so agent, model and host come from that node.
//!
//! [`GoalBar`] decides visibility and draws the pane. With
//! `[ui] goal_bar = auto` it appears only while there is at least one task
//! and disappears when there are none; `off` hides it; the palette entry
//! [`TOGGLE_ID`] toggles it for the session. When the terminal is too
//! narrow for the pane next to [`MIN_MAIN_WIDTH`] columns of main view,
//! it collapses into a progress indicator for the status line
//! ([`GoalBar::indicator`]). Clicking a task, or focusing it and pressing
//! Enter, asks the app to open the owning agent's node
//! ([`GoalBarOutput::OpenNode`]).

use crossterm::event::{Event, KeyEventKind};
use harness_config::ini::config::{Config, GoalBar as GoalBarMode};
use harness_events::reducer::RunState;
use harness_events::schema::{EventKind, NodeId, NodeStatus, RunEvent};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use serde_json::Value;

use crate::cards::payload_json;
use crate::chat::text::{right_align, truncate};
use crate::indicators::{Spinner, node_glyph};
use crate::interaction::{Dispatch, ElementId, HitMap, Interaction};
use crate::palette::{EntryKind as PaletteKind, PaletteEntry};
use crate::theme::{Theme, Token};

/// Palette entry that shows or hides the goal bar.
pub const TOGGLE_ID: &str = "goalbar.toggle";
/// Columns the main view keeps before the pane collapses.
pub const MIN_MAIN_WIDTH: u16 = 60;
/// Name of the tool whose calls carry the task list.
pub const TODO_TOOL: &str = "todo";

/// Status of a task.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskStatus {
    /// Not started.
    Pending,
    /// Being worked on.
    InProgress,
    /// Waiting on something.
    Blocked,
    /// Finished.
    Done,
    /// Failed.
    Failed,
    /// Dropped.
    Cancelled,
}

impl TaskStatus {
    /// Parses the `status` of a todo item (`pending`, `in_progress`,
    /// `blocked`, `done` / `completed`, `failed`, `cancelled`).
    pub fn parse(s: &str) -> Option<TaskStatus> {
        Some(match s.trim().to_lowercase().replace('-', "_").as_str() {
            "pending" | "todo" | "open" => TaskStatus::Pending,
            "in_progress" | "active" | "running" => TaskStatus::InProgress,
            "blocked" | "waiting" => TaskStatus::Blocked,
            "done" | "completed" | "complete" => TaskStatus::Done,
            "failed" | "error" => TaskStatus::Failed,
            "cancelled" | "canceled" | "skipped" => TaskStatus::Cancelled,
            _ => return None,
        })
    }

    /// Icon and token. In-progress tasks use the shared node indicator
    /// ([`node_glyph`]) so they spin like running map nodes.
    pub fn glyph(self, spinner: &Spinner) -> (char, Token) {
        let node = match self {
            TaskStatus::Pending => NodeStatus::Queued,
            TaskStatus::InProgress => NodeStatus::Running,
            TaskStatus::Blocked => NodeStatus::WaitingApproval,
            TaskStatus::Done => NodeStatus::Succeeded,
            TaskStatus::Failed => NodeStatus::Failed,
            TaskStatus::Cancelled => NodeStatus::Cancelled,
        };
        node_glyph(node, spinner)
    }
}

/// One task.
#[derive(Debug, Clone, PartialEq)]
pub struct GoalTask {
    /// Stable id (the todo item's `id`, else its title).
    pub id: String,
    /// Title.
    pub title: String,
    /// Status.
    pub status: TaskStatus,
    /// The node working on it (last to change it).
    pub owner: Option<NodeId>,
}

/// The goal and tasks of a session or run.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Goals {
    /// The goal.
    pub goal: Option<String>,
    /// Tasks in list order.
    pub tasks: Vec<GoalTask>,
    version: u64,
}

impl Goals {
    /// No goal, no tasks.
    pub fn new() -> Self {
        Self::default()
    }

    /// Changes whenever goal or tasks change.
    pub fn version(&self) -> u64 {
        self.version
    }

    /// Sets the goal (workflow `goal:`).
    pub fn set_goal(&mut self, goal: Option<String>) {
        self.goal = goal;
        self.version += 1;
    }

    /// Replaces the tasks (workflow `tasks:`), keeping owners of tasks with
    /// the same id.
    pub fn set_tasks(&mut self, tasks: Vec<GoalTask>) {
        let old = std::mem::take(&mut self.tasks);
        self.tasks = tasks
            .into_iter()
            .map(|mut t| {
                if t.owner.is_none() {
                    t.owner = old
                        .iter()
                        .find(|o| o.id == t.id)
                        .and_then(|o| o.owner.clone());
                }
                t
            })
            .collect();
        self.version += 1;
    }

    /// Folds a `tasks` event (the run's task board, recorded by the core
    /// after every change) or a `todo` tool call whose arguments carry the
    /// full list: `{"goal"?: str, "todos": [{"id"?, "title" | "content",
    /// "status"}]}`. Returns whether anything changed.
    pub fn apply(&mut self, ev: &RunEvent) -> bool {
        if let EventKind::Tasks { goal, tasks, .. } = &ev.kind {
            self.goal.clone_from(goal);
            self.tasks = tasks
                .iter()
                .map(|t| GoalTask {
                    id: t.id.clone(),
                    title: t.title.clone(),
                    status: TaskStatus::parse(&t.status).unwrap_or(TaskStatus::Pending),
                    owner: t.owner.clone(),
                })
                .collect();
            self.version += 1;
            return true;
        }
        let EventKind::ToolCall { tool, args, .. } = &ev.kind else {
            return false;
        };
        if tool != TODO_TOOL {
            return false;
        }
        let Some(args) = payload_json(args) else {
            return false;
        };
        let mut changed = false;
        if let Some(g) = args.get("goal").and_then(Value::as_str) {
            self.goal = Some(g.to_owned());
            changed = true;
        }
        let items = args
            .get("todos")
            .or_else(|| args.get("tasks"))
            .and_then(Value::as_array);
        if let Some(items) = items {
            let mut next = Vec::new();
            for it in items {
                let Some(title) = it
                    .get("title")
                    .or_else(|| it.get("content"))
                    .and_then(Value::as_str)
                else {
                    continue;
                };
                let id = it
                    .get("id")
                    .map(|v| v.as_str().map_or_else(|| v.to_string(), str::to_owned))
                    .unwrap_or_else(|| title.to_owned());
                let status = it
                    .get("status")
                    .and_then(Value::as_str)
                    .and_then(TaskStatus::parse)
                    .unwrap_or(TaskStatus::Pending);
                let prev = self.tasks.iter().find(|t| t.id == id);
                let moved = prev.is_none_or(|p| p.status != status || p.title != title);
                let owner = if moved && status != TaskStatus::Pending {
                    ev.node_id.clone()
                } else {
                    prev.and_then(|p| p.owner.clone())
                };
                next.push(GoalTask {
                    id,
                    title: title.to_owned(),
                    status,
                    owner,
                });
            }
            self.tasks = next;
            changed = true;
        }
        if changed {
            self.version += 1;
        }
        changed
    }

    /// Done tasks.
    pub fn done(&self) -> usize {
        self.tasks
            .iter()
            .filter(|t| t.status == TaskStatus::Done)
            .count()
    }

    /// All tasks.
    pub fn total(&self) -> usize {
        self.tasks.len()
    }

    /// Whether there are any tasks.
    pub fn has_tasks(&self) -> bool {
        !self.tasks.is_empty()
    }
}

/// What the goal bar asks the app to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GoalBarOutput {
    /// Jump to this node (map selection or node detail).
    OpenNode(NodeId),
    /// Esc.
    Back,
}

/// Element id of a task row.
pub fn task_id(i: usize) -> String {
    format!("goalbar.task.{i}")
}

/// The palette entry that toggles the bar.
pub fn palette_entry() -> PaletteEntry {
    PaletteEntry::new(
        TOGGLE_ID,
        "Toggle goal bar",
        "Show or hide the goal and task pane",
        PaletteKind::Builtin,
    )
}

/// Visibility and rendering of the goal bar.
pub struct GoalBar {
    theme: Theme,
    mode: GoalBarMode,
    width: u16,
    hidden: bool,
    spinner: Spinner,
    interaction: Interaction<usize>,
    owners: Vec<Option<NodeId>>,
}

impl GoalBar {
    /// A bar with `[ui] goal_bar` and `goal_bar_width`.
    pub fn new(theme: Theme, mode: GoalBarMode, width: u16, animations: bool) -> GoalBar {
        GoalBar {
            theme,
            mode,
            width: width.max(16),
            hidden: false,
            spinner: Spinner::new(animations),
            interaction: Interaction::new(),
            owners: Vec::new(),
        }
    }

    /// A bar configured from `harness.ini`.
    pub fn from_config(cfg: &Config, theme: Theme) -> GoalBar {
        GoalBar::new(
            theme,
            cfg.ui.goal_bar,
            cfg.ui.goal_bar_width,
            cfg.ui.animations,
        )
    }

    /// Width of the pane.
    pub fn width(&self) -> u16 {
        self.width
    }

    /// Toggles the bar (palette). Has no effect with `goal_bar = off`
    /// other than turning it on for the session.
    pub fn toggle(&mut self) {
        if self.mode == GoalBarMode::Off {
            self.mode = GoalBarMode::Auto;
            self.hidden = false;
        } else {
            self.hidden = !self.hidden;
        }
    }

    /// Runs a palette entry; true when it was [`TOGGLE_ID`].
    pub fn run_palette(&mut self, id: &str) -> bool {
        let ours = id == TOGGLE_ID;
        if ours {
            self.toggle();
        }
        ours
    }

    /// Whether the bar should be shown for `goals` at all.
    pub fn wanted(&self, goals: &Goals) -> bool {
        self.mode == GoalBarMode::Auto && !self.hidden && goals.has_tasks()
    }

    /// Splits `area` into the main view and the pane. The pane is `None`
    /// when it is not wanted or the terminal is too narrow.
    pub fn split(&self, area: Rect, goals: &Goals) -> (Rect, Option<Rect>) {
        if !self.wanted(goals) || area.width < self.width + MIN_MAIN_WIDTH {
            return (area, None);
        }
        let main = Rect::new(area.x, area.y, area.width - self.width, area.height);
        let pane = Rect::new(
            area.x + area.width - self.width,
            area.y,
            self.width,
            area.height,
        );
        (main, Some(pane))
    }

    /// The status-line progress indicator, shown when the bar is wanted
    /// but collapsed because the terminal is narrower than `term_width`
    /// needs.
    pub fn indicator(&self, goals: &Goals, term_width: u16) -> Option<Vec<Span<'static>>> {
        if !self.wanted(goals) || term_width >= self.width + MIN_MAIN_WIDTH {
            return None;
        }
        let t = &self.theme;
        let mut spans = vec![Span::styled(
            format!("☰ {}/{}", goals.done(), goals.total()),
            t.fg(Token::Number),
        )];
        if let Some(cur) = goals
            .tasks
            .iter()
            .find(|t| t.status == TaskStatus::InProgress)
        {
            spans.push(Span::styled(
                format!(" {}", truncate(&cur.title, 24)),
                t.fg(Token::Running),
            ));
        }
        Some(spans)
    }

    /// Advances the spinner.
    pub fn tick(&mut self) {
        self.spinner.tick();
    }

    /// Handles input: click a task, or Tab/arrows to it and Enter.
    pub fn handle(&mut self, ev: &Event) -> Option<GoalBarOutput> {
        let d = match ev {
            Event::Key(k) if k.kind != KeyEventKind::Release => self.interaction.handle_key(k),
            Event::Mouse(m) => self.interaction.handle_mouse(m),
            _ => None,
        }?;
        match d {
            Dispatch::Activate(i) => self
                .owners
                .get(i)
                .cloned()
                .flatten()
                .map(GoalBarOutput::OpenNode),
            Dispatch::Back => Some(GoalBarOutput::Back),
            _ => None,
        }
    }

    /// Focuses the first task (so keyboard users can enter the pane).
    pub fn focus_first(&mut self) {
        self.interaction.set_focus(task_id(0));
    }

    /// Draws the pane into `area` (from [`GoalBar::split`]).
    pub fn render(&mut self, frame: &mut Frame<'_>, area: Rect, goals: &Goals, run: &RunState) {
        let t = self.theme.clone();
        let mut hits: HitMap<usize> = self.interaction.begin_frame();
        let border = t.fg(Token::Border);
        let block = Block::new()
            .borders(Borders::ALL)
            .border_style(border)
            .title(Span::styled(
                " Goal ",
                t.fg(Token::Fg).add_modifier(Modifier::BOLD),
            ));
        let inner = block.inner(area);
        frame.render_widget(block, area);
        let w = usize::from(inner.width);
        let mut lines: Vec<(Option<usize>, Line<'static>)> = Vec::new();
        let goal = goals.goal.clone().unwrap_or_else(|| "(no goal set)".into());
        for l in
            crate::chat::text::wrap_line(Line::from(Span::styled(goal, t.fg(Token::Fg))), w, &[])
                .into_iter()
                .take(3)
        {
            lines.push((None, l));
        }
        lines.push((
            None,
            right_align(
                Line::from(Span::styled("Tasks ", t.fg(Token::Muted))),
                vec![Span::styled(
                    format!(" {}/{} ", goals.done(), goals.total()),
                    t.fg(Token::Number).add_modifier(Modifier::BOLD),
                )],
                w,
                Span::styled("─", border),
            ),
        ));
        self.owners = goals.tasks.iter().map(|t| t.owner.clone()).collect();
        for (i, task) in goals.tasks.iter().enumerate() {
            let (glyph, token) = task.status.glyph(&self.spinner);
            let focused = hits.is_focused(&ElementId::from(task_id(i)));
            let active = task.status == TaskStatus::InProgress;
            let row_style = if focused {
                t.focused()
            } else if active {
                t.bg(Token::Surface)
            } else {
                Style::default()
            };
            let title_style = if active {
                t.fg(Token::Running).add_modifier(Modifier::BOLD)
            } else if task.status == TaskStatus::Done {
                t.fg(Token::Muted)
            } else {
                t.fg(Token::Fg)
            };
            lines.push((
                Some(i),
                Line::from(vec![
                    Span::styled(format!("{glyph} "), t.fg(token)),
                    Span::styled(truncate(&task.title, w.saturating_sub(2)), title_style),
                ])
                .patch_style(row_style),
            ));
            if let Some(n) = task.owner.as_ref().and_then(|o| run.node(o)) {
                let mut parts: Vec<Span<'static>> = Vec::new();
                let who: Vec<String> = [
                    n.agent.clone().or_else(|| Some(n.name.clone())),
                    n.model.clone(),
                ]
                .into_iter()
                .flatten()
                .collect();
                parts.push(Span::styled(
                    format!("  {}", who.join(" · ")),
                    t.fg(Token::Muted),
                ));
                if let Some(h) = &n.host {
                    parts.push(Span::styled(format!(" · ⧉{h}"), t.fg(Token::Running)));
                }
                lines.push((
                    Some(i),
                    crate::chat::text::truncate_line(Line::from(parts), w).patch_style(row_style),
                ));
            }
        }
        let h = usize::from(inner.height);
        let mut out = Vec::new();
        let mut rows: Vec<(usize, u16)> = Vec::new();
        for (row, (task, line)) in lines.into_iter().take(h).enumerate() {
            if let Some(i) = task {
                rows.push((i, row as u16));
            }
            out.push(line);
        }
        // One region per task covering its rows.
        let mut k = 0;
        while k < rows.len() {
            let (i, first) = rows[k];
            let mut last = first;
            while k + 1 < rows.len() && rows[k + 1].0 == i {
                k += 1;
                last = rows[k].1;
            }
            hits.register(
                task_id(i),
                Rect::new(inner.x, inner.y + first, inner.width, last - first + 1),
                i,
            );
            k += 1;
        }
        frame.render_widget(Paragraph::new(out), inner);
        self.interaction.end_frame(hits);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::demo::events::{EventBuilder, NodeSpec};
    use crate::interaction::testing::{buffer_text, click, find_text, key, styled_snapshot};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use serde_json::json;

    fn todo(b: &mut EventBuilder, node: &str, call: &str, todos: Value) {
        b.tool_call(
            node,
            call,
            TODO_TOOL,
            json!({"goal": "Fix header parsing bug (#142)", "todos": todos}),
        );
    }

    fn run() -> (EventBuilder, Goals, RunState) {
        let mut b = EventBuilder::new();
        b.start(NodeSpec::step("main").agent("implementer").model("sonnet"));
        b.start(
            NodeSpec::subagent("r1", "main")
                .agent("researcher")
                .model("haiku")
                .host("gpu-box"),
        );
        todo(
            &mut b,
            "main",
            "t1",
            json!([
                {"id": "1", "content": "Reproduce with failing test", "status": "completed"},
                {"id": "2", "content": "Fix parse_header", "status": "in_progress"},
                {"id": "3", "content": "Bug-fix proof", "status": "pending"},
            ]),
        );
        todo(
            &mut b,
            "r1",
            "t2",
            json!([
                {"id": "1", "content": "Reproduce with failing test", "status": "completed"},
                {"id": "2", "content": "Fix parse_header", "status": "in_progress"},
                {"id": "4", "content": "Survey other callers", "status": "in_progress"},
                {"id": "3", "content": "Bug-fix proof", "status": "pending"},
            ]),
        );
        let mut g = Goals::new();
        for e in b.events() {
            g.apply(e);
        }
        let run = RunState::from_events(b.events());
        (b, g, run)
    }

    fn bar() -> GoalBar {
        GoalBar::new(Theme::default(), GoalBarMode::Auto, 36, false)
    }

    fn draw(
        bar: &mut GoalBar,
        g: &Goals,
        run: &RunState,
        w: u16,
    ) -> (Terminal<TestBackend>, Option<Rect>) {
        let mut term = Terminal::new(TestBackend::new(w, 16)).unwrap();
        let mut pane = None;
        term.draw(|f| {
            let (_, p) = bar.split(f.area(), g);
            pane = p;
            if let Some(p) = p {
                bar.render(f, p, g, run);
            }
        })
        .unwrap();
        (term, pane)
    }

    #[test]
    fn shows_goal_tasks_owner_model_host_and_progress() {
        let (_, g, run) = run();
        assert_eq!((g.done(), g.total()), (1, 4));
        assert_eq!(g.tasks[2].owner, Some(NodeId::from("r1")));
        assert_eq!(
            g.tasks[1].owner,
            Some(NodeId::from("main")),
            "unchanged task keeps its owner"
        );
        let mut b = bar();
        let (term, pane) = draw(&mut b, &g, &run, 120);
        assert!(pane.is_some());
        let text = buffer_text(term.backend().buffer());
        for needle in [
            "Fix header parsing bug (#142)",
            "1/4",
            "✔ Reproduce with failing test",
            "◐ Fix parse_header",
            "implementer · sonnet",
            "researcher · haiku · ⧉gpu-box",
            "○ Bug-fix proof",
        ] {
            assert!(text.contains(needle), "{needle}\n{text}");
        }
        // In-progress tasks are highlighted.
        let buf = term.backend().buffer();
        let (x, y) = find_text(buf, "Fix parse_header").unwrap();
        assert_eq!(buf[(x, y)].fg, Theme::default().color(Token::Running));
        assert_eq!(buf[(x, y)].bg, Theme::default().color(Token::Surface));
        insta::assert_snapshot!("goalbar", styled_snapshot(buf));
    }

    #[test]
    fn only_shown_with_tasks_and_updates_live() {
        let mut b = EventBuilder::new();
        b.start(NodeSpec::step("main"));
        let mut g = Goals::new();
        let bar = bar();
        assert_eq!(bar.split(Rect::new(0, 0, 120, 10), &g).1, None);
        todo(
            &mut b,
            "main",
            "t1",
            json!([{"content": "Write tests", "status": "pending"}]),
        );
        assert!(g.apply(b.events().last().unwrap()));
        assert!(bar.split(Rect::new(0, 0, 120, 10), &g).1.is_some());
        todo(
            &mut b,
            "main",
            "t2",
            json!([{"content": "Write tests", "status": "done"}]),
        );
        g.apply(b.events().last().unwrap());
        assert_eq!(g.tasks[0].status, TaskStatus::Done);
        todo(&mut b, "main", "t3", json!([]));
        g.apply(b.events().last().unwrap());
        assert_eq!(
            bar.split(Rect::new(0, 0, 120, 10), &g).1,
            None,
            "disappears with no tasks"
        );
    }

    #[test]
    fn tasks_jump_to_their_owner_by_click_and_keyboard() {
        let (_, g, run) = run();
        let mut b = bar();
        let (term, _) = draw(&mut b, &g, &run, 120);
        let (x, y) = find_text(term.backend().buffer(), "Survey other callers").unwrap();
        assert_eq!(
            b.handle(&click(x, y)),
            Some(GoalBarOutput::OpenNode(NodeId::from("r1")))
        );
        b.focus_first();
        b.handle(&key("down"));
        assert_eq!(
            b.handle(&key("enter")),
            Some(GoalBarOutput::OpenNode(NodeId::from("main")))
        );
        assert_eq!(b.handle(&key("esc")), Some(GoalBarOutput::Back));
    }

    #[test]
    fn narrow_terminals_collapse_to_a_status_indicator_and_config_and_palette_control_it() {
        let (_, g, _) = run();
        let mut b = bar();
        assert_eq!(b.split(Rect::new(0, 0, 80, 10), &g).1, None);
        let ind = b.indicator(&g, 80).unwrap();
        let text: String = ind.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, "☰ 1/4 Fix parse_header");
        assert!(b.indicator(&g, 120).is_none());
        // Width from the config.
        let mut cfg = Config::defaults();
        cfg.ui.goal_bar_width = 40;
        let fb = GoalBar::from_config(&cfg, Theme::default());
        assert_eq!(fb.split(Rect::new(0, 0, 120, 10), &g).1.unwrap().width, 40);
        cfg.ui.goal_bar = GoalBarMode::Off;
        let mut off = GoalBar::from_config(&cfg, Theme::default());
        assert_eq!(off.split(Rect::new(0, 0, 120, 10), &g).1, None);
        // The palette toggles it.
        assert_eq!(palette_entry().id, TOGGLE_ID);
        assert!(off.run_palette(TOGGLE_ID));
        assert!(off.split(Rect::new(0, 0, 120, 10), &g).1.is_some());
        assert!(b.run_palette(TOGGLE_ID));
        assert_eq!(b.split(Rect::new(0, 0, 120, 10), &g).1, None);
    }

    #[test]
    fn task_board_snapshots_drive_the_bar() {
        use harness_events::schema::TaskItem;
        let mut b = EventBuilder::new();
        b.start(NodeSpec::step("main").agent("coder"));
        b.push(
            None,
            None,
            EventKind::Tasks {
                goal: Some("Ship the parser".into()),
                tasks: vec![
                    TaskItem {
                        id: "t1".into(),
                        title: "Write the test".into(),
                        status: "done".into(),
                        owner: Some("main".into()),
                        agent: Some("coder".into()),
                    },
                    TaskItem {
                        id: "t2".into(),
                        title: "Fix it".into(),
                        status: "in_progress".into(),
                        owner: Some("main".into()),
                        agent: Some("coder".into()),
                    },
                ],
                version: 2,
            },
        );
        let mut g = Goals::new();
        for e in b.events() {
            g.apply(e);
        }
        assert_eq!(g.goal.as_deref(), Some("Ship the parser"));
        assert_eq!((g.done(), g.total()), (1, 2));
        assert_eq!(g.tasks[1].status, TaskStatus::InProgress);
        assert_eq!(g.tasks[1].owner, Some(NodeId::from("main")));
    }
}
