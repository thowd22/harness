//! One hit-test and focus layer for the whole TUI.
//!
//! Every frame, components register their interactive regions in a
//! [`HitMap`] while they render: a clickable, focusable element with an action
//! ([`HitMap::register`]) and scrollable panes ([`HitMap::register_pane`]).
//! Overlays push a modal layer first ([`HitMap::push_modal`]) so that
//! everything below them stops receiving clicks and focus.
//!
//! [`Interaction`] keeps the map of the last rendered frame and turns input
//! into a [`Dispatch`]:
//!
//! - left click: the topmost region under the cursor gets focus and is
//!   activated; a click outside an open modal backs out of it,
//! - mouse wheel: scrolls the pane under the cursor,
//! - Tab / Down / Right and Shift+Tab / Up / Left: move focus between
//!   registered elements in registration order (wrapping),
//! - Enter: activates the focused element, Esc: backs out,
//! - PgUp / PgDn: scroll the focused pane by a page.
//!
//! Focus is tracked by [`ElementId`], so it survives re-renders as long as the
//! element is registered again. [`testing`] has helpers that build input
//! events and locate text in a rendered buffer.

use std::collections::HashMap;

use crossterm::event::{
    KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::layout::{Position, Rect};

/// Stable id of an interactive element, e.g. `help.close` or `card.17`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ElementId(pub String);

impl From<&str> for ElementId {
    fn from(s: &str) -> Self {
        ElementId(s.to_owned())
    }
}

impl From<String> for ElementId {
    fn from(s: String) -> Self {
        ElementId(s)
    }
}

/// Stable id of a scrollable pane, e.g. `chat` or `map`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PaneId(pub String);

impl From<&str> for PaneId {
    fn from(s: &str) -> Self {
        PaneId(s.to_owned())
    }
}

/// One registered interactive element.
#[derive(Debug, Clone, PartialEq)]
pub struct Region<A> {
    /// Element id.
    pub id: ElementId,
    /// Screen rectangle.
    pub rect: Rect,
    /// What activating it does.
    pub action: A,
    /// Layer index (higher is on top).
    pub layer: usize,
    /// Pane the element lives in, when registered inside one.
    pub pane: Option<PaneId>,
}

#[derive(Debug, Clone, PartialEq)]
struct Pane {
    id: PaneId,
    rect: Rect,
    layer: usize,
}

/// Interactive regions of one rendered frame.
#[derive(Debug, Clone)]
pub struct HitMap<A> {
    regions: Vec<Region<A>>,
    panes: Vec<Pane>,
    /// Modal layers: (layer index, rectangle).
    modals: Vec<(usize, Rect)>,
    layer: usize,
    focus: Option<ElementId>,
}

impl<A> Default for HitMap<A> {
    fn default() -> Self {
        HitMap {
            regions: Vec::new(),
            panes: Vec::new(),
            modals: Vec::new(),
            layer: 0,
            focus: None,
        }
    }
}

impl<A: Clone> HitMap<A> {
    /// An empty map.
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers a clickable, focusable element. Later registrations are on
    /// top of earlier ones in the same layer.
    pub fn register(&mut self, id: impl Into<ElementId>, rect: Rect, action: A) {
        let pane = self.pane_at_layer(rect, self.layer);
        self.regions.push(Region {
            id: id.into(),
            rect,
            action,
            layer: self.layer,
            pane,
        });
    }

    /// Registers a clickable button: like [`HitMap::register`], and its
    /// label is not part of the text selection ([`crate::selection::mask`]).
    pub fn register_button(&mut self, id: impl Into<ElementId>, rect: Rect, action: A) {
        crate::selection::mask(rect);
        self.register(id, rect, action);
    }

    /// Registers a scrollable pane. It is also a text region for mouse and
    /// keyboard selection ([`crate::selection`]); the view can refine it
    /// with [`crate::selection::update`].
    pub fn register_pane(&mut self, id: impl Into<PaneId>, rect: Rect) {
        let id = id.into();
        crate::selection::register(crate::selection::TextRegion::new(id.0.clone(), rect));
        self.panes.push(Pane {
            id,
            rect,
            layer: self.layer,
        });
    }

    /// Starts a new layer on top of everything registered so far.
    pub fn push_layer(&mut self) {
        self.layer += 1;
    }

    /// Starts a modal layer covering `rect`: lower layers get no clicks,
    /// scrolls or focus until the next frame.
    ///
    /// The modal is also a text region for selection (borders left out),
    /// covering everything below it.
    pub fn push_modal(&mut self, rect: Rect) {
        crate::selection::register(
            crate::selection::TextRegion::new(
                format!("modal@{},{},{}x{}", rect.x, rect.y, rect.width, rect.height),
                rect,
            )
            .chrome(crate::selection::Chrome::Drawing),
        );
        self.push_layer();
        self.modals.push((self.layer, rect));
    }

    /// Whether `id` had focus when this frame started rendering.
    pub fn is_focused(&self, id: &ElementId) -> bool {
        self.focus.as_ref() == Some(id)
    }

    /// The lowest layer that still receives input.
    fn active_floor(&self) -> usize {
        self.modals.last().map_or(0, |(layer, _)| *layer)
    }

    fn pane_at_layer(&self, rect: Rect, layer: usize) -> Option<PaneId> {
        let p = Position::new(rect.x, rect.y);
        self.panes
            .iter()
            .rev()
            .find(|pane| pane.layer <= layer && pane.rect.contains(p))
            .map(|pane| pane.id.clone())
    }

    /// The topmost active region under `(x, y)`.
    pub fn hit(&self, x: u16, y: u16) -> Option<&Region<A>> {
        let floor = self.active_floor();
        let p = Position::new(x, y);
        self.regions
            .iter()
            .enumerate()
            .filter(|(_, r)| r.layer >= floor && r.rect.contains(p))
            .max_by_key(|(i, r)| (r.layer, *i))
            .map(|(_, r)| r)
    }

    /// The topmost active pane under `(x, y)`.
    pub fn pane_at(&self, x: u16, y: u16) -> Option<&PaneId> {
        let floor = self.active_floor();
        let p = Position::new(x, y);
        self.panes
            .iter()
            .enumerate()
            .filter(|(_, pane)| pane.layer >= floor && pane.rect.contains(p))
            .max_by_key(|(i, pane)| (pane.layer, *i))
            .map(|(_, pane)| &pane.id)
    }

    /// Rectangle of pane `id`, if registered.
    pub fn pane_rect(&self, id: &PaneId) -> Option<Rect> {
        self.panes.iter().find(|p| &p.id == id).map(|p| p.rect)
    }

    /// Focusable elements that currently receive input, in focus order.
    pub fn focusable(&self) -> impl Iterator<Item = &Region<A>> {
        let floor = self.active_floor();
        self.regions.iter().filter(move |r| r.layer >= floor)
    }

    /// The region with id `id`, if registered and active.
    pub fn region(&self, id: &ElementId) -> Option<&Region<A>> {
        self.focusable().find(|r| &r.id == id)
    }

    /// Whether a modal layer is open.
    pub fn has_modal(&self) -> bool {
        !self.modals.is_empty()
    }

    /// Whether `(x, y)` is inside the topmost modal (always true without one).
    pub fn inside_modal(&self, x: u16, y: u16) -> bool {
        self.modals
            .last()
            .is_none_or(|(_, rect)| rect.contains(Position::new(x, y)))
    }
}

/// What an input event resolved to.
#[derive(Debug, Clone, PartialEq)]
pub enum Dispatch<A> {
    /// Run this element's action.
    Activate(A),
    /// Scroll `pane` by `lines` (negative = up).
    Scroll {
        /// The pane.
        pane: PaneId,
        /// Lines to scroll, negative is up.
        lines: i32,
    },
    /// Esc, or a click outside a modal: close the overlay or step back.
    Back,
    /// Focus moved to this element without activating it.
    Focus(ElementId),
}

/// Lines scrolled per wheel notch.
pub const WHEEL_LINES: i32 = 3;

/// The focus model plus the hit map of the last rendered frame.
#[derive(Debug, Clone)]
pub struct Interaction<A> {
    last: HitMap<A>,
    focus: Option<ElementId>,
    focused_pane: Option<PaneId>,
}

impl<A> Default for Interaction<A> {
    fn default() -> Self {
        Interaction {
            last: HitMap::default(),
            focus: None,
            focused_pane: None,
        }
    }
}

impl<A: Clone> Interaction<A> {
    /// A fresh model with nothing focused.
    pub fn new() -> Self {
        Self::default()
    }

    /// Starts a frame: returns an empty map that remembers the current focus
    /// (so components can draw it). Pass it back to [`Interaction::end_frame`].
    pub fn begin_frame(&self) -> HitMap<A> {
        HitMap {
            focus: self.focus.clone(),
            ..HitMap::default()
        }
    }

    /// Finishes a frame. If the focused element is gone or sits below a modal,
    /// focus moves to the first active element (inside a modal) or is cleared.
    /// Returns true when focus changed, so the caller should draw again to
    /// show it.
    pub fn end_frame(&mut self, map: HitMap<A>) -> bool {
        let still_there = self.focus.as_ref().is_some_and(|f| map.region(f).is_some());
        let mut changed = false;
        if !still_there {
            let next = if map.has_modal() {
                map.focusable().next().map(|r| r.id.clone())
            } else {
                None
            };
            changed = next != self.focus;
            self.focus = next;
        }
        self.last = map;
        changed
    }

    /// The map of the last rendered frame.
    pub fn hits(&self) -> &HitMap<A> {
        &self.last
    }

    /// The focused element.
    pub fn focused(&self) -> Option<&ElementId> {
        self.focus.as_ref()
    }

    /// Focuses `id` (takes effect for hit-testing immediately, for drawing on
    /// the next frame).
    pub fn set_focus(&mut self, id: impl Into<ElementId>) {
        self.focus = Some(id.into());
    }

    /// Clears focus.
    pub fn clear_focus(&mut self) {
        self.focus = None;
    }

    /// The pane PgUp/PgDn scroll: the focused element's pane, else the last
    /// pane clicked or scrolled, else the first registered pane.
    pub fn focused_pane(&self) -> Option<PaneId> {
        self.focus
            .as_ref()
            .and_then(|f| self.last.region(f))
            .and_then(|r| r.pane.clone())
            .or_else(|| self.focused_pane.clone())
            .or_else(|| {
                let floor = self.last.active_floor();
                self.last
                    .panes
                    .iter()
                    .find(|p| p.layer >= floor)
                    .map(|p| p.id.clone())
            })
    }

    /// Makes `pane` the target of PgUp/PgDn.
    pub fn set_focused_pane(&mut self, pane: impl Into<PaneId>) {
        self.focused_pane = Some(pane.into());
    }

    /// Moves focus forward (`forward = true`) or back, wrapping.
    pub fn move_focus(&mut self, forward: bool) -> Option<Dispatch<A>> {
        let ids: Vec<ElementId> = self.last.focusable().map(|r| r.id.clone()).collect();
        if ids.is_empty() {
            return None;
        }
        let pos = self
            .focus
            .as_ref()
            .and_then(|f| ids.iter().position(|i| i == f));
        let next = match (pos, forward) {
            (None, true) => 0,
            (None, false) => ids.len() - 1,
            (Some(i), true) => (i + 1) % ids.len(),
            (Some(i), false) => (i + ids.len() - 1) % ids.len(),
        };
        self.focus = Some(ids[next].clone());
        Some(Dispatch::Focus(ids[next].clone()))
    }

    /// Activates the focused element.
    pub fn activate_focused(&self) -> Option<Dispatch<A>> {
        let id = self.focus.as_ref()?;
        self.last
            .region(id)
            .map(|r| Dispatch::Activate(r.action.clone()))
    }

    /// Handles a key: focus movement, Enter, Esc and paging. Returns `None`
    /// for keys this layer does not use.
    pub fn handle_key(&mut self, key: &KeyEvent) -> Option<Dispatch<A>> {
        if key.kind == KeyEventKind::Release {
            return None;
        }
        let plain = key.modifiers.difference(KeyModifiers::SHIFT).is_empty();
        if !plain {
            return None;
        }
        match key.code {
            KeyCode::Tab if key.modifiers.contains(KeyModifiers::SHIFT) => self.move_focus(false),
            KeyCode::Tab | KeyCode::Down | KeyCode::Right => self.move_focus(true),
            KeyCode::BackTab | KeyCode::Up | KeyCode::Left => self.move_focus(false),
            KeyCode::Enter => self.activate_focused(),
            KeyCode::Esc => Some(Dispatch::Back),
            KeyCode::PageUp | KeyCode::PageDown => {
                let pane = self.focused_pane()?;
                let page = self
                    .last
                    .pane_rect(&pane)
                    .map_or(1, |r| i32::from(r.height.saturating_sub(1)).max(1));
                let lines = if key.code == KeyCode::PageUp {
                    -page
                } else {
                    page
                };
                Some(Dispatch::Scroll { pane, lines })
            }
            _ => None,
        }
    }

    /// Handles a mouse event: left click activates, wheel scrolls.
    pub fn handle_mouse(&mut self, ev: &MouseEvent) -> Option<Dispatch<A>> {
        let (x, y) = (ev.column, ev.row);
        match ev.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                if !self.last.inside_modal(x, y) {
                    return Some(Dispatch::Back);
                }
                if let Some(pane) = self.last.pane_at(x, y) {
                    self.focused_pane = Some(pane.clone());
                }
                let region = self.last.hit(x, y)?;
                self.focus = Some(region.id.clone());
                Some(Dispatch::Activate(region.action.clone()))
            }
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                let pane = self.last.pane_at(x, y)?.clone();
                self.focused_pane = Some(pane.clone());
                let lines = if ev.kind == MouseEventKind::ScrollUp {
                    -WHEEL_LINES
                } else {
                    WHEEL_LINES
                };
                Some(Dispatch::Scroll { pane, lines })
            }
            _ => None,
        }
    }
}

/// Scroll offsets per pane.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Scrolls {
    offsets: HashMap<PaneId, u16>,
}

impl Scrolls {
    /// Applies a scroll of `lines` to `pane`, never going below zero.
    pub fn scroll(&mut self, pane: &PaneId, lines: i32) {
        let cur = i32::from(self.offset(pane));
        let next = (cur + lines).clamp(0, i32::from(u16::MAX));
        self.offsets.insert(pane.clone(), next as u16);
    }

    /// Current offset of `pane`.
    pub fn offset(&self, pane: &PaneId) -> u16 {
        self.offsets.get(pane).copied().unwrap_or(0)
    }

    /// Offset clamped to `max` (and stored clamped, so scrolling back is
    /// immediate after overscrolling).
    pub fn clamped(&mut self, pane: &PaneId, max: u16) -> u16 {
        let v = self.offset(pane).min(max);
        self.offsets.insert(pane.clone(), v);
        v
    }
}

/// Helpers for simulating input against rendered frames in tests.
pub mod testing {
    use crossterm::event::{Event, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
    use ratatui::buffer::Buffer;
    use ratatui::style::{Modifier, Style};

    use crate::keys::KeyChord;

    fn mouse(kind: MouseEventKind, x: u16, y: u16) -> Event {
        Event::Mouse(MouseEvent {
            kind,
            column: x,
            row: y,
            modifiers: KeyModifiers::NONE,
        })
    }

    /// A left click at `(x, y)`.
    pub fn click(x: u16, y: u16) -> Event {
        mouse(MouseEventKind::Down(MouseButton::Left), x, y)
    }

    /// Moving the mouse with the left button held, to `(x, y)`.
    pub fn drag(x: u16, y: u16) -> Event {
        mouse(MouseEventKind::Drag(MouseButton::Left), x, y)
    }

    /// Releasing the left button at `(x, y)`.
    pub fn release(x: u16, y: u16) -> Event {
        mouse(MouseEventKind::Up(MouseButton::Left), x, y)
    }

    /// One wheel notch up at `(x, y)`.
    pub fn wheel_up(x: u16, y: u16) -> Event {
        mouse(MouseEventKind::ScrollUp, x, y)
    }

    /// One wheel notch down at `(x, y)`.
    pub fn wheel_down(x: u16, y: u16) -> Event {
        mouse(MouseEventKind::ScrollDown, x, y)
    }

    /// A key press from a chord string such as `ctrl+p`. Panics on bad input.
    pub fn key(chord: &str) -> Event {
        let c = KeyChord::parse(chord).unwrap_or_else(|e| panic!("{e}"));
        Event::Key(c.to_event())
    }

    /// Key presses from space-separated chords: `"tab tab enter"`.
    pub fn keys(seq: &str) -> Vec<Event> {
        seq.split_whitespace().map(key).collect()
    }

    /// Key presses typing `text` literally.
    pub fn type_text(text: &str) -> Vec<Event> {
        text.chars()
            .map(|c| {
                Event::Key(crossterm::event::KeyEvent::new(
                    crossterm::event::KeyCode::Char(c),
                    KeyModifiers::NONE,
                ))
            })
            .collect()
    }

    /// Plain text of `buf`, one line per row, trailing spaces trimmed.
    pub fn buffer_text(buf: &Buffer) -> String {
        let area = buf.area;
        (area.top()..area.bottom())
            .map(|y| {
                let row: String = (area.left()..area.right())
                    .map(|x| buf[(x, y)].symbol())
                    .collect();
                row.trim_end().to_owned()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Screen position of the first character of the first occurrence of
    /// `needle` in `buf` (single-width characters only).
    pub fn find_text(buf: &Buffer, needle: &str) -> Option<(u16, u16)> {
        let area = buf.area;
        let needle: Vec<char> = needle.chars().collect();
        for y in area.top()..area.bottom() {
            let row: Vec<String> = (area.left()..area.right())
                .map(|x| buf[(x, y)].symbol().to_owned())
                .collect();
            'start: for start in 0..row.len() {
                for (i, ch) in needle.iter().enumerate() {
                    let Some(cell) = row.get(start + i) else {
                        continue 'start;
                    };
                    if !cell.starts_with(*ch) {
                        continue 'start;
                    }
                }
                return Some((area.left() + start as u16, y));
            }
        }
        None
    }

    fn describe(style: Style) -> String {
        let mut parts = Vec::new();
        // `Reset` is the terminal default, the same as no colour.
        for (name, color) in [("fg", style.fg), ("bg", style.bg)] {
            let value = color.map(|c| format!("{c:?}")).filter(|c| c != "Reset");
            if let Some(v) = value {
                parts.push(format!("{name}={v}"));
            }
        }
        if style.add_modifier != Modifier::empty() {
            parts.push(format!("mod={:?}", style.add_modifier));
        }
        parts.join(" ")
    }

    /// The text of `buf` followed by its style runs, one `row x0-x1: style`
    /// line per run of identically styled, non-default cells. Used for styled
    /// snapshot tests.
    pub fn styled_snapshot(buf: &Buffer) -> String {
        let mut out = buffer_text(buf);
        out.push_str("\n--- styles ---\n");
        let area = buf.area;
        for y in area.top()..area.bottom() {
            let mut x = area.left();
            while x < area.right() {
                let style = buf[(x, y)].style();
                let start = x;
                while x < area.right() && buf[(x, y)].style() == style {
                    x += 1;
                }
                let d = describe(style);
                if !d.is_empty() {
                    out.push_str(&format!("{y} {start}-{}: {d}\n", x - 1));
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::testing::*;
    use super::*;
    use crossterm::event::Event;

    #[derive(Debug, Clone, PartialEq)]
    enum Act {
        A,
        B,
        C,
        Close,
    }

    fn rect(x: u16, y: u16, w: u16, h: u16) -> Rect {
        Rect::new(x, y, w, h)
    }

    fn frame(ix: &mut Interaction<Act>, modal: bool) {
        let mut m = ix.begin_frame();
        m.register_pane("left", rect(0, 0, 10, 10));
        m.register_pane("right", rect(10, 0, 10, 10));
        m.register("a", rect(0, 0, 10, 1), Act::A);
        m.register("b", rect(10, 0, 10, 1), Act::B);
        // c overlaps a; registered later so it is on top.
        m.register("c", rect(5, 0, 2, 1), Act::C);
        if modal {
            m.push_modal(rect(2, 4, 6, 3));
            m.register("close", rect(3, 5, 4, 1), Act::Close);
        }
        ix.end_frame(m);
    }

    fn mouse(ix: &mut Interaction<Act>, ev: Event) -> Option<Dispatch<Act>> {
        let Event::Mouse(m) = ev else { panic!() };
        ix.handle_mouse(&m)
    }

    fn press(ix: &mut Interaction<Act>, chord: &str) -> Option<Dispatch<Act>> {
        let Event::Key(k) = key(chord) else { panic!() };
        ix.handle_key(&k)
    }

    #[test]
    fn click_dispatches_to_topmost_region() {
        let mut ix = Interaction::new();
        frame(&mut ix, false);
        assert_eq!(
            mouse(&mut ix, click(1, 0)),
            Some(Dispatch::Activate(Act::A))
        );
        assert_eq!(ix.focused(), Some(&ElementId::from("a")));
        assert_eq!(
            mouse(&mut ix, click(5, 0)),
            Some(Dispatch::Activate(Act::C))
        );
        assert_eq!(
            mouse(&mut ix, click(12, 0)),
            Some(Dispatch::Activate(Act::B))
        );
        assert_eq!(mouse(&mut ix, click(12, 5)), None);
    }

    #[test]
    fn modal_blocks_lower_layers_and_outside_click_backs_out() {
        let mut ix = Interaction::new();
        frame(&mut ix, true);
        // Focus jumps into the modal.
        assert_eq!(ix.focused(), Some(&ElementId::from("close")));
        assert_eq!(
            mouse(&mut ix, click(4, 5)),
            Some(Dispatch::Activate(Act::Close))
        );
        // Inside the modal but not on a region: nothing.
        assert_eq!(mouse(&mut ix, click(2, 4)), None);
        // Outside the modal (on top of `a`): back out instead of activating a.
        assert_eq!(mouse(&mut ix, click(1, 0)), Some(Dispatch::Back));
        // Wheel over a pane below the modal does nothing.
        assert_eq!(mouse(&mut ix, wheel_down(15, 5)), None);
    }

    #[test]
    fn keyboard_focus_cycles_and_activates() {
        let mut ix = Interaction::new();
        frame(&mut ix, false);
        assert_eq!(press(&mut ix, "tab"), Some(Dispatch::Focus("a".into())));
        assert_eq!(press(&mut ix, "down"), Some(Dispatch::Focus("b".into())));
        assert_eq!(press(&mut ix, "right"), Some(Dispatch::Focus("c".into())));
        assert_eq!(
            press(&mut ix, "tab"),
            Some(Dispatch::Focus("a".into())),
            "wraps"
        );
        assert_eq!(
            press(&mut ix, "shift+tab"),
            Some(Dispatch::Focus("c".into()))
        );
        assert_eq!(press(&mut ix, "up"), Some(Dispatch::Focus("b".into())));
        assert_eq!(press(&mut ix, "left"), Some(Dispatch::Focus("a".into())));
        assert_eq!(press(&mut ix, "enter"), Some(Dispatch::Activate(Act::A)));
        assert_eq!(press(&mut ix, "esc"), Some(Dispatch::Back));
        assert_eq!(press(&mut ix, "ctrl+x"), None);
        // Focus survives a re-render.
        frame(&mut ix, false);
        assert_eq!(ix.focused(), Some(&ElementId::from("a")));
    }

    #[test]
    fn focus_is_confined_to_modal() {
        let mut ix = Interaction::new();
        frame(&mut ix, true);
        for _ in 0..3 {
            assert_eq!(press(&mut ix, "tab"), Some(Dispatch::Focus("close".into())));
        }
        assert_eq!(
            press(&mut ix, "enter"),
            Some(Dispatch::Activate(Act::Close))
        );
        // Once the modal is gone, focus that pointed into it is cleared.
        frame(&mut ix, false);
        assert_eq!(ix.focused(), None);
    }

    #[test]
    fn wheel_scrolls_pane_under_cursor_and_paging_scrolls_focused_pane() {
        let mut ix = Interaction::new();
        frame(&mut ix, false);
        assert_eq!(
            mouse(&mut ix, wheel_down(15, 5)),
            Some(Dispatch::Scroll {
                pane: "right".into(),
                lines: WHEEL_LINES
            })
        );
        assert_eq!(
            mouse(&mut ix, wheel_up(3, 5)),
            Some(Dispatch::Scroll {
                pane: "left".into(),
                lines: -WHEEL_LINES
            })
        );
        // Focusing b (in the right pane) makes PgDn scroll the right pane.
        press(&mut ix, "tab");
        press(&mut ix, "tab");
        assert_eq!(ix.focused(), Some(&ElementId::from("b")));
        assert_eq!(
            press(&mut ix, "pgdn"),
            Some(Dispatch::Scroll {
                pane: "right".into(),
                lines: 9
            })
        );
        assert_eq!(
            press(&mut ix, "pgup"),
            Some(Dispatch::Scroll {
                pane: "right".into(),
                lines: -9
            })
        );
    }

    #[test]
    fn scroll_offsets_clamp() {
        let mut s = Scrolls::default();
        let p = PaneId::from("p");
        s.scroll(&p, -5);
        assert_eq!(s.offset(&p), 0);
        s.scroll(&p, 50);
        assert_eq!(s.clamped(&p, 10), 10);
        s.scroll(&p, -3);
        assert_eq!(s.offset(&p), 7);
    }

    #[test]
    fn helpers_build_events_and_find_text() {
        assert_eq!(keys("ctrl+p enter").len(), 2);
        assert_eq!(type_text("hi").len(), 2);
        let mut buf = ratatui::buffer::Buffer::empty(rect(0, 0, 20, 2));
        buf.set_string(3, 1, "[ Close ]", ratatui::style::Style::new());
        assert_eq!(find_text(&buf, "Close"), Some((5, 1)));
        assert_eq!(find_text(&buf, "Open"), None);
        assert_eq!(buffer_text(&buf), "\n   [ Close ]");
    }
}
