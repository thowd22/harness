//! Terminal-safe panic handling.
//!
//! A panic while the TUI owns the terminal (raw mode, alternate screen, mouse
//! capture) would otherwise leave the user's shell unusable. [`install_panic_hook`]
//! wraps the existing panic hook so that, in order, it:
//!
//! 1. restores the terminal ([`restore_terminal`]),
//! 2. records the panic (message, location, backtrace) with `tracing` at
//!    `error` level under target [`PANIC_TARGET`], so it lands in the
//!    redacted log file,
//! 3. runs the previous hook, which prints the panic to stderr on a sane
//!    terminal.
//!
//! Call it once at startup, after logging is initialised and before entering
//! raw mode.

use std::backtrace::Backtrace;
use std::io::{self, Write};

use crossterm::event::{DisableBracketedPaste, DisableMouseCapture};
use crossterm::terminal::{LeaveAlternateScreen, disable_raw_mode, is_raw_mode_enabled};
use crossterm::{cursor::Show, execute};

/// `tracing` target used for panic log records.
pub const PANIC_TARGET: &str = "harness::panic";

/// Returns the terminal to cooked mode: leaves the alternate screen, disables
/// mouse capture and bracketed paste, shows the cursor and disables raw mode.
///
/// Does nothing when raw mode is not enabled, so it is safe to call from
/// headless runs and multiple times.
pub fn restore_terminal() -> io::Result<()> {
    if !is_raw_mode_enabled()? {
        return Ok(());
    }
    let mut out = io::stdout();
    // Attempt every step even if one fails; report the first error.
    let screen = execute!(
        out,
        LeaveAlternateScreen,
        DisableMouseCapture,
        DisableBracketedPaste,
        Show
    );
    let raw = disable_raw_mode();
    let _ = out.flush();
    screen.and(raw)
}

/// Installs the terminal-restoring panic hook using [`restore_terminal`].
pub fn install_panic_hook() {
    install_panic_hook_with(|| {
        let _ = restore_terminal();
    });
}

/// Installs the panic hook with a custom `restore` step (used by tests and by
/// callers with non-standard terminal setup). The previous hook still runs
/// last.
pub fn install_panic_hook_with<F>(restore: F)
where
    F: Fn() + Send + Sync + 'static,
{
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        restore();
        let backtrace = Backtrace::capture();
        tracing::error!(
            target: PANIC_TARGET,
            backtrace = %backtrace,
            "panic: {info}"
        );
        previous(info);
    }));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restore_is_noop_without_raw_mode() {
        // Test runners never enable raw mode; this must not error or print.
        restore_terminal().unwrap();
        restore_terminal().unwrap();
    }
}
