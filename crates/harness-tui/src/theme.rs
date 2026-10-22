//! Semantic colour tokens and the Dracula theme.
//!
//! Components never name raw colours. They ask a [`Theme`] for the style of a
//! semantic [`Token`] (`ok`, `error`, `running`, `accent`, ...), and the theme
//! maps it to a concrete colour for the terminal's [`ColorDepth`]:
//!
//! - truecolor when `COLORTERM` is `truecolor` or `24bit`,
//! - otherwise the nearest xterm-256 colour when `TERM` advertises 256 colours,
//! - otherwise a fixed 16-colour mapping that keeps status colours distinct,
//! - no colour at all when `NO_COLOR` is set or `[ui] theme = none`; icons,
//!   layout and text modifiers (bold, italic, reverse for focus) remain.
//!
//! The default mapping is Dracula (see `backlog/docs/doc-7`). Every token can
//! be overridden in the `[theme]` section of `harness.ini` (`accent = #ff00ff`).
//! Derived tokens such as `card.ok` or `diff.added.bg` follow their base token
//! unless they are overridden themselves.
//!
//! The `no_raw_colors_outside_theme` test enforces that no module other than
//! this one uses `ratatui::style::Color` directly.

use std::fmt;

use harness_config::ini::Location;
use harness_config::ini::config::{Background, Config, UiTheme};
use ratatui::style::{Color, Modifier, Style};

/// A 24-bit RGB colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Rgb(pub u8, pub u8, pub u8);

impl Rgb {
    /// Parses `#rrggbb` (case-insensitive). Returns `None` for anything else.
    pub fn parse_hex(s: &str) -> Option<Rgb> {
        let hex = s.strip_prefix('#')?;
        if hex.len() != 6 || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
            return None;
        }
        let byte = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).ok();
        Some(Rgb(byte(0)?, byte(2)?, byte(4)?))
    }

    /// Blends `self` over `base` with `percent` (0-100) opacity.
    pub fn blend_over(self, base: Rgb, percent: u8) -> Rgb {
        let p = u32::from(percent.min(100));
        let mix = |fg: u8, bg: u8| {
            let v = (u32::from(fg) * p + u32::from(bg) * (100 - p) + 50) / 100;
            v as u8
        };
        Rgb(
            mix(self.0, base.0),
            mix(self.1, base.1),
            mix(self.2, base.2),
        )
    }
}

impl fmt::Display for Rgb {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "#{:02x}{:02x}{:02x}", self.0, self.1, self.2)
    }
}

/// How a token gets its colour when it is not overridden.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenSource {
    /// A fixed palette colour.
    Fixed(Rgb),
    /// The same colour as another token.
    Alias(Token),
    /// Another token blended over `bg` at the given percentage.
    Tint(Token, u8),
}

/// The 16 basic ANSI colours used by the 16-colour fallback.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(missing_docs)]
pub enum Ansi16 {
    Black,
    Red,
    Green,
    Yellow,
    Blue,
    Magenta,
    Cyan,
    Gray,
    DarkGray,
    LightRed,
    LightGreen,
    LightYellow,
    LightBlue,
    LightMagenta,
    LightCyan,
    White,
}

impl Ansi16 {
    const ALL: [Ansi16; 16] = [
        Ansi16::Black,
        Ansi16::Red,
        Ansi16::Green,
        Ansi16::Yellow,
        Ansi16::Blue,
        Ansi16::Magenta,
        Ansi16::Cyan,
        Ansi16::Gray,
        Ansi16::DarkGray,
        Ansi16::LightRed,
        Ansi16::LightGreen,
        Ansi16::LightYellow,
        Ansi16::LightBlue,
        Ansi16::LightMagenta,
        Ansi16::LightCyan,
        Ansi16::White,
    ];

    /// Typical xterm RGB value, used to pick the nearest colour for overrides.
    fn rgb(self) -> Rgb {
        match self {
            Ansi16::Black => Rgb(0, 0, 0),
            Ansi16::Red => Rgb(205, 0, 0),
            Ansi16::Green => Rgb(0, 205, 0),
            Ansi16::Yellow => Rgb(205, 205, 0),
            Ansi16::Blue => Rgb(0, 0, 238),
            Ansi16::Magenta => Rgb(205, 0, 205),
            Ansi16::Cyan => Rgb(0, 205, 205),
            Ansi16::Gray => Rgb(229, 229, 229),
            Ansi16::DarkGray => Rgb(127, 127, 127),
            Ansi16::LightRed => Rgb(255, 0, 0),
            Ansi16::LightGreen => Rgb(0, 255, 0),
            Ansi16::LightYellow => Rgb(255, 255, 0),
            Ansi16::LightBlue => Rgb(92, 92, 255),
            Ansi16::LightMagenta => Rgb(255, 0, 255),
            Ansi16::LightCyan => Rgb(0, 255, 255),
            Ansi16::White => Rgb(255, 255, 255),
        }
    }

    fn nearest(rgb: Rgb) -> Ansi16 {
        Ansi16::ALL
            .into_iter()
            .min_by_key(|c| distance(c.rgb(), rgb))
            .unwrap_or(Ansi16::White)
    }

    fn color(self) -> Color {
        match self {
            Ansi16::Black => Color::Black,
            Ansi16::Red => Color::Red,
            Ansi16::Green => Color::Green,
            Ansi16::Yellow => Color::Yellow,
            Ansi16::Blue => Color::Blue,
            Ansi16::Magenta => Color::Magenta,
            Ansi16::Cyan => Color::Cyan,
            Ansi16::Gray => Color::Gray,
            Ansi16::DarkGray => Color::DarkGray,
            Ansi16::LightRed => Color::LightRed,
            Ansi16::LightGreen => Color::LightGreen,
            Ansi16::LightYellow => Color::LightYellow,
            Ansi16::LightBlue => Color::LightBlue,
            Ansi16::LightMagenta => Color::LightMagenta,
            Ansi16::LightCyan => Color::LightCyan,
            Ansi16::White => Color::White,
        }
    }
}

macro_rules! tokens {
    ($( $(#[$doc:meta])* $variant:ident = $name:literal => $source:expr, $ansi:expr; )*) => {
        /// A semantic colour token. Its `harness.ini` name is [`Token::name`].
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub enum Token {
            $( $(#[$doc])* $variant, )*
        }

        impl Token {
            /// Every token, in declaration order.
            pub const ALL: &'static [Token] = &[$(Token::$variant),*];

            /// The token's name in the `[theme]` section, e.g. `card.ok`.
            pub fn name(self) -> &'static str {
                match self { $(Token::$variant => $name,)* }
            }

            /// The Dracula default for this token.
            pub fn dracula(self) -> TokenSource {
                use TokenSource::*;
                match self { $(Token::$variant => $source,)* }
            }

            /// 16-colour fallback for the default mapping (`None` = terminal default).
            fn ansi16(self) -> Option<Ansi16> {
                match self { $(Token::$variant => $ansi,)* }
            }
        }
    };
}

tokens! {
    /// Main background (Dracula Background).
    Bg = "bg" => Fixed(Rgb(0x28, 0x2a, 0x36)), Some(Ansi16::Black);
    /// Cards, selection, focused row (Current Line).
    Surface = "surface" => Fixed(Rgb(0x44, 0x47, 0x5a)), Some(Ansi16::DarkGray);
    /// Default text and assistant text.
    Fg = "fg" => Fixed(Rgb(0xf8, 0xf8, 0xf2)), None;
    /// Timestamps, metadata, borders (Comment).
    Muted = "muted" => Fixed(Rgb(0x62, 0x72, 0xa4)), Some(Ansi16::Gray);
    /// Reasoning text (italic).
    Reasoning = "reasoning" => Alias(Token::Muted), Some(Ansi16::Gray);
    /// Spinners and active nodes (Cyan).
    Running = "running" => Fixed(Rgb(0x8b, 0xe9, 0xfd)), Some(Ansi16::Cyan);
    /// File paths.
    Path = "path" => Alias(Token::Running), Some(Ansi16::Cyan);
    /// Links.
    Link = "link" => Alias(Token::Running), Some(Ansi16::Cyan);
    /// Success (Green).
    Ok = "ok" => Fixed(Rgb(0x50, 0xfa, 0x7b)), Some(Ansi16::Green);
    /// Warnings (Orange).
    Warn = "warn" => Fixed(Rgb(0xff, 0xb8, 0x6c)), Some(Ansi16::Yellow);
    /// Waiting on the user.
    Approval = "approval" => Alias(Token::Warn), Some(Ansi16::Yellow);
    /// Stalled streams.
    Stalled = "stalled" => Alias(Token::Warn), Some(Ansi16::Yellow);
    /// User message bar (Pink).
    User = "user" => Fixed(Rgb(0xff, 0x79, 0xc6)), Some(Ansi16::LightMagenta);
    /// Syntax keywords.
    Keyword = "keyword" => Alias(Token::User), Some(Ansi16::LightMagenta);
    /// Tool names, focus border, palette match (Purple).
    Accent = "accent" => Fixed(Rgb(0xbd, 0x93, 0xf9)), Some(Ansi16::Magenta);
    /// Agent names.
    Agent = "agent" => Alias(Token::Accent), Some(Ansi16::Magenta);
    /// Errors (Red).
    Error = "error" => Fixed(Rgb(0xff, 0x55, 0x55)), Some(Ansi16::Red);
    /// Tokens, cost, durations (Yellow).
    Number = "number" => Fixed(Rgb(0xf1, 0xfa, 0x8c)), Some(Ansi16::LightYellow);
    /// Search matches.
    Highlight = "highlight" => Alias(Token::Number), Some(Ansi16::LightYellow);
    /// Unfocused borders.
    Border = "border" => Alias(Token::Muted), Some(Ansi16::Gray);
    /// Focused border.
    BorderFocus = "border.focus" => Alias(Token::Accent), Some(Ansi16::Magenta);
    /// Pending card background.
    CardPending = "card.pending" => Alias(Token::Surface), None;
    /// Successful card background (ok at 8 %).
    CardOk = "card.ok" => Tint(Token::Ok, 8), None;
    /// Failed card background (error at 10 %).
    CardError = "card.error" => Tint(Token::Error, 10), None;
    /// Approval card background (approval at 10 %).
    CardApproval = "card.approval" => Tint(Token::Approval, 10), None;
    /// Added diff lines (gutter sign and text).
    DiffAdded = "diff.added" => Alias(Token::Ok), Some(Ansi16::Green);
    /// Removed diff lines (gutter sign and text).
    DiffRemoved = "diff.removed" => Alias(Token::Error), Some(Ansi16::Red);
    /// Added diff line background (ok at 15 %).
    DiffAddedBg = "diff.added.bg" => Tint(Token::Ok, 15), None;
    /// Removed diff line background (error at 15 %).
    DiffRemovedBg = "diff.removed.bg" => Tint(Token::Error, 15), None;
}

impl Token {
    /// Looks a token up by its `[theme]` name.
    pub fn from_name(name: &str) -> Option<Token> {
        Token::ALL.iter().copied().find(|t| t.name() == name)
    }
}

impl fmt::Display for Token {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// Resolved RGB value for every token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Palette {
    colors: Vec<Rgb>,
    overridden: Vec<bool>,
}

impl Palette {
    /// The default Dracula palette.
    pub fn dracula() -> Palette {
        Palette::with_overrides(&[])
    }

    /// Dracula with the given per-token overrides. Aliases and tints follow
    /// overridden base tokens unless they are overridden themselves.
    pub fn with_overrides(overrides: &[(Token, Rgb)]) -> Palette {
        let n = Token::ALL.len();
        let mut explicit: Vec<Option<Rgb>> = vec![None; n];
        for (t, rgb) in overrides {
            explicit[*t as usize] = Some(*rgb);
        }
        let colors = Token::ALL
            .iter()
            .map(|t| resolve(*t, &explicit, 0))
            .collect();
        Palette {
            colors,
            overridden: explicit.iter().map(Option::is_some).collect(),
        }
    }

    /// The RGB value of `token`.
    pub fn rgb(&self, token: Token) -> Rgb {
        self.colors[token as usize]
    }

    /// Whether `token` was set explicitly (not inherited from Dracula).
    pub fn is_overridden(&self, token: Token) -> bool {
        self.overridden[token as usize]
    }
}

fn resolve(token: Token, explicit: &[Option<Rgb>], depth: u8) -> Rgb {
    if let Some(rgb) = explicit[token as usize] {
        return rgb;
    }
    // Sources form a shallow DAG; the depth cap is a cheap cycle guard.
    if depth > 8 {
        return Rgb(0, 0, 0);
    }
    match token.dracula() {
        TokenSource::Fixed(rgb) => rgb,
        TokenSource::Alias(other) => resolve(other, explicit, depth + 1),
        TokenSource::Tint(other, pct) => resolve(other, explicit, depth + 1)
            .blend_over(resolve(Token::Bg, explicit, depth + 1), pct),
    }
}

/// How many colours the terminal can show.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorDepth {
    /// 24-bit colour.
    TrueColor,
    /// xterm 256-colour palette.
    Ansi256,
    /// The 16 basic ANSI colours.
    Ansi16,
    /// No colour (`NO_COLOR` or `[ui] theme = none`).
    None,
}

impl ColorDepth {
    /// Detects the depth from the process environment.
    pub fn detect() -> ColorDepth {
        ColorDepth::detect_with(|k| std::env::var(k).ok())
    }

    /// Detects the depth from `env`: `NO_COLOR` (non-empty) wins, then
    /// `COLORTERM=truecolor|24bit`, then a `TERM` containing `256color`,
    /// otherwise 16 colours.
    pub fn detect_with(env: impl Fn(&str) -> Option<String>) -> ColorDepth {
        if env("NO_COLOR").is_some_and(|v| !v.is_empty()) {
            return ColorDepth::None;
        }
        let colorterm = env("COLORTERM").unwrap_or_default().to_ascii_lowercase();
        if colorterm == "truecolor" || colorterm == "24bit" {
            return ColorDepth::TrueColor;
        }
        if env("TERM").is_some_and(|t| t.contains("256color")) {
            return ColorDepth::Ansi256;
        }
        ColorDepth::Ansi16
    }
}

/// A `[theme]` problem found while building the theme.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThemeError {
    /// Where the bad entry is, when known.
    pub location: Option<String>,
    /// What is wrong.
    pub message: String,
}

impl fmt::Display for ThemeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.location {
            Some(loc) => write!(f, "{loc}: {}", self.message),
            None => f.write_str(&self.message),
        }
    }
}

impl std::error::Error for ThemeError {}

/// Resolved theme: palette, colour depth and background policy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Theme {
    palette: Palette,
    depth: ColorDepth,
    paint_background: bool,
}

impl Default for Theme {
    fn default() -> Self {
        Theme::new(Palette::dracula(), ColorDepth::TrueColor, true)
    }
}

impl Theme {
    /// Creates a theme from its parts.
    pub fn new(palette: Palette, depth: ColorDepth, paint_background: bool) -> Theme {
        Theme {
            palette,
            depth,
            paint_background,
        }
    }

    /// Builds the theme from `harness.ini` settings and the process environment.
    pub fn from_config(cfg: &Config) -> Result<Theme, Vec<ThemeError>> {
        Theme::from_config_with(cfg, ColorDepth::detect())
    }

    /// Builds the theme for a known terminal `depth`. `[ui] theme = none`
    /// forces [`ColorDepth::None`]. Unknown `[theme]` token names are errors
    /// that carry the `file:line` of the entry; hex values were already
    /// validated by `harness_config` (also with line numbers), but are checked
    /// again here so a hand-built [`Config`] cannot slip through.
    pub fn from_config_with(cfg: &Config, depth: ColorDepth) -> Result<Theme, Vec<ThemeError>> {
        let mut errors = Vec::new();
        let mut overrides = Vec::new();
        for (key, value) in &cfg.theme {
            let location = cfg
                .origins
                .get(&format!("theme.{key}"))
                .map(Location::to_string);
            match (Token::from_name(key), Rgb::parse_hex(value)) {
                (Some(t), Some(rgb)) => overrides.push((t, rgb)),
                (None, _) => errors.push(ThemeError {
                    location,
                    message: format!(
                        "unknown theme token `{key}`; known tokens: {}",
                        Token::ALL
                            .iter()
                            .map(|t| t.name())
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                }),
                (Some(_), None) => errors.push(ThemeError {
                    location,
                    message: format!("expected a hex colour like `#bd93f9`, got `{value}`"),
                }),
            }
        }
        if !errors.is_empty() {
            return Err(errors);
        }
        let depth = if cfg.ui.theme == UiTheme::None {
            ColorDepth::None
        } else {
            depth
        };
        let paint = cfg.ui.background == Background::Theme;
        Ok(Theme::new(
            Palette::with_overrides(&overrides),
            depth,
            paint,
        ))
    }

    /// The palette.
    pub fn palette(&self) -> &Palette {
        &self.palette
    }

    /// The colour depth in use.
    pub fn depth(&self) -> ColorDepth {
        self.depth
    }

    /// Whether colour is enabled at all.
    pub fn has_color(&self) -> bool {
        self.depth != ColorDepth::None
    }

    /// Whether the theme paints its own background (`[ui] background = theme`).
    pub fn paints_background(&self) -> bool {
        self.paint_background
    }

    /// Returns a copy with a different colour depth.
    pub fn with_depth(mut self, depth: ColorDepth) -> Theme {
        self.depth = depth;
        self
    }

    /// The concrete terminal colour for `token` at the current depth.
    /// [`Color::Reset`] means "terminal default".
    pub fn color(&self, token: Token) -> Color {
        let rgb = self.palette.rgb(token);
        match self.depth {
            ColorDepth::None => Color::Reset,
            ColorDepth::TrueColor => Color::Rgb(rgb.0, rgb.1, rgb.2),
            ColorDepth::Ansi256 => Color::Indexed(nearest_256(rgb)),
            ColorDepth::Ansi16 => {
                if self.palette.is_overridden(token) {
                    Ansi16::nearest(rgb).color()
                } else {
                    token.ansi16().map_or(Color::Reset, Ansi16::color)
                }
            }
        }
    }

    /// Foreground style for `token`. `reasoning` is italic.
    pub fn fg(&self, token: Token) -> Style {
        let mut style = Style::new();
        if self.has_color() {
            style = style.fg(self.color(token));
        }
        if token == Token::Reasoning {
            style = style.add_modifier(Modifier::ITALIC);
        }
        style
    }

    /// Background style for `token` (no-op without colour).
    pub fn bg(&self, token: Token) -> Style {
        if self.has_color() {
            Style::new().bg(self.color(token))
        } else {
            Style::new()
        }
    }

    /// Foreground `fg` over background `bg`.
    pub fn fg_on(&self, fg: Token, bg: Token) -> Style {
        self.fg(fg).patch(self.bg(bg))
    }

    /// Base style for the whole screen: `fg` text over `bg` when the theme
    /// paints the background, otherwise over the terminal's own background.
    pub fn base(&self) -> Style {
        let style = self.fg(Token::Fg);
        if self.paint_background && self.has_color() {
            style.bg(self.color(Token::Bg))
        } else {
            style
        }
    }

    /// Style of the focused element: `surface` background in colour, reverse
    /// video without colour so focus stays visible.
    pub fn focused(&self) -> Style {
        if self.has_color() {
            self.bg(Token::Surface)
        } else {
            Style::new().add_modifier(Modifier::REVERSED)
        }
    }

    /// Border style, focused or not. Without colour a focused border is bold.
    pub fn border(&self, focused: bool) -> Style {
        if focused {
            self.fg(Token::BorderFocus).add_modifier(Modifier::BOLD)
        } else {
            self.fg(Token::Border)
        }
    }
}

fn distance(a: Rgb, b: Rgb) -> u32 {
    let d = |x: u8, y: u8| {
        let v = i32::from(x) - i32::from(y);
        (v * v) as u32
    };
    d(a.0, b.0) + d(a.1, b.1) + d(a.2, b.2)
}

/// Nearest xterm-256 index for `rgb`, choosing between the 6x6x6 cube
/// (16-231) and the grey ramp (232-255).
pub fn nearest_256(rgb: Rgb) -> u8 {
    const LEVELS: [u8; 6] = [0, 95, 135, 175, 215, 255];
    let level = |v: u8| -> usize {
        (0..6)
            .min_by_key(|&i| (i32::from(LEVELS[i]) - i32::from(v)).abs())
            .unwrap_or(0)
    };
    let (r, g, b) = (level(rgb.0), level(rgb.1), level(rgb.2));
    let cube = Rgb(LEVELS[r], LEVELS[g], LEVELS[b]);
    let cube_index = 16 + 36 * r + 6 * g + b;

    let avg = (u32::from(rgb.0) + u32::from(rgb.1) + u32::from(rgb.2)) / 3;
    let grey_step = ((avg.saturating_sub(8)) / 10).min(23) as u8;
    let grey_value = 8 + 10 * grey_step;
    let grey = Rgb(grey_value, grey_value, grey_value);

    if distance(grey, rgb) < distance(cube, rgb) {
        232 + grey_step
    } else {
        cube_index as u8
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interaction::testing::styled_snapshot;
    use harness_config::ini::load_str;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::text::{Line, Span};
    use ratatui::widgets::{Block, Borders, Paragraph};
    use std::collections::HashSet;
    use std::path::Path;

    const STATUS: [Token; 6] = [
        Token::Ok,
        Token::Warn,
        Token::Error,
        Token::Running,
        Token::Accent,
        Token::User,
    ];

    fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let pairs: Vec<(String, String)> = pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect();
        move |k| pairs.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone())
    }

    #[test]
    fn dracula_matches_design_doc() {
        let p = Palette::dracula();
        let hex = |t| p.rgb(t).to_string();
        assert_eq!(hex(Token::Bg), "#282a36");
        assert_eq!(hex(Token::Surface), "#44475a");
        assert_eq!(hex(Token::Fg), "#f8f8f2");
        assert_eq!(hex(Token::Muted), "#6272a4");
        assert_eq!(hex(Token::Running), "#8be9fd");
        assert_eq!(hex(Token::Ok), "#50fa7b");
        assert_eq!(hex(Token::Warn), "#ffb86c");
        assert_eq!(hex(Token::User), "#ff79c6");
        assert_eq!(hex(Token::Accent), "#bd93f9");
        assert_eq!(hex(Token::Error), "#ff5555");
        assert_eq!(hex(Token::Number), "#f1fa8c");
        assert_eq!(p.rgb(Token::Approval), p.rgb(Token::Warn));
        assert_eq!(p.rgb(Token::CardPending), p.rgb(Token::Surface));
        assert_eq!(p.rgb(Token::DiffAdded), p.rgb(Token::Ok));
        assert_eq!(p.rgb(Token::DiffRemoved), p.rgb(Token::Error));
    }

    #[test]
    fn card_tints_and_diff_backgrounds_blend_over_bg() {
        let p = Palette::dracula();
        let bg = p.rgb(Token::Bg);
        assert_eq!(p.rgb(Token::CardOk), p.rgb(Token::Ok).blend_over(bg, 8));
        assert_eq!(
            p.rgb(Token::CardError),
            p.rgb(Token::Error).blend_over(bg, 10)
        );
        assert_eq!(
            p.rgb(Token::CardApproval),
            p.rgb(Token::Warn).blend_over(bg, 10)
        );
        assert_eq!(
            p.rgb(Token::DiffAddedBg),
            p.rgb(Token::Ok).blend_over(bg, 15)
        );
        assert_eq!(
            p.rgb(Token::DiffRemovedBg),
            p.rgb(Token::Error).blend_over(bg, 15)
        );
        // 8 % green over Dracula bg, computed by hand.
        assert_eq!(p.rgb(Token::CardOk), Rgb(0x2b, 0x3b, 0x3c));
    }

    #[test]
    fn token_names_round_trip_and_are_unique() {
        let mut seen = HashSet::new();
        for t in Token::ALL {
            assert!(seen.insert(t.name()), "duplicate {}", t.name());
            assert_eq!(Token::from_name(t.name()), Some(*t));
        }
        assert_eq!(Token::from_name("nope"), None);
    }

    #[test]
    fn hex_parsing() {
        assert_eq!(Rgb::parse_hex("#BD93f9"), Some(Rgb(0xbd, 0x93, 0xf9)));
        for bad in ["bd93f9", "#bd93f", "#bd93fg", "#bd93f9a", ""] {
            assert_eq!(Rgb::parse_hex(bad), None, "{bad}");
        }
    }

    #[test]
    fn depth_detection() {
        use ColorDepth::*;
        assert_eq!(
            ColorDepth::detect_with(env(&[("COLORTERM", "truecolor")])),
            TrueColor
        );
        assert_eq!(
            ColorDepth::detect_with(env(&[("COLORTERM", "24bit")])),
            TrueColor
        );
        assert_eq!(
            ColorDepth::detect_with(env(&[("TERM", "xterm-256color")])),
            Ansi256
        );
        assert_eq!(ColorDepth::detect_with(env(&[("TERM", "xterm")])), Ansi16);
        assert_eq!(ColorDepth::detect_with(env(&[])), Ansi16);
        assert_eq!(
            ColorDepth::detect_with(env(&[("COLORTERM", "truecolor"), ("NO_COLOR", "1")])),
            None
        );
        // An empty NO_COLOR does not disable colour (no-color.org).
        assert_eq!(
            ColorDepth::detect_with(env(&[("COLORTERM", "truecolor"), ("NO_COLOR", "")])),
            TrueColor
        );
    }

    #[test]
    fn fallbacks_keep_status_colours_distinct() {
        for depth in [
            ColorDepth::TrueColor,
            ColorDepth::Ansi256,
            ColorDepth::Ansi16,
        ] {
            let theme = Theme::default().with_depth(depth);
            let colors: HashSet<_> = STATUS.iter().map(|t| theme.color(*t)).collect();
            assert_eq!(colors.len(), STATUS.len(), "{depth:?}: {colors:?}");
            // Status colours must also differ from the background.
            for t in STATUS {
                assert_ne!(theme.color(t), theme.color(Token::Bg), "{depth:?} {t}");
            }
        }
    }

    #[test]
    fn nearest_256_examples() {
        assert_eq!(nearest_256(Rgb(0, 0, 0)), 16);
        assert_eq!(nearest_256(Rgb(255, 255, 255)), 231);
        assert_eq!(nearest_256(Rgb(0xff, 0x55, 0x55)), 203);
        // Dracula bg is a dark grey-blue; the grey ramp wins.
        assert_eq!(nearest_256(Rgb(0x28, 0x2a, 0x36)), 235);
    }

    #[test]
    fn no_color_keeps_modifiers_and_drops_colours() {
        let theme = Theme::default().with_depth(ColorDepth::None);
        assert!(!theme.has_color());
        for t in Token::ALL {
            let s = theme.fg(*t).patch(theme.bg(*t));
            assert_eq!(s.fg, None, "{t}");
            assert_eq!(s.bg, None, "{t}");
        }
        assert!(
            theme
                .fg(Token::Reasoning)
                .add_modifier
                .contains(Modifier::ITALIC)
        );
        assert!(theme.focused().add_modifier.contains(Modifier::REVERSED));
        assert_eq!(theme.base().bg, None);
    }

    #[test]
    fn ui_theme_none_disables_colour() {
        let cfg = load_str("[ui]\ntheme = none\n", None).unwrap();
        let theme = Theme::from_config_with(&cfg, ColorDepth::TrueColor).unwrap();
        assert_eq!(theme.depth(), ColorDepth::None);
    }

    #[test]
    fn background_setting_controls_painting() {
        let cfg = load_str("[ui]\nbackground = theme\n", None).unwrap();
        let theme = Theme::from_config_with(&cfg, ColorDepth::TrueColor).unwrap();
        assert_eq!(theme.base().bg, Some(theme.color(Token::Bg)));

        let cfg = load_str("[ui]\nbackground = terminal\n", None).unwrap();
        let theme = Theme::from_config_with(&cfg, ColorDepth::TrueColor).unwrap();
        assert!(!theme.paints_background());
        assert_eq!(theme.base().bg, None);
    }

    #[test]
    fn every_token_can_be_overridden() {
        let mut text = String::from("[theme]\n");
        for t in Token::ALL {
            text.push_str(&format!("{} = #010203\n", t.name()));
        }
        let cfg = load_str(&text, None).unwrap();
        let theme = Theme::from_config_with(&cfg, ColorDepth::TrueColor).unwrap();
        for t in Token::ALL {
            assert_eq!(theme.palette().rgb(*t), Rgb(1, 2, 3), "{t}");
            assert_eq!(theme.color(*t), Color::Rgb(1, 2, 3)); // raw-color-ok
        }
    }

    #[test]
    fn derived_tokens_follow_overridden_base() {
        let cfg = load_str("[theme]\nwarn = #ff0000\n", None).unwrap();
        let theme = Theme::from_config_with(&cfg, ColorDepth::TrueColor).unwrap();
        let p = theme.palette();
        assert_eq!(p.rgb(Token::Approval), Rgb(255, 0, 0));
        assert_eq!(
            p.rgb(Token::CardApproval),
            Rgb(255, 0, 0).blend_over(p.rgb(Token::Bg), 10)
        );
        // In 16-colour mode an override maps to its nearest ANSI colour.
        let theme16 = theme.with_depth(ColorDepth::Ansi16);
        assert_eq!(theme16.color(Token::Warn), Ansi16::LightRed.color());
    }

    #[test]
    fn invalid_hex_is_a_load_error_with_line_number() {
        let errs = load_str(
            "[ui]\nmouse = true\n[theme]\naccent = purple\n",
            Some(Path::new("/c/harness.ini")),
        )
        .unwrap_err();
        let msg = errs.to_string();
        assert!(msg.contains("/c/harness.ini:4"), "{msg}");
        assert!(msg.contains("purple"), "{msg}");
    }

    #[test]
    fn unknown_token_is_an_error_with_line_number() {
        let cfg = load_str(
            "[theme]\naccent = #ffffff\naccnet = #000000\n",
            Some(Path::new("/c/harness.ini")),
        )
        .unwrap();
        let errs = Theme::from_config_with(&cfg, ColorDepth::TrueColor).unwrap_err();
        assert_eq!(errs.len(), 1);
        let msg = errs[0].to_string();
        assert!(
            msg.starts_with("/c/harness.ini:3: unknown theme token `accnet`"),
            "{msg}"
        );
    }

    /// Enforces "no component uses a raw colour": every module except this
    /// one must go through [`Theme`]. A line may opt out with `raw-color-ok`.
    #[test]
    fn no_raw_colors_outside_theme() {
        let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let shorthand = [
            ".black()",
            ".red()",
            ".green()",
            ".yellow()",
            ".blue()",
            ".magenta()",
            ".cyan()",
            ".gray()",
            ".dark_gray()",
            ".white()",
            ".light_",
            ".on_",
        ];
        let mut offenders = Vec::new();
        let mut stack = vec![src.clone()];
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    stack.push(path);
                    continue;
                }
                if path.extension().is_none_or(|e| e != "rs") || path.ends_with("src/theme.rs") {
                    continue;
                }
                let text = std::fs::read_to_string(&path).unwrap();
                for (i, line) in text.lines().enumerate() {
                    let code = line.trim_start();
                    if code.starts_with("//") || line.contains("raw-color-ok") {
                        continue;
                    }
                    let raw = code.contains("Color::")
                        || code.contains("style::Color")
                        || shorthand.iter().any(|s| code.contains(s));
                    if raw {
                        offenders.push(format!("{}:{}: {}", path.display(), i + 1, code));
                    }
                }
            }
        }
        assert!(
            offenders.is_empty(),
            "raw colours found; use theme::Theme tokens instead:\n{}",
            offenders.join("\n")
        );
    }

    /// A small card + diff + status sample rendered with theme tokens only.
    fn sample(theme: &Theme) -> String {
        let mut term = Terminal::new(TestBackend::new(40, 6)).unwrap();
        term.draw(|f| {
            let area = f.area();
            f.render_widget(Block::new().style(theme.base()), area);
            let block = Block::new()
                .borders(Borders::ALL)
                .border_style(theme.border(false))
                .style(theme.bg(Token::CardOk))
                .title(Line::from(vec![
                    Span::styled("✔ ", theme.fg(Token::Ok)),
                    Span::styled("edit ", theme.fg(Token::Accent)),
                    Span::styled("src/a.rs", theme.fg(Token::Path)),
                ]));
            let body = Paragraph::new(vec![
                Line::from(Span::styled(
                    "+ added",
                    theme.fg_on(Token::DiffAdded, Token::DiffAddedBg),
                )),
                Line::from(Span::styled(
                    "- removed",
                    theme.fg_on(Token::DiffRemoved, Token::DiffRemovedBg),
                )),
                Line::from(vec![
                    Span::styled("⠹ ", theme.fg(Token::Running)),
                    Span::styled("thinking ", theme.fg(Token::Reasoning)),
                    Span::styled("$0.02", theme.fg(Token::Number)),
                ]),
            ])
            .block(block);
            f.render_widget(body, area);
        })
        .unwrap();
        styled_snapshot(term.backend().buffer())
    }

    #[test]
    fn snapshot_sample_truecolor() {
        insta::assert_snapshot!(sample(&Theme::default()));
    }

    #[test]
    fn snapshot_sample_256() {
        insta::assert_snapshot!(sample(&Theme::default().with_depth(ColorDepth::Ansi256)));
    }

    #[test]
    fn snapshot_sample_16() {
        insta::assert_snapshot!(sample(&Theme::default().with_depth(ColorDepth::Ansi16)));
    }

    #[test]
    fn snapshot_sample_no_color() {
        insta::assert_snapshot!(sample(&Theme::default().with_depth(ColorDepth::None)));
    }
}
