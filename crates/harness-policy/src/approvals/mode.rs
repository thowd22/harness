//! Autonomy mode selection: ini, palette and per agent.

use crate::rules::Mode;

/// The warning shown when the mode becomes yolo.
pub const YOLO_WARNING: &str = "nothing is confirmed: commands, edits, pushes and publishes run \
                                without asking (deny rules and the sandbox still apply)";

/// Prefix of the palette command ids that select a mode.
pub const PALETTE_MODE_PREFIX: &str = "mode.";

/// One command-palette entry that selects a mode.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaletteModeEntry {
    /// Command id (`mode.ask`, `mode.auto-edit`, `mode.auto`).
    pub id: String,
    /// Title shown in the palette.
    pub title: String,
    /// Short help text.
    pub description: &'static str,
    /// The mode it selects.
    pub mode: Mode,
}

/// Palette entries for every mode, most supervised first.
pub fn palette_entries() -> Vec<PaletteModeEntry> {
    Mode::ALL
        .into_iter()
        .map(|mode| PaletteModeEntry {
            id: format!("{PALETTE_MODE_PREFIX}{mode}"),
            title: format!("Mode: {mode}"),
            description: match mode {
                Mode::Ask => "Ask before every edit and command",
                Mode::AutoEdit => "Edit files freely, ask before commands",
                Mode::Auto => "Act without asking (rules and outward guard still apply)",
                Mode::Yolo => "Never ask: nothing is confirmed (deny rules still apply)",
            },
            mode,
        })
        .collect()
}

/// The session's autonomy mode.
///
/// The ini value (`[general] mode`) is the default; the palette overrides it
/// for the session; an agent's YAML `mode` can only make it stricter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModeState {
    ini: Mode,
    palette: Option<Mode>,
}

impl ModeState {
    /// Start from the ini mode.
    pub fn new(ini: Mode) -> Self {
        Self { ini, palette: None }
    }

    /// The ini default.
    pub fn ini(&self) -> Mode {
        self.ini
    }

    /// Override the mode for this session (command palette).
    pub fn set_palette(&mut self, mode: Mode) {
        self.palette = Some(mode);
    }

    /// Apply a palette command id (`mode.auto`); returns the new mode, or
    /// `None` if the id is not a mode command.
    pub fn apply_palette_command(&mut self, id: &str) -> Option<Mode> {
        let mode = Mode::parse(id.strip_prefix(PALETTE_MODE_PREFIX)?)?;
        self.set_palette(mode);
        Some(mode)
    }

    /// Select the next mode (Ask → AutoEdit → Auto → Yolo → Ask), e.g.
    /// for a key.
    pub fn cycle(&mut self) -> Mode {
        let next = match self.session() {
            Mode::Ask => Mode::AutoEdit,
            Mode::AutoEdit => Mode::Auto,
            Mode::Auto => Mode::Yolo,
            Mode::Yolo => Mode::Ask,
        };
        self.set_palette(next);
        next
    }

    /// Drop the palette override.
    pub fn reset(&mut self) {
        self.palette = None;
    }

    /// The session mode (palette override or ini default).
    pub fn session(&self) -> Mode {
        self.palette.unwrap_or(self.ini)
    }

    /// The mode for an agent whose YAML sets `agent` (if any): the stricter
    /// of the session mode and the agent's.
    pub fn effective(&self, agent: Option<Mode>) -> Mode {
        match agent {
            Some(a) => a.min(self.session()),
            None => self.session(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ini_palette_agent() {
        let mut s = ModeState::new(Mode::AutoEdit);
        assert_eq!(s.session(), Mode::AutoEdit);
        assert_eq!(s.apply_palette_command("mode.auto"), Some(Mode::Auto));
        assert_eq!(s.session(), Mode::Auto);
        assert_eq!(s.effective(Some(Mode::Ask)), Mode::Ask);
        assert_eq!(s.effective(None), Mode::Auto);
        s.set_palette(Mode::Ask);
        assert_eq!(
            s.effective(Some(Mode::Auto)),
            Mode::Ask,
            "agent cannot loosen"
        );
        assert_eq!(s.apply_palette_command("theme.dark"), None);
        assert_eq!(s.apply_palette_command("mode.bogus"), None);
        s.reset();
        assert_eq!(s.session(), Mode::AutoEdit);
        assert_eq!(s.cycle(), Mode::Auto);
        assert_eq!(s.cycle(), Mode::Yolo);
        assert_eq!(s.cycle(), Mode::Ask);
        // An agent's mode caps yolo like any other.
        s.set_palette(Mode::Yolo);
        assert_eq!(s.effective(Some(Mode::Auto)), Mode::Auto);
    }

    #[test]
    fn palette_lists_all_modes() {
        let e = palette_entries();
        assert_eq!(
            e.iter().map(|e| e.id.as_str()).collect::<Vec<_>>(),
            ["mode.ask", "mode.auto-edit", "mode.auto", "mode.yolo"]
        );
        let mut s = ModeState::new(Mode::Ask);
        for entry in &e {
            assert_eq!(s.apply_palette_command(&entry.id), Some(entry.mode));
        }
    }

    #[test]
    fn mode_serde_uses_ini_spelling() {
        assert_eq!(
            serde_json::to_string(&Mode::AutoEdit).unwrap(),
            "\"auto-edit\""
        );
        assert_eq!(Mode::parse("auto-edit"), Some(Mode::AutoEdit));
        assert_eq!(Mode::parse("yolo"), Some(Mode::Yolo));
        assert_eq!(serde_json::to_string(&Mode::Yolo).unwrap(), "\"yolo\"");
        let m: Mode = serde_norway::from_str("ask").unwrap();
        assert_eq!(m, Mode::Ask);
    }
}
