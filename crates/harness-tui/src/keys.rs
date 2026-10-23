//! Keybindings from `harness.ini`.
//!
//! Only a handful of actions have keys ([`Action`]); everything else lives in
//! the command palette. Defaults:
//!
//! | action | keys |
//! |---|---|
//! | `send` | `enter` |
//! | `newline` | `shift+enter`, `alt+enter` |
//! | `cancel` | `esc` |
//! | `switch_view` | `tab` |
//! | `palette` | `ctrl+p` |
//! | `interrupt` | `ctrl+c` |
//! | `quit` | `ctrl+q` |
//! | `copy` | `ctrl+y` |
//! | `select` | `ctrl+t` |
//!
//! A `[keys]` entry replaces the defaults for that action. A chord bound to
//! two actions, or to a fixed key ([`FIXED_KEYS`]), is a load error with the
//! `file:line` of the offending entry. The help overlay ([`HelpOverlay`]) is
//! generated from the active [`Keymap`].

use std::collections::BTreeMap;
use std::fmt;

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use harness_config::ini::Location;
use harness_config::ini::config::{Config, KEY_ACTIONS};
use ratatui::Frame;
use ratatui::layout::{Alignment, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

use crate::interaction::{ElementId, HitMap};
use crate::theme::{Theme, Token};

/// A configurable action.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Action {
    /// Open the command palette.
    Palette,
    /// Send the input.
    Send,
    /// Insert a newline in the input.
    Newline,
    /// Cancel or go back.
    Cancel,
    /// Interrupt the running agent.
    Interrupt,
    /// Switch between chat and map.
    SwitchView,
    /// Quit harness.
    Quit,
    /// Copy the focused message, code block, tool output, diff or log
    /// entry (the last assistant message when nothing is focused).
    Copy,
    /// Start keyboard text selection in the transcript.
    Select,
}

impl Action {
    /// Every action, in help-overlay order.
    pub const ALL: [Action; 9] = [
        Action::Send,
        Action::Newline,
        Action::Cancel,
        Action::SwitchView,
        Action::Palette,
        Action::Copy,
        Action::Select,
        Action::Interrupt,
        Action::Quit,
    ];

    /// The `[keys]` name of the action.
    pub fn name(self) -> &'static str {
        match self {
            Action::Palette => "palette",
            Action::Send => "send",
            Action::Newline => "newline",
            Action::Cancel => "cancel",
            Action::Interrupt => "interrupt",
            Action::SwitchView => "switch_view",
            Action::Quit => "quit",
            Action::Copy => "copy",
            Action::Select => "select",
        }
    }

    /// Looks an action up by its `[keys]` name.
    pub fn from_name(name: &str) -> Option<Action> {
        Action::ALL.into_iter().find(|a| a.name() == name)
    }

    /// Short description for the help overlay.
    pub fn description(self) -> &'static str {
        match self {
            Action::Palette => "command palette",
            Action::Send => "send message",
            Action::Newline => "new line",
            Action::Cancel => "cancel / back",
            Action::Interrupt => "interrupt agent",
            Action::SwitchView => "switch view (chat / map)",
            Action::Quit => "quit",
            Action::Copy => "copy to clipboard",
            Action::Select => "select text in the transcript",
        }
    }

    /// Default chords.
    pub fn default_chords(self) -> &'static [&'static str] {
        match self {
            Action::Palette => &["ctrl+p"],
            Action::Send => &["enter"],
            Action::Newline => &["shift+enter", "alt+enter"],
            Action::Cancel => &["esc"],
            Action::Interrupt => &["ctrl+c"],
            Action::SwitchView => &["tab"],
            Action::Quit => &["ctrl+q"],
            Action::Copy => &["ctrl+y"],
            Action::Select => &["ctrl+t"],
        }
    }
}

impl fmt::Display for Action {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// Keys that are not configurable, shown in the help overlay. Binding an
/// action to the suspend chord is a conflict.
pub const FIXED_KEYS: &[(&str, &str)] = &[
    ("ctrl+z", "suspend (resume with fg)"),
    ("shift+tab / arrows", "move focus"),
    ("enter", "activate focused element"),
    ("pgup / pgdn", "scroll focused pane"),
    ("end", "jump to the latest message"),
    ("mouse", "click activates, wheel scrolls"),
];

/// The chord reserved for suspending to the shell.
pub const SUSPEND_CHORD: &str = "ctrl+z";

/// A key plus modifiers, e.g. `ctrl+p` or `shift+enter`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct KeyChord {
    /// The key.
    pub code: KeyCode,
    /// Modifiers (only control, alt, shift and super are kept).
    pub mods: KeyModifiers,
}

impl KeyChord {
    /// Builds a normalised chord: `P` becomes `shift+p`, `BackTab` becomes
    /// `shift+tab`, and shift is dropped for non-letter characters (`?`).
    pub fn new(code: KeyCode, mods: KeyModifiers) -> KeyChord {
        let mut mods = mods
            & (KeyModifiers::CONTROL
                | KeyModifiers::ALT
                | KeyModifiers::SHIFT
                | KeyModifiers::SUPER);
        let code = match code {
            KeyCode::BackTab => {
                mods |= KeyModifiers::SHIFT;
                KeyCode::Tab
            }
            KeyCode::Char(c) if c.is_uppercase() => {
                mods |= KeyModifiers::SHIFT;
                KeyCode::Char(c.to_lowercase().next().unwrap_or(c))
            }
            KeyCode::Char(c) => {
                if !c.is_alphabetic() {
                    mods.remove(KeyModifiers::SHIFT);
                }
                KeyCode::Char(c)
            }
            other => other,
        };
        KeyChord { code, mods }
    }

    /// The chord of a key event, or `None` for key releases.
    pub fn from_event(ev: &KeyEvent) -> Option<KeyChord> {
        (ev.kind != KeyEventKind::Release).then(|| KeyChord::new(ev.code, ev.modifiers))
    }

    /// A press event for this chord (used by tests and the help overlay).
    pub fn to_event(self) -> KeyEvent {
        KeyEvent::new(self.code, self.mods)
    }

    /// Parses `ctrl+p`, `shift+enter`, `alt+enter`, `esc`, `f1`, `pgup`, `a`.
    pub fn parse(s: &str) -> Result<KeyChord, String> {
        let s = s.trim().to_ascii_lowercase();
        if s.is_empty() {
            return Err("empty key chord".into());
        }
        // A trailing "+" means the plus key itself ("ctrl++").
        let (mods_part, key) = match s.strip_suffix("++") {
            Some(rest) => (rest, "+"),
            None => match s.rsplit_once('+') {
                Some((m, k)) => (m, k),
                None => ("", s.as_str()),
            },
        };
        let mut mods = KeyModifiers::NONE;
        for m in mods_part.split('+').filter(|m| !m.is_empty()) {
            mods |= match m {
                "ctrl" | "control" | "c" => KeyModifiers::CONTROL,
                "alt" | "meta" | "option" | "m" => KeyModifiers::ALT,
                "shift" | "s" => KeyModifiers::SHIFT,
                "super" | "cmd" | "win" => KeyModifiers::SUPER,
                other => return Err(format!("unknown modifier `{other}` in `{s}`")),
            };
        }
        let code = match key {
            "enter" | "return" => KeyCode::Enter,
            "esc" | "escape" => KeyCode::Esc,
            "tab" => KeyCode::Tab,
            "backtab" => KeyCode::BackTab,
            "backspace" => KeyCode::Backspace,
            "delete" | "del" => KeyCode::Delete,
            "insert" | "ins" => KeyCode::Insert,
            "home" => KeyCode::Home,
            "end" => KeyCode::End,
            "pageup" | "pgup" => KeyCode::PageUp,
            "pagedown" | "pgdn" => KeyCode::PageDown,
            "up" => KeyCode::Up,
            "down" => KeyCode::Down,
            "left" => KeyCode::Left,
            "right" => KeyCode::Right,
            "space" => KeyCode::Char(' '),
            k if k.len() > 1 && k.starts_with('f') && k[1..].parse::<u8>().is_ok() => {
                let n: u8 = k[1..].parse().unwrap_or(0);
                if !(1..=24).contains(&n) {
                    return Err(format!("unknown key `{k}` in `{s}`"));
                }
                KeyCode::F(n)
            }
            k if k.chars().count() == 1 => KeyCode::Char(k.chars().next().unwrap_or(' ')),
            k => return Err(format!("unknown key `{k}` in `{s}`")),
        };
        Ok(KeyChord::new(code, mods))
    }

    /// Whether `ev` is this chord.
    pub fn matches(&self, ev: &KeyEvent) -> bool {
        KeyChord::from_event(ev).is_some_and(|c| c == *self)
    }
}

impl fmt::Display for KeyChord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (m, name) in [
            (KeyModifiers::CONTROL, "ctrl+"),
            (KeyModifiers::ALT, "alt+"),
            (KeyModifiers::SHIFT, "shift+"),
            (KeyModifiers::SUPER, "super+"),
        ] {
            if self.mods.contains(m) {
                f.write_str(name)?;
            }
        }
        match self.code {
            KeyCode::Enter => f.write_str("enter"),
            KeyCode::Esc => f.write_str("esc"),
            KeyCode::Tab => f.write_str("tab"),
            KeyCode::Backspace => f.write_str("backspace"),
            KeyCode::Delete => f.write_str("delete"),
            KeyCode::Insert => f.write_str("insert"),
            KeyCode::Home => f.write_str("home"),
            KeyCode::End => f.write_str("end"),
            KeyCode::PageUp => f.write_str("pgup"),
            KeyCode::PageDown => f.write_str("pgdn"),
            KeyCode::Up => f.write_str("up"),
            KeyCode::Down => f.write_str("down"),
            KeyCode::Left => f.write_str("left"),
            KeyCode::Right => f.write_str("right"),
            KeyCode::Char(' ') => f.write_str("space"),
            KeyCode::Char(c) => write!(f, "{c}"),
            KeyCode::F(n) => write!(f, "f{n}"),
            other => write!(f, "{other:?}"),
        }
    }
}

/// A problem in `[keys]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyError {
    /// `file:line` of the entry, when known.
    pub location: Option<String>,
    /// What is wrong.
    pub message: String,
}

impl fmt::Display for KeyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.location {
            Some(loc) => write!(f, "{loc}: {}", self.message),
            None => f.write_str(&self.message),
        }
    }
}

impl std::error::Error for KeyError {}

/// The active bindings: action → chords.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Keymap {
    bindings: BTreeMap<Action, Vec<KeyChord>>,
}

impl Default for Keymap {
    fn default() -> Self {
        Keymap::defaults()
    }
}

impl Keymap {
    /// The default bindings.
    pub fn defaults() -> Keymap {
        let bindings = Action::ALL
            .into_iter()
            .map(|a| {
                let chords = a
                    .default_chords()
                    .iter()
                    .map(|c| KeyChord::parse(c).expect("default chords parse"))
                    .collect();
                (a, chords)
            })
            .collect();
        Keymap { bindings }
    }

    /// Defaults overridden by `cfg.keys`; reports bad chords and conflicts.
    pub fn from_config(cfg: &Config) -> Result<Keymap, Vec<KeyError>> {
        let location = |action: &str| {
            cfg.origins
                .get(&format!("keys.{action}"))
                .map(Location::to_string)
        };
        Keymap::from_entries(
            cfg.keys
                .iter()
                .map(|(a, chords)| (a.as_str(), chords.as_slice(), location(a))),
        )
    }

    /// Defaults overridden by `(action, chords, location)` entries.
    pub fn from_entries<'a>(
        entries: impl IntoIterator<Item = (&'a str, &'a [String], Option<String>)>,
    ) -> Result<Keymap, Vec<KeyError>> {
        let mut map = Keymap::defaults();
        let mut errors = Vec::new();
        let mut origin: BTreeMap<Action, Option<String>> = BTreeMap::new();
        for (name, chords, location) in entries {
            let Some(action) = Action::from_name(name) else {
                errors.push(KeyError {
                    location,
                    message: format!(
                        "unknown key action `{name}`; known: {}",
                        KEY_ACTIONS.join(", ")
                    ),
                });
                continue;
            };
            let mut parsed = Vec::new();
            for c in chords {
                match KeyChord::parse(c) {
                    Ok(chord) if !parsed.contains(&chord) => parsed.push(chord),
                    Ok(_) => {}
                    Err(e) => errors.push(KeyError {
                        location: location.clone(),
                        message: format!("invalid key chord for `{name}`: {e}"),
                    }),
                }
            }
            map.bindings.insert(action, parsed);
            origin.insert(action, location);
        }

        // Conflicts: a chord bound to more than one action or to a fixed key.
        let suspend = KeyChord::parse(SUSPEND_CHORD).expect("suspend chord parses");
        let mut owner: BTreeMap<String, Action> = BTreeMap::new();
        for action in Action::ALL {
            for chord in &map.bindings[&action] {
                let loc = origin.get(&action).cloned().flatten();
                if *chord == suspend {
                    errors.push(KeyError {
                        location: loc.clone(),
                        message: format!(
                            "`{chord}` for `{action}` conflicts with the fixed suspend key"
                        ),
                    });
                }
                if let Some(other) = owner.insert(chord.to_string(), action) {
                    let loc = loc.or_else(|| origin.get(&other).cloned().flatten());
                    errors.push(KeyError {
                        location: loc,
                        message: format!(
                            "`{chord}` is bound to both `{other}` and `{action}`; change one of them in [keys]"
                        ),
                    });
                }
            }
        }
        if errors.is_empty() {
            Ok(map)
        } else {
            Err(errors)
        }
    }

    /// The action bound to `ev`, if any.
    pub fn lookup(&self, ev: &KeyEvent) -> Option<Action> {
        let chord = KeyChord::from_event(ev)?;
        self.bindings
            .iter()
            .find(|(_, chords)| chords.contains(&chord))
            .map(|(a, _)| *a)
    }

    /// The chords bound to `action`.
    pub fn chords(&self, action: Action) -> &[KeyChord] {
        self.bindings.get(&action).map_or(&[], Vec::as_slice)
    }

    /// Display string for `action`'s chords, e.g. `shift+enter, alt+enter`.
    pub fn describe(&self, action: Action) -> String {
        let chords: Vec<String> = self
            .chords(action)
            .iter()
            .map(ToString::to_string)
            .collect();
        if chords.is_empty() {
            "unbound".into()
        } else {
            chords.join(", ")
        }
    }

    /// Rows for the help overlay: `(keys, description)` for every action, then
    /// the fixed keys.
    pub fn help_rows(&self) -> Vec<(String, &'static str)> {
        Action::ALL
            .into_iter()
            .map(|a| (self.describe(a), a.description()))
            .chain(FIXED_KEYS.iter().map(|(k, d)| ((*k).to_owned(), *d)))
            .collect()
    }
}

/// Element id of the help overlay's close button.
pub const HELP_CLOSE_ID: &str = "help.close";

/// The help overlay listing the active bindings. It is modal: it registers a
/// modal layer in the [`HitMap`] with a focusable close button, so it can be
/// closed with Esc, Enter on the button, a click on the button or a click
/// outside it.
pub struct HelpOverlay<'a> {
    keymap: &'a Keymap,
    theme: &'a Theme,
}

impl<'a> HelpOverlay<'a> {
    /// Creates the overlay for `keymap`.
    pub fn new(keymap: &'a Keymap, theme: &'a Theme) -> HelpOverlay<'a> {
        HelpOverlay { keymap, theme }
    }

    /// The overlay rectangle, centred in `area`.
    pub fn area(&self, area: Rect) -> Rect {
        let rows = self.keymap.help_rows();
        let key_w = rows
            .iter()
            .map(|(k, _)| k.chars().count())
            .max()
            .unwrap_or(0);
        let desc_w = rows
            .iter()
            .map(|(_, d)| d.chars().count())
            .max()
            .unwrap_or(0);
        let width = ((key_w + desc_w + 7) as u16).min(area.width);
        let height = ((rows.len() + 4) as u16).min(area.height);
        Rect {
            x: area.x + (area.width - width) / 2,
            y: area.y + (area.height - height) / 2,
            width,
            height,
        }
    }

    /// Renders the overlay and registers its modal layer and close button,
    /// which dispatches `close`.
    pub fn render<A: Clone>(
        self,
        frame: &mut Frame<'_>,
        area: Rect,
        hits: &mut HitMap<A>,
        close: A,
    ) {
        let theme = self.theme;
        let rect = self.area(area);
        let rows = self.keymap.help_rows();
        let key_w = rows
            .iter()
            .map(|(k, _)| k.chars().count())
            .max()
            .unwrap_or(0);
        let mut lines: Vec<Line<'_>> = rows
            .into_iter()
            .map(|(keys, desc)| {
                Line::from(vec![
                    Span::styled(format!(" {keys:<key_w$}  "), theme.fg(Token::Accent)),
                    Span::styled(desc, theme.fg(Token::Fg)),
                ])
            })
            .collect();
        lines.push(Line::default());

        hits.push_modal(rect);
        let focused = hits.is_focused(&ElementId::from(HELP_CLOSE_ID));
        let button_style = if focused {
            theme.focused().patch(theme.fg(Token::Accent))
        } else {
            theme.fg(Token::Accent)
        };
        lines
            .push(Line::from(Span::styled("[ Close ]", button_style)).alignment(Alignment::Center));

        let block = Block::new()
            .borders(Borders::ALL)
            .border_style(theme.border(true))
            .title(Span::styled(" Keys ", theme.fg(Token::Accent)))
            .title_bottom(
                Line::from(Span::styled(" esc to close ", theme.fg(Token::Muted))).right_aligned(),
            )
            .style(theme.base());
        frame.render_widget(Clear, rect);
        frame.render_widget(Paragraph::new(lines).block(block), rect);

        // The button is on the last text row inside the border.
        let label = 9u16;
        let inner_w = rect.width.saturating_sub(2);
        let button = Rect {
            x: rect.x + 1 + inner_w.saturating_sub(label) / 2,
            y: rect.y + rect.height.saturating_sub(2),
            width: label.min(inner_w),
            height: 1,
        };
        hits.register(HELP_CLOSE_ID, button, close);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use harness_config::ini::load_str;
    use std::path::Path;

    fn ev(code: KeyCode, mods: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, mods)
    }

    #[test]
    fn defaults_match_spec() {
        let k = Keymap::defaults();
        let none = KeyModifiers::NONE;
        assert_eq!(k.lookup(&ev(KeyCode::Enter, none)), Some(Action::Send));
        assert_eq!(
            k.lookup(&ev(KeyCode::Enter, KeyModifiers::SHIFT)),
            Some(Action::Newline)
        );
        assert_eq!(
            k.lookup(&ev(KeyCode::Enter, KeyModifiers::ALT)),
            Some(Action::Newline)
        );
        assert_eq!(k.lookup(&ev(KeyCode::Esc, none)), Some(Action::Cancel));
        assert_eq!(k.lookup(&ev(KeyCode::Tab, none)), Some(Action::SwitchView));
        assert_eq!(
            k.lookup(&ev(KeyCode::Char('p'), KeyModifiers::CONTROL)),
            Some(Action::Palette)
        );
        assert_eq!(
            k.lookup(&ev(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            Some(Action::Interrupt)
        );
        assert_eq!(
            k.lookup(&ev(KeyCode::Char('q'), KeyModifiers::CONTROL)),
            Some(Action::Quit)
        );
        assert_eq!(k.lookup(&ev(KeyCode::Char('q'), none)), None);
        assert_eq!(k.lookup(&ev(KeyCode::BackTab, KeyModifiers::SHIFT)), None);
    }

    #[test]
    fn action_names_match_config_key_actions() {
        let mut ours: Vec<&str> = Action::ALL.iter().map(|a| a.name()).collect();
        let mut theirs = KEY_ACTIONS.to_vec();
        ours.sort_unstable();
        theirs.sort_unstable();
        assert_eq!(ours, theirs);
    }

    #[test]
    fn chord_parsing_and_display() {
        for (s, want) in [
            ("ctrl+p", "ctrl+p"),
            ("Ctrl+Shift+X", "ctrl+shift+x"),
            ("alt+enter", "alt+enter"),
            ("escape", "esc"),
            ("backtab", "shift+tab"),
            ("pageup", "pgup"),
            ("f5", "f5"),
            ("space", "space"),
            ("ctrl++", "ctrl++"),
            ("?", "?"),
        ] {
            assert_eq!(KeyChord::parse(s).unwrap().to_string(), want, "{s}");
        }
        for bad in ["", "hyper+x", "ctrl+foo", "f99"] {
            assert!(KeyChord::parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn event_normalisation() {
        // Terminals report Ctrl+Shift+P as uppercase P with shift.
        let chord = KeyChord::parse("ctrl+shift+p").unwrap();
        assert!(chord.matches(&ev(
            KeyCode::Char('P'),
            KeyModifiers::CONTROL | KeyModifiers::SHIFT
        )));
        assert!(chord.matches(&ev(KeyCode::Char('P'), KeyModifiers::CONTROL)));
        assert!(
            KeyChord::parse("shift+tab")
                .unwrap()
                .matches(&ev(KeyCode::BackTab, KeyModifiers::SHIFT))
        );
        assert!(
            KeyChord::parse("?")
                .unwrap()
                .matches(&ev(KeyCode::Char('?'), KeyModifiers::SHIFT))
        );
        let mut release = ev(KeyCode::Enter, KeyModifiers::NONE);
        release.kind = KeyEventKind::Release;
        assert!(!KeyChord::parse("enter").unwrap().matches(&release));
    }

    #[test]
    fn keys_section_overrides_defaults() {
        let cfg = load_str("[keys]\npalette = ctrl+k, f1\nquit = ctrl+x\n", None).unwrap();
        let k = Keymap::from_config(&cfg).unwrap();
        assert_eq!(
            k.lookup(&ev(KeyCode::Char('k'), KeyModifiers::CONTROL)),
            Some(Action::Palette)
        );
        assert_eq!(
            k.lookup(&ev(KeyCode::F(1), KeyModifiers::NONE)),
            Some(Action::Palette)
        );
        assert_eq!(
            k.lookup(&ev(KeyCode::Char('p'), KeyModifiers::CONTROL)),
            None
        );
        assert_eq!(
            k.lookup(&ev(KeyCode::Char('x'), KeyModifiers::CONTROL)),
            Some(Action::Quit)
        );
        // Untouched actions keep their defaults.
        assert_eq!(
            k.lookup(&ev(KeyCode::Enter, KeyModifiers::NONE)),
            Some(Action::Send)
        );
    }

    #[test]
    fn conflicts_are_reported_with_location() {
        let cfg = load_str("[keys]\npalette = tab\n", Some(Path::new("/c/harness.ini"))).unwrap();
        let errs = Keymap::from_config(&cfg).unwrap_err();
        assert_eq!(errs.len(), 1, "{errs:?}");
        let msg = errs[0].to_string();
        assert!(msg.starts_with("/c/harness.ini:2: "), "{msg}");
        assert!(msg.contains("`tab` is bound to both"), "{msg}");

        let cfg = load_str(
            "[keys]\nsend = ctrl+s\nquit = ctrl+s\n",
            Some(Path::new("/c/h.ini")),
        )
        .unwrap();
        let errs = Keymap::from_config(&cfg).unwrap_err();
        assert_eq!(errs.len(), 1, "{errs:?}");
        assert!(errs[0].to_string().contains("/c/h.ini:"), "{}", errs[0]);
    }

    #[test]
    fn suspend_and_bad_chords_are_errors() {
        let cfg = load_str("[keys]\ninterrupt = ctrl+z\n", None).unwrap();
        let errs = Keymap::from_config(&cfg).unwrap_err();
        assert!(errs[0].message.contains("suspend"), "{errs:?}");

        let cfg = load_str("[keys]\nquit = ctrl+nope\n", Some(Path::new("/c/h.ini"))).unwrap();
        let errs = Keymap::from_config(&cfg).unwrap_err();
        assert!(
            errs[0]
                .to_string()
                .starts_with("/c/h.ini:2: invalid key chord for `quit`"),
            "{}",
            errs[0]
        );
    }

    #[test]
    fn help_rows_follow_active_bindings() {
        let cfg = load_str("[keys]\npalette = ctrl+k\n", None).unwrap();
        let rows = Keymap::from_config(&cfg).unwrap().help_rows();
        assert!(rows.contains(&("ctrl+k".to_owned(), "command palette")));
        assert!(rows.contains(&("shift+enter, alt+enter".to_owned(), "new line")));
        assert!(!rows.iter().any(|(k, _)| k == "ctrl+p"));
        assert_eq!(rows.len(), Action::ALL.len() + FIXED_KEYS.len());
    }
}
