//! Copy actions of the chat: palette entries and the selection-mode bar.
//!
//! Every copy target is reachable three ways: its `⧉ copy` button (click,
//! or Tab to it and Enter), the `copy` key (`ctrl+y`: copies the focused
//! element, else the last assistant message) and these palette entries.
//! The `select` key (`ctrl+t`) or "Select text" starts keyboard selection:
//!
//! ```text
//! SELECT  ↑↓ move · space mark · y copy · esc done   3 lines   [copy] [done]
//! ```
//!
//! Up/Down (or `k`/`j`), PgUp/PgDn and Home/End (`g`/`G`) move the cursor,
//! Space or `v` sets the mark, `y` or Enter copies the range and leaves,
//! Esc leaves. A click on a transcript line moves the cursor there.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
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
            "Select lines of the transcript with the keyboard and copy them",
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

/// Draws the selection bar into `area` (one row) and registers its
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
    let muted = theme.fg(Token::Muted);
    let mut spans = vec![
        Span::styled(" SELECT ", theme.fg_on(Token::Bg, Token::Accent)),
        Span::styled("  ↑↓ move · space mark · y copy · esc done  ", muted),
        Span::styled(
            format!("{lines} line{}", if lines == 1 { "" } else { "s" }),
            theme.fg(Token::Number),
        ),
        Span::raw("  "),
    ];
    let mut x = area.x + spans.iter().map(|s| width(&s.content) as u16).sum::<u16>();
    let buttons = [
        (SELECT_COPY_ID, "[copy]", SelectAction::Copy),
        (
            SELECT_MARK_ID,
            if marked { "[unmark]" } else { "[mark]" },
            SelectAction::Mark,
        ),
        (SELECT_DONE_ID, "[done]", SelectAction::Done),
    ];
    for (id, label, action) in buttons {
        let w = width(label) as u16;
        let eid = ElementId::from(id);
        let mut st = theme.fg(Token::Accent);
        if hits.is_focused(&eid) {
            st = st.patch(theme.focused());
        }
        spans.push(Span::styled(label, st));
        spans.push(Span::raw(" "));
        if x + w <= area.right() {
            hits.register(eid, Rect::new(x, area.y, w, 1), wrap(action));
        }
        x = x.saturating_add(w + 1);
    }
    frame.render_widget(Paragraph::new(Line::from(spans)).style(theme.base()), area);
}
