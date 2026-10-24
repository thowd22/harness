//! The `/memory` browser: files, search, recent writes and dream history
//! with diffs, plus controls.
//!
//! The browser is a modal over the chat. It holds no I/O: the core fills it
//! with a [`MemoryData`] snapshot and answers [`BrowserOutcome`]s (load a
//! note preview or a diff, run a search, undo a commit, open `$EDITOR`,
//! toggle memory, run a dream pass) by calling [`MemoryBrowser::set_detail`],
//! [`MemoryBrowser::set_search_results`] or [`MemoryBrowser::set_data`].
//!
//! Every control is a button registered in the [`HitMap`]: clickable,
//! reachable with Tab/Shift+Tab and activated with Enter. Left/Right switch
//! tabs, Up/Down move the selection, typing edits the search query (Search
//! tab) and Esc closes the browser.
//!
//! Nothing is cut: tabs, rows, the search box and the buttons wrap onto
//! more rows when narrow, the drive path moves out of the title when the
//! border cannot hold it, and the detail pane wraps and scrolls
//! (PageUp/PageDown, Home/End or the wheel), showing its position in the bottom
//! border.

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};
use time::OffsetDateTime;

use crate::chat::text::{sanitize, width, wrap_text};
use crate::interaction::{ElementId, HitMap};
use crate::theme::{Theme, Token};

/// A tab of the browser.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Tab {
    /// Every note.
    Files,
    /// Search results.
    Search,
    /// Recent writes and forgets.
    Recent,
    /// Dream passes.
    Dreams,
}

impl Tab {
    /// All tabs in display order.
    pub const ALL: [Tab; 4] = [Tab::Files, Tab::Search, Tab::Recent, Tab::Dreams];

    /// Label.
    pub fn label(self) -> &'static str {
        match self {
            Tab::Files => "Files",
            Tab::Search => "Search",
            Tab::Recent => "Recent writes",
            Tab::Dreams => "Dreams",
        }
    }

    fn index(self) -> usize {
        Tab::ALL.iter().position(|t| *t == self).unwrap_or(0)
    }
}

/// A note in the Files tab.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileRow {
    /// Drive-relative path (`tools/cargo.md`).
    pub path: String,
    /// Title.
    pub title: String,
    /// Number of bullets.
    pub memories: usize,
}

/// A search hit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchRow {
    /// Note path.
    pub path: String,
    /// Note title.
    pub title: Option<String>,
    /// Matching bullets.
    pub snippets: Vec<String>,
}

/// A commit in the Recent or Dreams tab.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitRow {
    /// Commit id.
    pub commit: String,
    /// When.
    pub at: OffsetDateTime,
    /// Subject line.
    pub subject: String,
}

/// What the browser shows.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MemoryData {
    /// Drive location (shown in the title).
    pub drive: String,
    /// `[memory] enabled` for this session.
    pub enabled: bool,
    /// Every note.
    pub files: Vec<FileRow>,
    /// Recent writes, newest first.
    pub recent: Vec<CommitRow>,
    /// Dream passes, newest first.
    pub dreams: Vec<CommitRow>,
}

/// A browser control.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BrowserAction {
    /// Switch tab.
    Tab(Tab),
    /// Select a row of the current tab.
    Select(usize),
    /// Focus the search box.
    SearchBox,
    /// Open the selected note in `$EDITOR`.
    OpenEditor,
    /// Ask the agent to change the selected note.
    AskChange,
    /// Ask the agent to forget from the selected note.
    AskForget,
    /// Undo the selected commit.
    Undo,
    /// Toggle memory for this session.
    ToggleEnabled,
    /// Run a dream pass now.
    Dream,
    /// Close the browser.
    Close,
}

/// What the core should do after an input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BrowserOutcome {
    /// Not handled.
    Ignored,
    /// Redraw.
    Changed,
    /// The browser closed.
    Closed,
    /// Run a memory search and call [`MemoryBrowser::set_search_results`].
    Search(String),
    /// Load the note's text into [`MemoryBrowser::set_detail`].
    Preview(String),
    /// Load the commit's diff into [`MemoryBrowser::set_detail`].
    ShowDiff(String),
    /// Open the note (drive-relative path) in `$EDITOR`.
    OpenInEditor(String),
    /// Put this text in the input box (the user completes and sends it).
    Draft(String),
    /// Revert this commit.
    Undo(String),
    /// Turn memory on or off for this session (notes are kept).
    SetEnabled(bool),
    /// Run `/memory dream`.
    Dream,
}

/// Element id of the search box.
pub const SEARCH_BOX_ID: &str = "memory.search";

/// Scroll pane id of the detail pane (the wheel scrolls it).
pub const DETAIL_PANE: &str = "memory.detail";

/// The `/memory` browser state.
#[derive(Debug, Clone, Default)]
pub struct MemoryBrowser {
    data: MemoryData,
    open: bool,
    tab: Option<Tab>,
    selected: [usize; 4],
    query: String,
    results: Vec<SearchRow>,
    detail: Option<(String, String)>,
    /// First detail row shown.
    detail_scroll: std::cell::Cell<usize>,
    /// Largest first detail row in the last frame.
    detail_max: std::cell::Cell<usize>,
    /// Bring the selected row into view on the next frame (small screens,
    /// where the whole browser scrolls).
    follow: std::cell::Cell<bool>,
}

fn link(path: &str) -> &str {
    path.strip_suffix(".md").unwrap_or(path)
}

impl MemoryBrowser {
    /// A closed browser.
    pub fn new(data: MemoryData) -> Self {
        Self {
            data,
            ..Default::default()
        }
    }

    /// Replaces the data (after a write, undo, dream or toggle).
    pub fn set_data(&mut self, data: MemoryData) {
        self.data = data;
        for t in Tab::ALL {
            let n = self.rows(t);
            let s = &mut self.selected[t.index()];
            *s = (*s).min(n.saturating_sub(1));
        }
    }

    /// The current data.
    pub fn data(&self) -> &MemoryData {
        &self.data
    }

    /// Sets the search results.
    pub fn set_search_results(&mut self, rows: Vec<SearchRow>) {
        self.results = rows;
        self.selected[Tab::Search.index()] = 0;
    }

    /// Sets the detail pane (`key` is the note path or commit id it shows).
    pub fn set_detail(&mut self, key: impl Into<String>, text: impl Into<String>) {
        self.detail = Some((key.into(), text.into()));
        self.detail_scroll.set(0);
    }

    /// Scrolls the detail pane by `lines` rows (the wheel, PageUp/PageDown).
    pub fn scroll_detail(&mut self, lines: i32) {
        let max = self.detail_max.get() as i64;
        let next = (self.detail_scroll.get() as i64 + i64::from(lines)).clamp(0, max);
        self.detail_scroll.set(next as usize);
    }

    /// First detail row shown.
    pub fn detail_scroll(&self) -> usize {
        self.detail_scroll.get()
    }

    /// Opens the browser on the Files tab.
    pub fn open(&mut self) -> BrowserOutcome {
        self.open = true;
        self.tab = Some(Tab::Files);
        self.request_detail()
    }

    /// Closes it.
    pub fn close(&mut self) {
        self.open = false;
    }

    /// Whether it is shown.
    pub fn is_open(&self) -> bool {
        self.open
    }

    /// The current tab.
    pub fn tab(&self) -> Tab {
        self.tab.unwrap_or(Tab::Files)
    }

    /// The search query.
    pub fn query(&self) -> &str {
        &self.query
    }

    fn rows(&self, t: Tab) -> usize {
        match t {
            Tab::Files => self.data.files.len(),
            Tab::Search => self.results.len(),
            Tab::Recent => self.data.recent.len(),
            Tab::Dreams => self.data.dreams.len(),
        }
    }

    fn sel(&self) -> usize {
        self.selected[self.tab().index()]
    }

    /// The note selected in Files or Search.
    pub fn selected_path(&self) -> Option<&str> {
        match self.tab() {
            Tab::Files => self.data.files.get(self.sel()).map(|f| f.path.as_str()),
            Tab::Search => self.results.get(self.sel()).map(|r| r.path.as_str()),
            _ => None,
        }
    }

    /// The commit selected in Recent or Dreams.
    pub fn selected_commit(&self) -> Option<&str> {
        match self.tab() {
            Tab::Recent => self.data.recent.get(self.sel()).map(|c| c.commit.as_str()),
            Tab::Dreams => self.data.dreams.get(self.sel()).map(|c| c.commit.as_str()),
            _ => None,
        }
    }

    fn request_detail(&self) -> BrowserOutcome {
        self.follow.set(true);
        if let Some(p) = self.selected_path() {
            BrowserOutcome::Preview(p.to_owned())
        } else if let Some(c) = self.selected_commit() {
            BrowserOutcome::ShowDiff(c.to_owned())
        } else {
            BrowserOutcome::Changed
        }
    }

    fn set_tab(&mut self, t: Tab) -> BrowserOutcome {
        self.tab = Some(t);
        self.request_detail()
    }

    /// Applies a control.
    pub fn activate(&mut self, action: BrowserAction) -> BrowserOutcome {
        match action {
            BrowserAction::Tab(t) => self.set_tab(t),
            BrowserAction::Select(i) => {
                let n = self.rows(self.tab());
                if i < n {
                    self.selected[self.tab().index()] = i;
                }
                self.request_detail()
            }
            BrowserAction::SearchBox => {
                self.tab = Some(Tab::Search);
                BrowserOutcome::Changed
            }
            BrowserAction::OpenEditor => {
                self.selected_path().map_or(BrowserOutcome::Ignored, |p| {
                    BrowserOutcome::OpenInEditor(p.to_owned())
                })
            }
            BrowserAction::AskChange => self.selected_path().map_or(BrowserOutcome::Ignored, |p| {
                BrowserOutcome::Draft(format!("Update the memory note [[{}]]: ", link(p)))
            }),
            BrowserAction::AskForget => self.selected_path().map_or(BrowserOutcome::Ignored, |p| {
                BrowserOutcome::Draft(format!(
                    "Forget this from the memory note [[{}]]: ",
                    link(p)
                ))
            }),
            BrowserAction::Undo => self.selected_commit().map_or(BrowserOutcome::Ignored, |c| {
                BrowserOutcome::Undo(c.to_owned())
            }),
            BrowserAction::ToggleEnabled => BrowserOutcome::SetEnabled(!self.data.enabled),
            BrowserAction::Dream => BrowserOutcome::Dream,
            BrowserAction::Close => {
                self.close();
                BrowserOutcome::Closed
            }
        }
    }

    /// Browser keys (call before the interaction layer). Tab, Enter and
    /// clicks are left to [`crate::interaction::Interaction`]; Enter on the
    /// focused search box runs the search. `focused` is the focused element.
    pub fn handle_key(&mut self, key: &KeyEvent, focused: Option<&ElementId>) -> BrowserOutcome {
        if !self.open || key.kind == KeyEventKind::Release {
            return BrowserOutcome::Ignored;
        }
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let in_search = self.tab() == Tab::Search;
        let n = self.rows(self.tab());
        let i = self.tab().index();
        match key.code {
            KeyCode::Esc => {
                self.close();
                BrowserOutcome::Closed
            }
            KeyCode::Left => {
                let t = Tab::ALL[(i + Tab::ALL.len() - 1) % Tab::ALL.len()];
                self.set_tab(t)
            }
            KeyCode::Right => self.set_tab(Tab::ALL[(i + 1) % Tab::ALL.len()]),
            KeyCode::PageUp => {
                self.scroll_detail(-5);
                BrowserOutcome::Changed
            }
            KeyCode::PageDown => {
                self.scroll_detail(5);
                BrowserOutcome::Changed
            }
            KeyCode::Home => {
                self.scroll_detail(-i32::MAX);
                BrowserOutcome::Changed
            }
            KeyCode::End => {
                self.scroll_detail(i32::MAX);
                BrowserOutcome::Changed
            }
            KeyCode::Up => {
                self.selected[i] = self.selected[i].saturating_sub(1);
                self.request_detail()
            }
            KeyCode::Down if n > 0 => {
                self.selected[i] = (self.selected[i] + 1).min(n - 1);
                self.request_detail()
            }
            // Enter searches unless a button or row has focus.
            KeyCode::Enter
                if in_search
                    && focused
                        .is_none_or(|f| f.0 == SEARCH_BOX_ID || f.0.starts_with("memory.tab.")) =>
            {
                if self.query.trim().is_empty() {
                    BrowserOutcome::Changed
                } else {
                    BrowserOutcome::Search(self.query.trim().to_owned())
                }
            }
            KeyCode::Backspace if in_search => {
                self.query.pop();
                BrowserOutcome::Changed
            }
            KeyCode::Char(c) if in_search && !ctrl => {
                self.query.push(c);
                BrowserOutcome::Changed
            }
            _ => BrowserOutcome::Ignored,
        }
    }

    /// A bracketed paste: appended to the search query (line breaks and
    /// tabs become spaces).
    pub fn paste(&mut self, text: &str) {
        self.query.extend(
            text.chars()
                .map(|c| {
                    if matches!(c, '\n' | '\r' | '\t') {
                        ' '
                    } else {
                        c
                    }
                })
                .filter(|c| !c.is_control()),
        );
    }

    /// Draws the browser as a modal over `area`.
    ///
    /// Top to bottom: the drive (when the title cannot hold it), the tabs,
    /// the search box, the rows, the detail and the buttons, all wrapping.
    /// When that cannot fit (a small screen) the whole body scrolls
    /// instead, keeping the selected row in view.
    pub fn render<A: Clone>(
        &self,
        frame: &mut Frame<'_>,
        area: Rect,
        theme: &Theme,
        hits: &mut HitMap<A>,
        wrap: &dyn Fn(BrowserAction) -> A,
    ) {
        if !self.open {
            return;
        }
        let w = area.width.saturating_sub(4).clamp(20.min(area.width), 110);
        let h = area.height.saturating_sub(2).clamp(8.min(area.height), 34);
        let rect = Rect::new(
            area.x + (area.width - w) / 2,
            area.y + (area.height - h) / 2,
            w,
            h,
        );
        hits.push_modal(rect);
        frame.render_widget(Clear, rect);
        let state = if self.data.enabled { "on" } else { "off" };
        let drive = sanitize(&self.data.drive);
        let head = format!(" Memory ({state}) ");
        // The drive shows in the title when the border holds it.
        let drive_in_title = width(&head) + width(&drive) + 1 + 2 <= w as usize;
        let title = if drive_in_title {
            format!("{head}{drive} ")
        } else {
            head
        };
        let block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(theme.border(true))
            .title(Span::styled(title, theme.fg(Token::Accent)))
            .style(theme.bg(Token::Surface).patch(theme.fg(Token::Fg)));
        let inner = block.inner(rect);
        frame.render_widget(block, rect);
        if inner.height == 0 || inner.width == 0 {
            return;
        }
        let iw = inner.width as usize;
        let plain = theme.bg(Token::Surface).patch(theme.fg(Token::Fg));
        let focus_style = |id: &str, on: Style, off: Style| {
            if hits.is_focused(&ElementId::from(id)) {
                theme.focused()
            } else if on != off {
                on
            } else {
                off
            }
        };
        // Sections.
        let mut top = Section::default();
        if !drive_in_title {
            for t in wrap_text(&drive, iw) {
                top.lines
                    .push(Line::from(Span::styled(t, theme.fg(Token::Path))));
            }
        }
        let tabs: Vec<(String, String, Style, BrowserAction)> = Tab::ALL
            .iter()
            .map(|&t| {
                let id = format!("memory.tab.{}", t.index());
                let on = if t == self.tab() {
                    theme
                        .fg(Token::Accent)
                        .add_modifier(Modifier::BOLD | Modifier::UNDERLINED)
                } else {
                    theme.fg(Token::Muted)
                };
                let st = focus_style(&id, on, on);
                (id, format!(" {} ", t.label()), st, BrowserAction::Tab(t))
            })
            .collect();
        top.flow(tabs, iw);
        if self.tab() == Tab::Search {
            let st = focus_style(SEARCH_BOX_ID, theme.fg(Token::Fg), theme.fg(Token::Fg));
            let rows = wrap_text(&format!("search: {}▏", self.query), iw);
            top.hits.push(SectionHit {
                row: top.lines.len(),
                x: 0,
                w: iw,
                h: rows.len(),
                id: SEARCH_BOX_ID.to_owned(),
                action: BrowserAction::SearchBox,
                button: false,
            });
            top.lines
                .extend(rows.into_iter().map(|t| Line::from(Span::styled(t, st))));
        }
        let mut buttons: Vec<(&str, &str, BrowserAction)> = Vec::new();
        match self.tab() {
            Tab::Files | Tab::Search if self.selected_path().is_some() => {
                buttons.push((
                    "memory.btn.edit",
                    "open in editor",
                    BrowserAction::OpenEditor,
                ));
                buttons.push((
                    "memory.btn.change",
                    "ask to change",
                    BrowserAction::AskChange,
                ));
                buttons.push((
                    "memory.btn.forget",
                    "ask to forget",
                    BrowserAction::AskForget,
                ));
            }
            Tab::Recent | Tab::Dreams if self.selected_commit().is_some() => {
                buttons.push(("memory.btn.undo", "undo", BrowserAction::Undo));
            }
            _ => {}
        }
        let toggle = if self.data.enabled {
            "memory off"
        } else {
            "memory on"
        };
        buttons.push(("memory.btn.toggle", toggle, BrowserAction::ToggleEnabled));
        buttons.push(("memory.btn.dream", "dream now", BrowserAction::Dream));
        buttons.push(("memory.btn.close", "close", BrowserAction::Close));
        let mut bottom = Section::default();
        let link = theme.fg(Token::Link);
        bottom.flow(
            buttons
                .into_iter()
                .map(|(id, label, a)| {
                    (
                        id.to_owned(),
                        format!("[ {label} ]"),
                        focus_style(id, link, link),
                        a,
                    )
                })
                .collect(),
            iw,
        );
        // Rows: each wraps; the selected one is highlighted.
        let mut list = Section::default();
        let rows = self.row_texts();
        let sel = self.sel().min(rows.len().saturating_sub(1));
        let mut sel_rows = (0, 1);
        if rows.is_empty() {
            let msg = match self.tab() {
                Tab::Files => "No notes yet.",
                Tab::Search => "Type a query and press Enter.",
                Tab::Recent => "No memory writes yet.",
                Tab::Dreams => "No dream passes yet.",
            };
            list.lines
                .push(Line::from(Span::styled(msg, theme.fg(Token::Muted))));
        }
        let mut starts = Vec::new();
        for (row, text) in rows.iter().enumerate() {
            let id = format!("memory.row.{}.{row}", self.tab().index());
            let st = if row == sel {
                theme.focused()
            } else {
                focus_style(&id, plain, plain)
            };
            let parts = wrap_text(text, iw);
            starts.push((list.lines.len(), parts.len()));
            if row == sel {
                sel_rows = (list.lines.len(), parts.len());
            }
            list.hits.push(SectionHit {
                row: list.lines.len(),
                x: 0,
                w: iw,
                h: parts.len(),
                id,
                action: BrowserAction::Select(row),
                button: false,
            });
            list.lines.extend(
                parts
                    .into_iter()
                    .map(|t| Line::from(Span::styled(format!("{t:<iw$}"), st))),
            );
        }
        let mut detail = Section::default();
        if let Some((key, text)) = &self.detail
            && (self.selected_path() == Some(key.as_str())
                || self.selected_commit() == Some(key.as_str()))
        {
            for l in text.lines() {
                let token = if l.starts_with('+') && !l.starts_with("+++") {
                    Token::DiffAdded
                } else if l.starts_with('-') && !l.starts_with("---") {
                    Token::DiffRemoved
                } else {
                    Token::Muted
                };
                detail.lines.extend(
                    wrap_text(l, iw)
                        .into_iter()
                        .map(|t| Line::from(Span::styled(t, theme.fg(token)))),
                );
            }
        }
        let ih = inner.height as usize;
        let fixed = top.lines.len() + bottom.lines.len();
        let avail = ih.saturating_sub(fixed);
        let mut pos = None;
        if avail > sel_rows.1 {
            // The usual layout: rows above the detail, both scrolling.
            let list_h = (avail / 2).max(sel_rows.1).min(avail - 1);
            let mut y = inner.y;
            y = top.blit(frame, hits, wrap, inner, y, 0, top.lines.len());
            // The list scrolls by item to keep the selection whole.
            let mut first = sel_rows.0;
            let mut k = starts.iter().position(|&(s, _)| s == first).unwrap_or(0);
            while k > 0 && sel_rows.0 + sel_rows.1 - starts[k - 1].0 <= list_h {
                k -= 1;
                first = starts[k].0;
            }
            list.blit(frame, hits, wrap, inner, y, first, list_h);
            y += list_h as u16;
            let detail_h = avail - list_h;
            let max = detail.lines.len().saturating_sub(detail_h);
            self.detail_max.set(max);
            let dfirst = self.detail_scroll.get().min(max);
            hits.register_pane(
                DETAIL_PANE,
                Rect::new(inner.x, y, inner.width, detail_h as u16),
            );
            detail.blit(frame, hits, wrap, inner, y, dfirst, detail_h);
            y += detail_h as u16;
            bottom.blit(frame, hits, wrap, inner, y, 0, bottom.lines.len());
            if max > 0 {
                pos = Some(format!(
                    " detail {}-{}/{} · pgup/pgdn ",
                    dfirst + 1,
                    dfirst + detail_h,
                    detail.lines.len()
                ));
            }
        } else {
            // Too small: everything in one column that scrolls.
            let mut all = Section::default();
            let list_at = top.lines.len();
            for s in [top, list, detail, bottom] {
                all.append(s);
            }
            let max = all.lines.len().saturating_sub(ih);
            self.detail_max.set(max);
            let mut first = self.detail_scroll.get().min(max);
            if self.follow.get() {
                let (s0, n) = (list_at + sel_rows.0, sel_rows.1);
                if s0 < first || s0 + n > first + ih {
                    first = s0.min(max);
                }
            }
            self.detail_scroll.set(first);
            hits.register_pane(DETAIL_PANE, inner);
            all.blit(frame, hits, wrap, inner, inner.y, first, ih);
            if max > 0 {
                pos = Some(format!(
                    " {}-{}/{} · pgup/pgdn ",
                    first + 1,
                    first + ih,
                    all.lines.len()
                ));
            }
        }
        self.follow.set(false);
        if let Some(pos) = pos {
            let pw = width(&pos) as u16;
            if rect.width > pw + 2 {
                frame.render_widget(
                    Paragraph::new(Span::styled(pos, theme.fg(Token::Muted))),
                    Rect::new(rect.right() - pw - 1, rect.bottom() - 1, pw, 1),
                );
            }
        }
    }

    fn row_texts(&self) -> Vec<String> {
        let stamp = |t: &OffsetDateTime| {
            format!("{:04}-{:02}-{:02}", t.year(), u8::from(t.month()), t.day())
        };
        match self.tab() {
            Tab::Files => self
                .data
                .files
                .iter()
                .map(|f| format!("{}  {} ({})", f.path, sanitize(&f.title), f.memories))
                .collect(),
            Tab::Search => self
                .results
                .iter()
                .map(|r| {
                    format!(
                        "{}  {}",
                        r.path,
                        sanitize(
                            &r.snippets
                                .first()
                                .cloned()
                                .or_else(|| r.title.clone())
                                .unwrap_or_default()
                        )
                    )
                })
                .collect(),
            Tab::Recent => self
                .data
                .recent
                .iter()
                .map(|c| {
                    format!(
                        "{}  {}  {}",
                        stamp(&c.at),
                        c.commit.get(..7).unwrap_or(&c.commit),
                        sanitize(&c.subject)
                    )
                })
                .collect(),
            Tab::Dreams => self
                .data
                .dreams
                .iter()
                .map(|c| {
                    format!(
                        "{}  {}  {}",
                        stamp(&c.at),
                        c.commit.get(..7).unwrap_or(&c.commit),
                        sanitize(&c.subject)
                    )
                })
                .collect(),
        }
    }
}

/// A clickable part of a [`Section`].
#[derive(Debug, Clone)]
struct SectionHit {
    row: usize,
    x: usize,
    w: usize,
    h: usize,
    id: String,
    action: BrowserAction,
    /// A button (its label is masked from text selection), not a row.
    button: bool,
}

/// Rows of the browser with their clickable parts, drawn through a window.
#[derive(Debug, Default)]
struct Section {
    lines: Vec<Line<'static>>,
    hits: Vec<SectionHit>,
}

impl Section {
    /// Adds labelled buttons left to right, one cell apart, wrapping onto
    /// more rows (a label wider than the row wraps on rows of its own).
    fn flow(&mut self, items: Vec<(String, String, Style, BrowserAction)>, width: usize) {
        let width = width.max(1);
        let mut row: Vec<Span<'static>> = Vec::new();
        let mut used = 0usize;
        for (id, label, style, action) in items {
            let lw = crate::chat::text::width(&label);
            if used > 0 && used + 1 + lw > width {
                self.lines.push(Line::from(std::mem::take(&mut row)));
                used = 0;
            }
            if lw > width {
                let parts = wrap_text(&label, width);
                self.hits.push(SectionHit {
                    row: self.lines.len(),
                    x: 0,
                    w: width,
                    h: parts.len(),
                    id,
                    action,
                    button: true,
                });
                self.lines.extend(
                    parts
                        .into_iter()
                        .map(|p| Line::from(Span::styled(p, style))),
                );
                continue;
            }
            if used > 0 {
                row.push(Span::raw(" "));
                used += 1;
            }
            self.hits.push(SectionHit {
                row: self.lines.len(),
                x: used,
                w: lw,
                h: 1,
                id,
                action,
                button: true,
            });
            row.push(Span::styled(label, style));
            used += lw;
        }
        if !row.is_empty() {
            self.lines.push(Line::from(row));
        }
    }

    fn append(&mut self, other: Section) {
        let base = self.lines.len();
        self.lines.extend(other.lines);
        self.hits.extend(other.hits.into_iter().map(|mut h| {
            h.row += base;
            h
        }));
    }

    /// Draws rows `first..first + n` at row `y` of `inner` and registers the parts
    /// shown. Returns the row below.
    #[allow(clippy::too_many_arguments)]
    fn blit<A: Clone>(
        &self,
        frame: &mut Frame<'_>,
        hits: &mut HitMap<A>,
        wrap: &dyn Fn(BrowserAction) -> A,
        inner: Rect,
        y: u16,
        first: usize,
        n: usize,
    ) -> u16 {
        let (x, width) = (inner.x, inner.width);
        for (i, l) in self.lines.iter().skip(first).take(n).enumerate() {
            frame.render_widget(
                Paragraph::new(l.clone()),
                Rect::new(x, y + i as u16, width, 1).intersection(frame.area()),
            );
        }
        for h in &self.hits {
            let top = h.row.max(first);
            let bottom = (h.row + h.h).min(first + n);
            if bottom > top {
                let r = Rect::new(
                    x + h.x as u16,
                    y + (top - first) as u16,
                    h.w as u16,
                    (bottom - top) as u16,
                );
                if h.button {
                    hits.register_button(ElementId(h.id.clone()), r, wrap(h.action.clone()));
                } else {
                    hits.register(ElementId(h.id.clone()), r, wrap(h.action.clone()));
                }
            }
        }
        y + n.min(self.lines.len().saturating_sub(first)) as u16
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interaction::testing::{buffer_text, find_text};
    use crate::interaction::{Dispatch, Interaction};
    use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use time::macros::datetime;

    fn data() -> MemoryData {
        MemoryData {
            drive: "~/.local/share/harness/memory".into(),
            enabled: true,
            files: vec![
                FileRow {
                    path: "MEMORY.md".into(),
                    title: "Memory".into(),
                    memories: 2,
                },
                FileRow {
                    path: "tools/cargo.md".into(),
                    title: "Cargo".into(),
                    memories: 1,
                },
            ],
            recent: vec![CommitRow {
                commit: "aaaaaaa1111".into(),
                at: datetime!(2026-10-21 10:00 UTC),
                subject: "memory: write tools/cargo.md".into(),
            }],
            dreams: vec![CommitRow {
                commit: "bbbbbbb2222".into(),
                at: datetime!(2026-10-22 03:00 UTC),
                subject: "memory: dream".into(),
            }],
        }
    }

    struct Ui {
        b: MemoryBrowser,
        it: Interaction<BrowserAction>,
        term: Terminal<TestBackend>,
    }

    impl Ui {
        fn new() -> Self {
            let mut b = MemoryBrowser::new(data());
            assert_eq!(b.open(), BrowserOutcome::Preview("MEMORY.md".into()));
            let mut ui = Ui {
                b,
                it: Interaction::new(),
                term: Terminal::new(TestBackend::new(100, 24)).unwrap(),
            };
            ui.draw();
            ui
        }
        fn draw(&mut self) {
            let theme = Theme::default();
            let mut hits = self.it.begin_frame();
            let b = &self.b;
            self.term
                .draw(|f| b.render(f, f.area(), &theme, &mut hits, &|a| a))
                .unwrap();
            self.it.end_frame(hits);
        }
        fn text(&self) -> String {
            buffer_text(self.term.backend().buffer())
        }
        fn click(&mut self, needle: &str) -> BrowserOutcome {
            let (x, y) = find_text(self.term.backend().buffer(), needle)
                .unwrap_or_else(|| panic!("{needle} not shown:\n{}", self.text()));
            let ev = MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: x + 1,
                row: y,
                modifiers: KeyModifiers::NONE,
            };
            let out = match self.it.handle_mouse(&ev) {
                Some(Dispatch::Activate(a)) => self.b.activate(a),
                other => panic!("{other:?}"),
            };
            self.draw();
            out
        }
        fn key(&mut self, code: KeyCode) -> BrowserOutcome {
            let k = KeyEvent::new(code, KeyModifiers::NONE);
            let focused = self.it.focused().cloned();
            let mut out = self.b.handle_key(&k, focused.as_ref());
            if out == BrowserOutcome::Ignored {
                out = match self.it.handle_key(&k) {
                    Some(Dispatch::Activate(a)) => self.b.activate(a),
                    Some(Dispatch::Back) => self.b.activate(BrowserAction::Close),
                    _ => BrowserOutcome::Changed,
                };
            }
            self.draw();
            out
        }
    }

    #[test]
    fn shows_files_search_recent_and_dreams_with_diffs() {
        let mut ui = Ui::new();
        ui.b.set_detail("MEMORY.md", "# Memory\n- Prefer tabs");
        ui.draw();
        let t = ui.text();
        assert!(t.contains("Memory (on)"));
        assert!(t.contains("tools/cargo.md  Cargo (1)"));
        assert!(t.contains("- Prefer tabs"));
        // Recent writes tab, by mouse.
        assert_eq!(
            ui.click("Recent writes"),
            BrowserOutcome::ShowDiff("aaaaaaa1111".into())
        );
        ui.b.set_detail("aaaaaaa1111", "+- Use nextest");
        ui.draw();
        assert!(ui.text().contains("memory: write tools/cargo.md"));
        assert!(ui.text().contains("+- Use nextest"));
        // Dreams tab, by keyboard.
        assert_eq!(
            ui.key(KeyCode::Right),
            BrowserOutcome::ShowDiff("bbbbbbb2222".into())
        );
        assert!(ui.text().contains("memory: dream"));
        // Search tab: type and press Enter.
        ui.key(KeyCode::Left);
        ui.key(KeyCode::Left);
        assert_eq!(ui.b.tab(), Tab::Search);
        for c in "nextest".chars() {
            ui.key(KeyCode::Char(c));
        }
        assert_eq!(
            ui.key(KeyCode::Enter),
            BrowserOutcome::Search("nextest".into())
        );
        ui.b.set_search_results(vec![SearchRow {
            path: "tools/cargo.md".into(),
            title: Some("Cargo".into()),
            snippets: vec!["Use nextest".into()],
        }]);
        ui.draw();
        assert!(ui.text().contains("tools/cargo.md  Use nextest"));
    }

    #[test]
    fn note_actions_by_mouse_and_keyboard() {
        let mut ui = Ui::new();
        assert_eq!(
            ui.key(KeyCode::Down),
            BrowserOutcome::Preview("tools/cargo.md".into())
        );
        assert_eq!(
            ui.click("open in editor"),
            BrowserOutcome::OpenInEditor("tools/cargo.md".into())
        );
        assert_eq!(
            ui.click("ask to change"),
            BrowserOutcome::Draft("Update the memory note [[tools/cargo]]: ".into())
        );
        assert_eq!(
            ui.click("ask to forget"),
            BrowserOutcome::Draft("Forget this from the memory note [[tools/cargo]]: ".into())
        );
        // Keyboard: Tab through the controls to "open in editor" and press Enter.
        let mut ui = Ui::new();
        ui.key(KeyCode::Down);
        let mut out = BrowserOutcome::Ignored;
        for _ in 0..12 {
            ui.key(KeyCode::Tab);
            if ui.it.focused() == Some(&ElementId::from("memory.btn.edit")) {
                out = ui.key(KeyCode::Enter);
                break;
            }
        }
        assert_eq!(out, BrowserOutcome::OpenInEditor("tools/cargo.md".into()));
    }

    #[test]
    fn undo_toggle_dream_and_close() {
        let mut ui = Ui::new();
        ui.click("Dreams");
        assert_eq!(
            ui.click("[ undo ]"),
            BrowserOutcome::Undo("bbbbbbb2222".into())
        );
        assert_eq!(ui.click("memory off"), BrowserOutcome::SetEnabled(false));
        let mut d = data();
        d.enabled = false;
        ui.b.set_data(d);
        ui.draw();
        assert!(ui.text().contains("Memory (off)"));
        assert!(ui.text().contains("memory on ]"));
        ui.click(" Files ");
        assert!(
            ui.text().contains("tools/cargo.md"),
            "notes are kept when memory is off"
        );
        assert_eq!(ui.click("dream now"), BrowserOutcome::Dream);
        assert_eq!(ui.key(KeyCode::Esc), BrowserOutcome::Closed);
        assert!(!ui.b.is_open());
        let mut ui = Ui::new();
        assert_eq!(ui.click("[ close ]"), BrowserOutcome::Closed);
    }
}
