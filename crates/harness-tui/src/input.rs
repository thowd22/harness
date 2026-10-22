//! The prompt editor.
//!
//! - Multi-line editing with a cursor: the `newline` key (Shift+Enter or
//!   Alt+Enter by default) inserts a line break, arrows, Home/End,
//!   Backspace and Delete edit, and the `send` key submits.
//! - History: Up on the first line recalls earlier messages, Down on the
//!   last line walks forward again and finally restores the draft.
//! - Bracketed paste: pasted text is inserted as typed text, except that a
//!   large paste (more than [`PASTE_COLLAPSE_LINES`] lines or
//!   [`PASTE_COLLAPSE_BYTES`] bytes) collapses to a placeholder such as
//!   `[paste #1 · 120 lines]`. The placeholder is one character for editing
//!   (Backspace deletes it), expands in place on Ctrl+E or a click, and is
//!   always replaced by the full text when the message is sent. Pastes
//!   arrive exactly: line breaks (`\r\n` and `\r` become `\n`), tabs
//!   (shown as spaces to the next multiple of [`TAB_WIDTH`]) and every
//!   Unicode character, including private-use characters the editor uses
//!   internally for placeholders (stored as one-character pastes).
//! - Slash commands: typing `/` at the start opens inline completion with
//!   each command's arguments, description and scope. Up/Down select, Tab
//!   or Enter complete (Enter on an exact match without arguments sends it),
//!   Esc closes, and rows are clickable.

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::{Position, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph};
use unicode_width::UnicodeWidthChar;

use crate::chat::text::{truncate, width};
use crate::interaction::{ElementId, HitMap};
use crate::keys::Action;
use crate::theme::{Theme, Token};

/// Element id of the input box (same as [`crate::app::INPUT_ID`]).
pub const INPUT_ID: &str = "input";
/// Pastes with more lines than this collapse to a placeholder.
pub const PASTE_COLLAPSE_LINES: usize = 8;
/// Pastes larger than this many bytes collapse to a placeholder.
pub const PASTE_COLLAPSE_BYTES: usize = 1000;
/// Maximum visible input rows; longer drafts scroll.
pub const MAX_INPUT_ROWS: usize = 8;
/// Maximum completion rows.
pub const MAX_COMPLETIONS: usize = 8;
/// Prompt shown before the first row.
pub const PROMPT: &str = "› ";
/// Tab stops of the input display.
pub const TAB_WIDTH: usize = 4;
/// Fixed chord that expands all paste placeholders.
pub const EXPAND_PASTE_CHORD: &str = "ctrl+e";

const PASTE_BASE: u32 = 0xF0000;

/// One display row: `(text, paste index)` pieces.
type Row = Vec<(String, Option<usize>)>;

/// A slash command offered by completion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlashCommand {
    /// Name without the slash, e.g. `review` or `project:deploy`.
    pub name: String,
    /// One-line description.
    pub description: String,
    /// Argument hint, e.g. `<branch>`.
    pub args: Option<String>,
    /// Source scope, e.g. `builtin`, `global`, `project`.
    pub scope: Option<String>,
}

impl SlashCommand {
    /// A command without arguments or scope.
    pub fn new(name: impl Into<String>, description: impl Into<String>) -> SlashCommand {
        SlashCommand {
            name: name.into(),
            description: description.into(),
            args: None,
            scope: None,
        }
    }
}

/// Clickable parts of the input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InputAction {
    /// Focus the input.
    Focus,
    /// Expand paste placeholder `n` (0-based).
    ExpandPaste(usize),
    /// Accept completion row `n`.
    Complete(usize),
}

/// Result of an input event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InputOutcome {
    /// Not handled by the input.
    Ignored,
    /// Handled; redraw.
    Changed,
    /// The user sent this text (placeholders expanded).
    Submit(String),
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Completion {
    matches: Vec<usize>,
    selected: usize,
}

/// The editor state.
#[derive(Debug, Clone, Default)]
pub struct Input {
    text: String,
    cursor: usize,
    pastes: Vec<String>,
    history: Vec<String>,
    hist_pos: Option<usize>,
    stash: String,
    commands: Vec<SlashCommand>,
    completion: Option<Completion>,
    dismissed: bool,
    scroll: usize,
}

fn placeholder(i: usize) -> char {
    char::from_u32(PASTE_BASE + i as u32).unwrap_or('\u{FFFD}')
}

fn paste_index(c: char) -> Option<usize> {
    let v = c as u32;
    (PASTE_BASE..PASTE_BASE + 0xFFFD)
        .contains(&v)
        .then(|| (v - PASTE_BASE) as usize)
}

impl Input {
    /// An empty editor.
    pub fn new() -> Input {
        Input::default()
    }

    /// Sets the slash commands offered by completion.
    pub fn set_commands(&mut self, commands: Vec<SlashCommand>) {
        self.commands = commands;
        self.refresh_completion();
    }

    /// The raw draft (placeholders as private-use characters).
    pub fn raw(&self) -> &str {
        &self.text
    }

    /// The draft with every placeholder replaced by its paste.
    pub fn text(&self) -> String {
        self.text
            .chars()
            .map(|c| match paste_index(c) {
                Some(i) => self.pastes.get(i).cloned().unwrap_or_default(),
                None => c.to_string(),
            })
            .collect()
    }

    /// The draft as displayed (placeholders as labels).
    pub fn display(&self) -> String {
        self.text
            .chars()
            .map(|c| match paste_index(c) {
                Some(i) => self.label(i),
                None => c.to_string(),
            })
            .collect()
    }

    /// Whether the draft is empty.
    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    /// Cursor position in bytes of [`Input::raw`].
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// Submitted messages, oldest first.
    pub fn history(&self) -> &[String] {
        &self.history
    }

    /// Replaces the draft.
    pub fn set_text(&mut self, text: &str) {
        self.text = self.escape(text);
        self.cursor = self.text.len();
        self.dismissed = false;
        self.refresh_completion();
    }

    /// Whether paste `i` is one literal private-use character (see
    /// [`Input::escape`]) rather than a collapsed paste.
    fn is_literal(&self, i: usize) -> bool {
        let p = self.pastes.get(i).map_or("", String::as_str);
        let mut cs = p.chars();
        matches!((cs.next(), cs.next()), (Some(c), None) if paste_index(c).is_some())
    }

    fn label(&self, i: usize) -> String {
        let p = self.pastes.get(i).map_or("", String::as_str);
        if self.is_literal(i) {
            return p.to_owned();
        }
        let n = (0..=i).filter(|&j| !self.is_literal(j)).count();
        let lines = p.lines().count().max(1);
        format!("[paste #{n} · {lines} lines]")
    }

    /// `s` with every character of the placeholder range stored as a
    /// one-character paste, so typed or pasted text is never mistaken for
    /// a placeholder.
    fn escape(&mut self, s: &str) -> String {
        if !s.chars().any(|c| paste_index(c).is_some()) {
            return s.to_owned();
        }
        s.chars()
            .map(|c| {
                if paste_index(c).is_some() {
                    self.pastes.push(c.to_string());
                    placeholder(self.pastes.len() - 1)
                } else {
                    c
                }
            })
            .collect()
    }

    fn insert_str(&mut self, s: &str) {
        let s = self.escape(s);
        self.insert_raw(&s);
    }

    /// Inserts `s` as is (placeholders included).
    fn insert_raw(&mut self, s: &str) {
        self.text.insert_str(self.cursor, s);
        self.cursor += s.len();
        self.hist_pos = None;
        self.refresh_completion();
    }

    /// Inserts typed text at the cursor.
    pub fn insert_char(&mut self, c: char) {
        let mut b = [0; 4];
        self.insert_str(c.encode_utf8(&mut b));
    }

    /// Handles a bracketed paste: small pastes are inserted, large ones
    /// collapse to a placeholder.
    pub fn paste(&mut self, text: &str) {
        let text = text.replace("\r\n", "\n").replace('\r', "\n");
        let lines = text.lines().count();
        if lines > PASTE_COLLAPSE_LINES || text.len() > PASTE_COLLAPSE_BYTES {
            self.pastes.push(text);
            let c = placeholder(self.pastes.len() - 1);
            self.insert_raw(c.encode_utf8(&mut [0; 4]));
        } else {
            self.insert_str(&text);
        }
    }

    /// Number of collapsed pastes in the draft.
    pub fn placeholders(&self) -> usize {
        self.text
            .chars()
            .filter_map(paste_index)
            .filter(|&i| !self.is_literal(i))
            .count()
    }

    /// Expands placeholder `i` in place. Returns false if it is not in the draft.
    pub fn expand_paste(&mut self, i: usize) -> bool {
        let c = placeholder(i);
        if self.is_literal(i) {
            return false;
        }
        let Some(pos) = self.text.find(c) else {
            return false;
        };
        let full = self.pastes.get(i).cloned().unwrap_or_default();
        let full = self.escape(&full);
        self.text.replace_range(pos..pos + c.len_utf8(), &full);
        if self.cursor > pos {
            self.cursor = self.cursor + full.len() - c.len_utf8();
        }
        true
    }

    /// Expands every placeholder in the draft.
    pub fn expand_all(&mut self) -> bool {
        let ids: Vec<usize> = self.text.chars().filter_map(paste_index).collect();
        let mut any = false;
        for i in ids {
            any |= self.expand_paste(i);
        }
        any
    }

    fn line_start(&self) -> usize {
        self.text[..self.cursor].rfind('\n').map_or(0, |i| i + 1)
    }

    fn line_end(&self) -> usize {
        self.text[self.cursor..]
            .find('\n')
            .map_or(self.text.len(), |i| self.cursor + i)
    }

    fn is_first_line(&self) -> bool {
        !self.text[..self.cursor].contains('\n')
    }

    fn is_last_line(&self) -> bool {
        !self.text[self.cursor..].contains('\n')
    }

    fn prev_char(&self) -> Option<usize> {
        self.text[..self.cursor]
            .char_indices()
            .next_back()
            .map(|(i, _)| i)
    }

    fn next_char(&self) -> Option<usize> {
        self.text[self.cursor..]
            .chars()
            .next()
            .map(|c| self.cursor + c.len_utf8())
    }

    fn move_vertical(&mut self, up: bool) {
        let col = self.text[self.line_start()..self.cursor].chars().count();
        let target_start = if up {
            let start = self.line_start();
            if start == 0 {
                return;
            }
            self.text[..start - 1].rfind('\n').map_or(0, |i| i + 1)
        } else {
            let end = self.line_end();
            if end >= self.text.len() {
                return;
            }
            end + 1
        };
        let line_end = self.text[target_start..]
            .find('\n')
            .map_or(self.text.len(), |i| target_start + i);
        let mut pos = target_start;
        for (n, (i, c)) in self.text[target_start..line_end].char_indices().enumerate() {
            if n >= col {
                break;
            }
            pos = target_start + i + c.len_utf8();
        }
        self.cursor = pos;
    }

    fn history_prev(&mut self) -> bool {
        if self.history.is_empty() {
            return false;
        }
        let next = match self.hist_pos {
            None => {
                self.stash = self.text.clone();
                self.history.len() - 1
            }
            Some(0) => return false,
            Some(i) => i - 1,
        };
        self.hist_pos = Some(next);
        self.text = self.escape(&self.history[next].clone());
        self.cursor = self.text.len();
        true
    }

    fn history_next(&mut self) -> bool {
        let Some(i) = self.hist_pos else {
            return false;
        };
        if i + 1 < self.history.len() {
            self.hist_pos = Some(i + 1);
            self.text = self.escape(&self.history[i + 1].clone());
        } else {
            self.hist_pos = None;
            self.text = std::mem::take(&mut self.stash);
        }
        self.cursor = self.text.len();
        true
    }

    /// Sends the draft: returns the expanded text and clears the editor.
    pub fn submit(&mut self) -> Option<String> {
        let full = self.text();
        if full.trim().is_empty() {
            return None;
        }
        self.history.push(full.clone());
        self.text.clear();
        self.cursor = 0;
        self.pastes.clear();
        self.hist_pos = None;
        self.stash.clear();
        self.completion = None;
        self.dismissed = false;
        self.scroll = 0;
        Some(full)
    }

    // ---- completion ----

    /// The command token being typed, when completion applies.
    fn slash_token(&self) -> Option<&str> {
        let rest = self.text.strip_prefix('/')?;
        let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
        (self.cursor <= end + 1).then(|| &rest[..end])
    }

    fn refresh_completion(&mut self) {
        let Some(tok) = self.slash_token().map(str::to_lowercase) else {
            self.completion = None;
            self.dismissed = false;
            return;
        };
        if self.dismissed {
            return;
        }
        let mut prefix: Vec<usize> = Vec::new();
        let mut inner: Vec<usize> = Vec::new();
        for (i, c) in self.commands.iter().enumerate() {
            let name = c.name.to_lowercase();
            let bare = name.rsplit(':').next().unwrap_or(&name).to_owned();
            if name.starts_with(&tok) || bare.starts_with(&tok) {
                prefix.push(i);
            } else if name.contains(&tok) {
                inner.push(i);
            }
        }
        prefix.sort_by(|a, b| self.commands[*a].name.cmp(&self.commands[*b].name));
        inner.sort_by(|a, b| self.commands[*a].name.cmp(&self.commands[*b].name));
        prefix.extend(inner);
        let selected = self
            .completion
            .as_ref()
            .map_or(0, |c| c.selected)
            .min(prefix.len().saturating_sub(1));
        self.completion = (!prefix.is_empty()).then_some(Completion {
            matches: prefix,
            selected,
        });
    }

    /// Commands currently offered, with the selected index.
    pub fn completions(&self) -> Option<(Vec<&SlashCommand>, usize)> {
        let c = self.completion.as_ref()?;
        Some((
            c.matches.iter().map(|&i| &self.commands[i]).collect(),
            c.selected,
        ))
    }

    /// Accepts completion row `row`. Returns true if it submits (exact
    /// match without arguments).
    fn accept(&mut self, row: usize) {
        let Some(c) = &self.completion else {
            return;
        };
        let Some(&idx) = c.matches.get(row) else {
            return;
        };
        let name = self.commands[idx].name.clone();
        let rest_start = self.text[1..]
            .find(char::is_whitespace)
            .map_or(self.text.len(), |i| i + 1);
        let rest = self.text[rest_start..].trim_start().to_owned();
        self.text = format!("/{name} {rest}");
        self.cursor = name.len() + 2;
        self.completion = None;
        self.dismissed = true;
    }

    /// Runs a clicked action.
    pub fn activate(&mut self, action: &InputAction) -> InputOutcome {
        match action {
            InputAction::Focus => InputOutcome::Changed,
            InputAction::ExpandPaste(i) => {
                self.expand_paste(*i);
                InputOutcome::Changed
            }
            InputAction::Complete(row) => {
                self.accept(*row);
                InputOutcome::Changed
            }
        }
    }

    /// Handles a key. `action` is what the keymap bound the key to.
    pub fn handle_key(&mut self, key: &KeyEvent, action: Option<Action>) -> InputOutcome {
        if key.kind == KeyEventKind::Release {
            return InputOutcome::Ignored;
        }
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if ctrl && key.code == KeyCode::Char('e') {
            return if self.expand_all() {
                InputOutcome::Changed
            } else {
                InputOutcome::Ignored
            };
        }
        if let Some(c) = &mut self.completion {
            let n = c.matches.len();
            match key.code {
                KeyCode::Up => {
                    c.selected = (c.selected + n - 1) % n;
                    return InputOutcome::Changed;
                }
                KeyCode::Down => {
                    c.selected = (c.selected + 1) % n;
                    return InputOutcome::Changed;
                }
                KeyCode::Tab => {
                    let row = c.selected;
                    self.accept(row);
                    return InputOutcome::Changed;
                }
                KeyCode::Esc => {
                    self.completion = None;
                    self.dismissed = true;
                    return InputOutcome::Changed;
                }
                _ => {}
            }
            if action == Some(Action::Send) {
                let row = c.selected;
                let cmd = &self.commands[c.matches[row]];
                let exact = self.slash_token() == Some(cmd.name.as_str());
                if !(exact && cmd.args.is_none()) {
                    self.accept(row);
                    return InputOutcome::Changed;
                }
            }
        }
        match action {
            Some(Action::Send) => {
                return match self.submit() {
                    Some(t) => InputOutcome::Submit(t),
                    None => InputOutcome::Ignored,
                };
            }
            Some(Action::Newline) => {
                self.insert_char('\n');
                return InputOutcome::Changed;
            }
            _ => {}
        }
        let text_mods = key.modifiers.difference(KeyModifiers::SHIFT).is_empty();
        match key.code {
            KeyCode::Char(c) if text_mods => self.insert_char(c),
            KeyCode::Backspace => match self.prev_char() {
                Some(p) => {
                    self.text.replace_range(p..self.cursor, "");
                    self.cursor = p;
                    self.refresh_completion();
                }
                None => return InputOutcome::Ignored,
            },
            KeyCode::Delete => match self.next_char() {
                Some(n) => {
                    self.text.replace_range(self.cursor..n, "");
                    self.refresh_completion();
                }
                None => return InputOutcome::Ignored,
            },
            KeyCode::Left if text_mods => match self.prev_char() {
                Some(p) => self.cursor = p,
                None => return InputOutcome::Ignored,
            },
            KeyCode::Right if text_mods => match self.next_char() {
                Some(n) => self.cursor = n,
                None => return InputOutcome::Ignored,
            },
            KeyCode::Home => self.cursor = self.line_start(),
            KeyCode::End => self.cursor = self.line_end(),
            KeyCode::Up if text_mods => {
                if self.is_first_line() {
                    if !self.history_prev() {
                        return InputOutcome::Ignored;
                    }
                } else {
                    self.move_vertical(true);
                }
            }
            KeyCode::Down if text_mods => {
                if self.is_last_line() {
                    if !self.history_next() {
                        return InputOutcome::Ignored;
                    }
                } else {
                    self.move_vertical(false);
                }
            }
            _ => return InputOutcome::Ignored,
        }
        InputOutcome::Changed
    }

    // ---- layout ----

    /// Display rows `(cells, cursor)` at `width`: each row is a list of
    /// `(text, paste index)` pieces. Returns the rows and the cursor's
    /// `(row, col)`.
    fn rows(&self, width: usize) -> (Vec<Row>, (usize, usize)) {
        let avail = width.saturating_sub(width_of(PROMPT)).max(1);
        let mut rows: Vec<Row> = vec![Vec::new()];
        let mut w = 0;
        let mut cursor = (0, 0);
        let mut pos = 0;
        let push = |rows: &mut Vec<Row>, s: String, p: Option<usize>| {
            let row = rows.last_mut().expect("row");
            match (row.last_mut(), p) {
                (Some((t, None)), None) => t.push_str(&s),
                _ => row.push((s, p)),
            }
        };
        for c in self.text.chars() {
            if pos == self.cursor {
                cursor = (rows.len() - 1, w);
            }
            pos += c.len_utf8();
            if c == '\n' {
                rows.push(Vec::new());
                w = 0;
                continue;
            }
            let (s, p) = match paste_index(c) {
                Some(i) if self.is_literal(i) => (self.label(i), None),
                Some(i) => (self.label(i), Some(i)),
                // Tabs are shown as spaces to the next tab stop.
                None if c == '\t' => (" ".repeat(TAB_WIDTH - w % TAB_WIDTH), None),
                None => (c.to_string(), None),
            };
            let cw = width_of(&s);
            if w + cw > avail && w > 0 {
                rows.push(Vec::new());
                w = 0;
                if pos - c.len_utf8() == self.cursor {
                    cursor = (rows.len() - 1, 0);
                }
            }
            push(&mut rows, s, p);
            w += cw;
        }
        if pos == self.cursor {
            if w >= avail {
                rows.push(Vec::new());
                w = 0;
            }
            cursor = (rows.len() - 1, w);
        }
        (rows, cursor)
    }

    /// Rows the input needs at `width` (1..=[`MAX_INPUT_ROWS`]).
    pub fn height(&self, width: u16) -> u16 {
        let (rows, _) = self.rows(width as usize);
        rows.len().clamp(1, MAX_INPUT_ROWS) as u16
    }

    /// Rows the completion popup needs (0 when closed).
    pub fn completion_height(&self) -> u16 {
        self.completion
            .as_ref()
            .map_or(0, |c| c.matches.len().min(MAX_COMPLETIONS) as u16)
    }

    /// Draws the input into `area`, registers it (and placeholders) in
    /// `hits`, and places the cursor when `focused`.
    pub fn render<A: Clone>(
        &mut self,
        frame: &mut Frame<'_>,
        area: Rect,
        theme: &Theme,
        focused: bool,
        hits: &mut HitMap<A>,
        wrap: &dyn Fn(InputAction) -> A,
    ) {
        hits.register(INPUT_ID, area, wrap(InputAction::Focus));
        let (rows, (crow, ccol)) = self.rows(area.width as usize);
        let h = area.height as usize;
        if h == 0 || area.width == 0 {
            return;
        }
        if crow < self.scroll {
            self.scroll = crow;
        } else if crow >= self.scroll + h {
            self.scroll = crow + 1 - h;
        }
        let base = if focused {
            theme.focused()
        } else {
            theme.base()
        };
        let mut lines = Vec::new();
        for (r, row) in rows.iter().enumerate().skip(self.scroll).take(h) {
            let prefix = if r == 0 {
                PROMPT.to_owned()
            } else {
                " ".repeat(width_of(PROMPT))
            };
            let mut spans = vec![Span::styled(prefix, theme.fg(Token::Accent))];
            let mut x = area.x + width_of(PROMPT) as u16;
            for (text, paste) in row {
                let w = width_of(text) as u16;
                match paste {
                    Some(i) => {
                        let id = ElementId(format!("paste.{i}"));
                        let mut st = theme.fg(Token::Highlight);
                        if hits.is_focused(&id) {
                            st = st.patch(theme.focused());
                        }
                        spans.push(Span::styled(text.clone(), st));
                        hits.register(
                            id,
                            Rect::new(
                                x,
                                area.y + (r - self.scroll) as u16,
                                w.min(area.right().saturating_sub(x)),
                                1,
                            ),
                            wrap(InputAction::ExpandPaste(*i)),
                        );
                    }
                    None => spans.push(Span::styled(text.clone(), theme.fg(Token::Fg))),
                }
                x = x.saturating_add(w);
            }
            lines.push(Line::from(spans));
        }
        if self.text.is_empty() {
            lines = vec![Line::from(vec![
                Span::styled(PROMPT, theme.fg(Token::Accent)),
                Span::styled("Type a message, / for commands", theme.fg(Token::Muted)),
            ])];
        }
        frame.render_widget(Paragraph::new(lines).style(base), area);
        if focused {
            let x = area.x + (width_of(PROMPT) + ccol) as u16;
            let y = area.y + (crow - self.scroll) as u16;
            if x < area.right() && y < area.bottom() {
                frame.set_cursor_position(Position::new(x, y));
            }
        }
    }

    /// Draws the completion popup into `area` (sized by
    /// [`Input::completion_height`]) and registers its rows.
    pub fn render_completion<A: Clone>(
        &self,
        frame: &mut Frame<'_>,
        area: Rect,
        theme: &Theme,
        hits: &mut HitMap<A>,
        wrap: &dyn Fn(InputAction) -> A,
    ) {
        let Some((cmds, selected)) = self.completions() else {
            return;
        };
        frame.render_widget(Clear, area);
        let w = area.width as usize;
        let name_w = cmds
            .iter()
            .map(|c| width(&c.name) + c.args.as_ref().map_or(0, |a| width(a) + 1) + 1)
            .max()
            .unwrap_or(0)
            .min(w / 2);
        let first = selected.saturating_sub(MAX_COMPLETIONS - 1);
        let mut lines = Vec::new();
        for (row, (i, c)) in cmds
            .iter()
            .enumerate()
            .skip(first)
            .take(area.height as usize)
            .enumerate()
        {
            let id = ElementId(format!("completion.{i}"));
            let sel = i == selected || hits.is_focused(&id);
            let base = if sel {
                theme.focused()
            } else {
                theme.bg(Token::Surface)
            };
            let mut name = format!("/{}", c.name);
            if let Some(a) = &c.args {
                name.push(' ');
                name.push_str(a);
            }
            let name = format!("{:<name_w$}", truncate(&name, name_w));
            let scope = c.scope.clone().map(|s| format!(" {s}")).unwrap_or_default();
            let desc_w = w.saturating_sub(name_w + 2 + width(&scope));
            let spans = vec![
                Span::styled(" ", base),
                Span::styled(name, base.patch(theme.fg(Token::Accent))),
                Span::styled(" ", base),
                Span::styled(
                    format!("{:<desc_w$}", truncate(&c.description, desc_w)),
                    base.patch(theme.fg(Token::Fg)),
                ),
                Span::styled(scope, base.patch(theme.fg(Token::Muted))),
            ];
            lines.push(Line::from(spans));
            hits.register(
                id,
                Rect::new(area.x, area.y + row as u16, area.width, 1),
                wrap(InputAction::Complete(i)),
            );
        }
        frame.render_widget(Paragraph::new(lines).style(theme.bg(Token::Surface)), area);
    }
}

fn width_of(s: &str) -> usize {
    s.chars().map(|c| c.width().unwrap_or(0)).sum()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interaction::testing::{buffer_text, find_text};
    use crate::interaction::{Dispatch, Interaction};
    use crate::keys::Keymap;
    use crossterm::event::Event;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn press(input: &mut Input, chord: &str) -> InputOutcome {
        let Event::Key(k) = crate::interaction::testing::key(chord) else {
            unreachable!()
        };
        let action = Keymap::defaults().lookup(&k);
        input.handle_key(&k, action)
    }

    fn typ(input: &mut Input, s: &str) {
        for c in s.chars() {
            input.insert_char(c);
        }
    }

    #[test]
    fn multiline_editing_and_cursor_movement() {
        let mut i = Input::new();
        typ(&mut i, "hello");
        press(&mut i, "shift+enter");
        typ(&mut i, "world");
        assert_eq!(i.text(), "hello\nworld");
        press(&mut i, "up");
        press(&mut i, "home");
        typ(&mut i, ">");
        assert_eq!(i.text(), ">hello\nworld");
        press(&mut i, "down");
        press(&mut i, "end");
        press(&mut i, "backspace");
        assert_eq!(i.text(), ">hello\nworl");
        press(&mut i, "left");
        press(&mut i, "delete");
        assert_eq!(i.text(), ">hello\nwor");
        assert_eq!(
            press(&mut i, "enter"),
            InputOutcome::Submit(">hello\nwor".into())
        );
        assert!(i.is_empty());
    }

    #[test]
    fn history_recall() {
        let mut i = Input::new();
        for m in ["one", "two"] {
            typ(&mut i, m);
            press(&mut i, "enter");
        }
        typ(&mut i, "draft");
        press(&mut i, "up");
        assert_eq!(i.text(), "two");
        press(&mut i, "up");
        assert_eq!(i.text(), "one");
        assert_eq!(press(&mut i, "up"), InputOutcome::Ignored);
        press(&mut i, "down");
        assert_eq!(i.text(), "two");
        press(&mut i, "down");
        assert_eq!(i.text(), "draft");
        assert_eq!(i.history(), &["one".to_owned(), "two".to_owned()]);
    }

    #[test]
    fn small_pastes_insert_and_large_pastes_collapse() {
        let mut i = Input::new();
        i.paste("a\r\nb");
        assert_eq!(i.text(), "a\nb");
        assert_eq!(i.placeholders(), 0);
        let big: String = (1..=120).map(|n| format!("line {n}\n")).collect();
        typ(&mut i, " ");
        i.paste(&big);
        assert_eq!(i.placeholders(), 1);
        assert_eq!(i.display(), "a\nb [paste #1 · 120 lines]");
        assert_eq!(i.height(80), 2);
        // Backspace removes the whole placeholder.
        let mut j = i.clone();
        press(&mut j, "backspace");
        assert_eq!(j.text(), "a\nb ");
        // Ctrl+E expands it in place; sending always sends the full text.
        let mut k = i.clone();
        assert_eq!(press(&mut k, "ctrl+e"), InputOutcome::Changed);
        assert_eq!(k.placeholders(), 0);
        assert_eq!(k.text(), format!("a\nb {big}"));
        assert_eq!(
            press(&mut i, "enter"),
            InputOutcome::Submit(format!("a\nb {big}"))
        );
    }

    #[test]
    fn pastes_arrive_exactly_tabs_unicode_and_private_use() {
        let mut i = Input::new();
        let text = "fn main() {\n\tlet s = \"ünïcødé 日本 🚀\";\r\n}\u{F0000}\u{F0001}x";
        i.paste(text);
        let want = "fn main() {\n\tlet s = \"ünïcødé 日本 🚀\";\n}\u{F0000}\u{F0001}x";
        assert_eq!(i.text(), want);
        assert_eq!(i.placeholders(), 0, "private-use chars are not pastes");
        // The display shows tabs as spaces to the next tab stop.
        assert!(i.display().contains("\u{F0000}"));
        let (rows, _) = i.rows(80);
        let row1: String = rows[1].iter().map(|(t, _)| t.as_str()).collect();
        assert_eq!(row1, "    let s = \"ünïcødé 日本 🚀\";");
        // A later collapsed paste is still #1, and Ctrl+E keeps literals.
        i.paste(&"y\n".repeat(20));
        assert!(
            i.display().ends_with("[paste #1 · 20 lines]"),
            "{}",
            i.display()
        );
        press(&mut i, "ctrl+e");
        assert_eq!(i.text(), format!("{want}{}", "y\n".repeat(20)));
        assert_eq!(
            press(&mut i, "enter"),
            InputOutcome::Submit(format!("{want}{}", "y\n".repeat(20)))
        );
        // History recall of a message with private-use chars is exact too.
        press(&mut i, "up");
        assert_eq!(i.text(), format!("{want}{}", "y\n".repeat(20)));
    }

    #[test]
    fn very_large_pastes_collapse_and_send_in_full() {
        let mut i = Input::new();
        let big: String = (0..50_000).map(|n| format!("{n}\tline ü\n")).collect();
        i.paste(&big);
        assert_eq!(i.placeholders(), 1);
        assert_eq!(i.display(), "[paste #1 · 50000 lines]");
        assert_eq!(i.height(80), 1);
        assert_eq!(press(&mut i, "enter"), InputOutcome::Submit(big));
    }

    #[test]
    fn tab_moves_the_cursor_to_the_next_stop() {
        let mut i = Input::new();
        i.paste("a\tb");
        let (_, cursor) = i.rows(80);
        assert_eq!(cursor, (0, 5), "a + 3 spaces + b");
    }

    fn commands() -> Vec<SlashCommand> {
        vec![
            SlashCommand {
                name: "review".into(),
                description: "Review the diff".into(),
                args: Some("[branch]".into()),
                scope: Some("project".into()),
            },
            SlashCommand::new("help", "Show keys"),
            SlashCommand::new("compact", "Compact the context"),
        ]
    }

    #[test]
    fn slash_opens_completion_with_descriptions_and_args() {
        let mut i = Input::new();
        i.set_commands(commands());
        typ(&mut i, "/");
        let (c, sel) = i.completions().unwrap();
        assert_eq!(c.len(), 3);
        assert_eq!(sel, 0);
        typ(&mut i, "re");
        let (c, _) = i.completions().unwrap();
        assert_eq!(
            c.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(),
            vec!["review"]
        );
        // Enter completes a command that takes arguments.
        assert_eq!(press(&mut i, "enter"), InputOutcome::Changed);
        assert_eq!(i.text(), "/review ");
        assert!(i.completions().is_none());
        // Esc closes; Down/Up select; Tab completes.
        let mut i = Input::new();
        i.set_commands(commands());
        typ(&mut i, "/");
        press(&mut i, "down");
        assert_eq!(i.completions().unwrap().1, 1);
        press(&mut i, "tab");
        assert_eq!(i.text(), "/help ");
        let mut i = Input::new();
        i.set_commands(commands());
        typ(&mut i, "/c");
        assert_eq!(press(&mut i, "esc"), InputOutcome::Changed);
        assert!(i.completions().is_none());
        // Enter on an exact match without arguments sends it.
        let mut i = Input::new();
        i.set_commands(commands());
        typ(&mut i, "/help");
        assert_eq!(press(&mut i, "enter"), InputOutcome::Submit("/help".into()));
        // Not at the start: no completion.
        let mut i = Input::new();
        i.set_commands(commands());
        typ(&mut i, "see /help");
        assert!(i.completions().is_none());
    }

    fn draw(input: &mut Input, it: &mut Interaction<InputAction>) -> Terminal<TestBackend> {
        let theme = Theme::default();
        let mut term = Terminal::new(TestBackend::new(60, 6)).unwrap();
        let mut hits = it.begin_frame();
        term.draw(|f| {
            let ch = input.completion_height();
            let area = Rect::new(0, 5 - ch, 60, ch);
            input.render_completion(f, area, &theme, &mut hits, &|a| a);
            input.render(f, Rect::new(0, 5, 60, 1), &theme, true, &mut hits, &|a| a);
        })
        .unwrap();
        it.end_frame(hits);
        term
    }

    #[test]
    fn completion_rows_and_placeholders_are_clickable_and_focusable() {
        let mut input = Input::new();
        input.set_commands(commands());
        typ(&mut input, "/");
        let mut it = Interaction::new();
        let term = draw(&mut input, &mut it);
        let text = buffer_text(term.backend().buffer());
        assert!(text.contains("/review [branch]"), "{text}");
        assert!(text.contains("Review the diff"));
        assert!(text.contains("project"));
        let (x, y) = find_text(term.backend().buffer(), "/help").unwrap();
        let ev = crossterm::event::MouseEvent {
            kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
            column: x,
            row: y,
            modifiers: KeyModifiers::NONE,
        };
        let Some(Dispatch::Activate(a)) = it.handle_mouse(&ev) else {
            panic!("no hit")
        };
        input.activate(&a);
        assert_eq!(input.text(), "/help ");

        // A placeholder is a focusable element: Tab to it, Enter expands.
        let mut input = Input::new();
        input.paste(&"x\n".repeat(50));
        let mut it = Interaction::new();
        let term = draw(&mut input, &mut it);
        assert!(buffer_text(term.backend().buffer()).contains("[paste #1 · 50 lines]"));
        let tab = KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE);
        it.handle_key(&tab); // input
        it.handle_key(&tab); // placeholder
        let Some(Dispatch::Activate(a)) =
            it.handle_key(&KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
        else {
            panic!("no activation")
        };
        assert_eq!(a, InputAction::ExpandPaste(0));
        input.activate(&a);
        assert_eq!(input.placeholders(), 0);
    }

    #[test]
    fn long_lines_wrap_and_cursor_follows() {
        let mut i = Input::new();
        typ(&mut i, &"x".repeat(99));
        assert_eq!(i.height(52), 2);
        assert_eq!(i.rows(52).1, (1, 49));
        typ(&mut i, "x");
        // A full last row puts the cursor at the start of the next one.
        assert_eq!(i.height(52), 3);
        assert_eq!(i.rows(52).1, (2, 0));
    }
}
