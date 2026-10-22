//! The inline "Updated memory" card shown for every memory write.
//!
//! A card is built from the run log's `memory` event
//! ([`MemoryCard::from_event`]): it lists the note and the bullets that were
//! saved or forgotten, says that the change applies to future sessions, and
//! offers an **Undo** button (focusable with Tab, Enter to activate,
//! clickable). Undo emits [`MemoryCardAction::Undo`] with the commit id;
//! the core reverts it with `harness_memory::dream::history::undo` and calls
//! [`MemoryCard::mark_undone`].

use harness_events::schema::{EventKind, MemoryAction, RunEvent};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Modifier;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Paragraph};
use time::OffsetDateTime;

use crate::chat::text::{sanitize, truncate, width};
use crate::interaction::{ElementId, HitMap};
use crate::theme::{Theme, Token};

/// Display width (the `width` parameter of [`MemoryCard::lines`] shadows
/// the text helper).
fn width_of(s: &str) -> usize {
    width(s)
}

/// Text shown under the bullets.
pub const FUTURE_SESSIONS_NOTE: &str =
    "Applies to future sessions; this session keeps its current memory.";
/// Label of the undo button.
pub const UNDO_LABEL: &str = "[ undo ]";

/// What a card asks the core to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MemoryCardAction {
    /// Revert the commit.
    Undo(String),
}

/// One memory change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryCard {
    /// Written, updated or deleted.
    pub action: MemoryAction,
    /// The commit id (or note path when the core has no commit).
    pub memory_id: String,
    /// The note, parsed from the summary.
    pub note: Option<String>,
    /// The bullets saved or forgotten.
    pub bullets: Vec<String>,
    /// When it happened (from the event).
    pub ts: OffsetDateTime,
    /// Set once the change was undone.
    pub undone: bool,
}

/// Splits a `MemoryChange::summary` (`saved in <note>: a | b`) into the
/// note and the bullets. Unknown shapes keep the whole text as one bullet.
pub fn parse_summary(summary: &str) -> (Option<String>, Vec<String>) {
    let rest = summary
        .strip_prefix("saved in ")
        .or_else(|| summary.strip_prefix("forgot in "));
    match rest.and_then(|r| r.split_once(": ")) {
        Some((note, bullets)) => (
            Some(note.trim().to_owned()),
            bullets
                .split(" | ")
                .map(str::trim)
                .filter(|b| !b.is_empty())
                .map(str::to_owned)
                .collect(),
        ),
        None => (None, vec![summary.to_owned()]),
    }
}

/// Element id of a card's undo button.
pub fn undo_button_id(memory_id: &str) -> String {
    format!("memory.undo.{memory_id}")
}

impl MemoryCard {
    /// The card for a `memory` event that changed the drive (recalls have
    /// no card).
    pub fn from_event(ev: &RunEvent) -> Option<MemoryCard> {
        let EventKind::Memory {
            action,
            memory_id,
            summary,
        } = &ev.kind
        else {
            return None;
        };
        if *action == MemoryAction::Recalled {
            return None;
        }
        let (note, bullets) = parse_summary(summary);
        Some(MemoryCard {
            action: *action,
            memory_id: memory_id.clone(),
            note,
            bullets,
            ts: ev.ts,
            undone: false,
        })
    }

    /// The commit to revert, when `memory_id` is a commit id.
    pub fn commit(&self) -> Option<&str> {
        let id = self.memory_id.as_str();
        (id.len() >= 7 && id.chars().all(|c| c.is_ascii_hexdigit())).then_some(id)
    }

    /// Records that the change was reverted (the button disappears).
    pub fn mark_undone(&mut self) {
        self.undone = true;
    }

    fn title(&self) -> &'static str {
        match self.action {
            MemoryAction::Deleted => "Forgot memory",
            _ => "Updated memory",
        }
    }

    /// Rows the card needs.
    pub fn height(&self) -> u16 {
        // Borders + bullets + note line + button/status line.
        (2 + self.bullets.len() + 2) as u16
    }

    /// The card as transcript lines `width` columns wide, and the index of
    /// the line holding the undo button (when there is one).
    pub fn lines(
        &self,
        theme: &Theme,
        width: usize,
        focused: bool,
    ) -> (Vec<Line<'static>>, Option<usize>) {
        let width = width.max(12);
        let iw = width - 4;
        let border = theme.fg(Token::Agent);
        let note = self.note.as_deref().unwrap_or("memory");
        let title = format!(" {} ", self.title());
        let note = truncate(&sanitize(note), iw.saturating_sub(width_of(&title) + 2));
        let used = 2 + width_of(&title) + width_of(&note) + 1;
        let mut out = vec![Line::from(vec![
            Span::styled("╭─", border),
            Span::styled(title, border.add_modifier(Modifier::BOLD)),
            Span::styled(format!("{note} "), theme.fg(Token::Path)),
            Span::styled(
                format!("{}╮", "─".repeat(width.saturating_sub(used + 1))),
                border,
            ),
        ])];
        let row = |spans: Vec<Span<'static>>| {
            let w: usize = spans.iter().map(|s| width_of(&s.content)).sum();
            let mut v = vec![Span::styled("│ ", border)];
            v.extend(spans);
            v.push(Span::raw(" ".repeat(iw.saturating_sub(w))));
            v.push(Span::styled(" │", border));
            Line::from(v)
        };
        let (sign, token) = if self.action == MemoryAction::Deleted {
            ("−", Token::DiffRemoved)
        } else {
            ("+", Token::DiffAdded)
        };
        for b in &self.bullets {
            out.push(row(vec![
                Span::styled(format!("{sign} "), theme.fg(token)),
                Span::styled(
                    truncate(&sanitize(b), iw.saturating_sub(2)),
                    theme.fg(Token::Fg),
                ),
            ]));
        }
        out.push(row(vec![Span::styled(
            truncate(FUTURE_SESSIONS_NOTE, iw),
            theme.fg(Token::Muted),
        )]));
        let mut button = None;
        if self.undone {
            out.push(row(vec![Span::styled("undone", theme.fg(Token::Muted))]));
        } else if self.commit().is_some() {
            let style = if focused {
                theme.focused()
            } else {
                theme.fg(Token::Link)
            };
            button = Some(out.len());
            out.push(row(vec![Span::styled(UNDO_LABEL, style)]));
        }
        out.push(Line::from(Span::styled(
            format!("╰{}╯", "─".repeat(width.saturating_sub(2))),
            border,
        )));
        (out, button)
    }

    /// Draws the card in `area` and registers the undo button.
    pub fn render<A: Clone>(
        &self,
        frame: &mut Frame<'_>,
        area: Rect,
        theme: &Theme,
        hits: &mut HitMap<A>,
        wrap: &dyn Fn(MemoryCardAction) -> A,
    ) {
        let note = self.note.as_deref().unwrap_or("memory");
        let block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(theme.fg(Token::Agent))
            .title(Line::from(vec![
                Span::styled(
                    format!(" {} ", self.title()),
                    theme.fg(Token::Agent).add_modifier(Modifier::BOLD),
                ),
                Span::styled(format!("{} ", sanitize(note)), theme.fg(Token::Path)),
            ]));
        let inner = block.inner(area);
        frame.render_widget(block, area);
        let iw = inner.width as usize;
        let sign = if self.action == MemoryAction::Deleted {
            "−"
        } else {
            "+"
        };
        let token = if self.action == MemoryAction::Deleted {
            Token::DiffRemoved
        } else {
            Token::DiffAdded
        };
        let mut lines: Vec<Line<'static>> = self
            .bullets
            .iter()
            .map(|b| {
                Line::from(vec![
                    Span::styled(format!("{sign} "), theme.fg(token)),
                    Span::styled(
                        truncate(&sanitize(b), iw.saturating_sub(2)),
                        theme.fg(Token::Fg),
                    ),
                ])
            })
            .collect();
        lines.push(Line::from(Span::styled(
            truncate(FUTURE_SESSIONS_NOTE, iw),
            theme.fg(Token::Muted),
        )));
        let status_row = inner.y + lines.len() as u16;
        frame.render_widget(Paragraph::new(lines), inner);
        if status_row >= inner.bottom() {
            return;
        }
        if self.undone {
            frame.render_widget(
                Paragraph::new(Span::styled("undone", theme.fg(Token::Muted))),
                Rect::new(inner.x, status_row, inner.width, 1),
            );
            return;
        }
        if let Some(commit) = self.commit() {
            let id = ElementId(undo_button_id(&self.memory_id));
            let style = if hits.is_focused(&id) {
                theme.focused()
            } else {
                theme.fg(Token::Link)
            };
            let w = (width(UNDO_LABEL) as u16).min(inner.width);
            let rect = Rect::new(inner.x, status_row, w, 1);
            frame.render_widget(Paragraph::new(Span::styled(UNDO_LABEL, style)), rect);
            hits.register(id, rect, wrap(MemoryCardAction::Undo(commit.to_owned())));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interaction::testing::{buffer_text, find_text};
    use crate::interaction::{Dispatch, Interaction};
    use crossterm::event::{
        KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
    };
    use harness_events::schema::RunId;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use time::macros::datetime;

    const SHA: &str = "0123456789abcdef0123456789abcdef01234567";

    fn ev(action: MemoryAction, summary: &str) -> RunEvent {
        RunEvent {
            seq: 0,
            ts: datetime!(2026-10-22 12:00 UTC),
            run_id: RunId(uuid::Uuid::nil()),
            node_id: None,
            parent_node_id: None,
            kind: EventKind::Memory {
                action,
                memory_id: SHA.into(),
                summary: summary.into(),
            },
        }
    }

    fn draw(card: &MemoryCard, it: &mut Interaction<MemoryCardAction>) -> Terminal<TestBackend> {
        let mut term = Terminal::new(TestBackend::new(70, 8)).unwrap();
        let theme = Theme::default();
        let mut hits = it.begin_frame();
        term.draw(|f| {
            let area = Rect::new(0, 0, 70, card.height());
            card.render(f, area, &theme, &mut hits, &|a| a)
        })
        .unwrap();
        it.end_frame(hits);
        term
    }

    #[test]
    fn cards_list_the_changes_and_say_future_sessions() {
        let card = MemoryCard::from_event(&ev(
            MemoryAction::Written,
            "saved in tools/cargo.md: Use nextest | Run with -j8",
        ))
        .unwrap();
        assert_eq!(card.note.as_deref(), Some("tools/cargo.md"));
        assert_eq!(card.bullets, ["Use nextest", "Run with -j8"]);
        let mut it = Interaction::new();
        let term = draw(&card, &mut it);
        let text = buffer_text(term.backend().buffer());
        assert!(text.contains("Updated memory"), "{text}");
        assert!(text.contains("tools/cargo.md"));
        assert!(text.contains("+ Use nextest"));
        assert!(text.contains("+ Run with -j8"));
        assert!(text.contains("Applies to future sessions"));
        assert!(text.contains(UNDO_LABEL));

        let forgot =
            MemoryCard::from_event(&ev(MemoryAction::Deleted, "forgot in MEMORY.md: tabs"))
                .unwrap();
        let text = buffer_text(draw(&forgot, &mut Interaction::new()).backend().buffer());
        assert!(text.contains("Forgot memory"));
        assert!(text.contains("− tabs"));
        let recalled = ev(MemoryAction::Recalled, "x");
        assert!(MemoryCard::from_event(&recalled).is_none());
    }

    #[test]
    fn undo_by_mouse_and_keyboard() {
        let mut card =
            MemoryCard::from_event(&ev(MemoryAction::Updated, "saved in MEMORY.md: tabs")).unwrap();
        // Mouse.
        let mut it = Interaction::new();
        let term = draw(&card, &mut it);
        let (x, y) = find_text(term.backend().buffer(), UNDO_LABEL).unwrap();
        let click = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: x + 1,
            row: y,
            modifiers: KeyModifiers::NONE,
        };
        assert_eq!(
            it.handle_mouse(&click),
            Some(Dispatch::Activate(MemoryCardAction::Undo(SHA.into())))
        );
        // Keyboard: Tab focuses the button, Enter activates it.
        let mut it = Interaction::new();
        draw(&card, &mut it);
        it.handle_key(&KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        draw(&card, &mut it);
        assert_eq!(
            it.handle_key(&KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            Some(Dispatch::Activate(MemoryCardAction::Undo(SHA.into())))
        );
        assert_eq!(
            it.handle_key(&KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
            Some(Dispatch::Back)
        );
        // After undo the button is gone.
        card.mark_undone();
        let mut it = Interaction::new();
        let term = draw(&card, &mut it);
        let text = buffer_text(term.backend().buffer());
        assert!(!text.contains(UNDO_LABEL));
        assert!(text.contains("undone"));
    }

    #[test]
    fn summaries_without_a_note_are_kept_whole() {
        assert_eq!(
            parse_summary("prefers tabs"),
            (None, vec!["prefers tabs".into()])
        );
        let mut c = MemoryCard::from_event(&ev(MemoryAction::Written, "x")).unwrap();
        c.memory_id = "tools/cargo.md".into();
        assert_eq!(c.commit(), None, "no undo without a commit id");
    }
}
