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
//!
//! Titles and bodies wrap inside the toast (at most [`TOAST_LINES`] rows);
//! a toast that cannot show everything ends with a hint that the full text
//! is in the history ([`TOAST_MORE`]). History rows wrap too; Up/Down,
//! Home/End and clicks select (the selected item scrolls into view),
//! PageUp/PageDown and the wheel scroll by rows, and the bottom border
//! shows the position.

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

use crate::chat::text::{sanitize, width, wrap_line_with, wrap_right, wrap_text};
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
/// Scroll pane id of the history list (the wheel scrolls it).
pub const HISTORY_PANE: &str = "notify.history";
/// Most text rows a toast shows.
pub const TOAST_LINES: usize = 6;
/// The last row of a toast that is cut: where the rest is.
pub const TOAST_MORE: &str = "… more in Notifications (palette)";

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
    /// First history row shown.
    top: usize,
    /// Scroll the selected item into view on the next frame.
    follow: bool,
    /// History rows that fit the last frame (for PageUp/PageDown).
    page: usize,
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
        self.top = 0;
        self.follow = true;
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
            NotifyAction::Select(i) => {
                self.selected = i;
                self.follow = true;
            }
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
        self.follow = true;
        match key.code {
            KeyCode::Esc => self.close_history(),
            KeyCode::Up => self.selected = self.selected.saturating_sub(1),
            KeyCode::Down if n > 0 => self.selected = (self.selected + 1).min(n - 1),
            KeyCode::Home => self.selected = 0,
            KeyCode::End => self.selected = n.saturating_sub(1),
            KeyCode::PageUp => self.scroll(-(self.page.max(1) as i32)),
            KeyCode::PageDown => self.scroll(self.page.max(1) as i32),
            _ => return false,
        }
        true
    }

    /// First history row shown (after the last frame).
    pub fn history_top(&self) -> usize {
        self.top
    }

    /// Scrolls the history by `lines` rows (the wheel and
    /// PageUp/PageDown); the selection stays.
    pub fn scroll(&mut self, lines: i32) {
        self.follow = false;
        self.top = (self.top as i64 + i64::from(lines)).max(0) as usize;
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
        let iw = (w - 2) as usize;
        for n in self.toasts() {
            let (icon, token) = n.level.style();
            // Title rows leave room for the `×` at the right of the first.
            let title = Line::from(vec![
                Span::styled(format!("{icon} "), theme.fg(token)),
                Span::styled(
                    sanitize(&n.title),
                    theme.fg(Token::Fg).add_modifier(Modifier::BOLD),
                ),
            ]);
            let mut lines = wrap_line_with(title, iw.saturating_sub(3), iw, &[Span::raw("  ")]);
            if let Some(b) = &n.body {
                lines.extend(
                    wrap_text(b, iw)
                        .into_iter()
                        .map(|t| Line::from(Span::styled(t, theme.fg(Token::Muted)))),
                );
            }
            let room = (area.bottom().saturating_sub(y) as usize)
                .saturating_sub(2)
                .min(TOAST_LINES);
            if room == 0 {
                break;
            }
            if lines.len() > room {
                let more: Vec<Line<'static>> = wrap_text(TOAST_MORE, iw)
                    .into_iter()
                    .map(|t| Line::from(Span::styled(t, theme.fg(Token::Accent))))
                    .collect();
                lines.truncate(room.saturating_sub(more.len()).max(1));
                lines.extend(more);
                lines.truncate(room);
            }
            let h = lines.len() as u16 + 2;
            let rect = Rect::new(x, y, w, h);
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
        let iw = w.saturating_sub(2) as usize;
        let items: Vec<&Notification> = self.history.iter().rev().collect();
        // Each item wraps; the time sits at the right of its first row.
        let item_lines = |n: &Notification, sel: bool| -> Vec<Line<'static>> {
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
                text.push_str(&sanitize(b).replace('\n', " "));
            }
            let left = Line::from(vec![
                Span::styled(format!("{icon} "), base.patch(theme.fg(token))),
                Span::styled(text, base.patch(theme.fg(Token::Fg))),
            ]);
            let right = vec![Span::styled(
                format!(" {ts}"),
                base.patch(theme.fg(Token::Muted)),
            )];
            wrap_right(
                left,
                right,
                iw.max(1),
                Span::styled(" ", base),
                &[Span::raw("  ")],
            )
            .into_iter()
            .map(|l| l.style(base))
            .collect()
        };
        let heights: Vec<usize> = items.iter().map(|n| item_lines(n, false).len()).collect();
        let content = heights.iter().sum::<usize>().max(1) as u16;
        let max_h = area.height.saturating_sub(2).clamp(3.min(area.height), 20);
        let h = (content + 2).clamp(3.min(area.height), max_h);
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
        let close = Rect::new(rect.right().saturating_sub(10), rect.y, 9.min(w), 1);
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
        hits.register_pane(HISTORY_PANE, inner);
        self.selected = self.selected.min(items.len() - 1);
        // Scroll by row; after a selection change, bring the selected item
        // into view (its start when it is taller than the list).
        let sel = self.selected;
        let starts: Vec<usize> = heights
            .iter()
            .scan(0, |acc, h| {
                let s = *acc;
                *acc += h;
                Some(s)
            })
            .collect();
        let total: usize = heights.iter().sum();
        let max = total.saturating_sub(rows);
        if self.follow {
            let (start, end) = (starts[sel], starts[sel] + heights[sel]);
            if start < self.top {
                self.top = start;
            } else if end > self.top + rows {
                self.top = start.min(end.saturating_sub(rows));
            }
            self.follow = false;
        }
        self.top = self.top.min(max);
        let first = self.top;
        let mut all = Vec::new();
        for (row, n) in items.iter().enumerate() {
            let id = ElementId(format!("notify.history.{row}"));
            let selected = row == sel || hits.is_focused(&id);
            let (s0, h0) = (starts[row], heights[row]);
            let top = s0.max(first);
            let bottom = (s0 + h0).min(first + rows);
            if bottom > top {
                hits.register(
                    id,
                    Rect::new(
                        inner.x,
                        inner.y + (top - first) as u16,
                        inner.width,
                        (bottom - top) as u16,
                    ),
                    wrap(NotifyAction::Select(row)),
                );
                all.extend(
                    item_lines(n, selected)
                        .into_iter()
                        .skip(top - s0)
                        .take(bottom - top),
                );
            }
        }
        let lines = all;
        self.page = rows.saturating_sub(1).max(1);
        frame.render_widget(Paragraph::new(lines), inner);
        if max > 0 {
            let pos = format!(" {}-{}/{} ", first + 1, (first + rows).min(total), total);
            let pw = width(&pos) as u16;
            if rect.width > pw + 2 {
                frame.render_widget(
                    Paragraph::new(Span::styled(pos, theme.fg(Token::Muted))),
                    Rect::new(rect.right() - pw - 1, rect.bottom() - 1, pw, 1),
                );
            }
        }
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
