//! Text selection with the mouse and the keyboard, in every view.
//!
//! Mouse capture (needed for clickable elements) turns off the terminal's
//! own drag-to-select, so the TUI selects text itself. No view implements
//! selection: while a frame renders, views *register* the text they draw
//! as [`TextRegion`]s (a screen rectangle whose rows map back to logical
//! text), and the app's [`Selector`] does the rest from the rendered
//! buffer:
//!
//! - **Regions.** Every scrollable pane registered with
//!   [`crate::interaction::HitMap::register_pane`] is a region
//!   automatically. A view refines its region with [`update`]: the scroll
//!   offset (so a selection stays on its text while it scrolls), per-row
//!   soft-wrap information ([`Join`], so wrapped lines are rejoined) and how
//!   to strip chrome ([`Chrome`]). The app adds a fallback region under each
//!   screen so headers and other text are selectable too.
//! - **Masks.** Buttons ([`crate::interaction::HitMap::register_button`])
//!   and overlays (the palette, modals) call [`mask`]: their cells belong
//!   to no region below them, so button labels are never copied and text
//!   under an overlay is not selected through it. Anything registered
//!   later (a region or a mask) covers what was registered before.
//! - **Transient overlays.** Toasts come and go on their own (every copy
//!   shows one), so they must not change what a selection copies. They
//!   call [`overlay`] before drawing: a selection cannot start on them and
//!   they are not highlighted, but the text drawn under them is kept and
//!   copied as if they were not there.
//! - **Rows.** Each row of a region has an ordinal: its position in the
//!   view's content, not on the screen. Rows seen while a selection is
//!   active are cached by ordinal, so a selection can grow past the visible
//!   area while the view scrolls (drag past an edge, keyboard cursor) and
//!   still copy every line in between.
//!
//! Input ([`Selector::mouse`]) works against the last frame drawn (its
//! regions and its buffer), so it does not depend on a frame being drawn
//! between events: a press, drag and release delivered in one batch copies
//! the same text as the same gesture drawn step by step.
//!
//! - a press without movement stays a click (it is passed on unchanged),
//! - press, drag and release selects from the press to the release and
//!   copies on release; a drag from a border drawn around a nested region
//!   (a panel's list inside its modal) starts at the nearest cell of that
//!   region; dragging past the top or bottom edge of a
//!   scrollable region scrolls it ([`Scroll`], repeated on ticks while the
//!   pointer stays there),
//! - a double click selects a word, a triple click a logical line,
//! - a new press or Esc clears the selection.
//!
//! The keyboard mode ([`Selector::start_keyboard`], [`Selector::key`])
//! puts a cursor in the largest visible region: arrows, Home/End and
//! PgUp/PgDn move it, Shift extends the selection, Tab moves to the next
//! region, Enter or `y` copies and leaves, Esc leaves.
//!
//! The registry is a per-thread collector between [`begin_frame`] and
//! [`end_frame`]; registering outside a frame (view unit tests) does
//! nothing.

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::time::{Duration, Instant};

use crossterm::event::MouseEventKind;
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent};
use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};
use ratatui::style::{Modifier, Style};
use unicode_width::UnicodeWidthStr;

use crate::clipboard::{COPY_DIFF_LABEL, COPY_LABEL, COPY_OUTPUT_LABEL};

/// Two presses on the same cell within this time are a double click.
pub const MULTI_CLICK: Duration = Duration::from_millis(400);

/// How a row continues the row above it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Join {
    /// A new logical line.
    #[default]
    Hard,
    /// A soft wrap that replaced a space: rejoined with one space.
    Space,
    /// A soft wrap inside a word: rejoined directly.
    Direct,
}

/// One row of a region.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Row {
    /// Position of the row in the view's content (stable while it
    /// scrolls); rows of a region are in increasing order.
    pub ord: u64,
    /// How the row continues the one above it.
    pub join: Join,
}

/// What in a region's rows is decoration rather than text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Chrome {
    /// Every cell is text.
    #[default]
    Plain,
    /// Box-drawing characters (borders, connectors) are blank.
    Drawing,
    /// Transcript-style rows: card frames, `▌`/`┊`/`│` markers, status
    /// icons, line-number gutters, `⧉ copy` labels and the expand hints
    /// are not text; card bottoms and rules are no row at all.
    Smart,
}

/// A rectangle of rendered text and how its rows map to logical text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextRegion {
    /// Stable id (the pane id for panes), e.g. `chat` or `diff`.
    pub id: String,
    /// Screen rectangle of the text (without borders).
    pub rect: Rect,
    /// One entry per row of `rect`, top to bottom.
    pub rows: Vec<Row>,
    /// Whether row ordinals are content positions and the mouse wheel
    /// scrolls the region (dragging past an edge then scrolls it).
    pub scrolls: bool,
    /// Decoration to leave out.
    pub chrome: Chrome,
}

impl TextRegion {
    /// A region whose rows are hard lines numbered from 0.
    pub fn new(id: impl Into<String>, rect: Rect) -> TextRegion {
        TextRegion {
            id: id.into(),
            rect,
            rows: plain_rows(0, rect.height),
            scrolls: false,
            chrome: Chrome::Plain,
        }
    }

    /// Rows numbered from `first` (the scroll offset), and the region
    /// scrolls.
    pub fn scrolled(mut self, first: usize) -> TextRegion {
        self.set_scroll(first);
        self
    }

    /// Like [`TextRegion::scrolled`], in place.
    pub fn set_scroll(&mut self, first: usize) {
        self.rows = plain_rows(first as u64, self.rect.height);
        self.scrolls = true;
    }

    /// With this chrome mode.
    pub fn chrome(mut self, chrome: Chrome) -> TextRegion {
        self.chrome = chrome;
        self
    }

    /// Sets each row's [`Join`] (missing entries stay hard).
    pub fn set_joins(&mut self, joins: impl IntoIterator<Item = Join>) {
        for (row, join) in self.rows.iter_mut().zip(joins) {
            row.join = join;
        }
    }
}

fn plain_rows(first: u64, height: u16) -> Vec<Row> {
    (0..u64::from(height))
        .map(|i| Row {
            ord: first + i,
            join: Join::Hard,
        })
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Item {
    Region(TextRegion),
    Mask(Rect),
    /// A transient overlay and the symbols drawn under it (row-major).
    Overlay(Rect, Vec<String>),
}

impl Item {
    fn rect(&self) -> Rect {
        match self {
            Item::Region(r) => r.rect,
            Item::Mask(r) | Item::Overlay(r, _) => *r,
        }
    }
}

/// How a cell of a region is covered by what was registered after it.
enum Cover<'a> {
    /// Visible.
    No,
    /// Under a transient overlay: this symbol was drawn there before it.
    Overlay(&'a str),
    /// Under a mask, a region or a modal: not part of the region.
    Opaque,
}

/// Everything one frame registered, in registration order.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FrameText {
    items: Vec<Item>,
}

impl FrameText {
    /// Adds `region`; a region with the same id is replaced in place.
    pub fn register(&mut self, region: TextRegion) {
        if region.rect.width == 0 || region.rect.height == 0 {
            return;
        }
        match self.position(&region.id) {
            Some(i) => self.items[i] = Item::Region(region),
            None => self.items.push(Item::Region(region)),
        }
    }

    /// Covers `rect`: its cells belong to no region registered before.
    pub fn mask(&mut self, rect: Rect) {
        if rect.width > 0 && rect.height > 0 {
            self.items.push(Item::Mask(rect));
        }
    }

    /// Covers `rect` with a transient overlay (see [`overlay`]); `buf`
    /// holds what is drawn under it now.
    pub fn overlay(&mut self, rect: Rect, buf: &Buffer) {
        let rect = rect.intersection(buf.area);
        if rect.width == 0 || rect.height == 0 {
            return;
        }
        let under = (rect.top()..rect.bottom())
            .flat_map(|y| (rect.left()..rect.right()).map(move |x| (x, y)))
            .map(|(x, y)| {
                buf.cell(Position::new(x, y))
                    .map_or_else(String::new, |c| c.symbol().to_owned())
            })
            .collect();
        self.items.push(Item::Overlay(rect, under));
    }

    fn position(&self, id: &str) -> Option<usize> {
        self.items
            .iter()
            .rposition(|i| matches!(i, Item::Region(r) if r.id == id))
    }

    /// The region with id `id`.
    pub fn region(&self, id: &str) -> Option<&TextRegion> {
        self.position(id).map(|i| match &self.items[i] {
            Item::Region(r) => r,
            Item::Mask(_) | Item::Overlay(..) => unreachable!(),
        })
    }

    fn region_mut(&mut self, id: &str) -> Option<&mut TextRegion> {
        let i = self.position(id)?;
        match &mut self.items[i] {
            Item::Region(r) => Some(r),
            Item::Mask(_) | Item::Overlay(..) => None,
        }
    }

    /// Ids of the registered regions, bottom to top.
    pub fn region_ids(&self) -> Vec<&str> {
        self.items
            .iter()
            .filter_map(|i| match i {
                Item::Region(r) => Some(r.id.as_str()),
                Item::Mask(_) | Item::Overlay(..) => None,
            })
            .collect()
    }

    /// The topmost region whose text is at `(x, y)` (`None` on a mask or
    /// an overlay, e.g. a button or a toast, or outside every region).
    pub fn region_at(&self, x: u16, y: u16) -> Option<&TextRegion> {
        let p = Position::new(x, y);
        match self.items.iter().rev().find(|i| i.rect().contains(p))? {
            Item::Region(r) => Some(r),
            Item::Mask(_) | Item::Overlay(..) => None,
        }
    }

    /// Whether something registered after item `idx` covers `(x, y)`.
    fn covered(&self, idx: usize, x: u16, y: u16) -> bool {
        let p = Position::new(x, y);
        self.items[idx + 1..].iter().any(|i| i.rect().contains(p))
    }

    /// How what was registered after item `idx` covers `(x, y)`.
    fn cover(&self, idx: usize, x: u16, y: u16) -> Cover<'_> {
        let p = Position::new(x, y);
        let mut under = None;
        for item in &self.items[idx + 1..] {
            match item {
                Item::Overlay(r, cells) if r.contains(p) => {
                    // The first overlay saw the region's own text.
                    if under.is_none() {
                        let i = usize::from(y - r.y) * usize::from(r.width) + usize::from(x - r.x);
                        under = Some(cells.get(i).map_or("", String::as_str));
                    }
                }
                Item::Overlay(..) => {}
                other if other.rect().contains(p) => return Cover::Opaque,
                _ => {}
            }
        }
        under.map_or(Cover::No, Cover::Overlay)
    }

    /// Cells of region `id` not covered by later items.
    fn visible_cells(&self, id: &str) -> usize {
        let Some(idx) = self.position(id) else {
            return 0;
        };
        let r = self.items[idx].rect();
        (r.top()..r.bottom())
            .flat_map(|y| (r.left()..r.right()).map(move |x| (x, y)))
            .filter(|(x, y)| !self.covered(idx, *x, *y))
            .count()
    }

    /// The text of row `r` of region `id` as rendered in `buf`.
    fn capture(&self, id: &str, r: usize, buf: &Buffer) -> Option<CachedRow> {
        let idx = self.position(id)?;
        let Item::Region(region) = &self.items[idx] else {
            return None;
        };
        let row = region.rows.get(r)?;
        let y = region.rect.y + r as u16;
        let mut cells = Vec::with_capacity(region.rect.width as usize);
        let mut wide = false;
        for x in region.rect.left()..region.rect.right() {
            if wide {
                // The second half of a wide character.
                wide = false;
                cells.push(Some(String::new()));
                continue;
            }
            let symbol = match self.cover(idx, x, y) {
                Cover::No => buf.cell(Position::new(x, y)).map(|c| c.symbol()),
                Cover::Overlay(s) => Some(s),
                Cover::Opaque => None,
            };
            match symbol {
                Some(s) => {
                    wide = s.width() > 1;
                    cells.push(Some(s.to_owned()));
                }
                None => cells.push(None),
            }
        }
        let mut skip = false;
        match region.chrome {
            Chrome::Plain => {}
            Chrome::Drawing => {
                // Borders go, and so does a status icon that leads a label
                // (`│ ✔ Coder │` in a map node): the first visible cell
                // after a border or the row start, followed by a blank.
                let mut leading = true;
                for i in 0..cells.len() {
                    let Some(c) = cells[i].as_deref() else {
                        continue;
                    };
                    let first = c.chars().next();
                    if first.is_some_and(is_box_drawing) {
                        cells[i] = Some(" ".into());
                        leading = true;
                    } else if c.trim().is_empty() {
                    } else {
                        let next_blank = cells
                            .get(i + 1)
                            .is_none_or(|n| n.as_deref().is_none_or(|n| n.trim().is_empty()));
                        if leading
                            && c.chars().count() == 1
                            && first.is_some_and(is_icon)
                            && next_blank
                        {
                            cells[i] = Some(" ".into());
                        }
                        leading = false;
                    }
                }
            }
            Chrome::Smart => skip = !smart_mask(&mut cells),
        }
        Some(CachedRow {
            cells,
            join: row.join,
            skip,
            trim_start: region.chrome == Chrome::Drawing,
        })
    }

    /// For a press on a border drawn around a region nested in region
    /// `id` (a panel's or picker's list inside its modal, a pane inside
    /// the screen), that inner region and its text cell nearest to
    /// `(x, y)`: so a drag from a border selects the text it frames, as a
    /// drag from a chat card's border does. A side border (`│`) frames
    /// the text on its row, a top or bottom border (`─`) the text in its
    /// column, a corner or junction the text diagonally inside it.
    /// `None` when `(x, y)` is no border or frames no nested region.
    fn inside_border(
        &self,
        id: &str,
        x: u16,
        y: u16,
        buf: &Buffer,
    ) -> Option<(String, (u16, u16))> {
        let c = buf.cell(Position::new(x, y))?.symbol().chars().next()?;
        if !is_box_drawing(c) {
            return None;
        }
        // How far from the border the text may start (a cell of padding).
        let (reach_x, reach_y) = match c {
            '│' | '┃' | '║' | '▌' | '▎' | '▏' | '▕' | '▐' => (2, 0),
            '─' | '━' | '═' => (0, 1),
            _ => (2, 1),
        };
        let outer_idx = self.position(id)?;
        let outer = self.items[outer_idx].rect();
        let gap = |p: u16, lo: u16, hi: u16| -> u16 {
            if p < lo {
                lo - p
            } else if p >= hi {
                p - hi + 1
            } else {
                0
            }
        };
        let mut best: Option<(u16, usize)> = None;
        for (idx, item) in self.items.iter().enumerate().skip(outer_idx + 1) {
            let Item::Region(r) = item else { continue };
            if r.rect.intersection(outer) != r.rect {
                continue;
            }
            let (dx, dy) = (
                gap(x, r.rect.left(), r.rect.right()),
                gap(y, r.rect.top(), r.rect.bottom()),
            );
            if dx > reach_x || dy > reach_y || (dx == 0 && dy == 0) {
                continue;
            }
            // The nearest cell must be the region's own, not covered.
            let cx = x.clamp(r.rect.left(), r.rect.right() - 1);
            let cy = y.clamp(r.rect.top(), r.rect.bottom() - 1);
            if !matches!(self.cover(idx, cx, cy), Cover::No | Cover::Overlay(_)) {
                continue;
            }
            let d = dx + dy;
            if best.is_none_or(|(bd, _)| d <= bd) {
                best = Some((d, idx));
            }
        }
        let (_, idx) = best?;
        let r = self.items[idx].rect();
        let Item::Region(region) = &self.items[idx] else {
            return None;
        };
        Some((
            region.id.clone(),
            (
                x.clamp(r.left(), r.right() - 1),
                y.clamp(r.top(), r.bottom() - 1),
            ),
        ))
    }
}

thread_local! {
    static COLLECTOR: RefCell<Option<FrameText>> = const { RefCell::new(None) };
}

fn with_frame(f: impl FnOnce(&mut FrameText)) {
    COLLECTOR.with(|c| {
        if let Some(ft) = c.borrow_mut().as_mut() {
            f(ft);
        }
    });
}

/// Starts collecting the regions of a frame on this thread.
pub fn begin_frame() {
    COLLECTOR.with(|c| *c.borrow_mut() = Some(FrameText::default()));
}

/// Stops collecting and returns what the frame registered.
pub fn end_frame() -> FrameText {
    COLLECTOR
        .with(|c| c.borrow_mut().take())
        .unwrap_or_default()
}

/// Registers a text region in the frame being rendered.
pub fn register(region: TextRegion) {
    with_frame(|ft| ft.register(region));
}

/// Changes the region `id` registered earlier in this frame (does nothing
/// when there is none).
pub fn update(id: &str, f: impl FnOnce(&mut TextRegion)) {
    with_frame(|ft| {
        if let Some(r) = ft.region_mut(id) {
            f(r);
        }
    });
}

/// Covers `rect` in the frame being rendered (a button label or an
/// overlay): its cells are not part of any region registered before.
pub fn mask(rect: Rect) {
    with_frame(|ft| ft.mask(rect));
}

/// Covers `rect` with a transient overlay (a toast) in the frame being
/// rendered. Call it before drawing the overlay into `buf`: a selection
/// cannot start on it and it is not highlighted, but the text under it
/// stays part of the regions below (it is copied as if the overlay were
/// not there).
pub fn overlay(rect: Rect, buf: &Buffer) {
    with_frame(|ft| ft.overlay(rect, buf));
}

/// Box drawing (U+2500–U+257F) and the block elements used as bars.
fn is_box_drawing(c: char) -> bool {
    ('\u{2500}'..='\u{257f}').contains(&c) || matches!(c, '▌' | '▎' | '▏' | '▕' | '▐')
}

/// Status icons that start transcript rows.
fn is_icon(c: char) -> bool {
    ('\u{2800}'..='\u{28ff}').contains(&c)
        || matches!(
            c,
            '◆' | '✔'
                | '✗'
                | '⊘'
                | '○'
                | '●'
                | '◐'
                | '⏸'
                | '⚠'
                | 'ℹ'
                | '✖'
                | '▸'
                | '▾'
                | '↻'
        )
}

/// Masks transcript chrome in `cells` (see [`Chrome::Smart`]). Returns
/// false when the row is only decoration.
fn smart_mask(cells: &mut [Option<String>]) -> bool {
    let ch = |cells: &[Option<String>], i: usize| -> char {
        cells
            .get(i)
            .and_then(|c| c.as_deref())
            .and_then(|s| s.chars().next())
            .unwrap_or(' ')
    };
    let n = cells.len();
    let find = |cells: &[Option<String>], pat: &str| -> Option<usize> {
        let p: Vec<char> = pat.chars().collect();
        (0..n.saturating_sub(p.len() - 1)).find(|&i| (0..p.len()).all(|k| ch(cells, i + k) == p[k]))
    };
    // A masked button (or a covering overlay) after the text: what follows
    // it on the row is the header's metadata (`⧉ copy · 14:32`).
    if let Some(start) = cells
        .iter()
        .position(|c| c.as_deref().is_some_and(|s| !s.trim().is_empty()))
        && let Some(gap) = (start..n).find(|&i| cells[i].is_none())
    {
        for c in &mut cells[gap..] {
            *c = None;
        }
    }
    // Button labels.
    for label in [COPY_OUTPUT_LABEL, COPY_DIFF_LABEL] {
        while let Some(i) = find(cells, label) {
            for c in &mut cells[i..i + label.chars().count()] {
                *c = None;
            }
        }
    }
    // A header's copy button and the timestamp after it.
    if let Some(i) = find(cells, COPY_LABEL) {
        for c in &mut cells[i..] {
            *c = None;
        }
    }
    let text: String = (0..n).map(|i| ch(cells, i)).collect();
    let t = text.trim();
    if t.ends_with("Enter to expand") || t == "Enter to collapse" {
        return false;
    }
    let decoration = |c: char| matches!(c, '─' | '╭' | '╮' | '╰' | '╯' | '│' | ' ' | '·');
    if t.chars().all(decoration) && !t.is_empty() {
        return false;
    }
    let first = |cells: &[Option<String>]| (0..n).find(|&i| ch(cells, i) != ' ');
    let blank = |cells: &mut [Option<String>], a: usize, b: usize| {
        for c in &mut cells[a..b.min(n)] {
            *c = None;
        }
    };
    // Card frame: `╭ header ─── right ╮`, `│ body │`, `╰───╯`.
    if let Some(p) = first(cells)
        && matches!(ch(cells, p), '╭' | '│' | '╰')
        && ch(cells, p + 1) == ' '
    {
        let header = ch(cells, p) == '╭';
        blank(cells, p, p + 2);
        if let Some(q) = (0..n).rev().find(|&i| ch(cells, i) != ' ')
            && matches!(ch(cells, q), '╮' | '│' | '╯')
        {
            if header && ch(cells, q) == '╮' {
                // The header rule between the title and the right-hand
                // metadata: the last run of `─` before a blank. A title
                // truncated to fit leaves it only a cell or two wide.
                let end = (p + 2..q)
                    .rev()
                    .find(|&i| ch(cells, i) == '─' && matches!(ch(cells, i + 1), ' ' | '╮'));
                if let Some(end) = end {
                    let mut start = end;
                    while start > p + 2 && ch(cells, start - 1) == '─' {
                        start -= 1;
                    }
                    blank(cells, start, end + 1);
                }
            }
            blank(cells, q, q + 1);
        }
    }
    // Runs of the border fill between a header's parts.
    let mut i = 0;
    while i < n {
        if ch(cells, i) == '─' {
            let start = i;
            while i < n && ch(cells, i) == '─' {
                i += 1;
            }
            if i - start >= 3 {
                blank(cells, start, i);
            }
        } else {
            i += 1;
        }
    }
    if let Some(p) = first(cells) {
        let (c0, c1) = (ch(cells, p), ch(cells, p + 1));
        if matches!(c0, '▌' | '┊' | '▎' | '│') && c1 == ' ' {
            // Message and block markers.
            blank(cells, p, p + 2);
        } else if is_icon(c0) && c1 == ' ' {
            blank(cells, p, p + 2);
        } else if let Some(bar) = (p..(p + 9).min(n)).find(|&i| ch(cells, i) == '│') {
            // Line-number gutters: `  12 │ text`, `   6 │-text`, `     │+x`.
            let before: String = (0..bar).map(|i| ch(cells, i)).collect();
            let digits = before.trim().chars().all(|c| c.is_ascii_digit());
            if bar >= 4 && digits && before.ends_with(' ') {
                let end = if ch(cells, bar + 1) == ' ' {
                    bar + 2
                } else {
                    bar + 1
                };
                blank(cells, 0, end);
            }
        }
    }
    true
}

/// One captured row of a region.
#[derive(Debug, Clone, PartialEq, Eq)]
struct CachedRow {
    /// One entry per column: the symbol, `""` for the second half of a
    /// wide character, `None` for chrome and covered cells.
    cells: Vec<Option<String>>,
    join: Join,
    /// Only decoration: not part of the text.
    skip: bool,
    /// Leading blanks are layout, not text (drawn boxes).
    trim_start: bool,
}

impl CachedRow {
    /// Text of columns `a..=b`.
    fn text(&self, a: usize, b: usize) -> String {
        self.cells
            .iter()
            .take(b.saturating_add(1))
            .skip(a)
            .flatten()
            .map(String::as_str)
            .collect()
    }

    /// The last column holding a visible character.
    fn last_content(&self) -> Option<usize> {
        self.cells
            .iter()
            .rposition(|c| c.as_deref().is_some_and(|s| !s.trim().is_empty()))
    }

    fn is_word(&self, i: usize) -> bool {
        self.cells
            .get(i)
            .and_then(|c| c.as_deref())
            .and_then(|s| s.chars().next())
            .is_some_and(|c| !c.is_whitespace() && !"\"'`()[]{}<>,;|".contains(c))
    }
}

/// A position in a region: row ordinal and column.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Point {
    /// Row ordinal.
    pub ord: u64,
    /// Column inside the region.
    pub col: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// Press and drag.
    Drag,
    /// Double or triple click.
    Clicks,
    /// The keyboard cursor.
    Keys,
}

/// The current selection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Selection {
    /// Region id.
    pub region: String,
    /// Where it started.
    pub anchor: Point,
    /// Where it ends (the cursor in keyboard mode).
    pub head: Point,
    kind: Kind,
    /// The mouse button is still down.
    active: bool,
}

impl Selection {
    /// `(start, end)` in content order, both inclusive.
    pub fn range(&self) -> (Point, Point) {
        if self.anchor <= self.head {
            (self.anchor, self.head)
        } else {
            (self.head, self.anchor)
        }
    }
}

/// A scroll the app should perform: a wheel notch at `(x, y)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Scroll {
    /// Column inside the region.
    pub x: u16,
    /// Row inside the region.
    pub y: u16,
    /// Up (towards the start) or down.
    pub up: bool,
}

/// What a mouse event did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MouseOutcome {
    /// Not a selection gesture: handle the event as usual.
    Pass,
    /// Used by the selection; nothing else should see it.
    Consumed,
    /// A selection finished: copy this text.
    Copy(String),
}

/// What a key did in keyboard selection mode.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyOutcome {
    /// Not a selection key.
    Ignored,
    /// The cursor or selection changed.
    Changed,
    /// Scroll, then the cursor moves on.
    Scroll(Scroll),
    /// Copy this text; the mode ended.
    Copy(String),
    /// The mode ended without copying.
    Exit,
}

#[derive(Debug, Clone)]
struct Press {
    x: u16,
    y: u16,
    /// The region a drag from here selects in.
    region: Option<String>,
    /// Where in it the drag starts: the press, or the nearest text cell
    /// when the press is on a border around the region.
    from: (u16, u16),
}

#[derive(Debug, Clone, Copy)]
struct Clicks {
    x: u16,
    y: u16,
    at: Instant,
    count: u8,
}

/// The selection state of the app: the last frame's regions, the current
/// selection and the rows seen while it is active.
#[derive(Debug, Clone, Default)]
pub struct Selector {
    frame: FrameText,
    /// The last frame as drawn (before the highlight), so mouse events
    /// can read its text without waiting for the next frame.
    buf: Buffer,
    screen: Rect,
    cache: HashMap<String, BTreeMap<u64, CachedRow>>,
    sel: Option<Selection>,
    press: Option<Press>,
    clicks: Option<Clicks>,
    pointer: Option<(u16, u16)>,
    edge: Option<Scroll>,
    /// Rows the keyboard cursor still has to move after a scroll.
    pending_rows: i32,
    /// Keyboard mode starts on the next frame (the one drawn after the
    /// palette or key that asked for it closed).
    pending_start: bool,
}

impl Selector {
    /// Nothing selected.
    pub fn new() -> Selector {
        Selector::default()
    }

    /// The regions of the last frame.
    pub fn frame(&self) -> &FrameText {
        &self.frame
    }

    /// The current selection.
    pub fn selection(&self) -> Option<&Selection> {
        self.sel.as_ref()
    }

    /// Whether keyboard selection mode is on.
    pub fn keyboard(&self) -> bool {
        self.pending_start || self.sel.as_ref().is_some_and(|s| s.kind == Kind::Keys)
    }

    /// Whether a drag is held past a region's edge (the app scrolls on
    /// every tick then).
    pub fn autoscroll(&self) -> Option<Scroll> {
        self.sel.as_ref().filter(|s| s.active)?;
        self.edge
    }

    /// Clears the selection. Returns whether there was one.
    pub fn clear(&mut self) -> bool {
        self.press = None;
        self.edge = None;
        self.pointer = None;
        self.pending_rows = 0;
        self.pending_start = false;
        self.cache.clear();
        self.sel.take().is_some()
    }

    /// The selected text, rejoined: soft-wrapped rows become one line,
    /// chrome is left out, trailing spaces and blank edge lines trimmed.
    pub fn text(&self) -> String {
        let Some(sel) = &self.sel else {
            return String::new();
        };
        let (lo, hi) = sel.range();
        let Some(rows) = self.cache.get(&sel.region) else {
            return String::new();
        };
        let mut out = String::new();
        let mut first = true;
        for (ord, row) in rows.range(lo.ord..=hi.ord) {
            if row.skip {
                continue;
            }
            let a = if *ord == lo.ord { lo.col as usize } else { 0 };
            let b = if *ord == hi.ord {
                hi.col as usize
            } else {
                usize::MAX
            };
            let piece = row.text(a, b);
            let piece = if row.trim_start {
                piece.trim()
            } else {
                piece.trim_end()
            };
            if first {
                out.push_str(piece);
                first = false;
                continue;
            }
            match row.join {
                Join::Hard => {
                    out.push('\n');
                    out.push_str(piece);
                }
                Join::Space => {
                    let p = piece.trim_start();
                    if !out.is_empty() && !out.ends_with([' ', '\n']) && !p.is_empty() {
                        out.push(' ');
                    }
                    out.push_str(p);
                }
                Join::Direct => out.push_str(piece.trim_start()),
            }
        }
        let mut lines: Vec<&str> = out.split('\n').collect();
        // Indentation every line shares is layout (nested log entries,
        // padded panes), not text. A first line cut mid-row has none.
        let from = usize::from(lo.col > 0);
        let indent = lines[from.min(lines.len())..]
            .iter()
            .filter(|l| !l.trim().is_empty())
            .map(|l| l.len() - l.trim_start_matches(' ').len())
            .min()
            .unwrap_or(0);
        for l in lines.iter_mut().skip(from) {
            *l = l.get(indent..).unwrap_or("");
        }
        let start = lines.iter().position(|l| !l.trim().is_empty());
        let end = lines.iter().rposition(|l| !l.trim().is_empty());
        match (start, end) {
            (Some(s), Some(e)) => lines[s..=e].join("\n"),
            _ => String::new(),
        }
    }

    /// The point of region `id` under `(x, y)`, clamped into it (above it
    /// is its start, below it its end).
    fn resolve(&self, id: &str, x: u16, y: u16) -> Option<Point> {
        let r = self.frame.region(id)?;
        let last = r.rows.len().checked_sub(1)?;
        let w = r.rect.width.saturating_sub(1);
        let (row, col) = if y < r.rect.y {
            (0, 0)
        } else if y >= r.rect.y + r.rows.len() as u16 {
            (last, w)
        } else {
            (usize::from(y - r.rect.y), x.saturating_sub(r.rect.x).min(w))
        };
        Some(Point {
            ord: r.rows[row].ord,
            col,
        })
    }

    /// Fills the row cache with the rows of `id` in the last frame.
    fn capture_last(&mut self, id: &str) {
        let buf = std::mem::take(&mut self.buf);
        self.capture(id, &buf);
        self.buf = buf;
    }

    /// Fills the row cache with the visible rows of `id`.
    fn capture(&mut self, id: &str, buf: &Buffer) {
        let Some(r) = self.frame.region(id) else {
            return;
        };
        let ords: Vec<u64> = r.rows.iter().map(|row| row.ord).collect();
        let rows = self.cache.entry(id.to_owned()).or_default();
        for (i, ord) in ords.into_iter().enumerate() {
            if let Some(row) = self.frame.capture(id, i, buf) {
                rows.insert(ord, row);
            }
        }
    }

    /// Selects the word at `(x, y)` (`line`: the logical line) in the
    /// last frame.
    fn select_at(&mut self, id: &str, x: u16, y: u16, line: bool) -> bool {
        let Some(r) = self.frame.region(id).cloned() else {
            return false;
        };
        let Some(i) = y.checked_sub(r.rect.y).map(usize::from) else {
            return false;
        };
        if i >= r.rows.len() {
            return false;
        }
        self.cache.clear();
        self.capture_last(id);
        let rows = &self.cache[id];
        let Some(row) = rows.get(&r.rows[i].ord) else {
            return false;
        };
        let col = usize::from(x.saturating_sub(r.rect.x));
        let (anchor, head) = if line {
            let mut a = i;
            while a > 0 && r.rows[a].join != Join::Hard {
                a -= 1;
            }
            let mut b = i;
            while b + 1 < r.rows.len() && r.rows[b + 1].join != Join::Hard {
                b += 1;
            }
            (
                Point {
                    ord: r.rows[a].ord,
                    col: 0,
                },
                Point {
                    ord: r.rows[b].ord,
                    col: r.rect.width.saturating_sub(1),
                },
            )
        } else {
            if !row.is_word(col) {
                return false;
            }
            let mut a = col;
            while a > 0 && row.is_word(a - 1) {
                a -= 1;
            }
            let mut b = col;
            while row.is_word(b + 1)
                || row
                    .cells
                    .get(b + 1)
                    .is_some_and(|c| c.as_deref() == Some(""))
            {
                b += 1;
            }
            let ord = r.rows[i].ord;
            (Point { ord, col: a as u16 }, Point { ord, col: b as u16 })
        };
        self.sel = Some(Selection {
            region: id.to_owned(),
            anchor,
            head,
            kind: Kind::Clicks,
            active: true,
        });
        true
    }

    /// Handles a mouse event at time `now` against the last frame. Returns
    /// what to do with the event and a scroll to perform (dragging past an
    /// edge).
    pub fn mouse(&mut self, ev: &MouseEvent, now: Instant) -> (MouseOutcome, Option<Scroll>) {
        let (x, y) = (ev.column, ev.row);
        match ev.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                self.clear();
                let region = self.frame.region_at(x, y).map(|r| r.id.clone());
                let count = match self.clicks {
                    Some(c)
                        if c.x == x
                            && c.y == y
                            && now.saturating_duration_since(c.at) <= MULTI_CLICK =>
                    {
                        if c.count >= 3 { 1 } else { c.count + 1 }
                    }
                    _ => 1,
                };
                self.clicks = Some(Clicks {
                    x,
                    y,
                    at: now,
                    count,
                });
                if count >= 2
                    && let Some(id) = &region
                {
                    self.select_at(id, x, y, count == 3);
                }
                let (region, from) = match region {
                    Some(id) => match self.frame.inside_border(&id, x, y, &self.buf) {
                        Some((inner, at)) => (Some(inner), at),
                        None => (Some(id), (x, y)),
                    },
                    None => (None, (x, y)),
                };
                self.press = Some(Press { x, y, region, from });
                (MouseOutcome::Pass, None)
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                let Some(Press {
                    x: px,
                    y: py,
                    region: Some(id),
                    from,
                }) = self.press.clone()
                else {
                    return (MouseOutcome::Consumed, None);
                };
                self.pointer = Some((x, y));
                let dragging = self.sel.as_ref().is_some_and(|s| s.kind == Kind::Drag);
                if !dragging {
                    // The first frame of a selection must show the row it
                    // starts on, so scrolling waits for the next event.
                    if (x, y) == (px, py) {
                        return (MouseOutcome::Consumed, None);
                    }
                    let Some(anchor) = self.resolve(&id, from.0, from.1) else {
                        return (MouseOutcome::Consumed, None);
                    };
                    self.cache.clear();
                    // The rows the drag starts on, as the user saw them.
                    self.capture_last(&id);
                    self.sel = Some(Selection {
                        region: id.clone(),
                        anchor,
                        head: anchor,
                        kind: Kind::Drag,
                        active: true,
                    });
                }
                if let Some(head) = self.resolve(&id, x, y)
                    && let Some(s) = &mut self.sel
                {
                    s.head = head;
                }
                self.edge = self.edge_scroll(&id, y);
                let scroll = if dragging { self.edge } else { None };
                (MouseOutcome::Consumed, scroll)
            }
            MouseEventKind::Up(MouseButton::Left) => {
                self.press = None;
                self.pointer = None;
                self.edge = None;
                match &mut self.sel {
                    Some(s) if s.active => {
                        s.active = false;
                        // Rows shown since the last capture (none when the
                        // gesture came in one batch, without a frame).
                        let id = s.region.clone();
                        self.capture_last(&id);
                        let text = self.text();
                        if text.is_empty() {
                            (MouseOutcome::Consumed, None)
                        } else {
                            (MouseOutcome::Copy(text), None)
                        }
                    }
                    _ => (MouseOutcome::Pass, None),
                }
            }
            _ => (MouseOutcome::Pass, None),
        }
    }

    /// The scroll for a pointer at row `y` dragging in region `id`: past
    /// (or on, at the screen's edge) its top or bottom row.
    fn edge_scroll(&self, id: &str, y: u16) -> Option<Scroll> {
        let r = self.frame.region(id).filter(|r| r.scrolls)?;
        let rect = r.rect;
        let top = y < rect.y || (y == rect.y && rect.y == self.screen.y);
        let bottom =
            y >= rect.bottom() || (y + 1 == rect.bottom() && rect.bottom() == self.screen.bottom());
        (top || bottom).then_some(Scroll {
            x: rect.x,
            y: rect.y,
            up: top,
        })
    }

    /// Starts keyboard selection: on the next frame, the cursor goes to the
    /// top of the largest visible region. Returns false when the last frame
    /// had no text at all.
    pub fn start_keyboard(&mut self) -> bool {
        if self.frame.region_ids().is_empty() {
            return false;
        }
        self.clear();
        self.pending_start = true;
        true
    }

    /// Puts the keyboard cursor in the largest visible region.
    fn start_in_frame(&mut self) -> bool {
        let best = self
            .frame
            .region_ids()
            .into_iter()
            .map(|id| (self.frame.visible_cells(id), id.to_owned()))
            .filter(|(n, _)| *n > 0)
            .max_by_key(|(n, _)| *n)
            .map(|(_, id)| id);
        match best {
            Some(id) => self.cursor_to(&id),
            None => false,
        }
    }

    fn cursor_to(&mut self, id: &str) -> bool {
        let Some(r) = self.frame.region(id) else {
            return false;
        };
        let Some(first) = r.rows.first() else {
            return false;
        };
        let p = Point {
            ord: first.ord,
            col: 0,
        };
        self.clear();
        self.sel = Some(Selection {
            region: id.to_owned(),
            anchor: p,
            head: p,
            kind: Kind::Keys,
            active: false,
        });
        true
    }

    /// Handles a key in keyboard selection mode.
    pub fn key(&mut self, k: &KeyEvent) -> KeyOutcome {
        if k.kind == KeyEventKind::Release || !self.keyboard() {
            return KeyOutcome::Ignored;
        }
        let shift = k.modifiers.contains(KeyModifiers::SHIFT);
        let plain = k.modifiers.difference(KeyModifiers::SHIFT).is_empty();
        if !plain {
            return KeyOutcome::Ignored;
        }
        let Some(sel) = self.sel.clone() else {
            return KeyOutcome::Ignored;
        };
        let Some(r) = self.frame.region(&sel.region).cloned() else {
            self.clear();
            return KeyOutcome::Exit;
        };
        let w = r.rect.width.saturating_sub(1);
        let idx = r.rows.iter().position(|row| row.ord == sel.head.ord);
        let mut head = sel.head;
        let mut scroll = None;
        let mut rows = 0i32;
        match k.code {
            KeyCode::Esc | KeyCode::Char('q') => {
                self.clear();
                return KeyOutcome::Exit;
            }
            KeyCode::Enter | KeyCode::Char('y') => {
                let text = if sel.anchor == sel.head {
                    // Nothing extended: the line under the cursor.
                    self.sel = Some(Selection {
                        anchor: Point { col: 0, ..sel.head },
                        head: Point { col: w, ..sel.head },
                        ..sel
                    });
                    self.text()
                } else {
                    self.text()
                };
                self.clear();
                return KeyOutcome::Copy(text);
            }
            KeyCode::Tab | KeyCode::BackTab => {
                let ids = self.frame.region_ids();
                let pos = ids.iter().position(|i| *i == sel.region).unwrap_or(0);
                let n = ids.len();
                let next = if k.code == KeyCode::Tab {
                    (pos + 1) % n
                } else {
                    (pos + n - 1) % n
                };
                let id = ids[next].to_owned();
                self.cursor_to(&id);
                return KeyOutcome::Changed;
            }
            KeyCode::Left => head.col = head.col.saturating_sub(1),
            KeyCode::Right => head.col = (head.col + 1).min(w),
            KeyCode::Home => head.col = 0,
            KeyCode::End => {
                head.col = self
                    .cache
                    .get(&sel.region)
                    .and_then(|c| c.get(&head.ord))
                    .and_then(CachedRow::last_content)
                    .map_or(w, |c| c as u16)
            }
            KeyCode::Up => rows = -1,
            KeyCode::Down => rows = 1,
            KeyCode::PageUp => rows = -(r.rows.len() as i32 - 1).max(1),
            KeyCode::PageDown => rows = (r.rows.len() as i32 - 1).max(1),
            _ => return KeyOutcome::Ignored,
        }
        if rows != 0 {
            let i = idx.unwrap_or(0) as i32;
            let target = i + rows;
            let last = r.rows.len() as i32 - 1;
            if (target < 0 || target > last) && r.scrolls {
                scroll = Some(Scroll {
                    x: r.rect.x,
                    y: r.rect.y,
                    up: target < 0,
                });
                self.pending_rows = if target < 0 { target } else { target - last };
            }
            head.ord = r.rows[target.clamp(0, last.max(0)) as usize].ord;
        }
        if let Some(s) = &mut self.sel {
            s.head = head;
            if !shift {
                s.anchor = head;
            }
        }
        match scroll {
            Some(s) => KeyOutcome::Scroll(s),
            None => KeyOutcome::Changed,
        }
    }

    /// Finishes a frame: takes the regions it registered, follows the
    /// pointer or the pending cursor move, caches the visible rows of the
    /// selected region and paints the selection into `buf` with `style`
    /// (the keyboard cursor with `cursor`). A selection whose region is
    /// gone is cleared.
    pub fn end_frame(&mut self, frame: FrameText, buf: &mut Buffer, style: Style, cursor: Style) {
        self.frame = frame;
        self.screen = buf.area;
        self.buf.clone_from(buf);
        if std::mem::take(&mut self.pending_start) {
            self.start_in_frame();
        }
        let Some(sel) = self.sel.clone() else {
            return;
        };
        let Some(region) = self.frame.region(&sel.region).cloned() else {
            self.clear();
            return;
        };
        if sel.kind == Kind::Drag
            && sel.active
            && let Some((x, y)) = self.pointer
            && let Some(head) = self.resolve(&sel.region, x, y)
            && let Some(s) = &mut self.sel
        {
            s.head = head;
        }
        if self.pending_rows != 0 {
            let idx = region.rows.iter().position(|r| r.ord == sel.head.ord);
            if let Some(i) = idx {
                let last = region.rows.len() as i32 - 1;
                let t = (i as i32 + self.pending_rows).clamp(0, last.max(0)) as usize;
                if let Some(s) = &mut self.sel {
                    let shift = s.anchor != s.head;
                    s.head.ord = region.rows[t].ord;
                    if !shift {
                        s.anchor = s.head;
                    }
                }
            }
            self.pending_rows = 0;
        }
        self.capture(&sel.region, buf);
        let Some(sel) = self.sel.clone() else {
            return;
        };
        let (lo, hi) = sel.range();
        let cache = &self.cache[&sel.region];
        let idx = self.frame.position(&sel.region);
        for (i, row) in region.rows.iter().enumerate() {
            if row.ord < lo.ord || row.ord > hi.ord {
                continue;
            }
            let Some(c) = cache.get(&row.ord) else {
                continue;
            };
            if c.skip {
                continue;
            }
            let a = if row.ord == lo.ord {
                lo.col as usize
            } else {
                0
            };
            let mut b = if row.ord == hi.ord {
                hi.col as usize
            } else {
                usize::MAX
            };
            match c.last_content() {
                Some(last) => b = b.min(last),
                None => continue,
            }
            let y = region.rect.y + i as u16;
            for col in a..=b {
                let x = region.rect.x + col as u16;
                // Overlays (toasts) on top are not highlighted.
                let on_top = idx.is_some_and(|idx| self.frame.covered(idx, x, y));
                if c.cells.get(col).is_some_and(Option::is_some)
                    && !on_top
                    && let Some(cell) = buf.cell_mut(Position::new(x, y))
                {
                    cell.set_style(style);
                }
            }
        }
        if sel.kind == Kind::Keys
            && let Some(i) = region.rows.iter().position(|r| r.ord == sel.head.ord)
            && let Some(cell) = buf.cell_mut(Position::new(
                region.rect.x + sel.head.col,
                region.rect.y + i as u16,
            ))
        {
            cell.set_style(cursor.add_modifier(Modifier::REVERSED | Modifier::UNDERLINED));
        }
    }
}

#[cfg(test)]
mod tests;
