//! Toast notifications and the notification history.
//!
//! Background events (memory updates, recoverable errors, remote
//! disconnects, budget warnings, finished runs) appear as toasts in the top
//! right corner. Toasts never take focus: they are drawn on top of the chat
//! and registered as clickable regions (a click dismisses one), but focus
//! stays where it was. They expire after [`TOAST_TTL`]; at most
//! [`MAX_TOASTS`] are shown. Every notification is kept in a history (up to
//! [`HISTORY_LIMIT`]) that the palette entry `Notifications` opens as an
//! overlay; Up/Down move, Esc or the close button closes it.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind};
use harness_events::schema::{EventKind, MemoryAction, RunEvent, RunStatus};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Modifier;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};
use time::OffsetDateTime;

use crate::chat::text::{sanitize, truncate, width};
use crate::interaction::{ElementId, HitMap};
use crate::theme::{Theme, Token};

/// How long a toast stays up.
pub const TOAST_TTL: Duration = Duration::from_secs(6);
/// Toasts shown at once.
pub const MAX_TOASTS: usize = 3;
/// Notifications kept in the history.
pub const HISTORY_LIMIT: usize = 500;
/// Width of a toast in cells.
pub const TOAST_WIDTH: u16 = 44;
/// Element id of the history close button.
pub const HISTORY_CLOSE_ID: &str = "notify.history.close";

/// Severity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    /// Informational.
    Info,
    /// Something finished well.
    Success,
    /// Needs attention soon.
    Warning,
    /// Something failed.
    Error,
}

impl Level {
    fn style(self) -> (char, Token) {
        match self {
            Level::Info => ('◆', Token::Accent),
            Level::Success => ('✔', Token::Ok),
            Level::Warning => ('⚠', Token::Warn),
            Level::Error => ('✗', Token::Error),
        }
    }
}

/// One notification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notification {
    /// Id.
    pub id: u64,
    /// Severity.
    pub level: Level,
    /// Title.
    pub title: String,
    /// Optional second line.
    pub body: Option<String>,
    /// When it happened (event time).
    pub ts: OffsetDateTime,
}

/// Clickable parts of toasts and the history.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotifyAction {
    /// Dismiss toast `id`.
    Dismiss(u64),
    /// Select history row `n`.
    Select(usize),
    /// Close the history.
    CloseHistory,
}

/// Toasts and history.
#[derive(Debug, Clone, Default)]
pub struct Notifications {
    history: VecDeque<Notification>,
    toasts: Vec<(u64, Instant)>,
    next_id: u64,
    history_open: bool,
    selected: usize,
}

impl Notifications {
    /// No notifications.
    pub fn new() -> Notifications {
        Notifications::default()
    }

    /// Adds a notification and shows it as a toast until `now + TOAST_TTL`.
    pub fn push(
        &mut self,
        level: Level,
        title: impl Into<String>,
        body: Option<String>,
        ts: OffsetDateTime,
        now: Instant,
    ) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        self.history.push_back(Notification {
            id,
            level,
            title: title.into(),
            body,
            ts,
        });
        while self.history.len() > HISTORY_LIMIT {
            self.history.pop_front();
        }
        self.toasts.push((id, now + TOAST_TTL));
        id
    }

    /// Adds the notification for a background run event, if it has one.
    pub fn push_event(&mut self, ev: &RunEvent, now: Instant) -> Option<u64> {
        let (level, title, body) = event_notification(ev)?;
        Some(self.push(level, title, body, ev.ts, now))
    }

    /// Removes expired toasts. Returns true if any were removed.
    pub fn expire(&mut self, now: Instant) -> bool {
        let before = self.toasts.len();
        self.toasts.retain(|(_, until)| *until > now);
        before != self.toasts.len()
    }

    /// When the next toast expires (so the caller can wake up then).
    pub fn next_expiry(&self) -> Option<Instant> {
        self.toasts.iter().map(|(_, t)| *t).min()
    }

    /// Dismisses toast `id`.
    pub fn dismiss(&mut self, id: u64) {
        self.toasts.retain(|(t, _)| *t != id);
    }

    /// Visible toasts, newest first.
    pub fn toasts(&self) -> Vec<&Notification> {
        self.toasts
            .iter()
            .rev()
            .take(MAX_TOASTS)
            .filter_map(|(id, _)| self.history.iter().find(|n| n.id == *id))
            .collect()
    }

    /// All notifications, oldest first.
    pub fn history(&self) -> impl Iterator<Item = &Notification> {
        self.history.iter()
    }

    /// Opens the history overlay (newest selected).
    pub fn open_history(&mut self) {
        self.history_open = true;
        self.selected = 0;
    }

    /// Closes the history overlay.
    pub fn close_history(&mut self) {
        self.history_open = false;
    }

    /// Whether the history overlay is open.
    pub fn is_history_open(&self) -> bool {
        self.history_open
    }

    /// Runs a clicked action. Returns true when something changed.
    pub fn activate(&mut self, action: NotifyAction) -> bool {
        match action {
            NotifyAction::Dismiss(id) => self.dismiss(id),
            NotifyAction::Select(i) => self.selected = i,
            NotifyAction::CloseHistory => self.close_history(),
        }
        true
    }

    /// Keys for the open history: Up/Down select, Esc closes.
    pub fn handle_key(&mut self, key: &KeyEvent) -> bool {
        if !self.history_open || key.kind == KeyEventKind::Release {
            return false;
        }
        let n = self.history.len();
        match key.code {
            KeyCode::Esc => self.close_history(),
            KeyCode::Up => self.selected = self.selected.saturating_sub(1),
            KeyCode::Down if n > 0 => self.selected = (self.selected + 1).min(n - 1),
            KeyCode::Home => self.selected = 0,
            KeyCode::End => self.selected = n.saturating_sub(1),
            _ => return false,
        }
        true
    }

    /// Draws toasts in the top-right corner of `area`. They are clickable
    /// (dismiss) but never move focus.
    pub fn render_toasts<A: Clone>(
        &self,
        frame: &mut Frame<'_>,
        area: Rect,
        theme: &Theme,
        hits: &mut HitMap<A>,
        wrap: &dyn Fn(NotifyAction) -> A,
    ) {
        let w = TOAST_WIDTH.min(area.width);
        if w < 10 {
            return;
        }
        let x = area.right() - w;
        let mut y = area.y;
        for n in self.toasts() {
            let h = if n.body.is_some() { 4 } else { 3 };
            if y + h > area.bottom() {
                break;
            }
            let rect = Rect::new(x, y, w, h);
            let (icon, token) = n.level.style();
            // A selection cannot start on a toast, but the text under it
            // is still copied: toasts come and go (every copy shows one).
            crate::selection::overlay(rect, frame.buffer_mut());
            frame.render_widget(Clear, rect);
            let block = Block::new()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(theme.fg(token))
                .style(theme.bg(Token::Surface));
            let inner = block.inner(rect);
            frame.render_widget(block, rect);
            let iw = inner.width as usize;
            let mut lines = vec![Line::from(vec![
                Span::styled(format!("{icon} "), theme.fg(token)),
                Span::styled(
                    truncate(&sanitize(&n.title), iw.saturating_sub(4)),
                    theme.fg(Token::Fg).add_modifier(Modifier::BOLD),
                ),
            ])];
            if let Some(b) = &n.body {
                lines.push(Line::from(Span::styled(
                    truncate(&sanitize(b), iw),
                    theme.fg(Token::Muted),
                )));
            }
            frame.render_widget(Paragraph::new(lines), inner);
            frame.render_widget(
                Paragraph::new(Span::styled("×", theme.fg(Token::Muted))),
                Rect::new(rect.right() - 3, rect.y + 1, 1, 1),
            );
            hits.register(
                format!("toast.{}", n.id),
                rect,
                wrap(NotifyAction::Dismiss(n.id)),
            );
            y += h;
        }
    }

    /// Draws the history overlay as a modal over `area`. `stamp` formats
    /// notification times (see [`crate::chat::timefmt::TimeFormat`]).
    pub fn render_history<A: Clone>(
        &mut self,
        frame: &mut Frame<'_>,
        area: Rect,
        theme: &Theme,
        stamp: &dyn Fn(OffsetDateTime) -> Option<String>,
        hits: &mut HitMap<A>,
        wrap: &dyn Fn(NotifyAction) -> A,
    ) {
        if !self.history_open {
            return;
        }
        let w = area.width.saturating_sub(4).clamp(20.min(area.width), 90);
        let h = area.height.saturating_sub(2).clamp(3.min(area.height), 20);
        let rect = Rect::new(
            area.x + (area.width - w) / 2,
            area.y + (area.height - h) / 4,
            w,
            h,
        );
        hits.push_modal(rect);
        frame.render_widget(Clear, rect);
        let block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(theme.border(true))
            .title(Span::styled(" Notifications ", theme.fg(Token::Accent)))
            .style(theme.bg(Token::Surface).patch(theme.fg(Token::Fg)));
        let inner = block.inner(rect);
        frame.render_widget(block, rect);
        // Close button in the top border.
        let close = Rect::new(rect.right().saturating_sub(10), rect.y, 9, 1);
        let close_style = if hits.is_focused(&ElementId::from(HISTORY_CLOSE_ID)) {
            theme.focused()
        } else {
            theme.fg(Token::Muted)
        };
        frame.render_widget(
            Paragraph::new(Span::styled("[ close ]", close_style)),
            close,
        );
        hits.register(HISTORY_CLOSE_ID, close, wrap(NotifyAction::CloseHistory));
        let rows = inner.height as usize;
        let items: Vec<&Notification> = self.history.iter().rev().collect();
        if items.is_empty() {
            frame.render_widget(
                Paragraph::new(Span::styled(
                    "No notifications yet.",
                    theme.fg(Token::Muted),
                )),
                inner,
            );
            return;
        }
        self.selected = self.selected.min(items.len() - 1);
        let first = self.selected.saturating_sub(rows.saturating_sub(1));
        let iw = inner.width as usize;
        let mut lines = Vec::new();
        for (row, n) in items.iter().enumerate().skip(first).take(rows) {
            let id = ElementId(format!("notify.history.{row}"));
            let sel = row == self.selected || hits.is_focused(&id);
            let base = if sel {
                theme.focused()
            } else {
                theme.bg(Token::Surface)
            };
            let (icon, token) = n.level.style();
            let ts = stamp(n.ts).unwrap_or_default();
            let mut text = sanitize(&n.title);
            if let Some(b) = &n.body {
                text.push_str(" — ");
                text.push_str(&sanitize(b));
            }
            let tw = iw.saturating_sub(3 + width(&ts));
            lines.push(Line::from(vec![
                Span::styled(format!("{icon} "), base.patch(theme.fg(token))),
                Span::styled(
                    format!("{:<tw$}", truncate(&text, tw)),
                    base.patch(theme.fg(Token::Fg)),
                ),
                Span::styled(format!(" {ts}"), base.patch(theme.fg(Token::Muted))),
            ]));
            hits.register(
                id,
                Rect::new(inner.x, inner.y + (row - first) as u16, inner.width, 1),
                wrap(NotifyAction::Select(row)),
            );
        }
        frame.render_widget(Paragraph::new(lines), inner);
    }
}

/// The notification for a background run event: memory changes,
/// recoverable errors, finished runs.
pub fn event_notification(ev: &RunEvent) -> Option<(Level, String, Option<String>)> {
    match &ev.kind {
        EventKind::Memory {
            action: action @ (MemoryAction::Written | MemoryAction::Updated | MemoryAction::Deleted),
            summary,
            ..
        } => {
            let verb = match action {
                MemoryAction::Written => "Memory saved",
                MemoryAction::Updated => "Memory updated",
                _ => "Memory deleted",
            };
            Some((Level::Info, verb.to_owned(), Some(summary.clone())))
        }
        EventKind::Error {
            message,
            recoverable: true,
            ..
        } => Some((
            Level::Warning,
            message.lines().next().unwrap_or("").to_owned(),
            None,
        )),
        EventKind::RunFinished { status, error } => match status {
            RunStatus::Succeeded => Some((Level::Success, "Run finished".into(), None)),
            RunStatus::Failed => Some((Level::Error, "Run failed".into(), error.clone())),
            RunStatus::Cancelled => Some((Level::Warning, "Run cancelled".into(), None)),
            RunStatus::Running => None,
        },
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chat::timefmt::TimeFormat;
    use crate::interaction::testing::{buffer_text, find_text};
    use crate::interaction::{Dispatch, Interaction};
    use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
    use harness_events::schema::RunId;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use time::macros::datetime;

    const TS: OffsetDateTime = datetime!(2026-10-22 14:32:00 UTC);

    #[test]
    fn toasts_expire_and_history_keeps_everything() {
        let t0 = Instant::now();
        let mut n = Notifications::new();
        n.push(
            Level::Info,
            "Memory updated",
            Some("prefers tabs".into()),
            TS,
            t0,
        );
        n.push(
            Level::Warning,
            "Remote gpu-box disconnected",
            None,
            TS,
            t0 + Duration::from_secs(2),
        );
        assert_eq!(n.toasts().len(), 2);
        assert_eq!(
            n.toasts()[0].title,
            "Remote gpu-box disconnected",
            "newest first"
        );
        assert_eq!(n.next_expiry(), Some(t0 + TOAST_TTL));
        assert!(!n.expire(t0 + Duration::from_secs(1)));
        assert!(n.expire(t0 + TOAST_TTL));
        assert_eq!(n.toasts().len(), 1);
        assert!(n.expire(t0 + TOAST_TTL + Duration::from_secs(2)));
        assert!(n.toasts().is_empty());
        assert_eq!(n.history().count(), 2);
        for i in 0..10 {
            n.push(Level::Info, format!("n{i}"), None, TS, t0);
        }
        assert_eq!(n.toasts().len(), MAX_TOASTS);
    }

    #[test]
    fn toasts_do_not_steal_focus_and_dismiss_on_click() {
        let theme = Theme::default();
        let t0 = Instant::now();
        let mut n = Notifications::new();
        n.push(
            Level::Warning,
            "Budget 80% used",
            Some("$4.00 of $5.00".into()),
            TS,
            t0,
        );
        let mut it: Interaction<NotifyAction> = Interaction::new();
        it.set_focus("input");
        let mut term = Terminal::new(TestBackend::new(80, 10)).unwrap();
        let mut hits = it.begin_frame();
        hits.register("input", Rect::new(0, 9, 80, 1), NotifyAction::Select(0));
        term.draw(|f| n.render_toasts(f, f.area(), &theme, &mut hits, &|a| a))
            .unwrap();
        it.end_frame(hits);
        assert_eq!(
            it.focused(),
            Some(&ElementId::from("input")),
            "focus unchanged"
        );
        let text = buffer_text(term.backend().buffer());
        assert!(text.contains("⚠ Budget 80% used"));
        assert!(text.contains("$4.00 of $5.00"));
        let (x, y) = find_text(term.backend().buffer(), "Budget").unwrap();
        assert!(x >= 80 - TOAST_WIDTH, "top right");
        let click = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: x,
            row: y,
            modifiers: KeyModifiers::NONE,
        };
        let Some(Dispatch::Activate(a)) = it.handle_mouse(&click) else {
            panic!()
        };
        n.activate(a);
        assert!(n.toasts().is_empty());
        assert_eq!(n.history().count(), 1);
    }

    #[test]
    fn history_overlay_keyboard_and_mouse() {
        let theme = Theme::default();
        let t0 = Instant::now();
        let mut n = Notifications::new();
        n.push(Level::Info, "first", None, TS, t0);
        n.push(Level::Error, "second", Some("details".into()), TS, t0);
        n.open_history();
        let mut it: Interaction<NotifyAction> = Interaction::new();
        let mut term = Terminal::new(TestBackend::new(80, 20)).unwrap();
        let mut hits = it.begin_frame();
        term.draw(|f| {
            n.render_history(
                f,
                f.area(),
                &theme,
                &|ts| TimeFormat::default().format(ts, TS),
                &mut hits,
                &|a| a,
            )
        })
        .unwrap();
        it.end_frame(hits);
        let text = buffer_text(term.backend().buffer());
        assert!(text.contains("Notifications"));
        assert!(text.contains("✗ second — details"));
        assert!(text.contains("◆ first"));
        assert!(text.contains("14:32:00"));
        assert!(n.handle_key(&KeyEvent::new(KeyCode::Down, KeyModifiers::NONE)));
        assert!(n.handle_key(&KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)));
        assert!(!n.is_history_open());
        // Mouse: the close button.
        n.open_history();
        let mut hits = it.begin_frame();
        term.draw(|f| {
            n.render_history(
                f,
                f.area(),
                &theme,
                &|ts| TimeFormat::default().format(ts, TS),
                &mut hits,
                &|a| a,
            )
        })
        .unwrap();
        it.end_frame(hits);
        let (x, y) = find_text(term.backend().buffer(), "[ close ]").unwrap();
        let click = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: x + 2,
            row: y,
            modifiers: KeyModifiers::NONE,
        };
        let Some(Dispatch::Activate(a)) = it.handle_mouse(&click) else {
            panic!()
        };
        n.activate(a);
        assert!(!n.is_history_open());
    }

    #[test]
    fn background_events_become_notifications() {
        let ev = |kind| RunEvent {
            seq: 0,
            ts: TS,
            run_id: RunId(uuid::Uuid::nil()),
            node_id: None,
            parent_node_id: None,
            kind,
        };
        let mut n = Notifications::new();
        let t0 = Instant::now();
        assert!(
            n.push_event(
                &ev(EventKind::Memory {
                    action: MemoryAction::Updated,
                    memory_id: "m".into(),
                    summary: "prefers tabs".into()
                }),
                t0
            )
            .is_some()
        );
        assert!(
            n.push_event(
                &ev(EventKind::Memory {
                    action: MemoryAction::Recalled,
                    memory_id: "m".into(),
                    summary: "x".into()
                }),
                t0
            )
            .is_none()
        );
        assert!(
            n.push_event(
                &ev(EventKind::Error {
                    message: "remote gpu-box disconnected".into(),
                    recoverable: true,
                    action: None,
                }),
                t0
            )
            .is_some()
        );
        assert!(
            n.push_event(&ev(EventKind::Cost { usd: 1.0 }), t0)
                .is_none()
        );
        let titles: Vec<&str> = n.history().map(|x| x.title.as_str()).collect();
        assert_eq!(
            titles,
            vec!["Memory updated", "remote gpu-box disconnected"]
        );
    }
}
