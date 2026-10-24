//! The command palette (Ctrl+P).
//!
//! One fuzzy-searchable list over every action: built-in commands,
//! workflows, skills, agents, past runs, remotes and settings. Each row
//! shows the title, description, source (kind and scope) and keybinding.
//! Entries that take arguments prompt for them inline, one at a time,
//! before running. Matching uses the shared [`crate::fuzzy`] matcher
//! (`nucleo-matcher`, also used by the slash dropdown) over haystacks converted
//! once in [`Palette::set_entries`], so opening and filtering 1000 entries
//! stays well under one 16 ms frame.
//!
//! Keys: type to filter, Up/Down (or Tab/Shift+Tab) to select, PgUp/PgDn to
//! page, Enter to run or to confirm an argument, Esc to step back or close.
//! Rows are clickable; clicking outside the palette closes it.

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use harness_config::discovery::Catalog;
use harness_config::ini::config::Config;
use nucleo_matcher::Utf32String;
use ratatui::Frame;
use ratatui::layout::{Position, Rect};
use ratatui::style::Modifier;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};

use crate::chat::text::{sanitize, truncate, truncate_line, width};
use crate::fuzzy::{self, Fuzzy};
use crate::interaction::{ElementId, HitMap};
use crate::item_detail::{ItemDetail, hard_end, hard_wrap};
use crate::keys::{Action, Keymap};
use crate::theme::{Theme, Token};

/// Built-in palette ids.
pub mod ids {
    /// Toggle mouse capture.
    pub const TOGGLE_MOUSE: &str = "app.toggle_mouse";
    /// Show the key help.
    pub const SHOW_KEYS: &str = "app.show_keys";
    /// Switch between chat and map.
    pub const SWITCH_VIEW: &str = "app.switch_view";
    /// Interrupt the running agent.
    pub const INTERRUPT: &str = "app.interrupt";
    /// Quit.
    pub const QUIT: &str = "app.quit";
    /// Open the notification history.
    pub const NOTIFICATIONS: &str = "notify.history";
    /// Cycle `[ui] timestamps` for this session.
    pub const TIMESTAMPS: &str = "setting.timestamps";
    /// Cycle `[ui] show_thinking` for this session.
    pub const SHOW_THINKING: &str = "setting.show_thinking";
    /// Jump to the end of the transcript.
    pub const FOLLOW: &str = "chat.follow";
    /// Prefix of workflow entries.
    pub const WORKFLOW_PREFIX: &str = "workflow.";
    /// Prefix of skill entries.
    pub const SKILL_PREFIX: &str = "skill.";
    /// Prefix of agent entries.
    pub const AGENT_PREFIX: &str = "agent.";
    /// Prefix of run entries.
    pub const RUN_PREFIX: &str = "run.";
    /// Prefix of remote entries.
    pub const REMOTE_PREFIX: &str = "remote.";
    /// Prefix of saved-session entries (`session.<id>`).
    pub const SESSION_PREFIX: &str = "session.";
}

/// Where an entry comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum EntryKind {
    /// A built-in command.
    Builtin,
    /// A workflow.
    Workflow,
    /// A skill.
    Skill,
    /// An agent.
    Agent,
    /// A past run.
    Run,
    /// A saved session (resumes it).
    Session,
    /// A remote host.
    Remote,
    /// A setting.
    Setting,
    /// An MCP prompt or resource.
    Mcp,
}

impl EntryKind {
    /// Display label.
    pub fn label(self) -> &'static str {
        match self {
            EntryKind::Builtin => "builtin",
            EntryKind::Workflow => "workflow",
            EntryKind::Skill => "skill",
            EntryKind::Agent => "agent",
            EntryKind::Run => "run",
            EntryKind::Session => "session",
            EntryKind::Remote => "remote",
            EntryKind::Setting => "setting",
            EntryKind::Mcp => "mcp",
        }
    }
}

/// An argument an entry prompts for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArgSpec {
    /// Name, shown as the prompt.
    pub name: String,
    /// Whether an empty value is refused.
    pub required: bool,
    /// Placeholder hint.
    pub hint: Option<String>,
}

impl ArgSpec {
    /// A required argument.
    pub fn required(name: impl Into<String>) -> ArgSpec {
        ArgSpec {
            name: name.into(),
            required: true,
            hint: None,
        }
    }

    /// An optional argument.
    pub fn optional(name: impl Into<String>) -> ArgSpec {
        ArgSpec {
            name: name.into(),
            required: false,
            hint: None,
        }
    }
}

/// One palette entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaletteEntry {
    /// Stable id passed back in [`Invocation`].
    pub id: String,
    /// Title.
    pub title: String,
    /// One-line description.
    pub description: String,
    /// Source kind.
    pub kind: EntryKind,
    /// Source scope (`builtin`, `global`, `project`), when it has one.
    pub scope: Option<String>,
    /// Keybinding, e.g. `ctrl+p`.
    pub keybinding: Option<String>,
    /// Arguments prompted for before running.
    pub args: Vec<ArgSpec>,
    /// Characters at the end of the title that stay visible when the row
    /// is too narrow: the head is cut instead (a run's tag, time and
    /// status). 0 cuts the end as usual.
    pub keep_tail: usize,
}

impl PaletteEntry {
    /// An entry without scope, keybinding or arguments.
    pub fn new(
        id: impl Into<String>,
        title: impl Into<String>,
        description: impl Into<String>,
        kind: EntryKind,
    ) -> PaletteEntry {
        PaletteEntry {
            id: id.into(),
            title: title.into(),
            description: description.into(),
            kind,
            scope: None,
            keybinding: None,
            args: Vec::new(),
            keep_tail: 0,
        }
    }

    /// Keeps the last `chars` characters of the title visible when it is
    /// cut ([`PaletteEntry::keep_tail`]).
    pub fn with_keep_tail(mut self, chars: usize) -> PaletteEntry {
        self.keep_tail = chars;
        self
    }

    /// With a keybinding.
    pub fn with_key(mut self, key: impl Into<String>) -> PaletteEntry {
        let k = key.into();
        self.keybinding = (!k.is_empty()).then_some(k);
        self
    }

    /// With a scope.
    pub fn with_scope(mut self, scope: impl Into<String>) -> PaletteEntry {
        self.scope = Some(scope.into());
        self
    }

    /// With arguments.
    pub fn with_args(mut self, args: Vec<ArgSpec>) -> PaletteEntry {
        self.args = args;
        self
    }
}

/// A chosen entry with its argument values.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Invocation {
    /// Entry id.
    pub id: String,
    /// Argument values, in [`PaletteEntry::args`] order.
    pub args: Vec<String>,
}

/// Clickable parts of the palette.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PaletteAction {
    /// Run (or start prompting for) visible row `n` of the filtered list.
    Select(usize),
    /// Scroll the selected entry's full text by this many rows (its ▲/▼).
    Detail(i32),
    /// Open or close the selected entry's full text (`→ full text`).
    ToggleDetail,
}

/// Element id prefix of the palette's full-text detail (see
/// [`crate::item_detail`]).
pub const DETAIL_PREFIX: &str = "palette";

/// Result of a palette event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PaletteOutcome {
    /// Not handled.
    Ignored,
    /// Handled; redraw.
    Changed,
    /// The palette closed without running anything.
    Closed,
    /// Run this.
    Run(Invocation),
}

#[derive(Debug, Clone)]
struct ArgPrompt {
    entry: usize,
    values: Vec<String>,
    current: String,
    error: Option<String>,
}

/// The palette state.
pub struct Palette {
    entries: Vec<PaletteEntry>,
    hay: Vec<Utf32String>,
    matcher: Fuzzy,
    open: bool,
    query: String,
    filtered: Vec<usize>,
    selected: usize,
    scroll: usize,
    prompt: Option<ArgPrompt>,
    page: usize,
    detail: ItemDetail,
}

impl std::fmt::Debug for Palette {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Palette")
            .field("entries", &self.entries.len())
            .field("open", &self.open)
            .field("query", &self.query)
            .finish()
    }
}

impl Default for Palette {
    fn default() -> Self {
        Palette::new(Vec::new())
    }
}

impl Palette {
    /// A closed palette over `entries`.
    pub fn new(entries: Vec<PaletteEntry>) -> Palette {
        let mut p = Palette {
            entries: Vec::new(),
            hay: Vec::new(),
            matcher: Fuzzy::new(),
            open: false,
            query: String::new(),
            filtered: Vec::new(),
            selected: 0,
            scroll: 0,
            prompt: None,
            page: 10,
            detail: ItemDetail::new(),
        };
        p.set_entries(entries);
        p
    }

    /// Replaces the entries (e.g. after a catalog reload).
    pub fn set_entries(&mut self, entries: Vec<PaletteEntry>) {
        self.hay = entries
            .iter()
            .map(|e| {
                let scope = e.scope.as_deref().unwrap_or("");
                Utf32String::from(
                    format!("{} {} {} {scope}", e.title, e.description, e.kind.label()).as_str(),
                )
            })
            .collect();
        self.entries = entries;
        self.prompt = None;
        self.refilter();
    }

    /// All entries.
    pub fn entries(&self) -> &[PaletteEntry] {
        &self.entries
    }

    /// Whether the palette is shown.
    pub fn is_open(&self) -> bool {
        self.open
    }

    /// Opens the palette with an empty query.
    pub fn open(&mut self) {
        self.open = true;
        self.query.clear();
        self.prompt = None;
        self.selected = 0;
        self.scroll = 0;
        self.refilter();
    }

    /// Opens the palette prompting for `entry`'s arguments right away (a
    /// slash command typed without its required arguments). The entry
    /// replaces one with the same id for this prompt.
    pub fn prompt_entry(&mut self, entry: PaletteEntry) {
        let idx = match self.entries.iter().position(|e| e.id == entry.id) {
            Some(i) => {
                self.entries[i] = entry;
                i
            }
            None => {
                let mut all = std::mem::take(&mut self.entries);
                all.push(entry);
                self.set_entries(all);
                self.entries.len() - 1
            }
        };
        self.open();
        if self.entries[idx].args.is_empty() {
            return;
        }
        self.prompt = Some(ArgPrompt {
            entry: idx,
            values: Vec::new(),
            current: String::new(),
            error: None,
        });
    }

    /// Closes the palette.
    pub fn close(&mut self) {
        self.open = false;
        self.prompt = None;
    }

    /// The query.
    pub fn query(&self) -> &str {
        &self.query
    }

    /// Sets the query and filters.
    pub fn set_query(&mut self, q: &str) {
        self.query = q.to_owned();
        self.selected = 0;
        self.scroll = 0;
        self.refilter();
    }

    /// A bracketed paste while open: appended to the argument being
    /// prompted for, else to the filter. Line breaks and tabs become
    /// spaces (both are one-line fields).
    pub fn paste(&mut self, text: &str) {
        if !self.open {
            return;
        }
        let one_line: String = text
            .replace("\r\n", " ")
            .chars()
            .map(|c| {
                if matches!(c, '\n' | '\r' | '\t') {
                    ' '
                } else {
                    c
                }
            })
            .filter(|c| !c.is_control())
            .collect();
        match &mut self.prompt {
            Some(p) => {
                p.current.push_str(&one_line);
                p.error = None;
            }
            None => {
                let q = format!("{}{one_line}", self.query);
                self.set_query(&q);
            }
        }
    }

    /// Entries matching the query, best first.
    pub fn matches(&self) -> Vec<&PaletteEntry> {
        self.filtered.iter().map(|&i| &self.entries[i]).collect()
    }

    /// The selected row of the filtered list.
    pub fn selected(&self) -> usize {
        self.selected
    }

    /// The argument being prompted for: `(entry title, arg name, value)`.
    pub fn prompting(&self) -> Option<(&str, &str, &str)> {
        let p = self.prompt.as_ref()?;
        let e = &self.entries[p.entry];
        let arg = e.args.get(p.values.len())?;
        Some((&e.title, &arg.name, &p.current))
    }

    fn refilter(&mut self) {
        if self.query.trim().is_empty() {
            let mut all: Vec<usize> = (0..self.entries.len()).collect();
            // By kind, then title; past runs and saved sessions keep their
            // order (newest first).
            all.sort_by(|&a, &b| {
                let (x, y) = (&self.entries[a], &self.entries[b]);
                x.kind.cmp(&y.kind).then_with(|| match x.kind {
                    EntryKind::Run | EntryKind::Session => a.cmp(&b),
                    _ => x.title.cmp(&y.title),
                })
            });
            self.filtered = all;
            return;
        }
        let pattern = fuzzy::query(&self.query);
        let mut scored: Vec<(u32, usize)> = self
            .hay
            .iter()
            .enumerate()
            .filter_map(|(i, h)| self.matcher.score(&pattern, h.slice(..)).map(|s| (s, i)))
            .collect();
        scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
        self.filtered = scored.into_iter().map(|(_, i)| i).collect();
    }

    fn choose(&mut self, row: usize) -> PaletteOutcome {
        let Some(&idx) = self.filtered.get(row) else {
            return PaletteOutcome::Ignored;
        };
        self.selected = row;
        if self.entries[idx].args.is_empty() {
            return self.finish(idx, Vec::new());
        }
        self.prompt = Some(ArgPrompt {
            entry: idx,
            values: Vec::new(),
            current: String::new(),
            error: None,
        });
        PaletteOutcome::Changed
    }

    fn finish(&mut self, idx: usize, args: Vec<String>) -> PaletteOutcome {
        self.close();
        PaletteOutcome::Run(Invocation {
            id: self.entries[idx].id.clone(),
            args,
        })
    }

    /// Runs a clicked action.
    pub fn activate(&mut self, action: &PaletteAction) -> PaletteOutcome {
        match action {
            PaletteAction::Select(row) => {
                if self.prompt.is_some() {
                    return PaletteOutcome::Ignored;
                }
                self.choose(*row)
            }
            PaletteAction::Detail(n) => {
                self.detail.scroll(*n);
                PaletteOutcome::Changed
            }
            PaletteAction::ToggleDetail => {
                self.detail.toggle();
                PaletteOutcome::Changed
            }
        }
    }

    /// Esc, or a click outside: leave the argument prompt, else close.
    pub fn back(&mut self) -> PaletteOutcome {
        if self.prompt.take().is_some() {
            PaletteOutcome::Changed
        } else {
            self.close();
            PaletteOutcome::Closed
        }
    }

    /// Handles a key while open.
    pub fn handle_key(&mut self, key: &KeyEvent) -> PaletteOutcome {
        if !self.open || key.kind == KeyEventKind::Release {
            return PaletteOutcome::Ignored;
        }
        let plain = key.modifiers.difference(KeyModifiers::SHIFT).is_empty();
        if let Some(p) = &mut self.prompt {
            match key.code {
                KeyCode::Esc => return self.back(),
                KeyCode::Enter => {
                    let e = &self.entries[p.entry];
                    let spec = &e.args[p.values.len()];
                    if spec.required && p.current.trim().is_empty() {
                        p.error = Some(format!("{} is required", spec.name));
                        return PaletteOutcome::Changed;
                    }
                    p.values.push(std::mem::take(&mut p.current));
                    p.error = None;
                    if p.values.len() == e.args.len() {
                        let (idx, vals) = (p.entry, std::mem::take(&mut p.values));
                        return self.finish(idx, vals);
                    }
                }
                KeyCode::Backspace => {
                    p.current.pop();
                }
                KeyCode::Char(c) if plain => p.current.push(c),
                _ => return PaletteOutcome::Ignored,
            }
            return PaletteOutcome::Changed;
        }
        let n = self.filtered.len();
        let shift = key.modifiers == KeyModifiers::SHIFT;
        match key.code {
            KeyCode::Esc => return self.back(),
            // Shift+Up/Down scroll the selected entry's full text.
            KeyCode::Up if shift => self.detail.scroll(-1),
            KeyCode::Down if shift => self.detail.scroll(1),
            // Right opens (and closes) the selected entry's full text.
            KeyCode::Right if plain => self.detail.toggle(),
            KeyCode::Enter => return self.choose(self.selected),
            KeyCode::Down | KeyCode::Tab
                if n > 0 && !key.modifiers.contains(KeyModifiers::SHIFT) =>
            {
                self.selected = (self.selected + 1) % n
            }
            KeyCode::Up | KeyCode::BackTab | KeyCode::Tab if n > 0 => {
                self.selected = (self.selected + n - 1) % n
            }
            KeyCode::PageDown if n > 0 => self.selected = (self.selected + self.page).min(n - 1),
            KeyCode::PageUp => self.selected = self.selected.saturating_sub(self.page),
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
            _ => return PaletteOutcome::Ignored,
        }
        PaletteOutcome::Changed
    }

    /// The palette's screen rectangle inside `area`.
    pub fn rect(area: Rect) -> Rect {
        let w = area.width.saturating_sub(4).clamp(20.min(area.width), 96);
        let h = area.height.saturating_sub(2).clamp(3.min(area.height), 22);
        Rect::new(
            area.x + (area.width - w) / 2,
            area.y + (area.height - h) / 4,
            w,
            h,
        )
    }

    /// Draws the palette as a modal over `area` and registers its rows.
    pub fn render<A: Clone>(
        &mut self,
        frame: &mut Frame<'_>,
        area: Rect,
        theme: &Theme,
        hits: &mut HitMap<A>,
        wrap: &dyn Fn(PaletteAction) -> A,
    ) {
        if !self.open {
            return;
        }
        let rect = Palette::rect(area);
        hits.push_modal(rect);
        frame.render_widget(Clear, rect);
        let block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(theme.border(true))
            .title(Span::styled(" Command palette ", theme.fg(Token::Accent)))
            .style(theme.bg(Token::Surface).patch(theme.fg(Token::Fg)));
        let inner = block.inner(rect);
        frame.render_widget(block, rect);
        if inner.height == 0 || inner.width == 0 {
            return;
        }
        let w = inner.width as usize;
        // Query / argument line, wrapped by character so the cursor lands
        // after the text typed so far.
        let (spans, typed) = match &self.prompt {
            Some(p) => {
                let e = &self.entries[p.entry];
                let spec = &e.args[p.values.len()];
                let mut spans = vec![
                    Span::styled(
                        format!("{} › ", sanitize(&e.title)),
                        theme.fg(Token::Accent),
                    ),
                    Span::styled(
                        format!("{}: ", sanitize(&spec.name)),
                        theme.fg(Token::Muted),
                    ),
                    Span::styled(sanitize(&p.current), theme.fg(Token::Fg)),
                ];
                let typed = spans.len();
                if p.current.is_empty()
                    && let Some(h) = &spec.hint
                {
                    spans.push(Span::styled(sanitize(h), theme.fg(Token::Muted)));
                }
                if let Some(err) = &p.error {
                    spans.push(Span::styled(format!("  {err}"), theme.fg(Token::Error)));
                }
                (spans, typed)
            }
            None => (
                vec![
                    Span::styled("› ", theme.fg(Token::Accent)),
                    Span::styled(sanitize(&self.query), theme.fg(Token::Fg)),
                    Span::styled(
                        format!("  {} of {}", self.filtered.len(), self.entries.len()),
                        theme.fg(Token::Muted),
                    ),
                ],
                2,
            ),
        };
        let (crow, ccol) = hard_end(&spans[..typed], w);
        let head = hard_wrap(spans, w);
        // The prompt has the whole palette; the query leaves room for rows.
        let room = if self.prompt.is_some() {
            inner.height as usize
        } else {
            (inner.height as usize).saturating_sub(2).max(1)
        };
        let head_h = head.len().min(room).max(1);
        let first = (crow + 1).saturating_sub(head_h);
        let shown: Vec<Line<'static>> = head.into_iter().skip(first).take(head_h).collect();
        frame.render_widget(
            Paragraph::new(shown),
            Rect::new(inner.x, inner.y, inner.width, head_h as u16),
        );
        let cy = (crow - first).min(head_h - 1) as u16;
        let cx = inner.x + ccol.min(w.saturating_sub(1)) as u16;
        frame.set_cursor_position(Position::new(cx, inner.y + cy));
        let mut list = Rect::new(
            inner.x,
            inner.y + head_h as u16,
            inner.width,
            inner.height.saturating_sub(head_h as u16),
        );
        if self.prompt.is_some() || list.height == 0 {
            self.page = (list.height as usize).max(1);
            return;
        }
        let pattern = (!self.query.trim().is_empty()).then(|| fuzzy::query(&self.query));
        // The selected entry's full text, when its row cuts it.
        let mut detail = Vec::new();
        if let Some(&sel) = self.filtered.get(self.selected) {
            self.scroll_to(list.height as usize);
            let widths = self.title_widths(list.height as usize, w);
            let e = &self.entries[sel];
            let title_w = widths.get(&e.kind).copied().unwrap_or(0);
            if row_parts(e, title_w, w).3 {
                detail = self.detail_lines(sel, theme);
            }
            self.detail.select(sel);
        }
        let cut = !detail.is_empty();
        let detail_h = self.detail.rows_for(&detail, list.width, list.height);
        list.height -= detail_h;
        self.detail.render_hint(
            frame,
            Rect::new(
                rect.x + 1,
                rect.bottom() - 1,
                rect.width.saturating_sub(2),
                1,
            ),
            cut,
            theme,
            hits,
            DETAIL_PREFIX,
            wrap(PaletteAction::ToggleDetail),
        );
        let rows = list.height as usize;
        self.page = rows.max(1);
        self.scroll_to(rows);
        let title_ws = self.title_widths(rows, w);
        let min_w = (w * 2 / 5).max(10);
        let mut lines = Vec::new();
        let mut indices = Vec::new();
        for (row, &idx) in self
            .filtered
            .iter()
            .enumerate()
            .skip(self.scroll)
            .take(rows)
        {
            let e = &self.entries[idx];
            let title_w = title_ws.get(&e.kind).copied().unwrap_or(min_w);
            let id = ElementId(format!("palette.row.{row}"));
            let sel = row == self.selected || hits.is_focused(&id);
            let base = if sel {
                theme.focused()
            } else {
                theme.bg(Token::Surface)
            };
            let (title, desc, source, _) = row_parts(e, title_w, w);
            indices.clear();
            if let Some(p) = &pattern {
                self.matcher.word_indices(p, &title, &mut indices);
            }
            let mut spans = vec![Span::styled(
                if sel { "▸ " } else { "  " },
                base.patch(theme.fg(Token::Accent)),
            )];
            spans.extend(fuzzy::highlight(
                &title,
                &indices,
                base.patch(theme.fg(Token::Fg)),
                base.patch(theme.fg(Token::Accent))
                    .add_modifier(Modifier::BOLD),
            ));
            spans.push(Span::styled(
                " ".repeat(title_w.saturating_sub(width(&title)) + 1),
                base,
            ));
            spans.push(Span::styled(desc, base.patch(theme.fg(Token::Muted))));
            spans.push(Span::styled(
                format!(" {source}"),
                base.patch(theme.fg(Token::Agent)),
            ));
            if let Some(key) = e.keybinding.as_deref().filter(|k| !k.is_empty()) {
                spans.push(Span::styled(
                    format!("  {key}"),
                    base.patch(theme.fg(Token::Keyword)),
                ));
            }
            lines.push(truncate_line(Line::from(spans), w));
            hits.register(
                id,
                Rect::new(list.x, list.y + (row - self.scroll) as u16, list.width, 1),
                wrap(PaletteAction::Select(row)),
            );
        }
        if self.filtered.is_empty() {
            lines.push(Line::from(Span::styled(
                "  no matches",
                theme.fg(Token::Muted),
            )));
        }
        frame.render_widget(Paragraph::new(lines), list);
        if detail_h > 0 {
            // Right below the rows shown.
            let used = (self.filtered.len() - self.scroll.min(self.filtered.len())).clamp(1, rows);
            let r = Rect::new(list.x, list.y + used as u16, list.width, detail_h);
            self.detail.render(
                frame,
                r,
                detail,
                theme,
                hits,
                DETAIL_PREFIX,
                wrap(PaletteAction::Detail(-1)),
                wrap(PaletteAction::Detail(1)),
            );
        }
    }

    /// Keeps the selection inside `rows` visible rows.
    fn scroll_to(&mut self, rows: usize) {
        let rows = rows.max(1);
        if self.selected < self.scroll {
            self.scroll = self.selected;
        } else if self.selected >= self.scroll + rows {
            self.scroll = self.selected + 1 - rows;
        }
    }

    /// Title column widths of the visible rows by kind: 2/5 of the row; a
    /// kind with longer titles (past runs) widens its own column up to 3/5
    /// so its rows stay aligned.
    fn title_widths(&self, rows: usize, w: usize) -> std::collections::BTreeMap<EntryKind, usize> {
        let (min_w, max_w) = ((w * 2 / 5).max(10), (w * 3 / 5).max(10));
        let mut title_ws: std::collections::BTreeMap<EntryKind, usize> = Default::default();
        for &i in self.filtered.iter().skip(self.scroll).take(rows) {
            let e = &self.entries[i];
            let t = title_ws.entry(e.kind).or_insert(min_w);
            *t = (*t).max(width(&sanitize(&e.title)).min(max_w));
        }
        title_ws
    }

    /// Entry `idx` in full, for the detail below the list: title,
    /// description, then source and key.
    fn detail_lines(&self, idx: usize, theme: &Theme) -> Vec<Line<'static>> {
        let e = &self.entries[idx];
        let mut v = vec![Line::from(Span::styled(
            sanitize(&e.title),
            theme.fg(Token::Fg).add_modifier(Modifier::BOLD),
        ))];
        if !e.description.is_empty() {
            v.extend(
                sanitize(&e.description)
                    .lines()
                    .map(|l| Line::from(Span::styled(l.to_owned(), theme.fg(Token::Muted)))),
            );
        }
        let mut src = vec![Span::styled(source_label(e), theme.fg(Token::Agent))];
        if let Some(k) = e.keybinding.as_deref().filter(|k| !k.is_empty()) {
            src.push(Span::styled(format!("  {k}"), theme.fg(Token::Keyword)));
        }
        v.push(Line::from(src));
        v
    }
}

/// `kind · scope`.
fn source_label(e: &PaletteEntry) -> String {
    match &e.scope {
        Some(s) => format!("{} · {}", e.kind.label(), sanitize(s)),
        None => e.kind.label().to_owned(),
    }
}

/// The parts of `e`'s row at `w` cells with a `title_w` title column:
/// the (possibly cut) title, the padded (possibly cut) description, the
/// source, and whether anything was cut.
fn row_parts(e: &PaletteEntry, title_w: usize, w: usize) -> (String, String, String, bool) {
    let full_title = sanitize(&e.title);
    let title = fit_title(&full_title, title_w, e.keep_tail);
    let source = source_label(e);
    let key = e.keybinding.clone().unwrap_or_default();
    let right = format!(" {source}{}{key}", if key.is_empty() { "" } else { "  " });
    let desc_w = w.saturating_sub(2 + title_w + 1 + width(&right));
    let full_desc = sanitize(&e.description).replace('\n', " ");
    let cut_desc = truncate(&full_desc, desc_w);
    let cut = title != full_title
        || cut_desc != full_desc
        || 2 + title_w + 1 + desc_w + width(&right) > w;
    (title, format!("{cut_desc:<desc_w$}"), source, cut)
}

/// Built-in commands, with keybindings from `keymap`, plus the autonomy
/// modes from `harness_policy::approvals::palette_entries` and session
/// settings.
pub fn builtin_entries(keymap: &Keymap) -> Vec<PaletteEntry> {
    let key = |a: Action| keymap.describe(a);
    let mut v = vec![
        PaletteEntry::new(
            ids::TOGGLE_MOUSE,
            "Toggle mouse",
            "Mouse capture on/off (off allows native text selection)",
            EntryKind::Builtin,
        ),
        PaletteEntry::new(
            ids::SHOW_KEYS,
            "Show keys",
            "List key bindings",
            EntryKind::Builtin,
        ),
        PaletteEntry::new(
            ids::SWITCH_VIEW,
            "Switch view",
            "Toggle between chat and agent map",
            EntryKind::Builtin,
        )
        .with_key(key(Action::SwitchView)),
        PaletteEntry::new(
            ids::INTERRUPT,
            "Interrupt",
            "Stop the running agent",
            EntryKind::Builtin,
        )
        .with_key(key(Action::Interrupt)),
        PaletteEntry::new(
            ids::NOTIFICATIONS,
            "Notifications",
            "Show the notification history",
            EntryKind::Builtin,
        ),
        PaletteEntry::new(
            ids::FOLLOW,
            "Jump to end",
            "Scroll the chat to the latest output",
            EntryKind::Builtin,
        ),
        PaletteEntry::new(ids::QUIT, "Quit", "Exit harness", EntryKind::Builtin)
            .with_key(key(Action::Quit)),
        PaletteEntry::new(
            ids::TIMESTAMPS,
            "Timestamps: cycle",
            "time → relative → off for this session",
            EntryKind::Setting,
        ),
        PaletteEntry::new(
            ids::SHOW_THINKING,
            "Thinking: cycle",
            "collapsed → expanded → hidden for this session",
            EntryKind::Setting,
        ),
    ];
    for m in harness_policy::approvals::palette_entries() {
        v.push(PaletteEntry::new(
            m.id,
            m.title,
            m.description,
            EntryKind::Setting,
        ));
    }
    v
}

/// `title` cut to `max` cells, keeping its last `keep` characters: the
/// head is shortened with `…` before them. Falls back to cutting the end
/// when even the kept part does not fit.
fn fit_title(title: &str, max: usize, keep: usize) -> String {
    if keep == 0 || width(title) <= max {
        return truncate(title, max);
    }
    let split = title.chars().count().saturating_sub(keep);
    let at = title
        .char_indices()
        .nth(split)
        .map_or(title.len(), |(i, _)| i);
    let (head, tail) = title.split_at(at);
    let room = max.saturating_sub(width(tail));
    if room < 2 {
        return truncate(title, max);
    }
    format!("{}{tail}", truncate(head, room))
}

/// Workflows, skills and agents from the catalog, with their scope.
pub fn catalog_entries(catalog: &Catalog) -> Vec<PaletteEntry> {
    let mut v = Vec::new();
    for e in catalog.workflows.active() {
        let w = &e.item;
        v.push(
            PaletteEntry::new(
                format!("{}{}", ids::WORKFLOW_PREFIX, w.command),
                format!("/{}", w.command),
                w.description
                    .clone()
                    .unwrap_or_else(|| format!("Run workflow {}", w.name)),
                EntryKind::Workflow,
            )
            .with_scope(e.origin.scope.label())
            .with_args(vec![ArgSpec::optional("input")]),
        );
    }
    for e in catalog.skills.active() {
        let s = &e.item;
        let args = match &s.args {
            Some(hint) => vec![ArgSpec {
                name: "args".into(),
                required: false,
                hint: Some(hint.clone()),
            }],
            None => Vec::new(),
        };
        v.push(
            PaletteEntry::new(
                format!("{}{}", ids::SKILL_PREFIX, s.name),
                format!("/{}", s.name),
                s.description.clone(),
                EntryKind::Skill,
            )
            .with_scope(e.origin.scope.label())
            .with_args(args),
        );
    }
    for e in catalog.agents.active() {
        let a = &e.item;
        v.push(
            PaletteEntry::new(
                format!("{}{}", ids::AGENT_PREFIX, a.name),
                format!("Agent: {}", a.display_name()),
                a.summary(),
                EntryKind::Agent,
            )
            .with_scope(e.origin.scope.label())
            .with_args(vec![ArgSpec::required("task")]),
        );
    }
    v
}

/// Past runs, newest first, one entry each:
/// `Coder · fix the header parser · 14:32 Oct 23 · ✔` with the outcome
/// (`Succeeded in 2m14s · $0.12 · 3 files changed`) as the description.
/// The id never shows, except as a short `#tag` suffix when two titles
/// would be identical. Times are shown in `offset`.
pub fn run_entries(
    runs: &[crate::history::RunSummary],
    offset: time::UtcOffset,
    now: time::OffsetDateTime,
) -> Vec<PaletteEntry> {
    let labels: Vec<String> = runs.iter().map(|r| r.label(offset, now)).collect();
    let dups = harness_events::names::collisions(&labels);
    runs.iter()
        .zip(dups)
        .map(|(r, dup)| {
            let id = r.run_id.to_string();
            // The tag sits before the time so it survives with it when a
            // narrow row cuts the title.
            let mut tail = String::new();
            if dup {
                tail.push(' ');
                tail.push_str(&harness_events::names::short_tag(&id));
            }
            tail.push_str(&r.label_tail(offset, now));
            PaletteEntry::new(
                format!("{}{id}", ids::RUN_PREFIX),
                format!("{}{tail}", r.label_head()),
                r.outcome(),
                EntryKind::Run,
            )
            .with_keep_tail(tail.chars().count())
        })
        .collect()
}

/// Replaces the entry of `live` in `entries` with one built from its current
/// state (title, status and duration), or adds it before the other runs when
/// it is not listed yet. The run list is read when a session starts, so
/// without this the run in progress keeps the untitled, `0ms` entry it had
/// then.
pub fn refresh_run_entry(
    entries: &mut Vec<PaletteEntry>,
    live: &crate::history::RunSummary,
    offset: time::UtcOffset,
    now: time::OffsetDateTime,
) {
    let Some(fresh) = run_entries(std::slice::from_ref(live), offset, now).pop() else {
        return;
    };
    match entries.iter().position(|e| e.id == fresh.id) {
        Some(i) => entries[i] = fresh,
        None => {
            let at = entries
                .iter()
                .position(|e| e.kind == EntryKind::Run)
                .unwrap_or(entries.len());
            entries.insert(at, fresh);
        }
    }
}

/// Remotes from `[remote.*]`.
pub fn remote_entries(cfg: &Config) -> Vec<PaletteEntry> {
    cfg.remotes
        .iter()
        .map(|(name, r)| {
            let user = r
                .user
                .as_deref()
                .map(|u| format!("{u}@"))
                .unwrap_or_default();
            PaletteEntry::new(
                format!("{}{name}", ids::REMOTE_PREFIX),
                format!("Remote: {name}"),
                format!("{user}{}:{}", r.host, r.port),
                EntryKind::Remote,
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interaction::testing::{buffer_text, find_text};
    use crate::interaction::{Dispatch, Interaction};
    use crossterm::event::{Event, MouseButton, MouseEvent, MouseEventKind};
    use harness_config::discovery::{BuiltinFile, Roots};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use std::time::Instant;

    fn key(chord: &str) -> KeyEvent {
        let Event::Key(k) = crate::interaction::testing::key(chord) else {
            unreachable!()
        };
        k
    }

    fn typ(p: &mut Palette, s: &str) {
        for c in s.chars() {
            p.handle_key(&KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        }
    }

    fn sample() -> Vec<PaletteEntry> {
        let mut v = builtin_entries(&Keymap::defaults());
        v.push(
            PaletteEntry::new(
                "workflow.review",
                "/review",
                "Review the diff",
                EntryKind::Workflow,
            )
            .with_scope("project")
            .with_args(vec![
                ArgSpec::required("branch"),
                ArgSpec::optional("focus"),
            ]),
        );
        v.extend(run_entries(
            &[test_run("fix parser")],
            time::UtcOffset::UTC,
            test_now(),
        ));
        v
    }

    fn test_now() -> time::OffsetDateTime {
        time::macros::datetime!(2026-10-23 18:00 UTC)
    }

    fn test_run(title: &str) -> crate::history::RunSummary {
        crate::history::RunSummary {
            run_id: harness_events::schema::RunId(
                uuid::Uuid::parse_str("0199f1a2-3b4c-7d5e-8f60-123456abcdef").unwrap(),
            ),
            name: "coder".into(),
            workflow: None,
            agent: Some("coder".into()),
            who: "Coder".into(),
            title: Some(title.into()),
            files_changed: 3,
            error: None,
            status: harness_events::schema::RunStatus::Succeeded,
            started_at: Some(time::macros::datetime!(2026-10-23 14:32 UTC)),
            duration_ms: Some(134_000),
            cost_usd: 0.12,
            host: None,
            nodes: 1,
            truncated: false,
        }
    }

    #[test]
    fn covers_every_source_kind() {
        static FILES: &[BuiltinFile] = &[
            BuiltinFile {
                path: "agents/researcher.yaml",
                contents: "name: researcher\ndescription: Reads code\nprompt: Be brief.\n",
            },
            BuiltinFile {
                path: "skills/commit/SKILL.md",
                contents: "---\nname: commit\ndescription: Write a commit message\nargs: <scope>\n---\nWrite it.\n",
            },
        ];
        let catalog = Catalog::load(&Roots::new(FILES));
        let mut cfg = Config::defaults();
        cfg.remotes.insert(
            "gpu-box".into(),
            harness_config::ini::config::Remote {
                host: "10.0.0.2".into(),
                user: Some("me".into()),
                port: 22,
                identity_file: None,
                labels: vec![],
                workdir: None,
                max_jobs: 1,
                sync: harness_config::ini::config::SyncMethod::Rsync,
                policy_bash_deny: vec![],
                policy_bash_ask: vec![],
                compression: harness_config::ini::config::SshCompression::Auto,
                compression_below_mbps: 20.0,
            },
        );
        let mut all = builtin_entries(&Keymap::defaults());
        all.extend(catalog_entries(&catalog));
        all.extend(run_entries(
            &[test_run("fix parser")],
            time::UtcOffset::UTC,
            test_now(),
        ));
        all.extend(remote_entries(&cfg));
        all.push(PaletteEntry::new(
            "workflow.x",
            "/x",
            "wf",
            EntryKind::Workflow,
        ));
        let kinds: std::collections::BTreeSet<EntryKind> = all.iter().map(|e| e.kind).collect();
        assert_eq!(kinds.len(), 7, "{kinds:?}");
        let agent = all
            .iter()
            .find(|e| e.id == "agent.researcher")
            .expect("agent entry");
        assert_eq!(agent.scope.as_deref(), Some("builtin"));
        let skill = all
            .iter()
            .find(|e| e.id == "skill.commit")
            .expect("skill entry");
        assert_eq!(skill.args[0].hint.as_deref(), Some("<scope>"));
        assert!(
            all.iter().any(|e| e.id == "mode.auto"),
            "autonomy modes are listed"
        );
        assert!(
            all.iter()
                .any(|e| e.id == "remote.gpu-box" && e.description.contains("10.0.0.2"))
        );
        let quit = all.iter().find(|e| e.id == ids::QUIT).unwrap();
        assert_eq!(quit.keybinding.as_deref(), Some("ctrl+q"));
    }

    #[test]
    fn fuzzy_filter_and_run() {
        let mut p = Palette::new(sample());
        assert!(!p.is_open());
        p.open();
        assert_eq!(p.matches().len(), p.entries().len());
        typ(&mut p, "tglmse");
        assert_eq!(p.matches()[0].id, ids::TOGGLE_MOUSE);
        assert_eq!(
            p.handle_key(&key("enter")),
            PaletteOutcome::Run(Invocation {
                id: ids::TOGGLE_MOUSE.into(),
                args: vec![]
            })
        );
        assert!(!p.is_open());
        p.open();
        typ(&mut p, "zzzzzz");
        assert!(p.matches().is_empty());
        assert_eq!(p.handle_key(&key("enter")), PaletteOutcome::Ignored);
        assert_eq!(p.handle_key(&key("esc")), PaletteOutcome::Closed);
    }

    #[test]
    fn arguments_are_prompted_inline() {
        let mut p = Palette::new(sample());
        p.open();
        typ(&mut p, "review");
        assert_eq!(p.handle_key(&key("enter")), PaletteOutcome::Changed);
        assert_eq!(p.prompting(), Some(("/review", "branch", "")));
        // Required arguments refuse empty values.
        p.handle_key(&key("enter"));
        assert_eq!(p.prompting().unwrap().1, "branch");
        typ(&mut p, "main");
        p.handle_key(&key("enter"));
        assert_eq!(p.prompting(), Some(("/review", "focus", "")));
        // Esc steps back to the list.
        assert_eq!(p.handle_key(&key("esc")), PaletteOutcome::Changed);
        assert!(p.prompting().is_none() && p.is_open());
        p.handle_key(&key("enter"));
        typ(&mut p, "main");
        p.handle_key(&key("enter"));
        typ(&mut p, "tests");
        assert_eq!(
            p.handle_key(&key("enter")),
            PaletteOutcome::Run(Invocation {
                id: "workflow.review".into(),
                args: vec!["main".into(), "tests".into()]
            })
        );
    }

    fn draw(p: &mut Palette, it: &mut Interaction<PaletteAction>) -> Terminal<TestBackend> {
        let theme = Theme::default();
        let mut term = Terminal::new(TestBackend::new(100, 24)).unwrap();
        let mut hits = it.begin_frame();
        term.draw(|f| p.render(f, f.area(), &theme, &mut hits, &|a| a))
            .unwrap();
        it.end_frame(hits);
        term
    }

    #[test]
    fn rows_show_description_source_and_keybinding_and_are_clickable() {
        let mut p = Palette::new(sample());
        p.open();
        let mut it = Interaction::new();
        let term = draw(&mut p, &mut it);
        let text = buffer_text(term.backend().buffer());
        assert!(text.contains("Command palette"));
        assert!(text.contains("Review the diff"));
        assert!(text.contains("workflow · project"));
        assert!(text.contains("ctrl+q"), "{text}");
        let (x, y) = find_text(term.backend().buffer(), "Show keys").unwrap();
        let click = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: x,
            row: y,
            modifiers: KeyModifiers::NONE,
        };
        let Some(Dispatch::Activate(a)) = it.handle_mouse(&click) else {
            panic!("row not clickable")
        };
        assert_eq!(
            p.activate(&a),
            PaletteOutcome::Run(Invocation {
                id: ids::SHOW_KEYS.into(),
                args: vec![]
            })
        );
        // A click outside the modal is Back.
        p.open();
        draw(&mut p, &mut it);
        let outside = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 0,
            row: 23,
            modifiers: KeyModifiers::NONE,
        };
        assert_eq!(it.handle_mouse(&outside), Some(Dispatch::Back));
        assert_eq!(p.back(), PaletteOutcome::Closed);
    }

    fn mouse_at(it: &mut Interaction<PaletteAction>, x: u16, y: u16) -> Option<PaletteAction> {
        let click = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: x,
            row: y,
            modifiers: KeyModifiers::NONE,
        };
        match it.handle_mouse(&click) {
            Some(Dispatch::Activate(a)) => Some(a),
            _ => None,
        }
    }

    /// A cut row offers `→ full text` on the bottom border; Right or a
    /// click opens the entry's full text below the rows, Shift+Down or ▼
    /// scrolls it, and Right or a click on `← hide full text` closes it.
    #[test]
    fn cut_rows_open_their_full_text_by_keyboard_and_mouse() {
        let long = "a description that is much too long for its column ".repeat(8) + "tail_end";
        let mut p = Palette::new(vec![PaletteEntry::new(
            "x",
            "Long entry",
            long.clone(),
            EntryKind::Workflow,
        )]);
        p.open();
        let mut it = Interaction::new();
        let term = draw(&mut p, &mut it);
        let text = buffer_text(term.backend().buffer());
        assert!(text.contains("→ full text"), "{text}");
        assert!(!text.contains("tail_end"), "{text}");
        // Keyboard: Right opens; the whole description wraps below.
        assert_eq!(p.handle_key(&key("right")), PaletteOutcome::Changed);
        let term = draw(&mut p, &mut it);
        let text = buffer_text(term.backend().buffer());
        assert!(text.contains("─ full text"), "{text}");
        assert!(text.contains("← hide full text"), "{text}");
        assert!(text.contains("tail_end"), "{text}");
        assert_eq!(p.handle_key(&key("right")), PaletteOutcome::Changed);
        let term = draw(&mut p, &mut it);
        assert!(!buffer_text(term.backend().buffer()).contains("tail_end"));
        // Mouse: click the hint to open, ▼ scrolls a short palette.
        let (x, y) = find_text(term.backend().buffer(), "→ full text").unwrap();
        let a = mouse_at(&mut it, x, y).expect("hint is clickable");
        assert_eq!(a, PaletteAction::ToggleDetail);
        p.activate(&a);
        let term = draw(&mut p, &mut it);
        assert!(buffer_text(term.backend().buffer()).contains("tail_end"));
        // A click on `← hide full text` closes it, a second click reopens.
        let (x, y) = find_text(term.backend().buffer(), "← hide full text").unwrap();
        let a = mouse_at(&mut it, x + 2, y).expect("hide is clickable");
        p.activate(&a);
        let term = draw(&mut p, &mut it);
        assert!(!buffer_text(term.backend().buffer()).contains("tail_end"));
        let (x, y) = find_text(term.backend().buffer(), "→ full text").unwrap();
        let a = mouse_at(&mut it, x, y).expect("hint is clickable");
        p.activate(&a);
        let theme = Theme::default();
        let mut small = Terminal::new(TestBackend::new(60, 12)).unwrap();
        let mut redraw = |p: &mut Palette, it: &mut Interaction<PaletteAction>| {
            let mut hits = it.begin_frame();
            small
                .draw(|f| p.render(f, f.area(), &theme, &mut hits, &|a| a))
                .unwrap();
            it.end_frame(hits);
            (
                buffer_text(small.backend().buffer()),
                find_text(small.backend().buffer(), "▼"),
            )
        };
        let (text, down) = redraw(&mut p, &mut it);
        assert!(text.contains("1-"), "a position shows: {text}");
        let (x, y) = down.expect("▼ shows when the text overflows");
        let a = mouse_at(&mut it, x, y).expect("▼ is clickable");
        assert_eq!(a, PaletteAction::Detail(1));
        p.activate(&a);
        let (text, _) = redraw(&mut p, &mut it);
        assert!(text.contains("2-"), "{text}");
        p.handle_key(&key("shift+down"));
        let (text, _) = redraw(&mut p, &mut it);
        assert!(text.contains("3-"), "{text}");
        p.handle_key(&key("shift+up"));
        let (text, _) = redraw(&mut p, &mut it);
        assert!(text.contains("2-"), "{text}");
    }

    #[test]
    fn long_queries_and_prompts_wrap_with_the_cursor_after_the_text() {
        let mut p = Palette::new(sample());
        p.open();
        let q = "x".repeat(150);
        p.set_query(&q);
        let mut it = Interaction::new();
        let mut term = draw(&mut p, &mut it);
        let text = buffer_text(term.backend().buffer());
        assert_eq!(text.matches('x').count(), 150, "{text}");
        let pos = term.get_cursor_position().unwrap();
        // "› " + 150 chars in a 94-wide palette: row 2, column 2 + 150 - 94.
        assert_eq!((pos.x, pos.y), (3 + 58, 2), "{text}");
    }

    #[test]
    fn title_highlights_words_that_match_it_when_others_match_elsewhere() {
        let mut p = Palette::new(vec![PaletteEntry::new(
            "custom.model",
            "Model",
            "Switch the model",
            EntryKind::Builtin,
        )]);
        p.open();
        typ(&mut p, "mod sw");
        assert_eq!(p.matches().len(), 1, "`sw` matches the description");
        let mut it = Interaction::new();
        let term = draw(&mut p, &mut it);
        let buf = term.backend().buffer();
        let (x, y) = find_text(buf, "Model").unwrap();
        let theme = Theme::default();
        let accent = theme.fg(Token::Accent).fg;
        for i in 0..5u16 {
            let cell = &buf[(x + i, y)];
            let hit = i < 3;
            assert_eq!(cell.modifier.contains(Modifier::BOLD), hit, "cell {i} bold");
            if hit {
                assert_eq!(cell.fg, accent.unwrap(), "cell {i} accent");
            }
        }
    }

    #[test]
    fn keyboard_navigation() {
        let mut p = Palette::new(sample());
        p.open();
        let first = p.matches()[0].id.clone();
        p.handle_key(&key("down"));
        assert_eq!(p.selected(), 1);
        p.handle_key(&key("up"));
        p.handle_key(&key("up"));
        assert_eq!(p.selected(), p.matches().len() - 1);
        p.handle_key(&key("down"));
        let PaletteOutcome::Run(inv) = p.handle_key(&key("enter")) else {
            panic!()
        };
        assert_eq!(inv.id, first);
    }

    #[test]
    fn opens_and_filters_1000_entries_within_a_frame() {
        let entries: Vec<PaletteEntry> = (0..1000)
            .map(|i| {
                PaletteEntry::new(
                    format!("skill.s{i}"),
                    format!("/skill-number-{i}"),
                    format!("Description of skill {i} for testing the palette"),
                    EntryKind::Skill,
                )
                .with_scope("global")
            })
            .collect();
        let theme = Theme::default();
        // The slowest frame of one open-and-type sequence on fresh state.
        let sequence = || {
            let mut p = Palette::new(entries.clone());
            let mut term = Terminal::new(TestBackend::new(100, 30)).unwrap();
            let it: Interaction<PaletteAction> = Interaction::new();
            let start = Instant::now();
            p.open();
            let mut hits = it.begin_frame();
            term.draw(|f| p.render(f, f.area(), &theme, &mut hits, &|a| a))
                .unwrap();
            let mut worst = start.elapsed();
            for c in "num9 test".chars() {
                let t = Instant::now();
                p.handle_key(&KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
                let mut hits = it.begin_frame();
                term.draw(|f| p.render(f, f.area(), &theme, &mut hits, &|a| a))
                    .unwrap();
                worst = worst.max(t.elapsed());
            }
            assert!(!p.matches().is_empty());
            worst
        };
        // Best of five runs: a slow implementation is slow every run, while
        // a scheduler hiccup on a shared CI runner only hits some of them.
        let worst = (0..5).map(|_| sequence()).min().unwrap();
        assert!(
            worst < std::time::Duration::from_millis(16),
            "worst frame {worst:?}"
        );
    }

    #[test]
    fn run_entries_read_as_agent_title_time_and_status() {
        let e = run_entries(
            &[test_run("fix the header parser")],
            time::UtcOffset::UTC,
            test_now(),
        );
        assert_eq!(
            e[0].title,
            "Coder · fix the header parser · 14:32 Oct 23 · ✔"
        );
        assert_eq!(
            e[0].description,
            "Succeeded in 2m14s · $0.12 · 3 files changed"
        );
        assert!(e[0].id.starts_with(ids::RUN_PREFIX));
        for text in [&e[0].title, &e[0].description] {
            assert!(harness_events::names::find_raw_id(text).is_none(), "{text}");
            assert!(!text.contains("0199"), "{text}");
        }
    }

    #[test]
    fn refreshing_the_live_run_replaces_or_adds_its_entry() {
        let mut stale = test_run("fix the header parser");
        stale.title = None;
        stale.status = harness_events::schema::RunStatus::Running;
        stale.duration_ms = Some(0);
        let mut v = builtin_entries(&Keymap::defaults());
        v.extend(run_entries(&[stale], time::UtcOffset::UTC, test_now()));
        let mut live = test_run("fix the header parser");
        live.status = harness_events::schema::RunStatus::Running;
        live.duration_ms = Some(76_000);
        refresh_run_entry(&mut v, &live, time::UtcOffset::UTC, test_now());
        let runs: Vec<_> = v.iter().filter(|e| e.kind == EntryKind::Run).collect();
        assert_eq!(runs.len(), 1);
        assert_eq!(
            runs[0].title,
            "Coder · fix the header parser · 14:32 Oct 23 · ◐"
        );
        assert!(runs[0].description.starts_with("Running for 1m16s"));
        // Not listed yet: it goes before the other runs.
        let mut v = run_entries(&[test_run("older")], time::UtcOffset::UTC, test_now());
        live.run_id = harness_events::schema::RunId(uuid::Uuid::now_v7());
        refresh_run_entry(&mut v, &live, time::UtcOffset::UTC, test_now());
        assert_eq!(v.len(), 2);
        assert!(
            v[0].title.contains("fix the header parser"),
            "{}",
            v[0].title
        );
    }

    #[test]
    fn colliding_run_titles_get_a_short_tag_only_then() {
        let a = test_run("fix it");
        let mut b = test_run("fix it");
        b.run_id = harness_events::schema::RunId(
            uuid::Uuid::parse_str("0199f1a2-3b4c-7d5e-8f60-999999fedcba").unwrap(),
        );
        let mut c = test_run("fix it");
        c.run_id = harness_events::schema::RunId(uuid::Uuid::now_v7());
        c.started_at = Some(time::macros::datetime!(2026-10-23 15:00 UTC));
        let e = run_entries(&[a, b, c], time::UtcOffset::UTC, test_now());
        assert_eq!(e[0].title, "Coder · fix it #abcdef · 14:32 Oct 23 · ✔");
        assert_eq!(e[1].title, "Coder · fix it #fedcba · 14:32 Oct 23 · ✔");
        assert_eq!(e[2].title, "Coder · fix it · 15:00 Oct 23 · ✔");
    }

    fn colliding_runs(n: usize) -> Vec<crate::history::RunSummary> {
        (0..n)
            .map(|i| {
                let mut r = test_run("Update the changelog for the release");
                r.run_id = harness_events::schema::RunId(
                    uuid::Uuid::parse_str(&format!("0199f1a2-3b4c-7d5e-8f60-00000000000{i}"))
                        .unwrap(),
                );
                r
            })
            .collect()
    }

    #[test]
    fn narrow_palettes_keep_the_tag_time_and_status_of_runs() {
        let mut p = Palette::new(run_entries(
            &colliding_runs(2),
            time::UtcOffset::UTC,
            test_now(),
        ));
        p.open();
        let theme = Theme::default();
        let mut it: Interaction<PaletteAction> = Interaction::new();
        let mut term = Terminal::new(TestBackend::new(80, 24)).unwrap();
        let mut hits = it.begin_frame();
        term.draw(|f| p.render(f, f.area(), &theme, &mut hits, &|a| a))
            .unwrap();
        it.end_frame(hits);
        let text = buffer_text(term.backend().buffer());
        let rows: Vec<&str> = text.lines().filter(|l| l.contains("Coder · ")).collect();
        assert_eq!(rows.len(), 2, "{text}");
        assert!(rows[0].contains("#000000 · 14:32 Oct 23 · ✔"), "{text}");
        assert!(rows[1].contains("#000001 · 14:32 Oct 23 · ✔"), "{text}");
        assert!(rows[0].contains('…'), "the title is cut instead: {text}");
    }

    #[test]
    fn fit_title_cuts_the_head_and_keeps_the_tail() {
        assert_eq!(fit_title("abcdefghij · ✔", 20, 4), "abcdefghij · ✔");
        assert_eq!(fit_title("abcdefghij · ✔", 9, 4), "abcd… · ✔");
        assert_eq!(fit_title("abcdefghij · ✔", 9, 0), "abcdefgh…");
        // Too narrow for the tail: cut the end as usual.
        assert_eq!(fit_title("abcdefghij · ✔", 4, 4), "abc…");
    }

    #[test]
    fn past_runs_stay_newest_first_with_an_empty_query() {
        let titles = ["Zebra crossing", "apple pie", "Mango"];
        let runs: Vec<_> = titles.iter().map(|t| test_run(t)).collect();
        let mut v = builtin_entries(&Keymap::defaults());
        v.extend(run_entries(&runs, time::UtcOffset::UTC, test_now()));
        let mut p = Palette::new(v);
        p.open();
        let shown: Vec<&str> = p
            .matches()
            .into_iter()
            .filter(|e| e.kind == EntryKind::Run)
            .map(|e| e.title.as_str())
            .collect();
        assert_eq!(shown.len(), 3);
        for (s, t) in shown.iter().zip(titles) {
            assert!(s.starts_with(&format!("Coder · {t}")), "{shown:?}");
        }
    }

    #[test]
    fn failed_runs_summarise_the_error() {
        let mut r = test_run("deploy");
        r.status = harness_events::schema::RunStatus::Failed;
        r.error = Some("verify failed: 2 tests\nmore".into());
        r.files_changed = 1;
        r.cost_usd = 0.0;
        let e = run_entries(&[r], time::UtcOffset::UTC, test_now());
        assert_eq!(e[0].title, "Coder · deploy · 14:32 Oct 23 · ✗");
        assert_eq!(
            e[0].description,
            "Failed after 2m14s: verify failed: 2 tests · 1 file changed"
        );
    }

    #[test]
    fn agent_entries_use_display_names_and_descriptions() {
        static FILES: &[BuiltinFile] = &[
            BuiltinFile {
                path: "agents/deep-research.yaml",
                contents: "name: deep-research\ndisplay_name: Deep Diver\ndescription: Reads widely\nprompt_text: Be brief.\n",
            },
            BuiltinFile {
                path: "agents/fixer.yaml",
                contents: "name: fixer\nprompt_text: Fix.\n",
            },
        ];
        let catalog = Catalog::load(&Roots::new(FILES));
        let e = catalog_entries(&catalog);
        let deep = e.iter().find(|e| e.id == "agent.deep-research").unwrap();
        assert_eq!(
            (deep.title.as_str(), deep.description.as_str()),
            ("Agent: Deep Diver", "Reads widely")
        );
        let fixer = e.iter().find(|e| e.id == "agent.fixer").unwrap();
        assert_eq!(
            (fixer.title.as_str(), fixer.description.as_str()),
            ("Agent: Fixer", "Run the Fixer agent")
        );
    }
}
