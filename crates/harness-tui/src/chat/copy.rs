//! Copy actions of the chat: palette entries and the selection-mode bar.
//!
//! Every copy target is reachable three ways: its `⧉ copy` button (click,
//! or Tab to it and Enter), the `copy` key (`ctrl+y`: copies the focused
//! element, else the last assistant message) and these palette entries.
//! The `select` key (`ctrl+t`) or "Select text" starts keyboard selection:
//!
//! ```text
//! SELECT  [copy] [mark] [done]  3 lines  ↑↓ move · space mark · y copy · esc done
//! ```
//!
//! On a narrow screen the bar wraps onto more rows ([`bar_height`]), so
//! every button and the key hint stay on screen.
//!
//! Up/Down (or `k`/`j`), PgUp/PgDn and Home/End (`g`/`G`) move the cursor,
//! Space or `v` sets the mark, `y` or Enter copies the range and leaves,
//! Esc leaves. A click on a transcript line moves the cursor there.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::Span;
use ratatui::widgets::Paragraph;

use super::text::width;
use crate::interaction::{ElementId, HitMap};
use crate::keys::{Action, Keymap};
use crate::palette::{EntryKind, PaletteEntry};
use crate::theme::{Theme, Token};

/// Palette entry ids of the copy actions.
pub mod ids {
    /// Copy the last assistant message.
    pub const MESSAGE: &str = "copy.message";
    /// Copy the last code block.
    pub const CODE: &str = "copy.code";
    /// Copy the focused (else last) tool output.
    pub const OUTPUT: &str = "copy.output";
    /// Copy the focused (else last) tool output with line numbers.
    pub const OUTPUT_NUMBERED: &str = "copy.output_numbered";
    /// Copy the focused (else last) diff.
    pub const DIFF: &str = "copy.diff";
    /// Copy the focused element.
    pub const FOCUSED: &str = "copy.focused";
    /// Start keyboard selection.
    pub const SELECT: &str = "copy.select";
}

/// Element id of the selection bar's copy button.
pub const SELECT_COPY_ID: &str = "select.copy";
/// Element id of the selection bar's mark button.
pub const SELECT_MARK_ID: &str = "select.mark";
/// Element id of the selection bar's done button.
pub const SELECT_DONE_ID: &str = "select.done";

/// The copy entries of the palette.
pub fn palette_entries(keymap: &Keymap) -> Vec<PaletteEntry> {
    let key = |a: Action| keymap.describe(a);
    let e =
        |id: &str, title: &str, desc: &str| PaletteEntry::new(id, title, desc, EntryKind::Builtin);
    vec![
        e(
            ids::FOCUSED,
            "Copy focused",
            "Copy the focused message, code block, output or file change",
        )
        .with_key(key(Action::Copy)),
        e(
            ids::MESSAGE,
            "Copy last message",
            "Copy the latest assistant message",
        ),
        e(
            ids::CODE,
            "Copy last code block",
            "Copy the latest code block",
        ),
        e(
            ids::OUTPUT,
            "Copy tool output",
            "Copy the focused or latest tool call output",
        ),
        e(
            ids::OUTPUT_NUMBERED,
            "Copy tool output with line numbers",
            "Like Copy tool output, keeping read line numbers",
        ),
        e(
            ids::DIFF,
            "Copy diff",
            "Copy the focused or latest file diff",
        ),
        e(
            ids::SELECT,
            "Select text",
            "Select text with the keyboard and copy it: transcript lines in the chat, a cursor on other screens",
        )
        .with_key(key(Action::Select)),
    ]
}

/// Buttons of the selection bar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectAction {
    /// Copy the range and leave.
    Copy,
    /// Set or clear the mark.
    Mark,
    /// Leave selection mode.
    Done,
}

/// The key hint of the selection bar.
pub const BAR_HINT: &str = "↑↓ move · space mark · y copy · esc done";

/// A piece of the selection bar: text, style and the button it is.
type BarItem = (
    String,
    ratatui::style::Style,
    Option<(&'static str, SelectAction)>,
);

/// The bar's pieces: (text, style, button), laid out left to right.
fn bar_items(theme: &Theme, lines: usize, marked: bool) -> Vec<BarItem> {
    let muted = theme.fg(Token::Muted);
    vec![
        (
            " SELECT ".to_owned(),
            theme.fg_on(Token::Bg, Token::Accent),
            None,
        ),
        (
            "[copy]".to_owned(),
            theme.fg(Token::Accent),
            Some((SELECT_COPY_ID, SelectAction::Copy)),
        ),
        (
            if marked { "[unmark]" } else { "[mark]" }.to_owned(),
            theme.fg(Token::Accent),
            Some((SELECT_MARK_ID, SelectAction::Mark)),
        ),
        (
            "[done]".to_owned(),
            theme.fg(Token::Accent),
            Some((SELECT_DONE_ID, SelectAction::Done)),
        ),
        (
            format!("{lines} line{}", if lines == 1 { "" } else { "s" }),
            theme.fg(Token::Number),
            None,
        ),
        (BAR_HINT.to_owned(), muted, None),
    ]
}

/// Places the pieces on rows of `width`: `(row, x, text)` per piece; a
/// piece wider than a row wraps on rows of its own.
fn bar_layout(texts: &[String], width: usize) -> Vec<Vec<(usize, usize, String)>> {
    let width = width.max(1);
    let mut out = Vec::new();
    let (mut row, mut used) = (0usize, 0usize);
    for t in texts {
        let w = super::text::width(t);
        if w > width {
            if used > 0 {
                row += 1;
            }
            let parts: Vec<(usize, usize, String)> = super::text::wrap_text(t, width)
                .into_iter()
                .enumerate()
                .map(|(i, p)| (row + i, 0, p))
                .collect();
            row += parts.len();
            used = 0;
            out.push(parts);
            continue;
        }
        if used > 0 && used + 2 + w > width {
            row += 1;
            used = 0;
        }
        let x = if used > 0 { used + 2 } else { 0 };
        out.push(vec![(row, x, t.clone())]);
        used = x + w;
    }
    out
}

/// Rows the selection bar needs at `width` (enough for any line count up
/// to a million and the `[unmark]` label).
pub fn bar_height(width: u16) -> u16 {
    let theme = Theme::default();
    let texts: Vec<String> = bar_items(&theme, 999_999, true)
        .into_iter()
        .map(|(t, _, _)| t)
        .collect();
    bar_layout(&texts, width as usize)
        .iter()
        .flatten()
        .map(|(r, _, _)| r + 1)
        .max()
        .unwrap_or(1) as u16
}

/// Draws the selection bar into `area` ([`bar_height`] rows) and registers its
/// buttons. `lines` is the number of selected lines, `marked` whether a
/// mark is set.
pub fn render_bar<A: Clone>(
    frame: &mut Frame<'_>,
    area: Rect,
    theme: &Theme,
    lines: usize,
    marked: bool,
    hits: &mut HitMap<A>,
    wrap: &dyn Fn(SelectAction) -> A,
) {
    frame.render_widget(Paragraph::new("").style(theme.base()), area);
    let items = bar_items(theme, lines, marked);
    let texts: Vec<String> = items.iter().map(|(t, _, _)| t.clone()).collect();
    let layout = bar_layout(&texts, area.width as usize);
    for ((_, style, button), parts) in items.into_iter().zip(layout) {
        let mut style = style;
        if let Some((id, _)) = button
            && hits.is_focused(&ElementId::from(id))
        {
            style = style.patch(theme.focused());
        }
        let mut rect: Option<Rect> = None;
        for (row, x, text) in parts {
            let (x, y) = (area.x + x as u16, area.y + row as u16);
            if y >= area.bottom() || x >= area.right() {
                continue;
            }
            let w = (width(&text) as u16).min(area.right() - x);
            let r = Rect::new(x, y, w, 1);
            frame.render_widget(Paragraph::new(Span::styled(text, style)), r);
            rect = Some(rect.map_or(r, |a| a.union(r)));
        }
        if let (Some((id, action)), Some(r)) = (button, rect) {
            hits.register_button(ElementId::from(id), r, wrap(action));
        }
    }
}
