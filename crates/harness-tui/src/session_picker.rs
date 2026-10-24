//! The `/resume` picker: a modal listing saved sessions, newest activity
//! first, searchable by name and description.
//!
//! ```text
//!        ╭ Resume session ───────────────────────────────────────╮
//!        │ › parser    2 of 14 sessions                          │
//!        │ ▸ Fix header parser          14:32 today · proj · ... │
//!        │     fix the header parser → add tests · edited src/…  │
//!        │   Header parser docs     yesterday 09:10 · proj · ... │
//!        │     write the parser docs                             │
//!        │ ↑↓ PgUp PgDn Home End move · Enter resume · Esc close │
//!        ╰───────────────────────────────────────────────────────╯
//! ```
//!
//! Each session takes two rows: its name with the last activity, project,
//! model, message count and cost on the right, then its one-line
//! description. The session already open is marked with `●`.
//!
//! Typing filters with fuzzy matching over the name, description,
//! project and tag, each field on its own (the best one counts); an
//! empty query keeps the given order (newest activity first), a query
//! orders by match quality. Up / Down / Tab, PgUp / PgDn
//! and Home / End move the selection, Enter resumes, Esc or a click
//! outside closes; a click on either row of a session resumes it and the
//! wheel moves the selection.
//!
//! The rows come from the core ([`SessionRow`]), already formatted, so
//! this module needs no access to the session store.

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use nucleo_matcher::Utf32String;
use ratatui::Frame;
use ratatui::layout::{Position, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};

use crate::chat::text::{sanitize, truncate, truncate_line, width, wrap_text};
use crate::fuzzy::{Fuzzy, query};
use crate::interaction::HitMap;
use crate::item_detail::{ItemDetail, hard_end, hard_wrap};
use crate::theme::{Theme, Token};

/// Title of the dialog.
pub const TITLE: &str = "Resume session";
/// Pane id of the row list (wheel scrolling).
pub const LIST_PANE: &str = "sessions.list";
/// Element id prefix of a session (`sessions.row.<index>`).
pub const ROW_PREFIX: &str = "sessions.row.";
/// Key hints on the bottom line.
pub const HINTS: &str = "↑↓ PgUp PgDn Home End move · Enter resume · Esc close · type to filter";
/// Widest dialog.
pub const MAX_WIDTH: u16 = 110;
/// Narrowest dialog, so the query line stays readable.
pub const MIN_WIDTH: u16 = 40;
/// Dialog height in percent of the terminal height.
pub const HEIGHT_PERCENT: u16 = 80;
/// Rows per session (name line, description line).
pub const ROWS_PER_SESSION: usize = 2;

/// One saved session, formatted for the picker by the core.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionRow {
    /// Session id (what resuming needs; never shown).
    pub id: String,
    /// Short name.
    pub name: String,
    /// One-line description.
    pub description: String,
    /// Last activity, e.g. `14:32 today`.
    pub when: String,
    /// Project, e.g. `~/code/proj`.
    pub project: String,
    /// Model reference.
    pub model: String,
    /// Message count, e.g. `12 msgs`.
    pub messages: String,
    /// Cost, e.g. `$0.0412`.
    pub cost: String,
    /// Short tag of the id (`#1a2b3c`), searchable.
    pub tag: String,
    /// The session open now.
    pub current: bool,
}

impl SessionRow {
    /// The right-hand details: time, messages, cost, model and project.
    /// The project comes last, so a narrow row trims the path, not the
    /// numbers.
    pub fn details(&self) -> String {
        [
            self.when.as_str(),
            self.messages.as_str(),
            self.cost.as_str(),
            self.model.as_str(),
            self.project.as_str(),
        ]
        .into_iter()
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(" · ")
    }
}

/// What a click on a registered element does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionAction {
    /// The session at this position in the filtered list: resume it.
    Row(usize),
    /// Scroll the selected session's full text by this many rows (▲/▼).
    Detail(i32),
    /// Open or close the selected session's full text (`→ full text`).
    ToggleDetail,
}

/// Element id prefix of the selected session's full text (see
/// [`crate::item_detail`]).
pub const DETAIL_PREFIX: &str = "sessions";

/// The result of an input event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionOutcome {
    /// Not handled.
    Ignored,
    /// State changed; redraw.
    Changed,
    /// Esc or a click outside: close the picker.
    Close,
    /// Resume this session id and close.
    Resume(String),
}

/// The `/resume` picker state. See the [module docs](self).
#[derive(Debug)]
pub struct SessionPicker {
    rows: Vec<SessionRow>,
    /// Per session: its name, description, project and tag, matched one
    /// at a time so a query cannot match letters spread across fields.
    hay: Vec<[Utf32String; 4]>,
    query: String,
    /// Indices into `rows` that match, in display order.
    visible: Vec<usize>,
    selected: usize,
    scroll: usize,
    page: usize,
    fuzzy: Fuzzy,
    detail: ItemDetail,
}

impl SessionPicker {
    /// A picker over `rows` (newest activity first). The first session
    /// that is not the open one is selected.
    pub fn new(rows: Vec<SessionRow>) -> SessionPicker {
        let hay = rows
            .iter()
            .map(|r| {
                [&r.name, &r.description, &r.project, &r.tag].map(|f| Utf32String::from(f.as_str()))
            })
            .collect();
        let mut p = SessionPicker {
            rows,
            hay,
            query: String::new(),
            visible: Vec::new(),
            selected: 0,
            scroll: 0,
            page: 1,
            fuzzy: Fuzzy::new(),
            detail: ItemDetail::new(),
        };
        p.refilter();
        p.selected = p
            .visible
            .iter()
            .position(|&i| !p.rows[i].current)
            .unwrap_or(0);
        p
    }

    /// Every session given.
    pub fn rows(&self) -> &[SessionRow] {
        &self.rows
    }

    /// The filter text.
    pub fn query(&self) -> &str {
        &self.query
    }

    /// Sets the filter text.
    pub fn set_query(&mut self, q: &str) {
        self.query = q.to_owned();
        self.refilter();
    }

    /// Appends pasted text (first line) to the filter.
    pub fn paste(&mut self, text: &str) {
        let line = text.lines().next().unwrap_or("");
        let q = format!("{}{line}", self.query);
        self.set_query(&q);
    }

    /// Sessions that match the filter, in display order.
    pub fn visible(&self) -> Vec<&SessionRow> {
        self.visible.iter().map(|&i| &self.rows[i]).collect()
    }

    /// The selected session.
    pub fn selected(&self) -> Option<&SessionRow> {
        self.visible.get(self.selected).map(|&i| &self.rows[i])
    }

    fn refilter(&mut self) {
        let keep = self.selected().map(|r| r.id.clone());
        let q = self.query.trim();
        if q.is_empty() {
            self.visible = (0..self.rows.len()).collect();
        } else {
            let pattern = query(q);
            let mut scored: Vec<(u32, usize)> = (0..self.rows.len())
                .filter_map(|i| {
                    let fuzzy = &mut self.fuzzy;
                    self.hay[i]
                        .iter()
                        .filter_map(|f| fuzzy.score(&pattern, f.slice(..)))
                        .max()
                        .map(|s| (s, i))
                })
                .collect();
            // Best match first; ties keep the newest-first order.
            scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
            self.visible = scored.into_iter().map(|(_, i)| i).collect();
        }
        self.selected = if q.is_empty() {
            keep.and_then(|id| self.visible.iter().position(|&i| self.rows[i].id == id))
                .unwrap_or(0)
        } else {
            0
        };
        self.scroll = 0;
    }

    fn move_by(&mut self, delta: isize) {
        if self.visible.is_empty() {
            return;
        }
        let last = self.visible.len() as isize - 1;
        self.selected = (self.selected as isize)
            .saturating_add(delta)
            .clamp(0, last) as usize;
    }

    fn choose(&self) -> SessionOutcome {
        match self.selected() {
            Some(r) => SessionOutcome::Resume(r.id.clone()),
            None => SessionOutcome::Ignored,
        }
    }

    /// Handles a key.
    pub fn handle_key(&mut self, key: &KeyEvent) -> SessionOutcome {
        if key.kind == KeyEventKind::Release {
            return SessionOutcome::Ignored;
        }
        let plain = key.modifiers.difference(KeyModifiers::SHIFT).is_empty();
        let page = self.page.max(1) as isize;
        let shift = key.modifiers == KeyModifiers::SHIFT;
        match key.code {
            KeyCode::Esc => return SessionOutcome::Close,
            // Shift+Up/Down scroll the selected session's full text.
            KeyCode::Up if shift => self.detail.scroll(-1),
            KeyCode::Down if shift => self.detail.scroll(1),
            // Right opens (and closes) the selected session's full text.
            KeyCode::Right if plain => self.detail.toggle(),
            KeyCode::Enter => return self.choose(),
            KeyCode::Tab if key.modifiers.contains(KeyModifiers::SHIFT) => self.move_by(-1),
            KeyCode::Down | KeyCode::Tab => self.move_by(1),
            KeyCode::Up | KeyCode::BackTab => self.move_by(-1),
            KeyCode::PageDown => self.move_by(page),
            KeyCode::PageUp => self.move_by(-page),
            KeyCode::Home => self.move_by(isize::MIN / 2),
            KeyCode::End => self.move_by(isize::MAX / 2),
            KeyCode::Backspace => {
                let mut q = self.query.clone();
                q.pop();
                self.set_query(&q);
            }
            KeyCode::Char(c) if plain => {
                let mut q = self.query.clone();
                q.push(c);
                self.set_query(&q);
            }
            _ => return SessionOutcome::Ignored,
        }
        SessionOutcome::Changed
    }

    /// Runs a clicked element's action.
    pub fn activate(&mut self, action: SessionAction) -> SessionOutcome {
        match action {
            SessionAction::Row(pos) if pos < self.visible.len() => {
                self.selected = pos;
                self.choose()
            }
            SessionAction::Row(_) => SessionOutcome::Ignored,
            SessionAction::Detail(n) => {
                self.detail.scroll(n);
                SessionOutcome::Changed
            }
            SessionAction::ToggleDetail => {
                self.detail.toggle();
                SessionOutcome::Changed
            }
        }
    }

    /// Mouse wheel: moves the selection by `lines` sessions.
    pub fn scroll(&mut self, lines: i32) -> SessionOutcome {
        self.move_by(lines as isize);
        SessionOutcome::Changed
    }

    /// The dialog's rectangle inside the terminal `area`.
    pub fn dialog_rect(&self, area: Rect) -> Rect {
        let cap = if area.width > 4 + MIN_WIDTH {
            area.width - 4
        } else {
            area.width
        };
        let w = MAX_WIDTH.min(cap).max(MIN_WIDTH.min(area.width));
        let full = self.full_text(w.saturating_sub(4) as usize, &Theme::default());
        let more = if self.detail.is_open() {
            ItemDetail::wanted(&full, w.saturating_sub(4)) as u16
        } else {
            0
        };
        let wanted =
            (2 + 2 + (self.rows.len().max(1) * ROWS_PER_SESSION) as u16 + 1).saturating_add(more);
        let tall = ((u32::from(area.height) * u32::from(HEIGHT_PERCENT) + 50) / 100) as u16;
        let h = wanted.min(tall.max(7)).min(area.height);
        Rect::new(
            area.x + (area.width - w) / 2,
            area.y + (area.height - h) / 2,
            w,
            h,
        )
    }

    /// The selected session in full (name, details, description) when its
    /// two rows at `w` cells cut any of it; else nothing.
    fn full_text(&self, w: usize, theme: &Theme) -> Vec<Line<'static>> {
        let Some(row) = self.selected() else {
            return Vec::new();
        };
        let (name, details) = name_parts(row, w);
        let desc = row_description(row);
        let cut = name != sanitize(&row.name)
            || details != sanitize(&row.details())
            || width(&desc) > w.saturating_sub(5);
        if !cut {
            return Vec::new();
        }
        vec![
            Line::from(Span::styled(
                sanitize(&row.name),
                theme.fg(Token::Fg).add_modifier(Modifier::BOLD),
            )),
            Line::from(Span::styled(
                sanitize(&row.details()),
                theme.fg(Token::Muted),
            )),
            Line::from(Span::styled(
                desc,
                theme.fg(Token::Muted).add_modifier(Modifier::ITALIC),
            )),
        ]
    }

    fn name_line(
        &self,
        theme: &Theme,
        row: &SessionRow,
        selected: bool,
        w: usize,
    ) -> Line<'static> {
        let base = if selected {
            theme.focused()
        } else {
            Style::new()
        };
        let mut name_style = base.patch(theme.fg(Token::Fg));
        if selected {
            name_style = name_style.add_modifier(Modifier::BOLD);
        }
        let prefix = vec![
            Span::styled(
                if selected { "▸" } else { " " },
                base.patch(theme.fg(Token::Accent)),
            ),
            Span::styled(
                if row.current { "●" } else { " " },
                base.patch(theme.fg(Token::Ok)),
            ),
            Span::styled(" ", base),
        ];
        let used = 3;
        let (name, details) = name_parts(row, w);
        let gap = w.saturating_sub(used + width(&name) + width(&details));
        let mut spans = prefix;
        spans.push(Span::styled(name, name_style));
        spans.push(Span::styled(" ".repeat(gap), base));
        spans.push(Span::styled(details, base.patch(theme.fg(Token::Muted))));
        Line::from(spans)
    }

    fn description_line(
        &self,
        theme: &Theme,
        row: &SessionRow,
        selected: bool,
        w: usize,
    ) -> Line<'static> {
        let base = if selected {
            theme.focused()
        } else {
            Style::new()
        };
        let text = row_description(row);
        let body = truncate(&text, w.saturating_sub(5));
        let fill = w.saturating_sub(5 + width(&body));
        Line::from(vec![
            Span::styled("     ", base),
            Span::styled(
                body,
                base.patch(theme.fg(Token::Muted))
                    .add_modifier(Modifier::ITALIC),
            ),
            Span::styled(" ".repeat(fill), base),
        ])
    }

    /// Draws the picker as a modal over the terminal `area` and registers
    /// its rows and list pane.
    pub fn render<A: Clone>(
        &mut self,
        frame: &mut Frame<'_>,
        area: Rect,
        theme: &Theme,
        hits: &mut HitMap<A>,
        wrap: &dyn Fn(SessionAction) -> A,
    ) {
        let rect = self.dialog_rect(area);
        if rect.width < 3 || rect.height < 3 {
            return;
        }
        hits.push_modal(rect);
        frame.render_widget(Clear, rect);
        let block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(theme.border(true))
            .title(Span::styled(format!(" {TITLE} "), theme.fg(Token::Accent)))
            .style(theme.base());
        let inner = block.inner(rect);
        frame.render_widget(block, rect);
        let inner = Rect {
            x: inner.x + 1.min(inner.width / 2),
            width: inner.width.saturating_sub(2),
            ..inner
        };
        if inner.height == 0 || inner.width == 0 {
            return;
        }
        let w = inner.width as usize;
        let line_at = |frame: &mut Frame<'_>, y: u16, line: Line<'static>| {
            frame.render_widget(
                Paragraph::new(truncate_line(line, w)),
                Rect::new(inner.x, y, inner.width, 1),
            );
        };

        let count = format!(
            "  {} of {} session{}",
            self.visible.len(),
            self.rows.len(),
            if self.rows.len() == 1 { "" } else { "s" }
        );
        // The query wraps (by character, so the cursor follows the text).
        let query = vec![
            Span::styled("› ", theme.fg(Token::Accent)),
            Span::styled(sanitize(&self.query), theme.fg(Token::Fg)),
        ];
        let (crow, ccol) = hard_end(&query, w);
        let mut head_spans = query;
        head_spans.push(Span::styled(count, theme.fg(Token::Muted)));
        let head = hard_wrap(head_spans, w);
        let head_h = head
            .len()
            .min((inner.height as usize).saturating_sub(2).max(1));
        let first = (crow + 1).saturating_sub(head_h);
        for (i, l) in head.into_iter().skip(first).take(head_h).enumerate() {
            line_at(frame, inner.y + i as u16, l);
        }
        let cy = inner.y + (crow - first).min(head_h - 1) as u16;
        frame.set_cursor_position(Position::new(
            inner.x + ccol.min(w.saturating_sub(1)) as u16,
            cy,
        ));
        let head_h = head_h as u16;

        // Key hints wrap at the bottom while the list keeps a row.
        let hints = wrap_text(HINTS, w);
        let hint_rows = if inner.height >= head_h + 3 {
            (hints.len() as u16).min(inner.height - head_h - 2)
        } else {
            0
        };
        for (i, h) in hints.into_iter().take(hint_rows as usize).enumerate() {
            line_at(
                frame,
                inner.bottom() - hint_rows + i as u16,
                Line::from(Span::styled(h, theme.fg(Token::Muted))),
            );
        }
        let mut list = Rect::new(
            inner.x,
            inner.y + head_h,
            inner.width,
            inner.height.saturating_sub(head_h + hint_rows),
        );
        let full = self.full_text(w, theme);
        if let Some(&i) = self.visible.get(self.selected) {
            self.detail.select(i);
        }
        let detail_h = self.detail.rows_for(&full, list.width, list.height);
        list.height -= detail_h;
        self.detail.render_hint(
            frame,
            Rect::new(
                rect.x + 1,
                rect.bottom() - 1,
                rect.width.saturating_sub(2),
                1,
            ),
            !full.is_empty(),
            theme,
            hits,
            DETAIL_PREFIX,
            wrap(SessionAction::ToggleDetail),
        );
        if detail_h > 0 {
            let r = Rect::new(list.x, list.bottom(), list.width, detail_h);
            self.detail.render(
                frame,
                r,
                full,
                theme,
                hits,
                DETAIL_PREFIX,
                wrap(SessionAction::Detail(-1)),
                wrap(SessionAction::Detail(1)),
            );
        }
        let per = (list.height as usize / ROWS_PER_SESSION).max(usize::from(list.height > 0));
        self.page = per.max(1);
        if list.height == 0 {
            return;
        }
        hits.register_pane(LIST_PANE, list);
        if self.visible.is_empty() {
            let msg = if self.rows.is_empty() {
                "no saved sessions yet".to_owned()
            } else {
                format!("no sessions match `{}`", sanitize(&self.query))
            };
            for (i, l) in wrap_text(&msg, w)
                .into_iter()
                .take(list.height as usize)
                .enumerate()
            {
                line_at(
                    frame,
                    list.y + i as u16,
                    Line::from(Span::styled(l, theme.fg(Token::Muted))),
                );
            }
            return;
        }
        if self.selected < self.scroll {
            self.scroll = self.selected;
        } else if self.selected >= self.scroll + per {
            self.scroll = self.selected + 1 - per;
        }
        self.scroll = self.scroll.min(self.visible.len().saturating_sub(per));
        for (slot, pos) in (self.scroll..self.visible.len()).take(per).enumerate() {
            let row = &self.rows[self.visible[pos]];
            let selected = pos == self.selected;
            let y = list.y + (slot * ROWS_PER_SESSION) as u16;
            line_at(frame, y, self.name_line(theme, row, selected, w));
            let mut h = 1;
            if y + 1 < list.bottom() {
                line_at(frame, y + 1, self.description_line(theme, row, selected, w));
                h = 2;
            }
            hits.register(
                format!("{ROW_PREFIX}{pos}"),
                Rect::new(list.x, y, list.width, h),
                wrap(SessionAction::Row(pos)),
            );
        }
    }
}

/// A session's name and details as its first row at `w` cells shows them:
/// the name keeps at least half of the row; the details give way.
fn name_parts(row: &SessionRow, w: usize) -> (String, String) {
    let name = sanitize(&row.name);
    let name_room = w.saturating_sub(3);
    let details_room = name_room.saturating_sub(width(&name).min(name_room / 2) + 2);
    let details = truncate(&sanitize(&row.details()), details_room);
    let name = truncate(&name, name_room.saturating_sub(width(&details) + 2).max(1));
    (name, details)
}

/// A session's description, or a note when it has none.
fn row_description(row: &SessionRow) -> String {
    if row.description.is_empty() {
        "(no messages yet)".to_owned()
    } else {
        sanitize(&row.description).replace('\n', " ")
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crossterm::event::KeyEvent;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    /// Sample rows, newest first.
    pub(crate) fn sample() -> Vec<SessionRow> {
        let row = |id: &str, name: &str, desc: &str, when: &str, current: bool| SessionRow {
            id: id.into(),
            name: name.into(),
            description: desc.into(),
            when: when.into(),
            project: "~/code/proj".into(),
            model: "anthropic/claude-sonnet-5-5".into(),
            messages: "12 msgs".into(),
            cost: "$0.0412".into(),
            tag: format!("#{}", &id[id.len() - 6..]),
            current,
        };
        vec![
            row(
                "s-000000aaaaaa",
                "Current work",
                "what is open now",
                "14:40 today",
                true,
            ),
            row(
                "s-000000bbbbbb",
                "Fix header parser",
                "fix the header parser → add tests · edited src/parse.rs",
                "14:32 today",
                false,
            ),
            row(
                "s-000000cccccc",
                "Release notes",
                "draft the release notes",
                "yesterday 09:10",
                false,
            ),
            row(
                "s-000000dddddd",
                "Docs pass",
                "parser docs and the guide",
                "Oct 21 16:05",
                false,
            ),
        ]
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn render(p: &mut SessionPicker, w: u16, h: u16) -> (String, HitMap<SessionAction>) {
        let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
        let mut hits = HitMap::default();
        term.draw(|f| p.render(f, f.area(), &Theme::default(), &mut hits, &|a| a))
            .unwrap();
        let buf = term.backend().buffer().clone();
        let mut text = String::new();
        for y in 0..h {
            for x in 0..w {
                text.push_str(buf[(x, y)].symbol());
            }
            text.push('\n');
        }
        (text, hits)
    }

    /// The first cell of `needle` in a [`render`] text.
    fn at(text: &str, needle: &str) -> (u16, u16) {
        text.lines()
            .enumerate()
            .find_map(|(y, l)| {
                let i = l.find(needle)?;
                Some((l[..i].chars().count() as u16, y as u16))
            })
            .unwrap_or_else(|| panic!("{needle:?} not in:\n{text}"))
    }

    fn click(hits: &HitMap<SessionAction>, (x, y): (u16, u16)) -> SessionAction {
        hits.hit(x, y).expect("clickable").action
    }

    /// A cut session offers `→ full text`; Right or a click on it shows
    /// its name, details and description wrapped, Shift+Down or ▼
    /// scrolls, Right or `← hide full text` closes.
    #[test]
    fn cut_sessions_open_their_full_text_by_keyboard_and_mouse() {
        let mut rows = sample();
        rows[1].description = "a long description of the work ".repeat(6) + "tail_end";
        let mut p = SessionPicker::new(rows);
        let (text, hits) = render(&mut p, 60, 20);
        assert!(text.contains("→ full text"), "{text}");
        assert!(!text.contains("tail_end"), "{text}");
        assert_eq!(p.handle_key(&key(KeyCode::Right)), SessionOutcome::Changed);
        let (text, _) = render(&mut p, 60, 20);
        assert!(text.contains("─ full text"), "{text}");
        assert!(text.contains("← hide full text"), "{text}");
        p.handle_key(&key(KeyCode::Right));
        let (text, hits2) = render(&mut p, 60, 20);
        assert!(!text.contains("─ full text"), "{text}");
        drop(hits);
        // Mouse.
        let a = click(&hits2, at(&text, "→ full text"));
        assert_eq!(a, SessionAction::ToggleDetail);
        p.activate(a);
        let (text, hits) = render(&mut p, 60, 20);
        assert!(text.contains("shift+↑↓ 1-"), "{text}");
        let a = click(&hits, at(&text, "▼"));
        assert_eq!(a, SessionAction::Detail(1));
        p.activate(a);
        let (text, _) = render(&mut p, 60, 20);
        assert!(text.contains("shift+↑↓ 2-"), "{text}");
        p.handle_key(&KeyEvent::new(KeyCode::Down, KeyModifiers::SHIFT));
        let (text, _) = render(&mut p, 60, 20);
        assert!(text.contains("shift+↑↓ 3-"), "{text}");
        p.handle_key(&KeyEvent::new(KeyCode::Up, KeyModifiers::SHIFT));
        let (text, hits) = render(&mut p, 60, 20);
        assert!(text.contains("shift+↑↓ 2-"), "{text}");
        let (x, y) = at(&text, "← hide full text");
        p.activate(click(&hits, (x + 2, y)));
        let (text, _) = render(&mut p, 60, 20);
        assert!(!text.contains("─ full text"), "{text}");
    }

    #[test]
    fn opens_on_the_newest_session_that_is_not_open() {
        let p = SessionPicker::new(sample());
        assert_eq!(p.selected().unwrap().name, "Fix header parser");
        assert_eq!(p.visible().len(), 4);
    }

    #[test]
    fn keyboard_moves_filters_and_resumes() {
        let mut p = SessionPicker::new(sample());
        assert_eq!(p.handle_key(&key(KeyCode::Down)), SessionOutcome::Changed);
        assert_eq!(p.selected().unwrap().name, "Release notes");
        p.handle_key(&key(KeyCode::End));
        assert_eq!(p.selected().unwrap().name, "Docs pass");
        p.handle_key(&key(KeyCode::Home));
        assert_eq!(p.selected().unwrap().name, "Current work");
        p.handle_key(&key(KeyCode::Tab));
        assert_eq!(p.selected().unwrap().name, "Fix header parser");
        p.handle_key(&key(KeyCode::BackTab));
        assert_eq!(p.selected().unwrap().name, "Current work");
        // Fuzzy over names and descriptions.
        for c in "relnot".chars() {
            p.handle_key(&key(KeyCode::Char(c)));
        }
        assert_eq!(p.query(), "relnot");
        assert_eq!(p.selected().unwrap().name, "Release notes");
        p.set_query("guide");
        assert_eq!(
            p.visible()
                .iter()
                .map(|r| r.name.as_str())
                .collect::<Vec<_>>(),
            ["Docs pass"],
            "descriptions are searched"
        );
        p.set_query("#cccccc");
        assert_eq!(p.selected().unwrap().name, "Release notes", "tags too");
        p.set_query("parser");
        let names: Vec<&str> = p.visible().iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names[0], "Fix header parser", "{names:?}");
        assert!(names.contains(&"Docs pass"), "{names:?}");
        assert_eq!(
            p.handle_key(&key(KeyCode::Enter)),
            SessionOutcome::Resume("s-000000bbbbbb".into())
        );
        p.handle_key(&key(KeyCode::Backspace));
        assert_eq!(p.query(), "parse");
        p.set_query("zzzz");
        assert!(p.visible().is_empty());
        assert_eq!(p.handle_key(&key(KeyCode::Enter)), SessionOutcome::Ignored);
        assert_eq!(p.handle_key(&key(KeyCode::Esc)), SessionOutcome::Close);
    }

    #[test]
    fn rows_show_name_time_details_and_description_and_are_clickable() {
        let mut p = SessionPicker::new(sample());
        let (text, hits) = render(&mut p, 120, 30);
        assert!(text.contains("Resume session"), "{text}");
        assert!(text.contains("4 of 4 sessions"), "{text}");
        assert!(
            text.contains(
                "14:32 today · 12 msgs · $0.0412 · anthropic/claude-sonnet-5-5 · ~/code/proj"
            ),
            "{text}"
        );
        assert!(text.contains("fix the header parser → add tests"), "{text}");
        assert!(text.contains("●"), "the open session is marked: {text}");
        assert!(text.contains(HINTS), "{text}");
        assert!(!text.contains("s-000000"), "ids are never shown: {text}");
        // Both rows of a session are one click target.
        let line = text
            .lines()
            .position(|l| l.contains("draft the release notes"))
            .unwrap();
        let rect = hits.region(&format!("{ROW_PREFIX}2").into()).unwrap().rect;
        assert_eq!((rect.y + 1) as usize, line, "description row");
        assert_eq!(rect.height, 2);
        assert_eq!(
            p.activate(SessionAction::Row(2)),
            SessionOutcome::Resume("s-000000cccccc".into())
        );
        assert_eq!(p.activate(SessionAction::Row(99)), SessionOutcome::Ignored);
        assert_eq!(p.scroll(1), SessionOutcome::Changed);
        assert_eq!(p.selected().unwrap().name, "Docs pass");
    }

    #[test]
    fn narrow_terminals_cut_details_before_names() {
        let mut p = SessionPicker::new(sample());
        let (text, _) = render(&mut p, 60, 20);
        assert!(text.contains("Fix header parser"), "{text}");
        assert!(text.contains("14:32 today"), "{text}");
        for line in text.lines() {
            assert!(width(line) <= 60, "{line}");
        }
    }

    #[test]
    fn long_lists_scroll_to_keep_the_selection_visible() {
        let rows: Vec<SessionRow> = (0..30)
            .map(|i| SessionRow {
                id: format!("s-{i:012}"),
                name: format!("Session {i}"),
                description: format!("work item {i}"),
                when: "14:32 today".into(),
                project: "p".into(),
                model: "m".into(),
                messages: "1 msg".into(),
                cost: "$0".into(),
                tag: format!("#{i:06}"),
                current: false,
            })
            .collect();
        let mut p = SessionPicker::new(rows);
        render(&mut p, 100, 20);
        p.handle_key(&key(KeyCode::End));
        let (text, _) = render(&mut p, 100, 20);
        assert!(text.contains("Session 29"), "{text}");
        assert!(!text.contains("Session 0 "), "{text}");
        p.handle_key(&key(KeyCode::PageUp));
        let (text, _) = render(&mut p, 100, 20);
        assert!(text.contains(&p.selected().unwrap().name), "{text}");
    }

    #[test]
    fn empty_lists_say_so() {
        let mut p = SessionPicker::new(Vec::new());
        let (text, _) = render(&mut p, 80, 20);
        assert!(text.contains("no saved sessions yet"), "{text}");
        let mut p = SessionPicker::new(sample());
        p.set_query("zzzz");
        let (text, _) = render(&mut p, 80, 20);
        assert!(text.contains("no sessions match `zzzz`"), "{text}");
    }

    #[test]
    fn a_query_must_match_within_one_field() {
        // Seen on macOS CI: "header" matched letters spread over the
        // description ("tHE releAse"), a temp dir ("tmpeQDiem") and
        // "pRoject" once the fields were joined into one string.
        let mut rows = sample();
        rows[2].name = "Write release notes".into();
        rows[2].description = "write the release notes".into();
        rows[2].project = "…/T/.tmpeQDiem/project".into();
        let mut p = SessionPicker::new(rows);
        for c in "header".chars() {
            p.handle_key(&key(KeyCode::Char(c)));
        }
        let names: Vec<&str> = p.visible().iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, ["Fix header parser"]);
    }
}
