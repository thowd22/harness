//! The full text of a picker's selected item.
//!
//! Lists that keep one row per item (the command palette, `/model`,
//! `/resume`) cut long titles, descriptions and cells to keep their columns
//! aligned. When the selected item lost text that way, the dialog's bottom
//! border offers `→ full text` ([`ItemDetail::render_hint`]): Right, or a
//! click on it, opens an [`ItemDetail`] at the bottom of the list: a
//! divider (`─ full text ─`) and the item's text wrapped to the list width
//! (`← hide` closes it again). When the text is taller than its rows the
//! divider shows `▲ ▼`, a `shift+↑↓` hint and the position (`1-3/9`);
//! Shift+Up/Down or clicking ▲/▼ scrolls it. The detail stays open while
//! the selection moves, showing each cut item in turn.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::chat::text::{width, wrap_lines};
use crate::interaction::{ElementId, HitMap};
use crate::theme::{Theme, Token};

/// Element id suffix of the scroll-up button (`<prefix>.detail.up`).
pub const UP_SUFFIX: &str = ".detail.up";
/// Element id suffix of the scroll-down button (`<prefix>.detail.down`).
pub const DOWN_SUFFIX: &str = ".detail.down";
/// Element id suffix of the open/close hint (`<prefix>.detail.toggle`).
pub const TOGGLE_SUFFIX: &str = ".detail.toggle";
/// The hint on a dialog's bottom border while the detail is closed.
pub const OPEN_HINT: &str = " → full text ";
/// The hint while it is open.
pub const CLOSE_HINT: &str = " ← hide full text ";

/// Scroll state of the detail of one selected item.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ItemDetail {
    open: bool,
    key: Option<usize>,
    scroll: usize,
    total: usize,
    view: usize,
}

impl ItemDetail {
    /// No item shown yet.
    pub fn new() -> ItemDetail {
        ItemDetail::default()
    }

    /// Shows the detail of item `key`; scrolling restarts when it changes.
    pub fn select(&mut self, key: usize) {
        if self.key != Some(key) {
            self.key = Some(key);
            self.scroll = 0;
        }
    }

    /// Whether the detail is open.
    pub fn is_open(&self) -> bool {
        self.open
    }

    /// Opens or closes the detail.
    pub fn toggle(&mut self) {
        self.open = !self.open;
        self.scroll = 0;
    }

    /// Rows to give the detail of `lines` (the selected item's full text,
    /// empty when nothing was cut) below a list of `avail` rows at `width`:
    /// none while closed.
    pub fn rows_for(&self, lines: &[Line<'static>], width: u16, avail: u16) -> u16 {
        if !self.open {
            return 0;
        }
        ItemDetail::height(ItemDetail::wanted(lines, width), avail)
    }

    /// Draws the open/close hint right-aligned on `row` (a dialog's bottom
    /// border, corners excluded) when the selected item was cut (`cut`) or
    /// the detail is open, and registers it with `toggle`.
    #[allow(clippy::too_many_arguments)]
    pub fn render_hint<A: Clone>(
        &self,
        frame: &mut Frame<'_>,
        row: Rect,
        cut: bool,
        theme: &Theme,
        hits: &mut HitMap<A>,
        prefix: &str,
        toggle: A,
    ) {
        if !(cut || self.open) || row.height == 0 {
            return;
        }
        let label = if self.open { CLOSE_HINT } else { OPEN_HINT };
        let w = width(label) as u16;
        if w + 2 > row.width {
            return;
        }
        let id = ElementId(format!("{prefix}{TOGGLE_SUFFIX}"));
        let st = if hits.is_focused(&id) {
            theme.focused()
        } else {
            theme.fg(Token::Link)
        };
        let r = Rect::new(row.right() - w - 1, row.y, w, 1);
        frame.render_widget(Paragraph::new(Span::styled(label, st)), r);
        hits.register(id, r, toggle);
    }

    /// First shown row of the wrapped text.
    pub fn offset(&self) -> usize {
        self.scroll
    }

    /// Whether the text is taller than the rows it got in the last frame.
    pub fn overflows(&self) -> bool {
        self.total > self.view
    }

    /// Scrolls the text by `lines` (negative: up).
    pub fn scroll(&mut self, lines: i32) {
        let max = self.total.saturating_sub(self.view.max(1)) as i64;
        self.scroll = (self.scroll as i64 + i64::from(lines)).clamp(0, max) as usize;
    }

    /// Rows the detail of `lines` wants at `width` (divider included).
    pub fn wanted(lines: &[Line<'static>], width: u16) -> usize {
        if lines.is_empty() || width == 0 {
            return 0;
        }
        1 + wrap_lines(lines.iter().cloned(), width as usize, &[]).len()
    }

    /// Rows to give a detail wanting `wanted` rows below a list of `avail`
    /// rows: at most half of them (at least 3), keeping a row for the list.
    pub fn height(wanted: usize, avail: u16) -> u16 {
        if wanted == 0 || avail < 4 {
            return 0;
        }
        let cap = (avail / 2).max(3).min(avail - 1);
        (wanted as u16).min(cap)
    }

    /// Draws the divider and the wrapped `lines` into `area` and registers
    /// the ▲/▼ buttons (`<prefix>.detail.up` / `.down`) with `up` / `down`.
    #[allow(clippy::too_many_arguments)]
    pub fn render<A: Clone>(
        &mut self,
        frame: &mut Frame<'_>,
        area: Rect,
        lines: Vec<Line<'static>>,
        theme: &Theme,
        hits: &mut HitMap<A>,
        prefix: &str,
        up: A,
        down: A,
    ) {
        if area.height == 0 || area.width == 0 {
            return;
        }
        let wrapped = wrap_lines(lines, area.width as usize, &[]);
        self.total = wrapped.len();
        self.view = (area.height - 1) as usize;
        self.scroll = self.scroll.min(self.total.saturating_sub(self.view.max(1)));
        let border = theme.fg(Token::Border);
        let mut spans = vec![
            Span::styled("─ ", border),
            Span::styled("full text ", theme.fg(Token::Muted)),
        ];
        let mut buttons = Vec::new();
        if self.overflows() {
            let x0 = area.x + spans.iter().map(|s| width(&s.content) as u16).sum::<u16>();
            for (i, (suffix, label, action)) in [(UP_SUFFIX, "▲", up), (DOWN_SUFFIX, "▼", down)]
                .into_iter()
                .enumerate()
            {
                let id = ElementId(format!("{prefix}{suffix}"));
                let st = if hits.is_focused(&id) {
                    theme.focused()
                } else {
                    theme.fg(Token::Link)
                };
                spans.push(Span::styled(label, st));
                spans.push(Span::styled(" ", border));
                buttons.push((id, x0 + 2 * i as u16, action));
            }
            let pos = format!(
                "shift+↑↓ {}-{}/{} ",
                self.scroll + 1,
                (self.scroll + self.view).min(self.total),
                self.total
            );
            spans.push(Span::styled(pos, theme.fg(Token::Muted)));
        }
        let used: usize = spans.iter().map(|s| width(&s.content)).sum();
        spans.push(Span::styled(
            "─".repeat((area.width as usize).saturating_sub(used)),
            border,
        ));
        frame.render_widget(
            Paragraph::new(Line::from(spans)),
            Rect::new(area.x, area.y, area.width, 1),
        );
        for (id, x, action) in buttons {
            if x < area.right() {
                hits.register(id, Rect::new(x, area.y, 1, 1), action);
            }
        }
        let body: Vec<Line<'static>> = wrapped
            .into_iter()
            .skip(self.scroll)
            .take(self.view)
            .collect();
        frame.render_widget(
            Paragraph::new(body),
            Rect::new(area.x, area.y + 1, area.width, area.height - 1),
        );
    }
}

/// Wraps `spans` at exactly `width` cells, by character rather than by
/// word, so the end of any prefix of `spans` lands where [`hard_end`] says:
/// used for query and argument lines, which show a cursor.
pub fn hard_wrap(spans: Vec<Span<'static>>, width: usize) -> Vec<Line<'static>> {
    let width = width.max(1);
    let mut out = vec![Vec::new()];
    let mut col = 0;
    for span in spans {
        let mut cur = String::new();
        for c in span.content.chars() {
            let cw = crate::chat::text::width(c.encode_utf8(&mut [0; 4]));
            if col + cw > width && col > 0 {
                if !cur.is_empty() {
                    out.last_mut()
                        .expect("a row")
                        .push(Span::styled(std::mem::take(&mut cur), span.style));
                }
                out.push(Vec::new());
                col = 0;
            }
            cur.push(c);
            col += cw;
        }
        if !cur.is_empty() {
            out.last_mut()
                .expect("a row")
                .push(Span::styled(cur, span.style));
        }
    }
    out.into_iter().map(Line::from).collect()
}

/// Row and column right after `spans` laid out by [`hard_wrap`] at
/// `width` (a full last row puts the position at the start of the next).
pub fn hard_end(spans: &[Span<'_>], width: usize) -> (usize, usize) {
    let width = width.max(1);
    let (mut row, mut col) = (0, 0);
    for c in spans.iter().flat_map(|s| s.content.chars()) {
        let cw = crate::chat::text::width(c.encode_utf8(&mut [0; 4]));
        if col + cw > width && col > 0 {
            row += 1;
            col = 0;
        }
        col += cw;
    }
    if col >= width {
        (row + 1, 0)
    } else {
        (row, col)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interaction::testing::{buffer_text, click, find_text};
    use crate::interaction::{Dispatch, Interaction};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn draw(
        term: &mut Terminal<TestBackend>,
        d: &mut ItemDetail,
        it: &mut Interaction<i32>,
        lines: &[Line<'static>],
    ) -> String {
        let mut hits = it.begin_frame();
        term.draw(|f| {
            d.render(
                f,
                f.area(),
                lines.to_vec(),
                &Theme::default(),
                &mut hits,
                "t",
                -1,
                1,
            )
        })
        .unwrap();
        it.end_frame(hits);
        buffer_text(term.backend().buffer())
    }

    #[test]
    fn scrolls_by_buttons_and_reports_position() {
        let mut d = ItemDetail::new();
        d.select(0);
        let lines: Vec<Line<'static>> = (0..6).map(|i| Line::from(format!("line {i}"))).collect();
        let mut it: Interaction<i32> = Interaction::new();
        let mut term = Terminal::new(TestBackend::new(30, 4)).unwrap();
        let text = draw(&mut term, &mut d, &mut it, &lines);
        assert!(text.contains("1-3/6") && text.contains("line 0"), "{text}");
        // Click ▼.
        let (x, y) = find_text(term.backend().buffer(), "▼").unwrap();
        let crossterm::event::Event::Mouse(m) = click(x, y) else {
            unreachable!()
        };
        match it.handle_mouse(&m) {
            Some(Dispatch::Activate(n)) => d.scroll(n),
            other => panic!("{other:?}"),
        }
        let text = draw(&mut term, &mut d, &mut it, &lines);
        assert!(text.contains("2-4/6") && text.contains("line 3"), "{text}");
        d.scroll(10);
        assert_eq!(d.offset(), 3);
        d.select(0);
        assert_eq!(d.offset(), 3, "same item keeps its position");
        d.select(1);
        assert_eq!(d.offset(), 0);
    }

    #[test]
    fn hard_wrap_matches_hard_end() {
        let spans = vec![Span::raw("› "), Span::raw("abcdefgh日本")];
        let rows = hard_wrap(spans.clone(), 4);
        let t: Vec<String> = rows
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect();
        assert_eq!(t, vec!["› ab", "cdef", "gh日", "本"]);
        assert_eq!(hard_end(&spans, 4), (3, 2));
        assert_eq!(hard_end(&[Span::raw("abcd")], 4), (1, 0));
    }

    #[test]
    fn toggles_open_and_hints_only_when_cut() {
        let mut d = ItemDetail::new();
        let lines = vec![Line::from("x")];
        assert_eq!(d.rows_for(&lines, 10, 20), 0, "closed");
        let mut term = Terminal::new(TestBackend::new(30, 1)).unwrap();
        let mut it: Interaction<u8> = Interaction::new();
        let mut hint = |d: &ItemDetail, cut: bool, it: &mut Interaction<u8>| {
            let mut hits = it.begin_frame();
            term.draw(|f| d.render_hint(f, f.area(), cut, &Theme::default(), &mut hits, "t", 7))
                .unwrap();
            it.end_frame(hits);
            buffer_text(term.backend().buffer())
        };
        assert!(!hint(&d, false, &mut it).contains("full text"));
        assert!(hint(&d, true, &mut it).contains("→ full text"));
        assert!(
            it.hits()
                .region(&ElementId::from("t.detail.toggle"))
                .is_some()
        );
        d.toggle();
        assert!(hint(&d, false, &mut it).contains("← hide full text"));
        assert_eq!(d.rows_for(&lines, 10, 20), 2);
    }

    #[test]
    fn height_keeps_a_list_row() {
        assert_eq!(ItemDetail::height(0, 20), 0);
        assert_eq!(ItemDetail::height(3, 3), 0);
        assert_eq!(ItemDetail::height(30, 20), 10);
        assert_eq!(ItemDetail::height(30, 4), 3);
        assert_eq!(ItemDetail::height(2, 20), 2);
    }
}
