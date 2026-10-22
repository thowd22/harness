//! The `/model` picker: a large modal listing every model in the catalog.
//!
//! [`ModelPicker`] shows the entries of a
//! [`Catalog`] grouped by provider, one
//! row per model and auth method, in aligned columns: model, provider,
//! alias, context window, input and output price, capabilities, auth method
//! and status.
//!
//! ```text
//!            ╭ Switch model ──────────────────────────────────────╮
//!            │ › son       2 of 31             ⟳ refreshing lists │
//!            │    Model            Provider   Alias   Context ... │
//!            │ anthropic · 1 model                                │
//!            │ ▸● claude-sonnet-5-5 anthropic  sonnet     200k ... │
//!            │ openrouter · 1 model                               │
//!            │    anthropic/clau…   openrouter            200k ... │
//!            │ ↑↓ PgUp PgDn Home End move · Enter switch · Esc …  │
//!            ╰────────────────────────────────────────────────────╯
//! ```
//!
//! Layout rules:
//!
//! - the dialog is [`HEIGHT_PERCENT`] of the terminal height
//!   ([`dialog_height`]) and centered,
//! - it is as wide as its widest row; when that is wider than the terminal
//!   minus [`MARGIN`] cells on each side, it is capped there and the widest
//!   text columns (status, model, alias, ...) are cut with an ellipsis. A
//!   row never wraps.
//!
//! The current model is marked with `●`. Models that cannot be used are
//! dimmed and their status column says why; choosing one keeps the picker
//! open and shows the reason. Typing filters the rows with fuzzy matching
//! (model id, provider, name and aliases). Up / Down / Tab, PgUp / PgDn and
//! Home / End move the selection, Enter switches, Esc closes; a click on a
//! row switches to it and the wheel scrolls.
//!
//! The picker opens from the catalog the core already has (the cached live
//! lists), so it needs no network; while a background refresh runs
//! ([`Catalog::refreshing`](harness_providers::catalog::Catalog::refreshing))
//! the top line shows a refresh indicator and new catalogs replace the rows
//! in place ([`ModelPicker::set_catalog`]).

use std::sync::Arc;

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use harness_providers::catalog::{Availability, Catalog, CatalogEntry, PriceOrigin};
use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Matcher, Utf32String};
use ratatui::Frame;
use ratatui::layout::{Position, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};

use crate::chat::text::{truncate, truncate_line, width};
use crate::interaction::HitMap;
use crate::theme::{Theme, Token};

/// Palette entry id that opens the picker.
pub const PALETTE_ID: &str = "model.pick";
/// Pane id of the row list (wheel scrolling).
pub const LIST_PANE: &str = "models.list";
/// Element id prefix of a row (`models.row.<item>`).
pub const ROW_PREFIX: &str = "models.row.";
/// Cells kept free on each side when the dialog is capped at the terminal
/// width.
pub const MARGIN: u16 = 2;
/// Dialog height in percent of the terminal height.
pub const HEIGHT_PERCENT: u16 = 90;
/// Narrowest dialog, so the query line stays readable.
pub const MIN_WIDTH: u16 = 48;
/// Column headers, in display order.
pub const COLUMNS: [&str; COLS] = [
    "Model",
    "Provider",
    "Alias",
    "Context",
    "In $/M",
    "Out $/M",
    "Capabilities",
    "Auth",
    "Status",
];
/// Shown while a background refresh of the live lists runs.
pub const REFRESHING: &str = "⟳ refreshing model lists";
/// Key hints on the bottom line.
pub const HINTS: &str = "↑↓ PgUp PgDn Home End move · Enter switch · Esc close · type to filter";

const COLS: usize = 9;
const RIGHT: [bool; COLS] = [false, false, false, true, true, true, false, false, false];
/// How columns are cut when the dialog is capped: `(column, floor)` in
/// order. Each stage narrows one column toward its floor, only as far as
/// needed, so the status reason goes first and the model id last.
const SHRINK: [(usize, usize); 15] = [
    (8, 24),
    (2, 8),
    (6, 12),
    (1, 10),
    (7, 7),
    (0, 30),
    (8, 12),
    (6, 8),
    (0, 16),
    (8, 4),
    (2, 4),
    (6, 4),
    (1, 4),
    (7, 4),
    (0, 4),
];
const GAP: usize = 2;
/// Selection marker, current marker, space.
const PREFIX: usize = 3;
/// Blank cells between the border and the content, each side.
const PAD: u16 = 1;

/// Height of the dialog for a terminal `height` rows tall:
/// [`HEIGHT_PERCENT`] of it, rounded to the nearest row.
pub fn dialog_height(height: u16) -> u16 {
    ((u32::from(height) * u32::from(HEIGHT_PERCENT) + 50) / 100) as u16
}

/// What a click on a registered element does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PickerAction {
    /// The row with this item index: select it and switch.
    Row(usize),
}

/// The result of an input event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PickerOutcome {
    /// Not handled.
    Ignored,
    /// State changed; redraw.
    Changed,
    /// Esc or a click outside: close the picker.
    Close,
    /// Switch to this `provider/model` reference and close.
    Switch(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Item {
    /// A provider group header with the number of rows under it.
    Header { provider: String, count: usize },
    /// An entry of the catalog (index).
    Entry(usize),
}

/// The `/model` picker state. See the [module docs](self).
pub struct ModelPicker {
    catalog: Arc<Catalog>,
    current: Option<String>,
    cells: Vec<[String; COLS]>,
    hay: Vec<Utf32String>,
    natural: [usize; COLS],
    query: String,
    items: Vec<Item>,
    matches: usize,
    selected: Option<usize>,
    scroll: usize,
    page: usize,
    notice: Option<String>,
    matcher: Matcher,
}

impl std::fmt::Debug for ModelPicker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ModelPicker")
            .field("entries", &self.catalog.entries().len())
            .field("current", &self.current)
            .field("query", &self.query)
            .field("selected", &self.selected)
            .finish_non_exhaustive()
    }
}

fn tokens(n: u32) -> String {
    match n {
        0 => "?".into(),
        n if n >= 1_000_000 && n % 1_000_000 == 0 => format!("{}M", n / 1_000_000),
        n if n >= 1_000_000 => format!("{:.2}M", f64::from(n) / 1e6)
            .trim_end_matches('0')
            .to_owned(),
        n if n >= 1_000 => format!("{}k", n / 1_000),
        n => n.to_string(),
    }
}

fn usd(p: Option<f64>, origin: PriceOrigin) -> String {
    match p {
        _ if origin == PriceOrigin::Plan => "plan".into(),
        Some(0.0) => "free".into(),
        Some(v) if v < 0.1 => format!("${v:.3}"),
        Some(v) => format!("${v:.2}"),
        None => "?".into(),
    }
}

fn cells(e: &CatalogEntry) -> [String; COLS] {
    let c = e.capabilities;
    let caps: Vec<&str> = [
        (c.tools, "tools"),
        (c.reasoning, "reasoning"),
        (c.vision, "vision"),
    ]
    .into_iter()
    .filter_map(|(on, name)| on.then_some(name))
    .collect();
    [
        e.model().to_owned(),
        e.provider().to_owned(),
        e.aliases.join(", "),
        tokens(e.context_window),
        usd(e.input_price(), e.price_origin),
        usd(e.output_price(), e.price_origin),
        caps.join(" "),
        e.auth.to_string(),
        match &e.availability {
            Availability::Usable => String::new(),
            Availability::NoTools => "chat only: no tool calling".into(),
            Availability::Unavailable(r) => r.clone(),
        },
    ]
}

fn pad(text: &str, w: usize, right: bool) -> String {
    let t = truncate(text, w);
    let fill = " ".repeat(w.saturating_sub(width(&t)));
    if right {
        format!("{fill}{t}")
    } else {
        format!("{t}{fill}")
    }
}

impl ModelPicker {
    /// A picker over `catalog`; `current` is the session's model
    /// (`provider/model` or an alias), marked and selected first.
    pub fn new(catalog: Arc<Catalog>, current: Option<String>) -> ModelPicker {
        let mut p = ModelPicker {
            catalog: Arc::new(Catalog::default()),
            current,
            cells: Vec::new(),
            hay: Vec::new(),
            natural: COLUMNS.map(width),
            query: String::new(),
            items: Vec::new(),
            matches: 0,
            selected: None,
            scroll: 0,
            page: 1,
            notice: None,
            matcher: Matcher::new(nucleo_matcher::Config::DEFAULT),
        };
        p.set_catalog(catalog);
        p.selected = p
            .current_item()
            .or_else(|| p.first_where(|e| e.usable()))
            .or_else(|| p.entry_items().first().copied());
        p
    }

    /// Replaces the catalog (a refresh finished), keeping the query and the
    /// selected model.
    pub fn set_catalog(&mut self, catalog: Arc<Catalog>) {
        let keep = self.selected_entry().map(|e| (e.reference.clone(), e.auth));
        self.catalog = catalog;
        self.cells = self.catalog.entries().iter().map(cells).collect();
        self.hay = self
            .catalog
            .entries()
            .iter()
            .map(|e| {
                Utf32String::from(format!(
                    "{} {} {} {}",
                    e.model(),
                    e.provider(),
                    e.display_name,
                    e.aliases.join(" ")
                ))
            })
            .collect();
        let mut natural = COLUMNS.map(width);
        for row in &self.cells {
            for (w, cell) in natural.iter_mut().zip(row) {
                *w = (*w).max(width(cell));
            }
        }
        self.natural = natural;
        self.refilter();
        if let Some((reference, auth)) = keep {
            let entries = self.catalog.entries();
            if let Some(i) = self.items.iter().position(|it| {
                matches!(it, Item::Entry(i) if entries[*i].reference == reference && entries[*i].auth == auth)
            }) {
                self.selected = Some(i);
            }
        }
    }

    /// The catalog shown.
    pub fn catalog(&self) -> &Catalog {
        &self.catalog
    }

    /// The filter text.
    pub fn query(&self) -> &str {
        &self.query
    }

    /// Sets the filter text.
    pub fn set_query(&mut self, q: &str) {
        self.query = q.to_owned();
        self.notice = None;
        self.refilter();
    }

    /// Appends pasted text (first line) to the filter.
    pub fn paste(&mut self, text: &str) {
        let line = text.lines().next().unwrap_or("");
        let q = format!("{}{line}", self.query);
        self.set_query(&q);
    }

    /// The selected entry.
    pub fn selected_entry(&self) -> Option<&CatalogEntry> {
        match self.items.get(self.selected?)? {
            Item::Entry(i) => self.catalog.entries().get(*i),
            Item::Header { .. } => None,
        }
    }

    /// Entries that match the filter, in display order.
    pub fn visible(&self) -> Vec<&CatalogEntry> {
        self.items
            .iter()
            .filter_map(|it| match it {
                Item::Entry(i) => self.catalog.entries().get(*i),
                Item::Header { .. } => None,
            })
            .collect()
    }

    /// The message shown after choosing a model that cannot be used.
    pub fn notice(&self) -> Option<&str> {
        self.notice.as_deref()
    }

    fn is_current(&self, e: &CatalogEntry) -> bool {
        self.current
            .as_deref()
            .is_some_and(|c| e.reference.to_string() == c || e.aliases.iter().any(|a| a == c))
    }

    fn entry_items(&self) -> Vec<usize> {
        self.items
            .iter()
            .enumerate()
            .filter(|(_, it)| matches!(it, Item::Entry(_)))
            .map(|(i, _)| i)
            .collect()
    }

    fn first_where(&self, f: impl Fn(&CatalogEntry) -> bool) -> Option<usize> {
        let entries = self.catalog.entries();
        self.items
            .iter()
            .position(|it| matches!(it, Item::Entry(i) if f(&entries[*i])))
    }

    fn current_item(&self) -> Option<usize> {
        self.first_where(|e| self.is_current(e) && e.usable())
            .or_else(|| self.first_where(|e| self.is_current(e)))
    }

    fn refilter(&mut self) {
        let keep = self.selected.and_then(|s| self.items.get(s).cloned());
        let catalog = self.catalog.clone();
        let pattern = (!self.query.trim().is_empty())
            .then(|| Pattern::parse(&self.query, CaseMatching::Smart, Normalization::Smart));
        let mut items: Vec<Item> = Vec::new();
        let mut header: Option<usize> = None;
        let mut matches = 0;
        for (i, e) in catalog.entries().iter().enumerate() {
            if let Some(p) = &pattern
                && p.score(self.hay[i].slice(..), &mut self.matcher).is_none()
            {
                continue;
            }
            let same = header.is_some_and(
                |h| matches!(&items[h], Item::Header { provider, .. } if provider == e.provider()),
            );
            if !same {
                header = Some(items.len());
                items.push(Item::Header {
                    provider: e.provider().to_owned(),
                    count: 0,
                });
            }
            if let Some(Item::Header { count, .. }) = header.map(|h| &mut items[h]) {
                *count += 1;
            }
            items.push(Item::Entry(i));
            matches += 1;
        }
        self.items = items;
        self.matches = matches;
        self.selected = keep
            .and_then(|k| self.items.iter().position(|it| *it == k))
            .or_else(|| self.entry_items().first().copied());
        self.scroll = 0;
    }

    fn move_by(&mut self, delta: isize) {
        let entries = self.entry_items();
        if entries.is_empty() {
            return;
        }
        let pos = self
            .selected
            .and_then(|s| entries.iter().position(|&i| i == s))
            .unwrap_or(0);
        let next = (pos as isize + delta).clamp(0, entries.len() as isize - 1) as usize;
        self.selected = Some(entries[next]);
        self.notice = None;
    }

    fn choose(&mut self) -> PickerOutcome {
        let Some(e) = self.selected_entry() else {
            return PickerOutcome::Ignored;
        };
        if e.usable() {
            return PickerOutcome::Switch(e.reference.to_string());
        }
        self.notice = Some(format!(
            "cannot switch to {}: {}",
            e.reference,
            e.reason().unwrap_or_default()
        ));
        PickerOutcome::Changed
    }

    /// Handles a key.
    pub fn handle_key(&mut self, key: &KeyEvent) -> PickerOutcome {
        if key.kind == KeyEventKind::Release {
            return PickerOutcome::Ignored;
        }
        let plain = key.modifiers.difference(KeyModifiers::SHIFT).is_empty();
        let page = self.page.max(1) as isize;
        match key.code {
            KeyCode::Esc => return PickerOutcome::Close,
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
            _ => return PickerOutcome::Ignored,
        }
        PickerOutcome::Changed
    }

    /// Runs a clicked element's action.
    pub fn activate(&mut self, action: PickerAction) -> PickerOutcome {
        match action {
            PickerAction::Row(item) => {
                if !matches!(self.items.get(item), Some(Item::Entry(_))) {
                    return PickerOutcome::Ignored;
                }
                self.selected = Some(item);
                self.choose()
            }
        }
    }

    /// Mouse wheel: moves the selection by `lines` rows.
    pub fn scroll(&mut self, lines: i32) -> PickerOutcome {
        self.move_by(lines as isize);
        PickerOutcome::Changed
    }

    /// Width of the widest row plus borders and padding.
    fn natural_width(&self) -> usize {
        PREFIX + self.natural.iter().sum::<usize>() + GAP * (COLS - 1) + 2 * PAD as usize + 2
    }

    /// The dialog's rectangle inside the terminal `area`.
    pub fn dialog_rect(&self, area: Rect) -> Rect {
        let cap = if area.width > 2 * MARGIN + 10 {
            area.width - 2 * MARGIN
        } else {
            area.width
        };
        let natural = self.natural_width().max(MIN_WIDTH as usize);
        let w = (natural.min(cap as usize)) as u16;
        let h = dialog_height(area.height).min(area.height);
        Rect::new(
            area.x + (area.width - w) / 2,
            area.y + (area.height - h) / 2,
            w,
            h,
        )
    }

    /// Column widths for a content area `avail` cells wide: the natural
    /// widths, cut stage by stage ([`SHRINK`]) until the row fits.
    fn column_widths(&self, avail: usize) -> [usize; COLS] {
        let mut w = self.natural;
        for (col, floor) in SHRINK {
            let total = PREFIX + w.iter().sum::<usize>() + GAP * (COLS - 1);
            let excess = total.saturating_sub(avail);
            if excess == 0 {
                break;
            }
            w[col] -= excess.min(w[col].saturating_sub(floor));
        }
        w
    }

    fn row_line(
        &self,
        theme: &Theme,
        widths: &[usize; COLS],
        entry: usize,
        selected: bool,
    ) -> Line<'static> {
        let e = &self.catalog.entries()[entry];
        let row = &self.cells[entry];
        let base = if selected {
            theme.focused()
        } else {
            Style::new()
        };
        let dim = !e.usable();
        let current = self.is_current(e);
        let tok = |t: Token| -> Style {
            let t = if dim { Token::Muted } else { t };
            base.patch(theme.fg(t))
        };
        let mut spans = vec![
            Span::styled(if selected { "▸" } else { " " }, tok(Token::Accent)),
            Span::styled(if current { "●" } else { " " }, tok(Token::Ok)),
            Span::styled(" ", base),
        ];
        let tokens = [
            Token::Fg,
            Token::Agent,
            Token::Keyword,
            Token::Number,
            Token::Number,
            Token::Number,
            Token::Muted,
            Token::Muted,
            match e.availability {
                Availability::NoTools => Token::Warn,
                _ => Token::Muted,
            },
        ];
        for (c, cell) in row.iter().enumerate() {
            if c > 0 {
                spans.push(Span::styled(" ".repeat(GAP), base));
            }
            let mut st = tok(tokens[c]);
            if c == 0 && (selected || current) && !dim {
                st = st.add_modifier(Modifier::BOLD);
            }
            if dim && c == COLS - 1 {
                st = st.add_modifier(Modifier::ITALIC);
            }
            spans.push(Span::styled(pad(cell, widths[c], RIGHT[c]), st));
        }
        Line::from(spans)
    }

    fn header_line(&self, theme: &Theme, provider: &str, count: usize) -> Line<'static> {
        let mut spans = vec![
            Span::styled(
                provider.to_owned(),
                theme.fg(Token::Accent).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!(" · {count} model{}", if count == 1 { "" } else { "s" }),
                theme.fg(Token::Muted),
            ),
        ];
        if let Some(st) = self.catalog.provider_status(provider) {
            if let Some(err) = &st.error {
                spans.push(Span::styled(
                    format!(" · live list refresh failed: {err}"),
                    theme.fg(Token::Warn),
                ));
            } else if st.stale && !self.catalog.refreshing() {
                spans.push(Span::styled(
                    " · live list not current",
                    theme.fg(Token::Muted),
                ));
            }
        }
        Line::from(spans)
    }

    /// Draws the picker as a modal over the terminal `area` and registers
    /// its rows and list pane.
    pub fn render<A: Clone>(
        &mut self,
        frame: &mut Frame<'_>,
        area: Rect,
        theme: &Theme,
        hits: &mut HitMap<A>,
        wrap: &dyn Fn(PickerAction) -> A,
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
            .title(Span::styled(" Switch model ", theme.fg(Token::Accent)))
            .style(theme.base());
        let inner = block.inner(rect);
        frame.render_widget(block, rect);
        let inner = Rect {
            x: inner.x + PAD.min(inner.width / 2),
            width: inner.width.saturating_sub(2 * PAD),
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

        // Query line with the match count and the refresh indicator.
        let mut spans = vec![
            Span::styled("› ", theme.fg(Token::Accent)),
            Span::styled(self.query.clone(), theme.fg(Token::Fg)),
            Span::styled(
                format!(
                    "  {} of {} models",
                    self.matches,
                    self.catalog.entries().len()
                ),
                theme.fg(Token::Muted),
            ),
        ];
        if self.catalog.refreshing() {
            let used: usize = spans.iter().map(|s| width(&s.content)).sum();
            let gap = w.saturating_sub(used + width(REFRESHING)).max(2);
            spans.push(Span::raw(" ".repeat(gap)));
            spans.push(Span::styled(REFRESHING, theme.fg(Token::Running)));
        }
        line_at(frame, inner.y, Line::from(spans));
        let cx = inner.x + (2 + width(&self.query)).min(w.saturating_sub(1)) as u16;
        frame.set_cursor_position(Position::new(cx, inner.y));
        if inner.height < 3 {
            return;
        }

        // Column headers.
        let widths = self.column_widths(w);
        let mut head = String::from(&" ".repeat(PREFIX));
        for (c, name) in COLUMNS.iter().enumerate() {
            if c > 0 {
                head.push_str(&" ".repeat(GAP));
            }
            head.push_str(&pad(name, widths[c], RIGHT[c]));
        }
        line_at(
            frame,
            inner.y + 1,
            Line::from(Span::styled(
                head,
                theme.fg(Token::Muted).add_modifier(Modifier::BOLD),
            )),
        );

        // Bottom line: the notice after an unusable choice, else key hints.
        let hint_rows = u16::from(inner.height >= 5);
        if hint_rows == 1 {
            let line = match &self.notice {
                Some(n) => Line::from(Span::styled(n.clone(), theme.fg(Token::Warn))),
                None => Line::from(Span::styled(HINTS, theme.fg(Token::Muted))),
            };
            line_at(frame, inner.bottom() - 1, line);
        }

        let list = Rect::new(
            inner.x,
            inner.y + 2,
            inner.width,
            inner.height.saturating_sub(2 + hint_rows),
        );
        let rows = list.height as usize;
        self.page = rows.max(1);
        if rows == 0 {
            return;
        }
        hits.register_pane(LIST_PANE, list);
        if self.items.is_empty() {
            let msg = if self.catalog.entries().is_empty() {
                "no models: add an API key to a [provider.*] section of harness.ini".to_owned()
            } else {
                format!("no models match `{}`", self.query)
            };
            line_at(
                frame,
                list.y,
                Line::from(Span::styled(msg, theme.fg(Token::Muted))),
            );
            return;
        }
        if let Some(sel) = self.selected {
            if sel < self.scroll {
                self.scroll = sel;
            } else if sel >= self.scroll + rows {
                self.scroll = sel + 1 - rows;
            }
            // Keep a group's header visible above its first row.
            if sel > 0 && self.scroll == sel && matches!(self.items[sel - 1], Item::Header { .. }) {
                self.scroll = sel - 1;
            }
        }
        self.scroll = self.scroll.min(self.items.len().saturating_sub(rows));
        for (row, item) in self.items.iter().enumerate().skip(self.scroll).take(rows) {
            let y = list.y + (row - self.scroll) as u16;
            match item {
                Item::Header { provider, count } => {
                    line_at(frame, y, self.header_line(theme, provider, *count))
                }
                Item::Entry(i) => {
                    let selected = self.selected == Some(row);
                    line_at(frame, y, self.row_line(theme, &widths, *i, selected));
                    hits.register(
                        format!("{ROW_PREFIX}{row}"),
                        Rect::new(list.x, y, list.width, 1),
                        wrap(PickerAction::Row(row)),
                    );
                }
            }
        }
    }
}

#[cfg(test)]
pub(crate) mod tests;
