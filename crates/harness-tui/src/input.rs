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
//!   each command's arguments, description and scope. Matching uses the
//!   palette's fuzzy matcher ([`crate::fuzzy`]): gaps are allowed (`/mdl`
//!   finds `/model`), namespaced commands match on the bare and the full
//!   name, and descriptions are searched too. Rows rank exact name matches
//!   first, then name prefixes, then other name matches by score, then
//!   description matches; ties keep alphabetical order. Matched characters
//!   are highlighted. Up/Down select, Tab or Enter complete (Enter on an
//!   exact match without arguments sends it), Esc closes, and rows are
//!   clickable.
//! - Argument choices: after `/name ` of a command whose first argument
//!   has fixed choices ([`SlashCommand::choices`], e.g. `/effort low`), the
//!   same popup offers the choices that start with what was typed. Up/Down
//!   select, Tab or Enter complete (Enter with nothing typed yet or on a
//!   fully typed choice sends), Esc closes, and rows are clickable.

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::{Position, Rect};
use ratatui::style::Modifier;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph};
use unicode_width::UnicodeWidthChar;

use crate::chat::text::{sanitize, truncate, truncate_line, width, wrap_line};
use crate::fuzzy::{self, Fuzzy};
use crate::interaction::{ElementId, HitMap};
use crate::keys::Action;
use crate::selection::{Join, TextRegion};
use crate::theme::{Theme, Token};
use nucleo_matcher::Utf32String;

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
    /// Fixed values of the first argument, offered by completion after
    /// `/name ` (empty: free text).
    pub choices: Vec<String>,
}

impl SlashCommand {
    /// A command without arguments or scope.
    pub fn new(name: impl Into<String>, description: impl Into<String>) -> SlashCommand {
        SlashCommand {
            name: name.into(),
            description: description.into(),
            args: None,
            scope: None,
            choices: Vec::new(),
        }
    }

    /// The same command with first-argument `choices`.
    pub fn with_choices(mut self, choices: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.choices = choices.into_iter().map(Into::into).collect();
        self
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
    matches: Vec<CompletionMatch>,
    selected: usize,
    /// The token the matches were computed for.
    token: String,
}

/// Completion of a command's first argument from its choices.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct ChoiceCompletion {
    /// Index into [`Input::commands`].
    command: usize,
    /// Indices into that command's choices, in declaration order.
    matches: Vec<usize>,
    selected: usize,
    /// What was typed of the argument.
    partial: String,
}

/// One ranked completion row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CompletionMatch {
    /// Index into [`Input::commands`].
    command: usize,
    /// What matched, for highlighting.
    field: Field,
}

/// The part of a command a query matched.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Field {
    /// Nothing (empty query).
    None,
    /// The full name, e.g. `project:deploy`.
    Full,
    /// The name after the last `:`, e.g. `deploy`.
    Bare,
    /// The description.
    Description,
}

/// Per-command match keys, converted once in [`Input::set_commands`] so a
/// keystroke only scores.
#[derive(Debug, Clone)]
struct CommandKeys {
    full: Utf32String,
    description: Utf32String,
    full_lower: String,
    bare_lower: String,
    /// Characters before the bare name (namespace and colon).
    ns: u32,
}

impl CommandKeys {
    fn new(c: &SlashCommand) -> CommandKeys {
        let (ns, bare) = match c.name.rsplit_once(':') {
            Some((ns, bare)) => (ns.chars().count() as u32 + 1, bare),
            None => (0, c.name.as_str()),
        };
        CommandKeys {
            full: Utf32String::from(c.name.as_str()),
            description: Utf32String::from(c.description.as_str()),
            full_lower: c.name.to_lowercase(),
            bare_lower: bare.to_lowercase(),
            ns,
        }
    }
}

/// Rank tiers of a completion match, best first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Tier {
    Exact,
    Prefix,
    Name,
    Description,
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
    /// Match keys, parallel to `commands`.
    keys: Vec<CommandKeys>,
    /// Command indices in alphabetical order (the tie-break).
    order: Vec<usize>,
    completion: Option<Completion>,
    dismissed: bool,
    /// Argument-choice completion (never open together with `completion`).
    choice: Option<ChoiceCompletion>,
    /// Esc closed the choice popup, or a choice was just accepted; it
    /// stays closed until the text leaves the argument position.
    choice_dismissed: bool,
    scroll: usize,
    fuzzy: Fuzzy,
    /// Width of the last render, for moving the cursor by display rows.
    width: usize,
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
        self.keys = commands.iter().map(CommandKeys::new).collect();
        let mut order: Vec<usize> = (0..commands.len()).collect();
        order.sort_by(|&a, &b| commands[a].name.cmp(&commands[b].name).then(a.cmp(&b)));
        self.order = order;
        self.commands = commands;
        self.refresh(false);
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
        self.choice = None;
        self.choice_dismissed = false;
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
        self.refresh(true);
    }

    /// Recomputes the completion. `narrow` allows reusing the previous
    /// matches as candidates (false after the command list changed).
    fn refresh(&mut self, narrow: bool) {
        let Some(tok) = self.slash_token().map(str::to_owned) else {
            self.completion = None;
            self.dismissed = false;
            self.refresh_choice();
            return;
        };
        self.choice = None;
        self.choice_dismissed = false;
        if self.dismissed {
            return;
        }
        let matches = self.rank(&tok, narrow);
        let selected = match &self.completion {
            Some(c) if c.token == tok => c.selected.min(matches.len().saturating_sub(1)),
            _ => 0,
        };
        self.completion = (!matches.is_empty()).then_some(Completion {
            matches,
            selected,
            token: tok,
        });
    }

    /// Commands matching `tok`, best first. An empty token lists every
    /// command alphabetically.
    ///
    /// Each command is scored once, on its full name: a gapped match on
    /// the bare name is also one on the full name, and the exact and
    /// prefix tiers compare the bare name directly. When `tok` extends the
    /// previous token only the previous matches can still match, so typing
    /// forward narrows the candidates instead of rescanning every command.
    fn rank(&mut self, tok: &str, narrow: bool) -> Vec<CompletionMatch> {
        if tok.is_empty() {
            return self
                .order
                .iter()
                .map(|&command| CompletionMatch {
                    command,
                    field: Field::None,
                })
                .collect();
        }
        let narrowed: Option<Vec<bool>> = self
            .completion
            .as_ref()
            .filter(|c| narrow && !c.token.is_empty() && tok.starts_with(c.token.as_str()))
            .map(|c| {
                let mut keep = vec![false; self.commands.len()];
                for m in &c.matches {
                    keep[m.command] = true;
                }
                keep
            });
        let lower = tok.to_lowercase();
        let name_pat = fuzzy::fuzzy(tok);
        let desc_pat = fuzzy::substring(tok);
        let mut scored: Vec<(Tier, u32, usize, CompletionMatch)> = Vec::new();
        for (rank, &i) in self.order.iter().enumerate() {
            if narrowed.as_ref().is_some_and(|keep| !keep[i]) {
                continue;
            }
            let k = &self.keys[i];
            let (tier, score, field) = match self.fuzzy.score(&name_pat, k.full.slice(..)) {
                None => match self.fuzzy.score(&desc_pat, k.description.slice(..)) {
                    Some(sc) => (Tier::Description, sc, Field::Description),
                    None => continue,
                },
                Some(sc) => {
                    let full_prefix = k.full_lower.starts_with(&lower);
                    let bare_prefix = k.ns > 0 && k.bare_lower.starts_with(&lower);
                    let tier = if k.full_lower == lower || (k.ns > 0 && k.bare_lower == lower) {
                        Tier::Exact
                    } else if full_prefix || bare_prefix {
                        Tier::Prefix
                    } else {
                        Tier::Name
                    };
                    // Highlight the bare name when that is what was typed.
                    let field = if bare_prefix && !full_prefix {
                        Field::Bare
                    } else {
                        Field::Full
                    };
                    (tier, sc, field)
                }
            };
            scored.push((tier, score, rank, CompletionMatch { command: i, field }));
        }
        scored.sort_unstable_by(|a, b| a.0.cmp(&b.0).then(b.1.cmp(&a.1)).then(a.2.cmp(&b.2)));
        scored.into_iter().map(|(.., m)| m).collect()
    }

    /// The command and partial argument being typed, when the text is
    /// `/name partial` with the cursor at its end and `name` is a command
    /// with argument choices.
    fn choice_target(&self) -> Option<(usize, String)> {
        let rest = self.text.strip_prefix('/')?;
        let (name, arg) = rest.split_once(' ')?;
        if arg.contains(char::is_whitespace) || self.cursor != self.text.len() {
            return None;
        }
        let command = self
            .commands
            .iter()
            .position(|c| c.name == name && !c.choices.is_empty())?;
        Some((command, arg.to_owned()))
    }

    /// Recomputes the argument-choice completion.
    fn refresh_choice(&mut self) {
        let Some((command, partial)) = self.choice_target() else {
            self.choice = None;
            self.choice_dismissed = false;
            return;
        };
        if self.choice_dismissed {
            self.choice = None;
            return;
        }
        let lower = partial.to_lowercase();
        let matches: Vec<usize> = self.commands[command]
            .choices
            .iter()
            .enumerate()
            .filter(|(_, c)| c.to_lowercase().starts_with(&lower))
            .map(|(i, _)| i)
            .collect();
        let selected = match &self.choice {
            Some(c) if c.command == command => c.selected.min(matches.len().saturating_sub(1)),
            _ => 0,
        };
        self.choice = (!matches.is_empty()).then_some(ChoiceCompletion {
            command,
            matches,
            selected,
            partial,
        });
    }

    /// Argument choices currently offered, with the selected index.
    pub fn choice_completions(&self) -> Option<(Vec<&str>, usize)> {
        let c = self.choice.as_ref()?;
        let choices = &self.commands[c.command].choices;
        Some((
            c.matches.iter().map(|&i| choices[i].as_str()).collect(),
            c.selected,
        ))
    }

    /// Accepts argument-choice row `row`: `/name choice`.
    fn accept_choice(&mut self, row: usize) {
        let Some(c) = &self.choice else {
            return;
        };
        let Some(&i) = c.matches.get(row) else {
            return;
        };
        let cmd = &self.commands[c.command];
        self.text = format!("/{} {}", cmd.name, cmd.choices[i]);
        self.cursor = self.text.len();
        self.choice = None;
        self.choice_dismissed = true;
    }

    /// Commands currently offered, with the selected index.
    pub fn completions(&self) -> Option<(Vec<&SlashCommand>, usize)> {
        let c = self.completion.as_ref()?;
        Some((
            c.matches
                .iter()
                .map(|m| &self.commands[m.command])
                .collect(),
            c.selected,
        ))
    }

    /// Accepts completion row `row`. Returns true if it submits (exact
    /// match without arguments).
    fn accept(&mut self, row: usize) {
        let Some(c) = &self.completion else {
            return;
        };
        let Some(idx) = c.matches.get(row).map(|m| m.command) else {
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
            InputAction::Complete(row) if self.choice.is_some() => {
                self.accept_choice(*row);
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
        if let Some(c) = &mut self.choice {
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
                    self.accept_choice(row);
                    return InputOutcome::Changed;
                }
                KeyCode::Esc => {
                    self.choice = None;
                    self.choice_dismissed = true;
                    return InputOutcome::Changed;
                }
                _ => {}
            }
            if action == Some(Action::Send) {
                let row = c.selected;
                let choice = &self.commands[c.command].choices[c.matches[row]];
                // Nothing typed yet: Enter sends the bare command (which
                // may open its own picker); a fully typed choice sends too.
                if !c.partial.is_empty() && *choice != c.partial {
                    self.accept_choice(row);
                    return InputOutcome::Changed;
                }
            }
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
                let cmd = &self.commands[c.matches[row].command];
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
            KeyCode::Up if text_mods && self.move_row(true) => {}
            KeyCode::Down if text_mods && self.move_row(false) => {}
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

    /// Moves the cursor one display row up or down at the last rendered
    /// width (rows of wrapped lines included), keeping its column. False
    /// on the first (last) row, where Up (Down) recalls history, or before
    /// the first render.
    fn move_row(&mut self, up: bool) -> bool {
        if self.width == 0 {
            return false;
        }
        let (rows, (crow, ccol), _) = self.layout_rows(self.width);
        let target = if up {
            if crow == 0 {
                return false;
            }
            crow - 1
        } else {
            if crow + 1 >= rows.len() {
                return false;
            }
            crow + 1
        };
        // The last character starting at or before `ccol` on the target
        // row; a wrapped row's end is its last character.
        let mut best = None;
        for (row, col, byte) in self.cells(self.width) {
            if row == target && (col <= ccol || best.is_none()) {
                best = Some(byte);
            }
        }
        match best {
            Some(b) => {
                self.cursor = b;
                true
            }
            None => false,
        }
    }

    /// `(row, column, byte)` of each character start, laid out like
    /// [`Input::layout_rows`].
    fn cells(&self, width: usize) -> Vec<(usize, usize, usize)> {
        let avail = width.saturating_sub(width_of(PROMPT)).max(1);
        let (mut row, mut w, mut pos) = (0, 0, 0);
        let mut out = Vec::new();
        for c in self.text.chars() {
            let start = pos;
            pos += c.len_utf8();
            if c == '\n' {
                // The end of the line, then the start of the next one.
                out.push((row, w, start));
                row += 1;
                w = 0;
                out.push((row, 0, pos));
                continue;
            }
            let cw = match paste_index(c) {
                Some(i) => width_of(&self.label(i)),
                None if c == '\t' => TAB_WIDTH - w % TAB_WIDTH,
                None => width_of(&c.to_string()).max(usize::from(c.is_control())),
            };
            if w + cw > avail && w > 0 {
                row += 1;
                w = 0;
            }
            out.push((row, w, start));
            w += cw;
        }
        out.push((row, w, pos));
        out
    }

    // ---- layout ----

    /// Display rows `(cells, cursor)` at `width`: each row is a list of
    /// `(text, paste index)` pieces. Returns the rows and the cursor's
    /// `(row, col)`.
    fn rows(&self, width: usize) -> (Vec<Row>, (usize, usize)) {
        let (rows, cursor, _) = self.layout_rows(width);
        (rows, cursor)
    }

    /// [`Input::rows`], plus for each row whether it starts a line of the
    /// text (the first row, or after a newline) rather than continuing a
    /// width-wrapped one.
    fn layout_rows(&self, width: usize) -> (Vec<Row>, (usize, usize), Vec<bool>) {
        let avail = width.saturating_sub(width_of(PROMPT)).max(1);
        let mut rows: Vec<Row> = vec![Vec::new()];
        let mut hard = vec![true];
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
                hard.push(true);
                w = 0;
                continue;
            }
            let (s, p) = match paste_index(c) {
                Some(i) if self.is_literal(i) => (self.label(i), None),
                Some(i) => (self.label(i), Some(i)),
                // Tabs are shown as spaces to the next tab stop, other
                // control characters as a space.
                None if c == '\t' => (" ".repeat(TAB_WIDTH - w % TAB_WIDTH), None),
                None if c.is_control() => (" ".to_owned(), None),
                None => (c.to_string(), None),
            };
            let cw = width_of(&s);
            if w + cw > avail && w > 0 {
                rows.push(Vec::new());
                hard.push(false);
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
                hard.push(false);
                w = 0;
            }
            cursor = (rows.len() - 1, w);
        }
        (rows, cursor, hard)
    }

    /// Registers the visible text (without the prompt) for text selection.
    /// A row that continues a width-wrapped row joins it with a space when
    /// the wrap fell on one, else directly.
    fn register_text(&self, area: Rect, rows: &[Row], hard: &[bool], h: usize) {
        let prompt = width_of(PROMPT) as u16;
        let rect = Rect {
            x: area.x + prompt.min(area.width),
            width: area.width.saturating_sub(prompt),
            ..area
        };
        let mut region = TextRegion::new("input", rect).scrolled(self.scroll);
        let joins = (self.scroll..self.scroll + h).map(|r| {
            if hard.get(r).is_none_or(|h| *h) {
                Join::Hard
            } else if rows
                .get(r - 1)
                .and_then(|row| row.last())
                .is_some_and(|(t, _)| t.ends_with(' '))
            {
                Join::Space
            } else {
                Join::Direct
            }
        });
        region.set_joins(joins);
        crate::selection::register(region);
    }

    /// Rows the input needs at `width` (1..=[`MAX_INPUT_ROWS`]).
    pub fn height(&self, width: u16) -> u16 {
        let (rows, _) = self.rows(width as usize);
        rows.len().clamp(1, MAX_INPUT_ROWS) as u16
    }

    /// Rows the completion popup needs (0 when closed).
    pub fn completion_height(&self) -> u16 {
        if let Some(c) = &self.choice {
            return c.matches.len().min(MAX_COMPLETIONS) as u16;
        }
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
        self.width = area.width as usize;
        let (rows, (crow, ccol), hard) = self.layout_rows(area.width as usize);
        let h = area.height as usize;
        if h == 0 || area.width == 0 {
            return;
        }
        if crow < self.scroll {
            self.scroll = crow;
        } else if crow >= self.scroll + h {
            self.scroll = crow + 1 - h;
        }
        if !self.text.is_empty() {
            self.register_text(area, &rows, &hard, h);
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
                        hits.register_button(
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

    /// Rows the completion popup needs at `width` (0 when closed): one
    /// per offered command (up to [`MAX_COMPLETIONS`]) plus, when the
    /// selected row cuts its name or description, the extra rows that
    /// command takes wrapped in full.
    pub fn completion_height_at(&self, width: u16) -> u16 {
        let n = self.completion_height();
        if n == 0 || self.choice.is_some() {
            return n;
        }
        let sel = self.selected_full(width as usize).map_or(1, |l| l.len());
        n - 1 + sel as u16
    }

    /// Width of the name column at `w` cells.
    fn completion_name_w(&self, w: usize) -> usize {
        let Some(comp) = &self.completion else {
            return 0;
        };
        comp.matches
            .iter()
            .map(|m| {
                let c = &self.commands[m.command];
                width(&sanitize(&c.name))
                    + c.args.as_ref().map_or(0, |a| width(&sanitize(a)) + 1)
                    + 1
            })
            .max()
            .unwrap_or(0)
            .min(w / 2)
    }

    /// The selected command's full text wrapped to `w` cells, when its row
    /// would cut it.
    fn selected_full(&self, w: usize) -> Option<Vec<Line<'static>>> {
        let comp = self.completion.as_ref()?;
        let c = &self.commands[comp.matches.get(comp.selected)?.command];
        let name_w = self.completion_name_w(w);
        let (name, desc, scope) = completion_texts(c);
        let desc_w = w.saturating_sub(name_w + 2 + width(&scope));
        let cut = width(&name) > name_w || width(&desc) > desc_w || name_w + 2 + width(&scope) > w;
        cut.then(|| {
            let line = Line::from(vec![
                Span::raw(" "),
                Span::raw(name),
                Span::raw("  "),
                Span::raw(desc),
                Span::raw(scope),
            ]);
            wrap_line(line, w, &[Span::raw("   ")])
        })
    }

    /// Draws the completion popup into `area` (sized by
    /// [`Input::completion_height_at`]) and registers its rows. Rows show
    /// one command each, cut to align; the selected command shows in full,
    /// wrapped over as many rows as it needs.
    pub fn render_completion<A: Clone>(
        &mut self,
        frame: &mut Frame<'_>,
        area: Rect,
        theme: &Theme,
        hits: &mut HitMap<A>,
        wrap: &dyn Fn(InputAction) -> A,
    ) {
        if self.choice.is_some() {
            return self.render_choices(frame, area, theme, hits, wrap);
        }
        let w = area.width as usize;
        let name_w = self.completion_name_w(w);
        let full = self.selected_full(w);
        let Input {
            completion: Some(comp),
            commands,
            keys,
            fuzzy: matcher,
            ..
        } = self
        else {
            return;
        };
        let selected = comp.selected;
        let n = comp.matches.len();
        let height = area.height as usize;
        frame.render_widget(Clear, area);
        if height == 0 || w == 0 || n == 0 {
            return;
        }
        let name_pat = fuzzy::fuzzy(&comp.token);
        let desc_pat = fuzzy::substring(&comp.token);
        let mut idx = Vec::new();
        // One row per command and all of the selected command's rows: as
        // many commands above it as fit, the rest below.
        let sel_h = full.as_ref().map_or(1, |l| l.len()).min(height);
        let first = selected - selected.min(height - sel_h);
        let mut lines: Vec<Line<'static>> = Vec::new();
        let mut i = first;
        while i < n && lines.len() < height {
            let c = &commands[comp.matches[i].command];
            let id = ElementId(format!("completion.{i}"));
            let sel = i == selected || hits.is_focused(&id);
            let base = if sel {
                theme.focused()
            } else {
                theme.bg(Token::Surface)
            };
            let m = comp.matches[i];
            let hit = base
                .patch(theme.fg(Token::Highlight))
                .add_modifier(Modifier::BOLD);
            let (full_name, full_desc, scope) = completion_texts(c);
            let expanded = i == selected && full.is_some();
            let (name, desc) = if expanded {
                (full_name, full_desc)
            } else {
                let desc_w = w.saturating_sub(name_w + 2 + width(&scope));
                (
                    format!("{:<name_w$}", truncate(&full_name, name_w)),
                    format!("{:<desc_w$}", truncate(&full_desc, desc_w)),
                )
            };
            // Highlights: name positions shift by one for the slash,
            // bare-name positions also by the namespace.
            let ns = keys[m.command].ns;
            let mut name_hits = Vec::new();
            let mut desc_hits = Vec::new();
            match m.field {
                Field::None => {}
                Field::Full => {
                    matcher.indices(&name_pat, &c.name, &mut idx);
                    name_hits = visible_hits(&name, idx.iter().map(|&p| p + 1));
                }
                Field::Bare => {
                    let bare = c.name.rsplit(':').next().unwrap_or(&c.name);
                    matcher.indices(&name_pat, bare, &mut idx);
                    name_hits = visible_hits(&name, idx.iter().map(|&p| p + ns + 1));
                }
                Field::Description => {
                    matcher.indices(&desc_pat, &c.description, &mut idx);
                    desc_hits = visible_hits(&desc, idx.iter().copied());
                }
            }
            let mut spans = vec![Span::styled(" ", base)];
            spans.extend(fuzzy::highlight(
                &name,
                &name_hits,
                base.patch(theme.fg(Token::Accent)),
                hit,
            ));
            spans.push(Span::styled(if expanded { "  " } else { " " }, base));
            spans.extend(fuzzy::highlight(
                &desc,
                &desc_hits,
                base.patch(theme.fg(Token::Fg)),
                hit,
            ));
            spans.push(Span::styled(scope, base.patch(theme.fg(Token::Muted))));
            let rows: Vec<Line<'static>> = if expanded {
                wrap_line(Line::from(spans), w, &[Span::styled("   ", base)])
                    .into_iter()
                    .map(|l| l.style(base))
                    .take(height - lines.len())
                    .collect()
            } else {
                vec![truncate_line(Line::from(spans), w)]
            };
            hits.register(
                id,
                Rect::new(
                    area.x,
                    area.y + lines.len() as u16,
                    area.width,
                    rows.len() as u16,
                ),
                wrap(InputAction::Complete(i)),
            );
            lines.extend(rows);
            i += 1;
        }
        frame.render_widget(Paragraph::new(lines).style(theme.bg(Token::Surface)), area);
    }
}

impl Input {
    /// Draws the argument-choice popup: one row per choice, the typed part
    /// highlighted, with the full command line it completes to.
    fn render_choices<A: Clone>(
        &self,
        frame: &mut Frame<'_>,
        area: Rect,
        theme: &Theme,
        hits: &mut HitMap<A>,
        wrap: &dyn Fn(InputAction) -> A,
    ) {
        let Some(comp) = &self.choice else {
            return;
        };
        frame.render_widget(Clear, area);
        let (w, height, n) = (
            area.width as usize,
            area.height as usize,
            comp.matches.len(),
        );
        if height == 0 || w == 0 || n == 0 {
            return;
        }
        let cmd = &self.commands[comp.command];
        let choice_w = cmd
            .choices
            .iter()
            .map(|c| width(&sanitize(c)))
            .max()
            .unwrap_or(0)
            .min(w / 2);
        let typed = comp.partial.chars().count() as u32;
        let first = comp.selected - comp.selected.min(height - 1);
        let mut lines: Vec<Line<'static>> = Vec::new();
        for (row, &i) in comp.matches.iter().enumerate().skip(first).take(height) {
            let id = ElementId(format!("completion.{row}"));
            let base = if row == comp.selected || hits.is_focused(&id) {
                theme.focused()
            } else {
                theme.bg(Token::Surface)
            };
            let hit = base
                .patch(theme.fg(Token::Highlight))
                .add_modifier(Modifier::BOLD);
            let choice = format!(
                "{:<choice_w$}",
                truncate(&sanitize(&cmd.choices[i]), choice_w)
            );
            let mut spans = vec![Span::styled(" ", base)];
            spans.extend(fuzzy::highlight(
                &choice,
                &(0..typed).collect::<Vec<_>>(),
                base.patch(theme.fg(Token::Accent)),
                hit,
            ));
            spans.push(Span::styled("  ", base));
            spans.push(Span::styled(
                format!("/{} {}", sanitize(&cmd.name), sanitize(&cmd.choices[i])),
                base.patch(theme.fg(Token::Muted)),
            ));
            hits.register(
                id,
                Rect::new(area.x, area.y + lines.len() as u16, area.width, 1),
                wrap(InputAction::Complete(row)),
            );
            lines.push(truncate_line(Line::from(spans), w).style(base));
        }
        frame.render_widget(Paragraph::new(lines).style(theme.bg(Token::Surface)), area);
    }
}

/// A command's name (with slash and argument hint), description and scope
/// as shown in completion.
fn completion_texts(c: &SlashCommand) -> (String, String, String) {
    let mut name = format!("/{}", sanitize(&c.name));
    if let Some(a) = &c.args {
        name.push(' ');
        name.push_str(&sanitize(a));
    }
    let desc = sanitize(&c.description).replace('\n', " ");
    let scope = c
        .scope
        .as_ref()
        .map(|s| format!(" {}", sanitize(s)))
        .unwrap_or_default();
    (name, desc, scope)
}

/// Match positions that are still on screen in `shown` (a possibly
/// truncated copy): the ellipsis that ends a truncated text is never
/// highlighted.
fn visible_hits(shown: &str, hits: impl Iterator<Item = u32>) -> Vec<u32> {
    let chars: Vec<char> = shown.chars().collect();
    hits.filter(|&p| chars.get(p as usize).is_some_and(|&c| c != '…' && c != ' '))
        .collect()
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
                choices: Vec::new(),
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

    /// Up and Down move the cursor by display rows inside a wrapped line
    /// (keeping the column), so every row of a long draft is reachable;
    /// on the first row Up still recalls history.
    #[test]
    fn up_and_down_walk_the_rows_of_a_wrapped_line() {
        let mut input = Input::new();
        let line = "abcdefghij".repeat(5);
        input.set_text(&line);
        let theme = Theme::default();
        let mut term = Terminal::new(TestBackend::new(22, 3)).unwrap();
        let mut draw = |input: &mut Input| {
            term.draw(|f| {
                let mut hits: HitMap<()> = HitMap::new();
                input.render(f, f.area(), &theme, true, &mut hits, &|_| ());
            })
            .unwrap();
            buffer_text(term.backend().buffer())
        };
        // 20 cells per row after the prompt: rows of 20, 20 and 10.
        draw(&mut input);
        assert_eq!(input.cursor(), 50);
        assert_eq!(press(&mut input, "up"), InputOutcome::Changed);
        assert_eq!(input.cursor(), 30, "same column one row up");
        assert_eq!(press(&mut input, "up"), InputOutcome::Changed);
        assert_eq!(input.cursor(), 10);
        let text = draw(&mut input);
        assert!(
            text.starts_with("› abcdefghij"),
            "scrolled to the top: {text}"
        );
        assert_eq!(press(&mut input, "up"), InputOutcome::Ignored, "no history");
        assert_eq!(press(&mut input, "down"), InputOutcome::Changed);
        assert_eq!(input.cursor(), 30);
        assert_eq!(press(&mut input, "down"), InputOutcome::Changed);
        assert_eq!(input.cursor(), 50, "the end of the shorter last row");
        // A wrapped previous line is entered at its last row.
        input.set_text(&format!("{line}\nxy"));
        draw(&mut input);
        assert_eq!(press(&mut input, "up"), InputOutcome::Changed);
        assert_eq!(input.cursor(), 42, "column 2 of the line's last row");
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

    fn effort_commands() -> Vec<SlashCommand> {
        vec![
            SlashCommand::new("effort", "Set the reasoning effort")
                .with_choices(["default", "low", "medium", "high"]),
            SlashCommand::new("help", "Show keys"),
        ]
    }

    #[test]
    fn argument_choices_complete_with_the_keyboard() {
        let mut i = Input::new();
        i.set_commands(effort_commands());
        // The command name completes first, without opening the choices.
        typ(&mut i, "/eff");
        press(&mut i, "tab");
        assert_eq!(i.text(), "/effort ");
        assert!(i.choice_completions().is_none());
        // Typing the argument offers the matching choices.
        typ(&mut i, "m");
        assert_eq!(i.choice_completions(), Some((vec!["medium"], 0)));
        assert_eq!(press(&mut i, "enter"), InputOutcome::Changed);
        assert_eq!(i.text(), "/effort medium");
        assert!(i.choice_completions().is_none(), "closed after accepting");
        assert_eq!(
            press(&mut i, "enter"),
            InputOutcome::Submit("/effort medium".into())
        );
        // An empty argument lists every choice; Down/Up select, Tab accepts.
        typ(&mut i, "/effort ");
        assert_eq!(
            i.choice_completions(),
            Some((vec!["default", "low", "medium", "high"], 0))
        );
        press(&mut i, "down");
        press(&mut i, "down");
        press(&mut i, "up");
        assert_eq!(i.choice_completions().unwrap().1, 1);
        press(&mut i, "tab");
        assert_eq!(i.text(), "/effort low");
        // Enter with nothing typed sends the bare command (its picker).
        i.set_text("");
        typ(&mut i, "/effort ");
        assert!(i.choice_completions().is_some());
        assert_eq!(
            press(&mut i, "enter"),
            InputOutcome::Submit("/effort ".into())
        );
        // A fully typed choice sends at once; Esc closes the popup.
        typ(&mut i, "/effort high");
        assert_eq!(
            press(&mut i, "enter"),
            InputOutcome::Submit("/effort high".into())
        );
        typ(&mut i, "/effort h");
        assert_eq!(press(&mut i, "esc"), InputOutcome::Changed);
        assert!(i.choice_completions().is_none());
        typ(&mut i, "i");
        assert!(i.choice_completions().is_none(), "stays closed after Esc");
        // No choices: commands without them, unknown values, later words.
        i.set_text("");
        typ(&mut i, "/help x");
        assert!(i.choice_completions().is_none());
        i.set_text("");
        typ(&mut i, "/effort x");
        assert!(i.choice_completions().is_none());
        i.set_text("");
        typ(&mut i, "/effort low more");
        assert!(i.choice_completions().is_none());
    }

    #[test]
    fn argument_choices_are_clickable() {
        let mut input = Input::new();
        input.set_commands(effort_commands());
        typ(&mut input, "/effort ");
        let mut it = Interaction::new();
        let term = draw(&mut input, &mut it);
        let text = buffer_text(term.backend().buffer());
        assert!(text.contains("/effort medium"), "{text}");
        let (x, y) = find_text(term.backend().buffer(), "high").unwrap();
        let ev = crossterm::event::MouseEvent {
            kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
            column: x,
            row: y,
            modifiers: KeyModifiers::NONE,
        };
        let Some(Dispatch::Activate(a)) = it.handle_mouse(&ev) else {
            panic!("no hit")
        };
        assert_eq!(a, InputAction::Complete(3));
        input.activate(&a);
        assert_eq!(input.text(), "/effort high");
        assert!(input.choice_completions().is_none());
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

    fn ranked() -> Vec<SlashCommand> {
        vec![
            SlashCommand::new("remodel", "Rebuild the scaffold"),
            SlashCommand::new("model", "Switch the model"),
            SlashCommand::new("mode", "Cycle the autonomy mode"),
            SlashCommand::new("compact", "Compact the context"),
            SlashCommand {
                name: "project:deploy".into(),
                description: "Deploy the app".into(),
                args: None,
                scope: Some("project".into()),
                choices: Vec::new(),
            },
            SlashCommand {
                name: "global:review".into(),
                description: "Run the reviewer".into(),
                args: None,
                scope: Some("global".into()),
                choices: Vec::new(),
            },
            SlashCommand {
                name: "audit".into(),
                description: "Workflow: security review of the diff".into(),
                args: None,
                scope: Some("project".into()),
                choices: Vec::new(),
            },
        ]
    }

    fn names(i: &Input) -> Vec<String> {
        i.completions()
            .map(|(c, _)| c.iter().map(|c| c.name.clone()).collect())
            .unwrap_or_default()
    }

    fn typed(s: &str) -> Input {
        let mut i = Input::new();
        i.set_commands(ranked());
        typ(&mut i, s);
        i
    }

    #[test]
    fn gapped_matches_rank_by_score() {
        // /mdl matches model and remodel; the match at the word start wins.
        assert_eq!(names(&typed("/mdl")), vec!["model", "remodel"]);
        // Upper case makes the match case sensitive (smart case).
        assert!(typed("/MDL").completions().is_none());
        assert!(typed("/xyz").completions().is_none());
    }

    #[test]
    fn exact_then_prefix_then_other_name_matches() {
        // Prefix matches tie on score and keep alphabetical order; the
        // gapped remodel comes after them.
        assert_eq!(names(&typed("/mod")), vec!["mode", "model", "remodel"]);
        // An exact match ranks first even when another name scores higher.
        assert_eq!(names(&typed("/model")), vec!["model", "remodel"]);
        assert_eq!(names(&typed("/mode"))[0], "mode");
        // Enter on the exact match sends it.
        let mut i = typed("/mode");
        assert_eq!(press(&mut i, "enter"), InputOutcome::Submit("/mode".into()));
    }

    #[test]
    fn descriptions_are_searched_below_name_matches() {
        // global:review matches by name, audit only by its description.
        assert_eq!(names(&typed("/review")), vec!["global:review", "audit"]);
        assert_eq!(names(&typed("/security")), vec!["audit"]);
        // Description matches are contiguous: a scattered s-c-a-f does not
        // pull in unrelated descriptions.
        assert_eq!(names(&typed("/scaffold")), vec!["remodel"]);
        // Tab completes a description match to the command name.
        let mut i = typed("/security");
        press(&mut i, "tab");
        assert_eq!(i.text(), "/audit ");
    }

    #[test]
    fn namespaced_commands_match_bare_and_full_names() {
        assert_eq!(names(&typed("/deploy")), vec!["project:deploy"]);
        assert_eq!(names(&typed("/dep")), vec!["project:deploy"]);
        assert_eq!(names(&typed("/project:dep")), vec!["project:deploy"]);
        assert_eq!(names(&typed("/pjdpl")), vec!["project:deploy"]);
        // Enter on a bare exact match completes the full name.
        let mut i = typed("/deploy");
        assert_eq!(press(&mut i, "enter"), InputOutcome::Changed);
        assert_eq!(i.text(), "/project:deploy ");
    }

    #[test]
    fn equal_scores_keep_a_stable_alphabetical_order() {
        let cmds: Vec<SlashCommand> = ["xc", "xa", "xb", "xa"]
            .iter()
            .enumerate()
            .map(|(n, s)| SlashCommand::new(*s, format!("d{n}")))
            .collect();
        let mut i = Input::new();
        i.set_commands(cmds);
        typ(&mut i, "/x");
        let (c, _) = i.completions().unwrap();
        let got: Vec<_> = c
            .iter()
            .map(|c| (c.name.as_str(), c.description.as_str()))
            .collect();
        assert_eq!(
            got,
            vec![("xa", "d1"), ("xa", "d3"), ("xb", "d2"), ("xc", "d0")]
        );
    }

    #[test]
    fn selection_resets_when_the_ranking_changes() {
        let mut i = typed("/mo");
        press(&mut i, "down");
        press(&mut i, "down");
        assert_eq!(i.completions().unwrap().1, 2);
        press(&mut i, "up");
        assert_eq!(i.completions().unwrap().1, 1);
        // Typing re-ranks and selects the best match again.
        typ(&mut i, "d");
        assert_eq!(i.completions().unwrap().1, 0);
        // Reloading commands with the same token keeps the selection.
        press(&mut i, "down");
        i.set_commands(ranked());
        assert_eq!(i.completions().unwrap().1, 1);
        press(&mut i, "esc");
        assert!(i.completions().is_none());
    }

    #[test]
    fn matched_characters_are_highlighted_with_theme_tokens() {
        let theme = Theme::default();
        let hit = theme.fg(Token::Highlight).fg;
        let mut input = typed("/mdl");
        let mut it = Interaction::new();
        let term = draw(&mut input, &mut it);
        let buf = term.backend().buffer();
        let (x, y) = find_text(buf, "/model").unwrap();
        let styled = |dx: u16| {
            let cell = &buf[(x + dx, y)];
            (
                cell.fg == hit.unwrap(),
                cell.modifier.contains(Modifier::BOLD),
            )
        };
        // "/model": m, d and l are hits; the slash, o and e are not.
        assert_eq!(styled(0), (false, false));
        assert_eq!(styled(1), (true, true));
        assert_eq!(styled(2), (false, false));
        assert_eq!(styled(3), (true, true));
        assert_eq!(styled(4), (false, false));
        assert_eq!(styled(5), (true, true));
        // A description match highlights the description.
        let mut input = typed("/security");
        let term = draw(&mut input, &mut it);
        let buf = term.backend().buffer();
        let (x, y) = find_text(buf, "security").unwrap();
        assert_eq!(buf[(x, y)].fg, hit.unwrap());
        assert_eq!(buf[(x + 8, y)].fg, theme.fg(Token::Fg).fg.unwrap());
        // Clicking the highlighted row still completes it.
        let (x, y) = find_text(buf, "/audit").unwrap();
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
        assert_eq!(input.text(), "/audit ");
    }

    #[test]
    fn typing_forward_narrows_to_the_same_results_as_a_rescan() {
        for q in ["/review", "/mdl", "/project:dep", "/secur", "/Mo", "/de"] {
            let mut typed_in = Input::new();
            typed_in.set_commands(ranked());
            typ(&mut typed_in, q);
            let mut fresh = Input::new();
            fresh.set_commands(ranked());
            fresh.set_text(q);
            assert_eq!(names(&typed_in), names(&fresh), "{q}");
            assert_eq!(typed_in.completion, fresh.completion, "{q}");
        }
        // Reloading a shorter list while open rescans instead of reusing
        // stale matches, and typing on keeps working.
        let mut i = typed("/m");
        i.set_commands(vec![SlashCommand::new("mode", "Cycle")]);
        typ(&mut i, "o");
        assert_eq!(names(&i), vec!["mode"]);
    }

    #[test]
    fn ranks_1000_commands_within_a_frame_per_keystroke() {
        let cmds: Vec<SlashCommand> = (0..1000)
            .map(|n| SlashCommand {
                name: format!(
                    "{}:command-number-{n}",
                    if n % 2 == 0 { "global" } else { "project" }
                ),
                description: format!("Description of command {n} for testing completion"),
                args: Some("[arg]".into()),
                scope: Some("global".into()),
                choices: Vec::new(),
            })
            .collect();
        let theme = Theme::default();
        // One typing sequence on fresh state: its slowest frame, the
        // per-key timings and the final matches.
        let sequence = || {
            let mut input = Input::new();
            input.set_commands(cmds.clone());
            let mut term = Terminal::new(TestBackend::new(100, 12)).unwrap();
            let it: Interaction<InputAction> = Interaction::new();
            let mut worst = std::time::Duration::ZERO;
            let mut steps = Vec::new();
            for c in "/cmdnum99".chars() {
                let t = std::time::Instant::now();
                input.insert_char(c);
                let ranked = t.elapsed();
                let mut hits = it.begin_frame();
                term.draw(|f| {
                    let ch = input.completion_height();
                    input.render_completion(f, Rect::new(0, 0, 100, ch), &theme, &mut hits, &|a| a);
                })
                .unwrap();
                steps.push((c, ranked, t.elapsed()));
                worst = worst.max(t.elapsed());
            }
            (worst, steps, names(&input))
        };
        let (_, _, got) = sequence();
        assert!(!got.is_empty());
        // Every number with two 9s after the gap-matched "cmdnum" prefix.
        assert_eq!(got.len(), 28, "{got:?}");
        assert!(got.iter().any(|n| n == "project:command-number-99"));
        // 16 ms in release builds; debug builds (plain `cargo test`, as CI
        // runs it) get 4x, like the frame budgets in tests/perf.rs.
        const FRAME_BUDGET: std::time::Duration =
            std::time::Duration::from_millis(if cfg!(debug_assertions) { 64 } else { 16 });
        // Best of five runs: a slow implementation is slow every run, while
        // a scheduler hiccup on a shared CI runner only hits some of them.
        let (worst, steps, _) = (0..5).map(|_| sequence()).min_by_key(|r| r.0).unwrap();
        assert!(
            worst < FRAME_BUDGET,
            "worst frame {worst:?}; (key, ranked, drawn): {steps:?}"
        );
    }
}
