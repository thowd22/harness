//! The agent map: a live graph of every agent, subagent and workflow step.
//!
//! [`MapView`] owns a folded [`RunState`] (fed live with
//! [`MapView::apply_run_event`], or replaced wholesale with
//! [`MapView::set_state`] for history and replay), the cached
//! [`crate::layout`] geometry and the [`live`] streams.
//!
//! - **Nodes** show the status icon ([`node_glyph`]), name, agent and model,
//!   host badge, duration, cost, retries, live tokens with the cache hit
//!   rate, and for running nodes a one-line live preview of the latest
//!   output or reasoning line (reasoning dimmed). Nodes waiting on an
//!   approval or stopped on their budget get an orange border.
//! - **Density** switches to one-line compact nodes when the full layout
//!   does not fit the viewport or the graph has more than
//!   [`COMPACT_NODE_LIMIT`] nodes.
//! - **Navigation**: arrows / `hjkl` move the selection to the nearest node
//!   in that direction and the viewport pans to keep it visible. Until the
//!   user moves it, the selection follows the active node (`f` resumes
//!   following). Enter opens node detail, or jumps to the approval prompt
//!   of a blocked node. Space toggles the live pane under the map, Esc
//!   closes it (or backs out). PgUp/PgDn and the wheel pan vertically.
//! - **Mouse**: every node is registered in the interaction layer; a click
//!   selects it and opens its live pane, a click on the selected node opens
//!   its detail.

pub mod live;
pub mod metrics;

use std::collections::HashMap;

use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseEventKind};
use harness_events::reducer::{NodeState, RunState};
use harness_events::schema::{NodeId, NodeStatus, RunEvent, RunStatus};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use time::OffsetDateTime;

use crate::chat::text::{cache_pct, fmt_cost, fmt_ms, fmt_tokens, truncate};
use crate::indicators::{Spinner, node_glyph};
use crate::interaction::{Dispatch, ElementId, HitMap, Interaction, PaneId};
use crate::layout::{BoxKind, Canvas, Density, GraphLayout, LayoutCache};
use crate::theme::{Theme, Token};

pub use live::{FrameThrottle, LivePane, LiveStreams};
pub use metrics::{Blocked, blocked, critical_path};

/// Graphs with more nodes than this always use compact nodes.
pub const COMPACT_NODE_LIMIT: usize = 40;

/// Scroll pane id of the map body.
pub const MAP_PANE: &str = "map";
/// Scroll pane id of the live pane.
pub const LIVE_PANE: &str = "map.live";
/// Element id of the live pane's close button.
pub const LIVE_CLOSE_ID: &str = "map.live.close";

/// Element id of a node on the map.
pub fn node_element_id(id: &NodeId) -> String {
    format!("map.node.{}", id.as_str())
}

/// What a click or Enter on a map element does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MapAction {
    /// Select a node (and open its live pane; open detail when already
    /// selected).
    Select(NodeId),
    /// Close the live pane.
    CloseLive,
}

/// What the map asks the app to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MapOutput {
    /// Open node detail for this node.
    OpenDetail(NodeId),
    /// Jump to the prompt the node waits on (approval queue or budget).
    OpenPrompt {
        /// The blocked node.
        node: NodeId,
        /// Why it is blocked.
        blocked: Blocked,
    },
    /// Esc with nothing left to close.
    Back,
}

/// Map settings from `[ui]`.
#[derive(Debug, Clone)]
pub struct MapConfig {
    /// Colours.
    pub theme: Theme,
    /// Animated spinners (`[ui] animations`).
    pub animations: bool,
    /// One-line live previews on running nodes (`[ui] live_preview`).
    pub live_preview: bool,
}

impl Default for MapConfig {
    fn default() -> Self {
        MapConfig {
            theme: Theme::default(),
            animations: true,
            live_preview: true,
        }
    }
}

impl MapConfig {
    /// Settings from `harness.ini`.
    pub fn from_config(cfg: &harness_config::ini::config::Config, theme: Theme) -> MapConfig {
        MapConfig {
            theme,
            animations: cfg.ui.animations,
            live_preview: cfg.ui.live_preview,
        }
    }
}

/// The map view.
pub struct MapView {
    cfg: MapConfig,
    run: RunState,
    full: LayoutCache,
    compact: LayoutCache,
    density: Density,
    selected: Option<NodeId>,
    follow: bool,
    pan: (u16, u16),
    last_view: Rect,
    live: LiveStreams,
    live_open: bool,
    live_back: usize,
    spinner: Spinner,
    interaction: Interaction<MapAction>,
    host_badges: HashMap<String, (String, Token)>,
    node_badges: HashMap<NodeId, (String, Token)>,
    frames: u64,
}

impl MapView {
    /// An empty map.
    pub fn new(cfg: MapConfig) -> MapView {
        MapView {
            spinner: Spinner::new(cfg.animations),
            cfg,
            run: RunState::new(),
            full: LayoutCache::new(),
            compact: LayoutCache::new(),
            density: Density::Full,
            selected: None,
            follow: true,
            pan: (0, 0),
            last_view: Rect::default(),
            live: LiveStreams::new(),
            live_open: false,
            live_back: 0,
            interaction: Interaction::new(),
            host_badges: HashMap::new(),
            node_badges: HashMap::new(),
            frames: 0,
        }
    }

    /// Folds a live run event into the state and the live streams.
    pub fn apply_run_event(&mut self, ev: &RunEvent) {
        self.run.apply(ev);
        self.live.apply(ev);
        if let (harness_events::schema::EventKind::ToolOutput { call_id, text, .. }, Some(node)) =
            (&ev.kind, &ev.node_id)
        {
            self.live.tool_output(node, call_id, text);
        }
        if self.follow {
            self.selected = self.active_node().or_else(|| self.selected.clone());
        }
    }

    /// Replaces the shown state (history, replay). Live streams are
    /// rebuilt from `events`.
    pub fn set_state<'a>(&mut self, events: impl IntoIterator<Item = &'a RunEvent>) {
        let mut run = RunState::new();
        let mut live = LiveStreams::new();
        for e in events {
            run.apply(e);
            live.apply(e);
            if let (
                harness_events::schema::EventKind::ToolOutput { call_id, text, .. },
                Some(node),
            ) = (&e.kind, &e.node_id)
            {
                live.tool_output(node, call_id, text);
            }
        }
        if run.run_id != self.run.run_id {
            self.full.clear();
            self.compact.clear();
            self.pan = (0, 0);
        }
        self.run = run;
        self.live = live;
        if self.follow
            || self
                .selected
                .as_ref()
                .is_some_and(|s| self.run.node(s).is_none())
        {
            self.selected = self.active_node().or_else(|| self.first_node());
        }
    }

    /// Appends live tool output (bash) for a node.
    pub fn tool_output(&mut self, node: &NodeId, call_id: &str, chunk: &str) {
        self.live.tool_output(node, call_id, chunk);
    }

    /// A badge on a node, e.g. its budget use. Badges of workflow nodes
    /// (containers, not drawn as boxes) show in the header.
    pub fn set_node_badge(&mut self, node: &NodeId, badge: &str, token: Token) {
        self.node_badges
            .insert(node.clone(), (badge.to_owned(), token));
    }

    /// Host health badges shown next to host names, e.g. `("gpu-box",
    /// ("●", Token::Ok))`.
    pub fn set_host_badge(&mut self, host: &str, badge: &str, token: Token) {
        self.host_badges
            .insert(host.to_owned(), (badge.to_owned(), token));
    }

    /// The folded run.
    pub fn run_state(&self) -> &RunState {
        &self.run
    }

    /// The live streams.
    pub fn live(&self) -> &LiveStreams {
        &self.live
    }

    /// The selected node.
    pub fn selected(&self) -> Option<&NodeId> {
        self.selected.as_ref()
    }

    /// Selects a node and stops following.
    pub fn select(&mut self, id: NodeId) {
        self.selected = Some(id);
        self.follow = false;
    }

    /// Whether the selection follows the active node.
    pub fn is_following(&self) -> bool {
        self.follow
    }

    /// Resumes following the active node.
    pub fn follow(&mut self) {
        self.follow = true;
        if let Some(a) = self.active_node() {
            self.selected = Some(a);
        }
    }

    /// Whether the live pane is open.
    pub fn live_open(&self) -> bool {
        self.live_open
    }

    /// Opens or closes the live pane.
    pub fn set_live_open(&mut self, open: bool) {
        self.live_open = open;
        self.live_back = 0;
    }

    /// Density used by the last frame.
    pub fn density(&self) -> Density {
        self.density
    }

    /// Viewport offset (graph coordinates) of the last frame.
    pub fn pan(&self) -> (u16, u16) {
        self.pan
    }

    /// Frames rendered.
    pub fn frames(&self) -> u64 {
        self.frames
    }

    /// Layout computations so far (full + compact).
    pub fn layout_computes(&self) -> u64 {
        self.full.computes() + self.compact.computes()
    }

    /// Whether something runs, so the spinner needs ticks.
    pub fn is_running(&self) -> bool {
        self.run.status == RunStatus::Running
            && self
                .run
                .nodes
                .iter()
                .any(|n| n.status == NodeStatus::Running)
    }

    /// Whether animation ticks are needed while the map is `visible`.
    pub fn needs_tick(&self, visible: bool) -> bool {
        self.spinner.needs_tick(self.is_running(), visible)
    }

    /// Advances the spinner.
    pub fn tick(&mut self) {
        self.spinner.tick();
    }

    /// Whether the live streams changed since the last call.
    pub fn take_stream_changes(&mut self) -> bool {
        self.live.take_changed()
    }

    /// The node the selection follows: the latest blocked node, else the
    /// most recently active running node.
    pub fn active_node(&self) -> Option<NodeId> {
        let pick = |f: &dyn Fn(&NodeState) -> bool| {
            self.run
                .nodes
                .iter()
                .filter(|n| n.kind.is_some() && f(n))
                .max_by_key(|n| n.last_seq)
                .map(|n| n.id.clone())
        };
        pick(&|n| n.status == NodeStatus::WaitingApproval)
            .or_else(|| pick(&|n| n.status == NodeStatus::Running))
    }

    fn first_node(&self) -> Option<NodeId> {
        self.current_layout()
            .and_then(|l| l.boxes.first().map(|b| b.id.clone()))
            .or_else(|| self.run.nodes.first().map(|n| n.id.clone()))
    }

    fn current_layout(&self) -> Option<&GraphLayout> {
        match self.density {
            Density::Full => self.full.current(),
            Density::Compact => self.compact.current(),
        }
    }

    fn enter(&self, id: &NodeId) -> MapOutput {
        match self.run.node(id).and_then(blocked) {
            Some(b) => MapOutput::OpenPrompt {
                node: id.clone(),
                blocked: b,
            },
            None => MapOutput::OpenDetail(id.clone()),
        }
    }

    fn activate(&mut self, a: MapAction) -> Option<MapOutput> {
        match a {
            MapAction::Select(id) => {
                let again = self.selected.as_ref() == Some(&id);
                self.select(id.clone());
                if again && self.live_open {
                    return Some(self.enter(&id));
                }
                self.set_live_open(true);
                None
            }
            MapAction::CloseLive => {
                self.set_live_open(false);
                None
            }
        }
    }

    fn move_selection(&mut self, dx: i32, dy: i32) {
        let Some(l) = self.current_layout() else {
            return;
        };
        let next = match &self.selected {
            Some(s) if l.get(s).is_some() => l.neighbour(s, dx, dy).cloned(),
            _ => l.boxes.first().map(|b| b.id.clone()),
        };
        if let Some(n) = next {
            self.select(n);
        }
    }

    fn pan_by(&mut self, dx: i32, dy: i32) {
        let x = (i32::from(self.pan.0) + dx).max(0);
        let y = (i32::from(self.pan.1) + dy).max(0);
        self.pan = (x as u16, y as u16);
        self.follow = false;
    }

    /// Handles terminal input. Tab is left to the app (view switching).
    pub fn handle(&mut self, ev: &Event) -> Option<MapOutput> {
        match ev {
            Event::Key(k) => self.handle_key(k),
            Event::Mouse(m) => {
                if let MouseEventKind::ScrollUp | MouseEventKind::ScrollDown = m.kind {
                    let lines = if m.kind == MouseEventKind::ScrollUp {
                        -crate::interaction::WHEEL_LINES
                    } else {
                        crate::interaction::WHEEL_LINES
                    };
                    let pane = self.interaction.hits().pane_at(m.column, m.row).cloned();
                    if pane.as_ref().is_some_and(|p| p.0 == LIVE_PANE) {
                        self.scroll_live(lines);
                    } else {
                        self.pan_by(0, lines);
                    }
                    return None;
                }
                match self.interaction.handle_mouse(m)? {
                    Dispatch::Activate(a) => self.activate(a),
                    Dispatch::Back => self.back(),
                    _ => None,
                }
            }
            _ => None,
        }
    }

    fn scroll_live(&mut self, lines: i32) {
        self.live_back = (self.live_back as i64 - i64::from(lines)).max(0) as usize;
    }

    fn back(&mut self) -> Option<MapOutput> {
        if self.live_open {
            self.set_live_open(false);
            None
        } else {
            Some(MapOutput::Back)
        }
    }

    /// Handles a key.
    pub fn handle_key(&mut self, k: &KeyEvent) -> Option<MapOutput> {
        if k.kind == KeyEventKind::Release
            || !k.modifiers.difference(KeyModifiers::SHIFT).is_empty()
        {
            return None;
        }
        match k.code {
            KeyCode::Left | KeyCode::Char('h') => self.move_selection(-1, 0),
            KeyCode::Right | KeyCode::Char('l') => self.move_selection(1, 0),
            KeyCode::Up | KeyCode::Char('k') => self.move_selection(0, -1),
            KeyCode::Down | KeyCode::Char('j') => self.move_selection(0, 1),
            KeyCode::Char('f') => self.follow(),
            KeyCode::Char(' ') => {
                let open = !self.live_open;
                self.set_live_open(open);
            }
            KeyCode::Enter => {
                if self
                    .interaction
                    .focused()
                    .is_some_and(|f| f.0 == LIVE_CLOSE_ID)
                {
                    self.interaction.clear_focus();
                    self.set_live_open(false);
                    return None;
                }
                let id = self.selected.clone().or_else(|| self.first_node())?;
                return Some(self.enter(&id));
            }
            KeyCode::Esc => return self.back(),
            KeyCode::PageUp | KeyCode::PageDown => {
                let page = i32::from(self.last_view.height.saturating_sub(1)).max(1);
                let d = if k.code == KeyCode::PageUp {
                    -page
                } else {
                    page
                };
                if self.live_open {
                    self.scroll_live(d);
                } else {
                    self.pan_by(0, d);
                }
            }
            _ => {}
        }
        None
    }

    fn choose_density(&mut self, view: Rect) -> Density {
        let n = self.run.nodes.len();
        if n > COMPACT_NODE_LIMIT {
            return Density::Compact;
        }
        let full = self.full.get(&self.run, Density::Full);
        if full.fits(view.width, view.height) {
            Density::Full
        } else {
            Density::Compact
        }
    }

    fn ensure_visible(&mut self, view: Rect) {
        let Some(sel) = self.selected.clone() else {
            return;
        };
        let Some(b) = self
            .current_layout()
            .and_then(|l| l.get(&sel))
            .map(|b| b.rect)
        else {
            return;
        };
        let (mut px, mut py) = self.pan;
        if b.x < px {
            px = b.x.saturating_sub(2);
        } else if b.x + b.width > px + view.width {
            px = (b.x + b.width).saturating_sub(view.width) + 1;
        }
        if b.y < py {
            py = b.y.saturating_sub(1);
        } else if b.y + b.height > py + view.height {
            py = (b.y + b.height).saturating_sub(view.height) + 1;
        }
        self.pan = (px, py);
    }

    /// Renders the map (header, graph, optional live pane) into `area`.
    pub fn render(&mut self, frame: &mut Frame<'_>, area: Rect, now: OffsetDateTime) {
        self.frames += 1;
        let theme = self.cfg.theme.clone();
        let mut hits = self.interaction.begin_frame();
        let [header, body] =
            Layout::vertical([Constraint::Length(1), Constraint::Min(1)]).areas(area);
        frame.render_widget(
            Paragraph::new(self.header_line(&theme, now, header.width)),
            header,
        );
        let (graph, live) = if self.live_open && body.height >= 8 {
            let [g, l] = Layout::vertical([Constraint::Percentage(60), Constraint::Percentage(40)])
                .areas(body);
            (g, Some(l))
        } else {
            (body, None)
        };
        self.render_graph(frame, graph, &theme, now, &mut hits);
        if let Some(l) = live {
            self.render_live(frame, l, &theme, &mut hits);
        }
        self.interaction.end_frame(hits);
    }

    fn header_line(&self, theme: &Theme, now: OffsetDateTime, width: u16) -> Line<'static> {
        let r = &self.run;
        let name = r.name.clone().unwrap_or_else(|| "run".into());
        // An interactive session's run stays open between turns.
        let idle = !r.is_working();
        let (glyph, token, status) = match r.status {
            RunStatus::Running if idle => ('●', Token::Ok, "ready"),
            RunStatus::Running => (self.spinner.glyph(), Token::Running, "running"),
            RunStatus::Succeeded => ('✔', Token::Ok, "succeeded"),
            RunStatus::Failed => ('✗', Token::Error, "failed"),
            RunStatus::Cancelled => ('⊘', Token::Muted, "cancelled"),
        };
        let muted = theme.fg(Token::Muted);
        let num = theme.fg(Token::Number);
        let mut spans = vec![
            Span::styled(
                format!(" {name} "),
                theme.fg(Token::Agent).add_modifier(Modifier::BOLD),
            ),
            Span::styled(format!(" {glyph} {status}"), theme.fg(token)),
        ];
        if let Some(ms) = metrics::run_elapsed_ms(r, now) {
            spans.push(Span::styled("  ", muted));
            spans.push(Span::styled(fmt_ms(ms), num));
        }
        spans.push(Span::styled("  ", muted));
        spans.push(Span::styled(fmt_cost(Some(r.cost_usd)), num));
        spans.push(Span::styled(
            format!(
                "  ↑{} ↓{}",
                fmt_tokens(r.tokens.input + r.tokens.cached_input),
                fmt_tokens(r.tokens.output)
            ),
            num,
        ));
        if let Some(p) = cache_pct(r.tokens.input, r.tokens.cached_input) {
            spans.push(Span::styled(format!(" cache {p}%"), muted));
        }
        let (cp, path) = critical_path(r, now);
        if path.len() > 1 || cp > 0 {
            spans.push(Span::styled("  critical path ", muted));
            spans.push(Span::styled(fmt_ms(cp), num));
        }
        let waiting = r.nodes.iter().filter(|n| blocked(n).is_some()).count();
        if waiting > 0 {
            spans.push(Span::styled(
                format!("  ⏸ {waiting} waiting"),
                theme.fg(Token::Approval).add_modifier(Modifier::BOLD),
            ));
        }
        for n in &r.nodes {
            if n.kind == Some(harness_events::schema::NodeKind::Workflow)
                && let Some((badge, token)) = self.node_badges.get(&n.id)
            {
                spans.push(Span::styled("  budget ", muted));
                spans.push(Span::styled(badge.clone(), theme.fg(*token)));
            }
        }
        let hosts = crate::indicators::active_remotes(r);
        if let Some(h) = hosts.first() {
            spans.push(Span::styled(format!("  ⧉ {h}"), theme.fg(Token::Running)));
        }
        crate::chat::text::truncate_line(Line::from(spans), usize::from(width))
    }

    fn render_graph(
        &mut self,
        frame: &mut Frame<'_>,
        area: Rect,
        theme: &Theme,
        now: OffsetDateTime,
        hits: &mut HitMap<MapAction>,
    ) {
        hits.register_pane(MAP_PANE, area);
        self.last_view = area;
        if self.run.nodes.is_empty() {
            frame.render_widget(
                Paragraph::new(Line::from(Span::styled(
                    " No agents yet.",
                    theme.fg(Token::Muted),
                ))),
                area,
            );
            return;
        }
        self.density = self.choose_density(area);
        let density = self.density;
        match density {
            Density::Full => {}
            Density::Compact => {
                self.compact.get(&self.run, Density::Compact);
            }
        }
        if self.selected.is_none() {
            self.selected = self.first_node();
        }
        self.ensure_visible(area);
        let layout = match density {
            Density::Full => self.full.current(),
            Density::Compact => self.compact.current(),
        }
        .expect("layout computed")
        .clone();
        // Clamp the pan to the graph.
        let max_x = layout.width.saturating_sub(area.width);
        let max_y = layout.height.saturating_sub(area.height);
        self.pan = (self.pan.0.min(max_x), self.pan.1.min(max_y));
        let view = Rect::new(self.pan.0, self.pan.1, area.width, area.height);
        let mut c = Canvas::new(view, theme.base());
        let edge = theme.fg(Token::Border);
        for e in &layout.edges {
            c.path(&e.points, edge, e.arrow);
        }
        for b in &layout.boxes {
            if !rects_overlap(b.rect, view) {
                continue;
            }
            let Some(n) = self.run.node(&b.id) else {
                continue;
            };
            let selected = self.selected.as_ref() == Some(&b.id);
            match (b.kind, density) {
                (BoxKind::Step, Density::Full) => {
                    self.draw_full(&mut c, b.rect, n, selected, theme, now)
                }
                _ => self.draw_line(&mut c, b.rect, n, selected, theme, now, b.kind),
            }
            let screen = to_screen(b.rect, view, area);
            if let Some(s) = screen {
                hits.register(node_element_id(&b.id), s, MapAction::Select(b.id.clone()));
            }
        }
        if density == Density::Full {
            for e in &layout.edges {
                if e.kind == harness_events::reducer::EdgeKind::Spawn
                    && layout.get(&e.from).is_some_and(|f| f.kind == BoxKind::Step)
                    && let Some(&(x, y)) = e.points.first()
                {
                    c.put(x, y, '┬', edge);
                }
            }
        }
        frame.render_widget(Paragraph::new(c.lines()), area);
    }

    fn border_style(&self, n: &NodeState, selected: bool, theme: &Theme) -> Style {
        if blocked(n).is_some() {
            theme.fg(Token::Approval).add_modifier(Modifier::BOLD)
        } else if selected {
            theme.fg(Token::BorderFocus).add_modifier(Modifier::BOLD)
        } else {
            theme.fg(Token::Border)
        }
    }

    fn host_spans(&self, host: &str, theme: &Theme) -> Vec<Span<'static>> {
        let mut v = vec![Span::styled(format!("⧉{host}"), theme.fg(Token::Running))];
        if let Some((badge, token)) = self.host_badges.get(host) {
            v.push(Span::styled(format!(" {badge}"), theme.fg(*token)));
        }
        v
    }

    fn preview(&self, n: &NodeState, theme: &Theme) -> Option<Span<'static>> {
        if !self.cfg.live_preview || n.status != NodeStatus::Running {
            return None;
        }
        let (text, reasoning) = self.live.preview(&n.id)?;
        let style = if reasoning {
            theme.fg(Token::Reasoning)
        } else {
            theme.fg(Token::Fg)
        };
        Some(Span::styled(text, style))
    }

    fn draw_full(
        &self,
        c: &mut Canvas,
        r: Rect,
        n: &NodeState,
        selected: bool,
        theme: &Theme,
        now: OffsetDateTime,
    ) {
        let bg = if selected {
            theme.focused()
        } else {
            Style::default()
        };
        c.fill(r, theme.base().patch(bg));
        c.frame(r, self.border_style(n, selected, theme));
        let w = r.width - 2;
        let x = r.x + 1;
        let (glyph, token) = node_glyph(n.status, &self.spinner);
        let muted = theme.fg(Token::Muted).patch(bg);
        let num = theme.fg(Token::Number).patch(bg);
        let host: Vec<Span<'static>> = n
            .host
            .as_ref()
            .map(|h| {
                self.host_spans(h, theme)
                    .into_iter()
                    .map(|s| s.patch_style(bg))
                    .collect()
            })
            .unwrap_or_default();
        let host_w: u16 = host
            .iter()
            .map(|s| crate::chat::text::width(&s.content) as u16)
            .sum();
        let name_w = if host_w > 0 && host_w + 6 <= w {
            w - host_w - 1
        } else {
            w
        };
        c.spans(
            x,
            r.y + 1,
            &[
                Span::styled(format!("{glyph} "), theme.fg(token).patch(bg)),
                Span::styled(
                    truncate(&n.name, usize::from(name_w.saturating_sub(2))),
                    theme
                        .fg(Token::Agent)
                        .patch(bg)
                        .add_modifier(Modifier::BOLD),
                ),
            ],
            name_w,
        );
        if name_w < w {
            c.spans(x + w - host_w, r.y + 1, &host, host_w);
        }
        let mut who = Vec::new();
        let agent = n
            .agent
            .clone()
            .or_else(|| n.kind.map(|k| format!("{k:?}").to_lowercase()));
        if let Some(a) = agent {
            who.push(Span::styled(a, muted));
        }
        if let Some(m) = &n.model {
            if !who.is_empty() {
                who.push(Span::styled(" · ", muted));
            }
            who.push(Span::styled(m.clone(), muted));
        }
        c.spans(x, r.y + 2, &who, w);
        let mut stats = Vec::new();
        if let Some(ms) = metrics::elapsed_ms(n, now) {
            stats.push(Span::styled(fmt_ms(ms), num));
            stats.push(Span::styled(" ", muted));
        }
        if n.cost_usd > 0.0 || n.model_calls > 0 {
            stats.push(Span::styled(fmt_cost(Some(n.cost_usd)), num));
            stats.push(Span::styled(" ", muted));
        }
        if n.retries > 0 {
            stats.push(Span::styled(
                format!("↻{}", n.retries),
                theme.fg(Token::Warn).patch(bg),
            ));
            stats.push(Span::styled(" ", muted));
        }
        if let Some(v) = n.last_verify_passed {
            let (s, t) = if v {
                ("verify ✔", Token::Ok)
            } else {
                ("verify ✗", Token::Error)
            };
            stats.push(Span::styled(s, theme.fg(t).patch(bg)));
        }
        c.spans(x, r.y + 3, &stats, w);
        if let Some((badge, token)) = self.node_badges.get(&n.id) {
            c.spans(
                x,
                r.y + 4,
                &[Span::styled(badge.clone(), theme.fg(*token).patch(bg))],
                w,
            );
        } else if n.model_calls > 0 {
            let mut tok = vec![Span::styled(
                format!(
                    "↑{} ↓{}",
                    fmt_tokens(n.tokens.input + n.tokens.cached_input),
                    fmt_tokens(n.tokens.output)
                ),
                num,
            )];
            if let Some(p) = metrics::node_cache_pct(n) {
                tok.push(Span::styled(format!(" cache {p}%"), muted));
            }
            c.spans(x, r.y + 4, &tok, w);
        }
        let last = if let Some(b) = blocked(n) {
            Some(Span::styled(
                format!("⏸ {} · Enter", b.short()),
                theme.fg(Token::Approval).patch(bg),
            ))
        } else if let Some(p) = self.preview(n, theme) {
            Some(p.patch_style(bg))
        } else {
            n.error.as_ref().map(|e| {
                Span::styled(
                    e.lines().next().unwrap_or("").to_owned(),
                    theme.fg(Token::Error).patch(bg),
                )
            })
        };
        if let Some(s) = last {
            let text = truncate(&s.content, usize::from(w));
            c.text(x, r.y + 5, &text, s.style, w);
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_line(
        &self,
        c: &mut Canvas,
        r: Rect,
        n: &NodeState,
        selected: bool,
        theme: &Theme,
        now: OffsetDateTime,
        kind: BoxKind,
    ) {
        let bg = if selected {
            theme.focused()
        } else {
            Style::default()
        };
        let (glyph, token) = node_glyph(n.status, &self.spinner);
        let name_token = if blocked(n).is_some() {
            Token::Approval
        } else {
            Token::Agent
        };
        let mut spans = vec![
            Span::styled(format!("{glyph} "), theme.fg(token).patch(bg)),
            Span::styled(
                n.name.clone(),
                theme.fg(name_token).patch(bg).add_modifier(Modifier::BOLD),
            ),
        ];
        if matches!(kind, BoxKind::Tree { .. })
            && let Some(m) = &n.model
        {
            spans.push(Span::styled(
                format!(" ({m})"),
                theme.fg(Token::Muted).patch(bg),
            ));
        }
        if let Some(ms) = metrics::elapsed_ms(n, now) {
            spans.push(Span::styled(
                format!(" {}", fmt_ms(ms)),
                theme.fg(Token::Number).patch(bg),
            ));
        }
        if n.cost_usd > 0.0 {
            spans.push(Span::styled(
                format!(" {}", fmt_cost(Some(n.cost_usd))),
                theme.fg(Token::Number).patch(bg),
            ));
        }
        if n.retries > 0 {
            spans.push(Span::styled(
                format!(" ↻{}", n.retries),
                theme.fg(Token::Warn).patch(bg),
            ));
        }
        if let Some(h) = &n.host {
            spans.push(Span::styled(" ", bg));
            spans.extend(
                self.host_spans(h, theme)
                    .into_iter()
                    .map(|s| s.patch_style(bg)),
            );
        }
        if let Some((badge, token)) = self.node_badges.get(&n.id) {
            spans.push(Span::styled(
                format!(" {badge}"),
                theme.fg(*token).patch(bg),
            ));
        }
        if matches!(kind, BoxKind::Tree { .. })
            && let Some(p) = self.preview(n, theme)
        {
            spans.push(Span::styled(" · ", theme.fg(Token::Muted).patch(bg)));
            spans.push(p.patch_style(bg));
        }
        c.spans(r.x, r.y, &spans, r.width);
    }

    fn render_live(
        &mut self,
        frame: &mut Frame<'_>,
        area: Rect,
        theme: &Theme,
        hits: &mut HitMap<MapAction>,
    ) {
        let name = self
            .selected
            .as_ref()
            .and_then(|s| self.run.node(s))
            .map(|n| n.name.clone())
            .unwrap_or_default();
        let focused = hits.is_focused(&ElementId::from(LIVE_CLOSE_ID));
        let block = Block::new()
            .borders(Borders::TOP)
            .border_style(theme.fg(Token::Border))
            .title(Line::from(vec![
                Span::styled(" live · ", theme.fg(Token::Muted)),
                Span::styled(name, theme.fg(Token::Agent)),
                Span::styled(" ", theme.fg(Token::Muted)),
            ]));
        let inner = block.inner(area);
        frame.render_widget(block, area);
        let close = Rect::new(area.x + area.width.saturating_sub(5), area.y, 4, 1);
        let style = if focused {
            theme.focused()
        } else {
            theme.fg(Token::Muted)
        };
        frame.render_widget(Paragraph::new(Span::styled("[x]", style)), close);
        hits.register(LIVE_CLOSE_ID, close, MapAction::CloseLive);
        hits.register_pane(PaneId::from(LIVE_PANE), inner);
        let lines = match self.selected.as_ref().and_then(|s| self.live.stream(s)) {
            Some(st) => LivePane {
                theme,
                tool_lines: 4,
            }
            .tail(st, inner, self.live_back),
            None => vec![Line::from(Span::styled(
                "no output yet",
                theme.fg(Token::Muted),
            ))],
        };
        frame.render_widget(Paragraph::new(lines), inner);
    }
}

fn rects_overlap(a: Rect, b: Rect) -> bool {
    a.x < b.x + b.width && b.x < a.x + a.width && a.y < b.y + b.height && b.y < a.y + a.height
}

/// Maps a graph rect to screen coordinates inside `area`, clipped.
fn to_screen(r: Rect, view: Rect, area: Rect) -> Option<Rect> {
    let x1 = r.x.max(view.x);
    let y1 = r.y.max(view.y);
    let x2 = (r.x + r.width).min(view.x + view.width);
    let y2 = (r.y + r.height).min(view.y + view.height);
    (x1 < x2 && y1 < y2).then(|| {
        Rect::new(
            area.x + (x1 - view.x),
            area.y + (y1 - view.y),
            x2 - x1,
            y2 - y1,
        )
    })
}

#[cfg(test)]
mod tests;
