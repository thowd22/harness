//! Run replay with a time scrubber.
//!
//! [`Replay`] holds a run's events and a cursor: the number of events
//! applied. [`Replay::state`] re-folds the events up to the cursor with the
//! shared reducer (incrementally when moving forward, from the nearest
//! checkpoint when moving back), and [`Replay::visible`] is the event
//! prefix the map ([`crate::map::MapView::set_state`]) and node detail
//! ([`crate::detail::DetailView::from_events`]) are rebuilt from, so they
//! show the run exactly as it was at that point.
//!
//! Stepping works by event and by node (the next or previous
//! `node_started` / `node_finished`). [`ReplayBar`] draws the controls and
//! the scrubber; every button and the scrubber track are clickable and the
//! keys are ←/→ (event), `[`/`]` (node), Home/End.

use crossterm::event::{Event, KeyCode, KeyEventKind, MouseButton, MouseEventKind};
use harness_events::reducer::RunState;
use harness_events::schema::{EventKind, RunEvent};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::chat::text::sanitize;
use crate::interaction::{Dispatch, ElementId, HitMap, Interaction};
use crate::theme::{Theme, Token};

/// Events between two cached states.
pub const CHECKPOINT_EVERY: usize = 512;

/// A run being replayed.
#[derive(Debug, Clone)]
pub struct Replay {
    events: Vec<RunEvent>,
    cursor: usize,
    state: RunState,
    applied: usize,
    checkpoints: Vec<RunState>,
}

impl Replay {
    /// A replay positioned at the end of `events`.
    pub fn new(events: Vec<RunEvent>) -> Replay {
        let len = events.len();
        let mut r = Replay {
            events,
            cursor: 0,
            state: RunState::new(),
            applied: 0,
            checkpoints: vec![RunState::new()],
        };
        r.seek(len);
        r
    }

    /// Number of events.
    pub fn len(&self) -> usize {
        self.events.len()
    }

    /// Whether the run has no events.
    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }

    /// Events applied so far.
    pub fn position(&self) -> usize {
        self.cursor
    }

    /// The events up to the cursor.
    pub fn visible(&self) -> &[RunEvent] {
        &self.events[..self.cursor]
    }

    /// The last applied event.
    pub fn current(&self) -> Option<&RunEvent> {
        self.cursor.checked_sub(1).map(|i| &self.events[i])
    }

    /// The run state at the cursor.
    pub fn state(&self) -> &RunState {
        &self.state
    }

    /// Moves the cursor to `n` applied events (clamped).
    pub fn seek(&mut self, n: usize) {
        let n = n.min(self.events.len());
        if n < self.applied {
            let cp = n / CHECKPOINT_EVERY;
            let cp = cp.min(self.checkpoints.len() - 1);
            self.state = self.checkpoints[cp].clone();
            self.applied = cp * CHECKPOINT_EVERY;
        }
        while self.applied < n {
            self.state.apply(&self.events[self.applied]);
            self.applied += 1;
            if self.applied.is_multiple_of(CHECKPOINT_EVERY)
                && self.checkpoints.len() == self.applied / CHECKPOINT_EVERY
            {
                self.checkpoints.push(self.state.clone());
            }
        }
        self.cursor = n;
    }

    /// Steps by `delta` events.
    pub fn step_event(&mut self, delta: i64) {
        let n = (self.cursor as i64 + delta).max(0) as usize;
        self.seek(n);
    }

    /// Steps to the next (`forward`) or previous node boundary: an event
    /// that starts or finishes a node.
    pub fn step_node(&mut self, forward: bool) {
        let is_boundary = |e: &RunEvent| {
            matches!(
                e.kind,
                EventKind::NodeStarted { .. } | EventKind::NodeFinished { .. }
            )
        };
        let target = if forward {
            (self.cursor..self.events.len())
                .find(|&i| is_boundary(&self.events[i]))
                .map_or(self.events.len(), |i| i + 1)
        } else {
            (0..self.cursor.saturating_sub(1))
                .rev()
                .find(|&i| is_boundary(&self.events[i]))
                .map_or(0, |i| i + 1)
        };
        self.seek(target);
    }
}

/// Replay controls.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplayAction {
    /// Previous node boundary.
    PrevNode,
    /// Previous event.
    PrevEvent,
    /// Next event.
    NextEvent,
    /// Next node boundary.
    NextNode,
    /// The scrubber track (the click column picks the position).
    Track,
}

/// Element id of a replay control.
pub fn control_id(a: ReplayAction) -> &'static str {
    match a {
        ReplayAction::PrevNode => "replay.prev_node",
        ReplayAction::PrevEvent => "replay.prev_event",
        ReplayAction::NextEvent => "replay.next_event",
        ReplayAction::NextNode => "replay.next_node",
        ReplayAction::Track => "replay.track",
    }
}

/// What the bar asks the app to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplayOutput {
    /// The cursor moved; rebuild the views from [`Replay::visible`].
    Moved,
    /// Esc: leave replay.
    Back,
}

/// The scrubber bar under the map.
pub struct ReplayBar {
    theme: Theme,
    track: Rect,
    interaction: Interaction<ReplayAction>,
}

impl ReplayBar {
    /// A bar.
    pub fn new(theme: Theme) -> ReplayBar {
        ReplayBar {
            theme,
            track: Rect::default(),
            interaction: Interaction::new(),
        }
    }

    fn act(&mut self, r: &mut Replay, a: ReplayAction, x: Option<u16>) -> Option<ReplayOutput> {
        let before = r.position();
        match a {
            ReplayAction::PrevNode => r.step_node(false),
            ReplayAction::PrevEvent => r.step_event(-1),
            ReplayAction::NextEvent => r.step_event(1),
            ReplayAction::NextNode => r.step_node(true),
            ReplayAction::Track => {
                let x = x?;
                let w = self.track.width.max(1) as usize;
                let off = x.saturating_sub(self.track.x) as usize;
                r.seek(
                    (off * r.len())
                        .div_ceil(w.saturating_sub(1).max(1))
                        .min(r.len()),
                );
            }
        }
        (r.position() != before).then_some(ReplayOutput::Moved)
    }

    /// Handles input against `replay`.
    pub fn handle(&mut self, replay: &mut Replay, ev: &Event) -> Option<ReplayOutput> {
        match ev {
            Event::Key(k) if k.kind != KeyEventKind::Release => {
                let a = match k.code {
                    KeyCode::Left => Some(ReplayAction::PrevEvent),
                    KeyCode::Right => Some(ReplayAction::NextEvent),
                    KeyCode::Char('[') => Some(ReplayAction::PrevNode),
                    KeyCode::Char(']') => Some(ReplayAction::NextNode),
                    KeyCode::Home => {
                        replay.seek(0);
                        return Some(ReplayOutput::Moved);
                    }
                    KeyCode::End => {
                        let n = replay.len();
                        replay.seek(n);
                        return Some(ReplayOutput::Moved);
                    }
                    _ => None,
                };
                if let Some(a) = a {
                    return self.act(replay, a, None);
                }
                match self.interaction.handle_key(k)? {
                    Dispatch::Activate(a) => self.act(replay, a, None),
                    Dispatch::Back => Some(ReplayOutput::Back),
                    _ => None,
                }
            }
            Event::Mouse(m) if m.kind == MouseEventKind::Down(MouseButton::Left) => {
                match self.interaction.handle_mouse(m)? {
                    Dispatch::Activate(a) => self.act(replay, a, Some(m.column)),
                    Dispatch::Back => Some(ReplayOutput::Back),
                    _ => None,
                }
            }
            _ => None,
        }
    }

    /// Rows the bar needs at `width` for `replay` (buttons, track and
    /// position flow onto more rows when one is too narrow).
    pub fn height(&self, width: u16, replay: &Replay) -> u16 {
        self.layout(Rect::new(0, 0, width, u16::MAX), replay).2
    }

    fn info(replay: &Replay) -> String {
        match replay.current() {
            Some(e) => sanitize(&format!(
                " {}/{} {:02}:{:02}:{:02} {}{}",
                replay.position(),
                replay.len(),
                e.ts.hour(),
                e.ts.minute(),
                e.ts.second(),
                e.kind.type_name(),
                e.node_id
                    .as_ref()
                    .map(|n| format!(" {n}"))
                    .unwrap_or_default()
            )),
            None => format!(" 0/{}", replay.len()),
        }
    }

    /// Button rects, the track rect, the rows used and the info rows.
    #[allow(clippy::type_complexity)]
    fn layout(
        &self,
        area: Rect,
        replay: &Replay,
    ) -> (
        Vec<(ReplayAction, &'static str, Rect)>,
        Rect,
        u16,
        Vec<(u16, String)>,
    ) {
        let aw = area.width.max(1);
        let (mut x, mut y) = (0u16, 0u16);
        let mut buttons = Vec::new();
        for (a, label) in BUTTONS {
            let w = (label.chars().count() as u16).min(aw);
            if x > 0 && x + w > aw {
                x = 0;
                y += 1;
            }
            buttons.push((
                a,
                label,
                Rect::new(area.x + x, area.y.saturating_add(y), w, 1),
            ));
            x += w + 1;
        }
        let info = Self::info(replay);
        let info_w = crate::chat::text::width(&info) as u16;
        let rem = aw.saturating_sub(x);
        let mut infos = Vec::new();
        let track;
        if rem >= 8 + info_w {
            track = Rect::new(area.x + x, area.y.saturating_add(y), rem - info_w, 1);
            infos.push((y, info));
        } else {
            if rem < 8 {
                x = 0;
                y += 1;
            }
            let tw = aw - x;
            track = Rect::new(area.x + x, area.y.saturating_add(y), tw, 1);
            for l in crate::chat::text::wrap_text(info.trim_start(), usize::from(aw)) {
                y += 1;
                infos.push((y, l));
            }
        }
        (buttons, track, y + 1, infos)
    }

    /// Renders the bar for `replay`, flowing onto the rows of `area`
    /// ([`ReplayBar::height`]).
    pub fn render(&mut self, frame: &mut Frame<'_>, area: Rect, replay: &Replay) {
        let t = self.theme.clone();
        let mut hits: HitMap<ReplayAction> = self.interaction.begin_frame();
        let (buttons, track, _, infos) = self.layout(area, replay);
        let inside = |r: Rect| r.y < area.bottom() && r.x < area.right() && r.width > 0;
        for (a, label, rect) in buttons {
            if !inside(rect) {
                continue;
            }
            let style = if hits.is_focused(&ElementId::from(control_id(a))) {
                t.focused()
            } else {
                t.fg(Token::Accent)
            };
            frame.render_widget(Paragraph::new(Span::styled(label, style)), rect);
            hits.register(control_id(a), rect, a);
        }
        self.track = track;
        let filled = if replay.is_empty() {
            0
        } else {
            (replay.position() * usize::from(self.track.width.saturating_sub(1))) / replay.len()
        };
        let mut bar = String::new();
        for i in 0..usize::from(self.track.width) {
            bar.push(if i == filled {
                '●'
            } else if i < filled {
                '━'
            } else {
                '─'
            });
        }
        if inside(track) {
            frame.render_widget(
                Paragraph::new(Line::from(Span::styled(bar, t.fg(Token::Running)))),
                track,
            );
            hits.register(control_id(ReplayAction::Track), track, ReplayAction::Track);
        }
        for (k, (row, text)) in infos.into_iter().enumerate() {
            let y = area.y.saturating_add(row);
            let x = if k == 0 && row == track.y - area.y {
                track.right()
            } else {
                area.x
            };
            let r = Rect::new(x, y, area.right().saturating_sub(x), 1);
            if inside(r) {
                frame.render_widget(Paragraph::new(Span::styled(text, t.fg(Token::Muted))), r);
            }
        }
        self.interaction.end_frame(hits);
    }
}

const BUTTONS: [(ReplayAction, &str); 4] = [
    (ReplayAction::PrevNode, "[◀◀ node]"),
    (ReplayAction::PrevEvent, "[◀ event]"),
    (ReplayAction::NextEvent, "[event ▶]"),
    (ReplayAction::NextNode, "[node ▶▶]"),
];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::demo::events::{EventBuilder, NodeSpec};
    use crate::detail::DetailView;
    use crate::interaction::testing::{click, find_text, key};
    use crate::map::{MapConfig, MapView};
    use harness_events::schema::{DeltaChannel, NodeId, NodeStatus};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn run() -> Vec<RunEvent> {
        let mut b = EventBuilder::new();
        b.run_started("r", None);
        b.start(NodeSpec::step("a"));
        b.model_call("a", "c1", "m");
        b.delta("a", "c1", DeltaChannel::Text, "first");
        b.finish("a", NodeStatus::Succeeded, None);
        b.start(NodeSpec::step("b").needs(&["a"]));
        b.finish("b", NodeStatus::Failed, Some("boom"));
        b.into_events()
    }

    #[test]
    fn refolds_up_to_the_cursor_and_views_show_that_state() {
        let mut r = Replay::new(run());
        assert_eq!(r.position(), 7);
        assert_eq!(
            r.state().node(&NodeId::from("b")).unwrap().status,
            NodeStatus::Failed
        );
        r.seek(3);
        let a = r.state().node(&NodeId::from("a")).unwrap();
        assert_eq!(a.status, NodeStatus::Running);
        assert!(r.state().node(&NodeId::from("b")).is_none());
        assert_eq!(*r.state(), RunState::from_events(r.visible()));
        // The map and detail show the state at the cursor.
        let mut map = MapView::new(MapConfig::default());
        map.set_state(r.visible());
        assert_eq!(map.run_state().nodes.len(), 1);
        let d = DetailView::from_events(
            NodeId::from("a"),
            Theme::default(),
            Default::default(),
            false,
            r.visible(),
        );
        assert!(d.is_running());
        r.seek(7);
        map.set_state(r.visible());
        assert_eq!(map.run_state().nodes.len(), 2);
    }

    #[test]
    fn steps_by_event_and_by_node() {
        let mut r = Replay::new(run());
        r.seek(0);
        r.step_event(1);
        assert_eq!(r.position(), 1);
        r.step_node(true);
        assert!(matches!(
            r.current().unwrap().kind,
            EventKind::NodeStarted { .. }
        ));
        assert_eq!(r.position(), 2);
        r.step_node(true);
        assert!(matches!(
            r.current().unwrap().kind,
            EventKind::NodeFinished { .. }
        ));
        assert_eq!(r.position(), 5);
        r.step_node(false);
        assert_eq!(r.position(), 2);
        r.step_event(-10);
        assert_eq!(r.position(), 0);
    }

    #[test]
    fn checkpoints_give_the_same_state_backwards() {
        let mut b = EventBuilder::new();
        b.start(NodeSpec::step("a"));
        for i in 0..2_000 {
            b.cost("a", 0.001 * f64::from(i % 3));
        }
        let events = b.into_events();
        let mut r = Replay::new(events.clone());
        r.seek(1_300);
        assert_eq!(*r.state(), RunState::from_events(&events[..1_300]));
        r.seek(700);
        assert_eq!(*r.state(), RunState::from_events(&events[..700]));
    }

    #[test]
    fn bar_buttons_and_track_work_by_click_and_keyboard() {
        let mut r = Replay::new(run());
        let mut bar = ReplayBar::new(Theme::default());
        let mut term = Terminal::new(TestBackend::new(100, 1)).unwrap();
        term.draw(|f| bar.render(f, f.area(), &r)).unwrap();
        let (x, y) = find_text(term.backend().buffer(), "[◀ event]").unwrap();
        assert_eq!(bar.handle(&mut r, &click(x, y)), Some(ReplayOutput::Moved));
        assert_eq!(r.position(), 6);
        let (x, y) = find_text(term.backend().buffer(), "[◀◀ node]").unwrap();
        bar.handle(&mut r, &click(x, y));
        assert_eq!(r.position(), 5, "just after the previous boundary");
        // Track: clicking its start rewinds to the beginning.
        let (x, y) = find_text(term.backend().buffer(), "[node ▶▶]").unwrap();
        bar.handle(&mut r, &click(x + 10, y));
        assert_eq!(r.position(), 0);
        bar.handle(&mut r, &key("right"));
        assert_eq!(r.position(), 1);
        bar.handle(&mut r, &key("]"));
        assert_eq!(r.position(), 2);
        bar.handle(&mut r, &key("end"));
        assert_eq!(r.position(), 7);
        // Focus a button with Tab and press Enter.
        term.draw(|f| bar.render(f, f.area(), &r)).unwrap();
        bar.handle(&mut r, &key("tab"));
        bar.handle(&mut r, &key("enter"));
        assert_eq!(r.position(), 6);
        assert_eq!(bar.handle(&mut r, &key("esc")), Some(ReplayOutput::Back));
    }

    /// At a narrow width the bar flows onto more rows ([`ReplayBar::height`])
    /// instead of dropping its buttons, and every button still works by
    /// click and by keyboard.
    #[test]
    fn a_narrow_bar_wraps_and_its_buttons_work_by_click_and_keyboard() {
        let mut r = Replay::new(run());
        let mut bar = ReplayBar::new(Theme::default());
        let h = bar.height(24, &r);
        assert!(h > 1, "wraps: {h}");
        let mut term = Terminal::new(TestBackend::new(24, h)).unwrap();
        term.draw(|f| bar.render(f, f.area(), &r)).unwrap();
        let text = crate::interaction::testing::buffer_text(term.backend().buffer());
        for b in ["[◀◀ node]", "[◀ event]", "[event ▶]", "[node ▶▶]", "/7"] {
            assert!(text.contains(b), "{b}: {text}");
        }
        let (x, y) = find_text(term.backend().buffer(), "[◀ event]").unwrap();
        assert_eq!(bar.handle(&mut r, &click(x, y)), Some(ReplayOutput::Moved));
        assert_eq!(r.position(), 6);
        // Tab moves from the clicked button to `[event ▶]`; Enter runs it.
        bar.handle(&mut r, &key("tab"));
        bar.handle(&mut r, &key("enter"));
        assert_eq!(r.position(), 7);
    }

    /// TASK-160 layout matrix: the bar fits at every size with nothing cut.
    #[test]
    fn replay_bar_fits_at_every_size() {
        use crate::layout_matrix::*;
        matrix("replay bar", |w, h, check| {
            let r = Replay::new(run());
            let mut bar = ReplayBar::new(Theme::default());
            let bh = bar.height(w, &r).min(h);
            let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
            let buf = frame(&mut t, |f| bar.render(f, Rect::new(0, 0, w, bh), &r));
            if check {
                let text = checked("replay bar", &buf);
                assert_reachable(
                    "replay bar",
                    &[text],
                    &["[◀◀ node]", "[◀ event]", "[event ▶]", "[node ▶▶]", "/7"],
                );
            }
        });
    }
}
