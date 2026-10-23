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

use crate::chat::text::{truncate, width};
use crate::fuzzy::{self, Fuzzy};
use crate::interaction::{ElementId, HitMap};
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
    /// A remote host.
    Remote,
    /// A setting.
    Setting,
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
            EntryKind::Remote => "remote",
            EntryKind::Setting => "setting",
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
        }
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
}

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
            all.sort_by(|&a, &b| {
                let (x, y) = (&self.entries[a], &self.entries[b]);
                (x.kind, &x.title).cmp(&(y.kind, &y.title))
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
        match key.code {
            KeyCode::Esc => return self.back(),
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
        // Query / argument line.
        let (line, cursor_x) = match &self.prompt {
            Some(p) => {
                let e = &self.entries[p.entry];
                let spec = &e.args[p.values.len()];
                let mut spans = vec![
                    Span::styled(format!("{} › ", e.title), theme.fg(Token::Accent)),
                    Span::styled(format!("{}: ", spec.name), theme.fg(Token::Muted)),
                ];
                let x = spans.iter().map(|s| width(&s.content)).sum::<usize>() + width(&p.current);
                if p.current.is_empty()
                    && let Some(h) = &spec.hint
                {
                    spans.push(Span::styled(h.clone(), theme.fg(Token::Muted)));
                } else {
                    spans.push(Span::styled(p.current.clone(), theme.fg(Token::Fg)));
                }
                if let Some(err) = &p.error {
                    spans.push(Span::styled(format!("  {err}"), theme.fg(Token::Error)));
                }
                (Line::from(spans), x)
            }
            None => {
                let spans = vec![
                    Span::styled("› ", theme.fg(Token::Accent)),
                    Span::styled(self.query.clone(), theme.fg(Token::Fg)),
                    Span::styled(
                        format!("  {} of {}", self.filtered.len(), self.entries.len()),
                        theme.fg(Token::Muted),
                    ),
                ];
                (Line::from(spans), 2 + width(&self.query))
            }
        };
        frame.render_widget(
            Paragraph::new(line),
            Rect::new(inner.x, inner.y, inner.width, 1),
        );
        let cx = inner.x + cursor_x.min(w.saturating_sub(1)) as u16;
        frame.set_cursor_position(Position::new(cx, inner.y));
        let list = Rect::new(
            inner.x,
            inner.y + 1,
            inner.width,
            inner.height.saturating_sub(1),
        );
        let rows = list.height as usize;
        self.page = rows.max(1);
        if self.prompt.is_some() || rows == 0 {
            return;
        }
        if self.selected < self.scroll {
            self.scroll = self.selected;
        } else if self.selected >= self.scroll + rows {
            self.scroll = self.selected + 1 - rows;
        }
        let pattern = (!self.query.trim().is_empty()).then(|| fuzzy::query(&self.query));
        let title_w = (w * 2 / 5).max(10);
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
            let id = ElementId(format!("palette.row.{row}"));
            let sel = row == self.selected || hits.is_focused(&id);
            let base = if sel {
                theme.focused()
            } else {
                theme.bg(Token::Surface)
            };
            let title = truncate(&e.title, title_w);
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
            let source = match &e.scope {
                Some(s) => format!("{} · {s}", e.kind.label()),
                None => e.kind.label().to_owned(),
            };
            let key = e.keybinding.clone().unwrap_or_default();
            let right = format!(" {source}{}{key}", if key.is_empty() { "" } else { "  " });
            let desc_w = w.saturating_sub(2 + title_w + 1 + width(&right));
            spans.push(Span::styled(
                format!("{:<desc_w$}", truncate(&e.description, desc_w)),
                base.patch(theme.fg(Token::Muted)),
            ));
            spans.push(Span::styled(
                format!(" {source}"),
                base.patch(theme.fg(Token::Agent)),
            ));
            if !key.is_empty() {
                spans.push(Span::styled(
                    format!("  {key}"),
                    base.patch(theme.fg(Token::Keyword)),
                ));
            }
            lines.push(Line::from(spans));
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
    }
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
                format!("Agent: {}", a.name),
                a.description
                    .clone()
                    .unwrap_or_else(|| "Run this agent".into()),
                EntryKind::Agent,
            )
            .with_scope(e.origin.scope.label())
            .with_args(vec![ArgSpec::required("task")]),
        );
    }
    v
}

/// Past runs as `(id, label)` pairs, newest first.
pub fn run_entries<'a>(runs: impl IntoIterator<Item = (&'a str, &'a str)>) -> Vec<PaletteEntry> {
    runs.into_iter()
        .map(|(id, label)| {
            PaletteEntry::new(
                format!("{}{id}", ids::RUN_PREFIX),
                format!("Run {label}"),
                format!("Open run {id}"),
                EntryKind::Run,
            )
        })
        .collect()
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
        v.extend(run_entries([("0199", "fix parser")]));
        v
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
        all.extend(run_entries([("0199", "fix parser")]));
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
        let mut p = Palette::new(entries);
        let theme = Theme::default();
        let mut term = Terminal::new(TestBackend::new(100, 30)).unwrap();
        let it: Interaction<PaletteAction> = Interaction::new();
        let mut worst = std::time::Duration::ZERO;
        let start = Instant::now();
        p.open();
        let mut hits = it.begin_frame();
        term.draw(|f| p.render(f, f.area(), &theme, &mut hits, &|a| a))
            .unwrap();
        worst = worst.max(start.elapsed());
        for c in "num9 test".chars() {
            let t = Instant::now();
            p.handle_key(&KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
            let mut hits = it.begin_frame();
            term.draw(|f| p.render(f, f.area(), &theme, &mut hits, &|a| a))
                .unwrap();
            worst = worst.max(t.elapsed());
        }
        assert!(!p.matches().is_empty());
        assert!(
            worst < std::time::Duration::from_millis(16),
            "worst frame {worst:?}"
        );
    }
}
