//! The clipboard behind every copy action.
//!
//! Mouse capture (needed for clickable elements) turns off the terminal's
//! own text selection, so the TUI copies by itself. [`Clipboard::copy`]
//! cleans the text first ([`plain`]: no ANSI codes or control characters,
//! secrets masked by the session [`Redactor`]) and then sends it one of two
//! ways, chosen by `[ui] clipboard` ([`ClipboardMode`]):
//!
//! - `osc52` (default): the OSC 52 escape sequence ([`osc52`]). The
//!   terminal sets its clipboard, so it works over SSH. Inside tmux
//!   (`$TMUX` set) the sequence is wrapped for DCS passthrough
//!   ([`tmux_passthrough`]; needs `set -g allow-passthrough on`).
//! - `system`: a clipboard tool ([`SystemTool`]): `wl-copy` on Wayland,
//!   `xclip` or `xsel` on X11, `pbcopy` on macOS. When none is found, or it
//!   fails, the copy falls back to OSC 52.
//! - `auto`: the system tool when one is usable locally (a display and the
//!   tool are there and the session is not over SSH), else OSC 52.
//!
//! The transcript's keyboard selection mode copies rendered lines;
//! [`plain_line`] turns one rendered line back into text without card
//! borders, gutters, line numbers or copy buttons.

use std::io::{self, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

pub use harness_config::ini::config::ClipboardMode;
use harness_events::redact::Redactor;

/// Label of a copy button (messages, code blocks).
pub const COPY_LABEL: &str = "⧉ copy";
/// Label of a tool card's "copy output" button.
pub const COPY_OUTPUT_LABEL: &str = "⧉ copy output";
/// Label of a tool card's "copy diff" button.
pub const COPY_DIFF_LABEL: &str = "⧉ copy diff";

/// How long a clipboard tool may take before the copy counts as done.
const TOOL_WAIT: Duration = Duration::from_secs(2);

/// Environment variables passed to clipboard tools (everything else,
/// including API keys, is scrubbed).
const TOOL_ENV: &[&str] = &[
    "PATH",
    "HOME",
    "DISPLAY",
    "WAYLAND_DISPLAY",
    "XDG_RUNTIME_DIR",
    "XAUTHORITY",
    "DBUS_SESSION_BUS_ADDRESS",
    "TMPDIR",
    "LANG",
    "LC_ALL",
    "LC_CTYPE",
];

/// The OSC 52 sequence that puts `text` on the clipboard.
pub fn osc52(text: &str) -> String {
    format!("\u{1b}]52;c;{}\u{7}", base64_encode(text.as_bytes()))
}

/// Wraps an escape sequence for tmux DCS passthrough: every ESC inside is
/// doubled and the whole sequence is sent as `ESC P tmux; … ESC \`.
pub fn tmux_passthrough(seq: &str) -> String {
    format!(
        "\u{1b}Ptmux;{}\u{1b}\\",
        seq.replace('\u{1b}', "\u{1b}\u{1b}")
    )
}

/// Standard base64 with padding.
pub fn base64_encode(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [
            chunk[0],
            chunk.get(1).copied().unwrap_or(0),
            chunk.get(2).copied().unwrap_or(0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            T[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            T[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

/// `text` without terminal escape sequences (CSI, OSC, DCS and two-byte
/// escapes) or control characters other than newline and tab. `\r\n`
/// becomes `\n`.
pub fn plain(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut it = text.chars().peekable();
    while let Some(c) = it.next() {
        match c {
            '\u{1b}' => match it.next() {
                // CSI: parameters and intermediates up to a final byte.
                Some('[') => {
                    for c in it.by_ref() {
                        if ('\u{40}'..='\u{7e}').contains(&c) {
                            break;
                        }
                    }
                }
                // OSC / DCS / APC / PM / SOS: up to BEL or ESC \.
                Some(']' | 'P' | '_' | '^' | 'X') => {
                    while let Some(c) = it.next() {
                        if c == '\u{7}' {
                            break;
                        }
                        if c == '\u{1b}' && it.peek() == Some(&'\\') {
                            it.next();
                            break;
                        }
                    }
                }
                _ => {}
            },
            '\r' if it.peek() == Some(&'\n') => {}
            '\n' | '\t' => out.push(c),
            c if c.is_control() => {}
            c => out.push(c),
        }
    }
    out
}

/// The text of one rendered transcript line, without card borders,
/// gutters (`  12 │`), copy buttons and padding. `None` for lines that are
/// only decoration (card bottoms, rules, expand hints).
pub fn plain_line(rendered: &str) -> Option<String> {
    let mut s = rendered.trim_end().to_owned();
    for label in [COPY_OUTPUT_LABEL, COPY_DIFF_LABEL] {
        s = s.replace(label, "");
    }
    // A message or code block header: drop the button and what follows it
    // (the right-aligned timestamp).
    if let Some(i) = s.find(COPY_LABEL) {
        s.truncate(i);
        if s.trim().is_empty() {
            return None;
        }
    }
    let s = s.trim_end();
    let decoration = |c: char| matches!(c, '─' | '╭' | '╮' | '╰' | '╯' | '│' | ' ' | '·');
    if s.chars().all(decoration) {
        return None;
    }
    let mut s = s.to_owned();
    // Card frame: `╭ header ──── right ╮` and `│ body │`.
    if let Some(rest) = s.strip_prefix("╭ ") {
        s = rest.trim_end_matches('╮').trim_end().to_owned();
    } else if let Some(rest) = s.strip_prefix("│ ") {
        s = rest.trim_end_matches('│').trim_end().to_owned();
    }
    // Runs of the border fill between a header's left and right parts.
    while let Some(i) = s.find("───") {
        let end = s[i..]
            .char_indices()
            .find(|(_, c)| *c != '─')
            .map_or(s.len(), |(j, _)| i + j);
        s.replace_range(i..end, "");
    }
    // Message and block markers.
    for marker in ["▌ ", "┊ ", "▎ ", "│ "] {
        if let Some(rest) = s.strip_prefix(marker) {
            s = rest.to_owned();
            break;
        }
    }
    // Line-number gutters: `  12 │ text` (read) and `   6 │-text` (diffs).
    if let Some(i) = s.find(" │")
        && i > 0
        && s[..i].trim_start().chars().all(|c| c.is_ascii_digit())
        && !s[..i].trim().is_empty()
    {
        let rest = &s[i + " │".len()..];
        s = rest.strip_prefix(' ').unwrap_or(rest).to_owned();
    }
    let t = s.trim();
    if t.ends_with("Enter to expand") || t == "Enter to collapse" {
        return None;
    }
    Some(s.trim_end().to_owned())
}

/// A clipboard tool and its arguments; the text goes to its stdin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SystemTool {
    /// Program name or path.
    pub program: String,
    /// Arguments.
    pub args: Vec<String>,
}

impl SystemTool {
    /// A tool from a program and its arguments.
    pub fn new(program: impl Into<String>, args: &[&str]) -> SystemTool {
        SystemTool {
            program: program.into(),
            args: args.iter().map(|a| (*a).to_owned()).collect(),
        }
    }

    /// The program's file name, for messages (`wl-copy`).
    pub fn name(&self) -> &str {
        Path::new(&self.program)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or(&self.program)
    }

    /// The tool for this session: `wl-copy` with `$WAYLAND_DISPLAY`,
    /// `xclip` or `xsel` with `$DISPLAY`, else `pbcopy`. `env` reads an
    /// environment variable and `exists` says whether a program is on PATH.
    pub fn detect(
        env: &dyn Fn(&str) -> Option<String>,
        exists: &dyn Fn(&str) -> bool,
    ) -> Option<SystemTool> {
        let set = |k: &str| env(k).is_some_and(|v| !v.is_empty());
        if set("WAYLAND_DISPLAY") && exists("wl-copy") {
            return Some(SystemTool::new("wl-copy", &[]));
        }
        if set("DISPLAY") {
            if exists("xclip") {
                return Some(SystemTool::new("xclip", &["-selection", "clipboard"]));
            }
            if exists("xsel") {
                return Some(SystemTool::new("xsel", &["--clipboard", "--input"]));
            }
        }
        exists("pbcopy").then(|| SystemTool::new("pbcopy", &[]))
    }

    /// Runs the tool with `text` on stdin and a scrubbed environment. Waits
    /// briefly for it to exit; tools that keep serving the selection count
    /// as done.
    pub fn copy(&self, text: &str) -> Result<(), String> {
        let mut cmd = Command::new(&self.program);
        cmd.args(&self.args)
            .env_clear()
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        for k in TOOL_ENV {
            if let Some(v) = std::env::var_os(k) {
                cmd.env(k, v);
            }
        }
        let mut child = cmd.spawn().map_err(|e| format!("{}: {e}", self.name()))?;
        let written = child
            .stdin
            .take()
            .map(|mut stdin| stdin.write_all(text.as_bytes()))
            .unwrap_or(Ok(()));
        let start = Instant::now();
        loop {
            match child.try_wait() {
                Ok(Some(status)) if status.success() => break,
                Ok(Some(status)) => return Err(format!("{} exited with {status}", self.name())),
                Ok(None) if start.elapsed() < TOOL_WAIT => {
                    std::thread::sleep(Duration::from_millis(10))
                }
                Ok(None) => {
                    // Still serving the selection: reap it in the background.
                    std::thread::spawn(move || child.wait());
                    break;
                }
                Err(e) => return Err(format!("{}: {e}", self.name())),
            }
        }
        written.map_err(|e| format!("{}: {e}", self.name()))
    }
}

/// Whether `program` is an executable file on `$PATH`.
fn on_path(program: &str) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::env::var_os("PATH").is_some_and(|paths| {
        std::env::split_paths(&paths).any(|dir| {
            std::fs::metadata(dir.join(program))
                .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        })
    })
}

/// What a copy did, for the toast.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Copied {
    /// How it was sent: `OSC 52`, `OSC 52 (tmux)` or the tool name.
    pub via: String,
    /// Lines copied.
    pub lines: usize,
    /// Characters copied.
    pub chars: usize,
    /// Why the configured tool was not used, when the copy fell back.
    pub fallback: Option<String>,
}

impl Copied {
    /// One-line summary, e.g. `3 lines · OSC 52`.
    pub fn summary(&self) -> String {
        let size = if self.lines > 1 {
            format!("{} lines", self.lines)
        } else {
            format!("{} chars", self.chars)
        };
        match &self.fallback {
            Some(why) => format!("{size} · {} ({why})", self.via),
            None => format!("{size} · {}", self.via),
        }
    }
}

/// The session clipboard: mode, detected tool, tmux wrapping and redactor.
#[derive(Debug, Clone)]
pub struct Clipboard {
    mode: ClipboardMode,
    tool: Option<SystemTool>,
    tmux: bool,
    remote: bool,
    redactor: Option<Arc<Redactor>>,
}

impl Default for Clipboard {
    fn default() -> Self {
        Clipboard::osc52_only()
    }
}

impl Clipboard {
    /// The clipboard for `mode`, detected from this process's environment.
    pub fn new(mode: ClipboardMode) -> Clipboard {
        let env = |k: &str| std::env::var(k).ok();
        Clipboard::from_env(mode, &env, &on_path)
    }

    /// The clipboard for `mode` with an explicit environment (tests).
    pub fn from_env(
        mode: ClipboardMode,
        env: &dyn Fn(&str) -> Option<String>,
        exists: &dyn Fn(&str) -> bool,
    ) -> Clipboard {
        let set = |k: &str| env(k).is_some_and(|v| !v.is_empty());
        let tool = match mode {
            ClipboardMode::Osc52 => None,
            ClipboardMode::System | ClipboardMode::Auto => SystemTool::detect(env, exists),
        };
        Clipboard {
            mode,
            tool,
            tmux: set("TMUX"),
            remote: set("SSH_CONNECTION") || set("SSH_TTY"),
            redactor: None,
        }
    }

    /// Plain OSC 52, no tmux wrapping.
    pub fn osc52_only() -> Clipboard {
        Clipboard {
            mode: ClipboardMode::Osc52,
            tool: None,
            tmux: false,
            remote: false,
            redactor: None,
        }
    }

    /// Uses `tool` for `system` and `auto`.
    pub fn with_tool(mut self, tool: Option<SystemTool>) -> Clipboard {
        self.tool = tool;
        self
    }

    /// Masks secrets with `redactor` before copying.
    pub fn with_redactor(mut self, redactor: Arc<Redactor>) -> Clipboard {
        self.redactor = Some(redactor);
        self
    }

    /// The configured mode.
    pub fn mode(&self) -> ClipboardMode {
        self.mode
    }

    /// The detected system tool.
    pub fn tool(&self) -> Option<&SystemTool> {
        self.tool.as_ref()
    }

    /// The text that will be copied: [`plain`], then redacted.
    pub fn prepare(&self, text: &str) -> String {
        let p = plain(text);
        match &self.redactor {
            Some(r) => r.redact(&p).into_owned(),
            None => p,
        }
    }

    /// The OSC 52 sequence for `text` as it will be written (tmux-wrapped
    /// inside tmux). `text` is used as is; see [`Clipboard::prepare`].
    pub fn sequence(&self, text: &str) -> String {
        let seq = osc52(text);
        if self.tmux {
            tmux_passthrough(&seq)
        } else {
            seq
        }
    }

    /// Copies `text`. `write_osc` writes an escape sequence to the
    /// terminal.
    pub fn copy(
        &self,
        text: &str,
        write_osc: &mut dyn FnMut(&str) -> io::Result<()>,
    ) -> Result<Copied, String> {
        let text = self.prepare(text);
        let lines = text.lines().count().max(1);
        let chars = text.chars().count();
        let use_tool = match self.mode {
            ClipboardMode::Osc52 => false,
            ClipboardMode::System => true,
            ClipboardMode::Auto => !self.remote && self.tool.is_some(),
        };
        let mut fallback = None;
        if use_tool {
            match &self.tool {
                Some(tool) => match tool.copy(&text) {
                    Ok(()) => {
                        return Ok(Copied {
                            via: tool.name().to_owned(),
                            lines,
                            chars,
                            fallback: None,
                        });
                    }
                    Err(e) => fallback = Some(e),
                },
                None => fallback = Some("no wl-copy, xclip, xsel or pbcopy found".to_owned()),
            }
        }
        write_osc(&self.sequence(&text)).map_err(|e| format!("terminal write failed: {e}"))?;
        Ok(Copied {
            via: if self.tmux { "OSC 52 (tmux)" } else { "OSC 52" }.to_owned(),
            lines,
            chars,
            fallback,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn env(vars: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let m: HashMap<String, String> = vars
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect();
        move |k| m.get(k).cloned()
    }

    #[test]
    fn osc52_is_the_exact_sequence() {
        assert_eq!(osc52("hi"), "\u{1b}]52;c;aGk=\u{7}");
        assert_eq!(osc52(""), "\u{1b}]52;c;\u{7}");
        assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
        assert_eq!(base64_encode(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64_encode("ü".as_bytes()), "w7w=");
    }

    #[test]
    fn tmux_wraps_for_passthrough() {
        assert_eq!(
            tmux_passthrough(&osc52("hi")),
            "\u{1b}Ptmux;\u{1b}\u{1b}]52;c;aGk=\u{7}\u{1b}\\"
        );
        let cb = Clipboard::from_env(
            ClipboardMode::Osc52,
            &env(&[("TMUX", "/tmp/t,1,0")]),
            &|_| false,
        );
        let mut out = Vec::new();
        let c = cb
            .copy("hi", &mut |s| {
                out.push(s.to_owned());
                Ok(())
            })
            .unwrap();
        assert_eq!(out, vec![tmux_passthrough(&osc52("hi"))]);
        assert_eq!(c.via, "OSC 52 (tmux)");
    }

    #[test]
    fn plain_strips_escapes_and_controls() {
        assert_eq!(
            plain("\u{1b}[1;31mred\u{1b}[0m \u{1b}]0;title\u{7}ok\r\n\ttab\u{8}"),
            "red ok\n\ttab"
        );
        assert_eq!(plain("a\u{1b}]52;c;x\u{1b}\\b"), "ab");
        assert_eq!(plain("日本 ünï"), "日本 ünï");
    }

    #[test]
    fn plain_line_drops_borders_gutters_and_buttons() {
        let cases = [
            (
                "╭ ✔ edit  src/a.rs  +1 −1 ────────── 3ms · 14:32 ╮",
                Some("✔ edit  src/a.rs  +1 −1  3ms · 14:32"),
            ),
            (
                "│    6 │-    let x = 1;                         │",
                Some("-    let x = 1;"),
            ),
            ("│    3 │ fn main() {}   │", Some("fn main() {}")),
            ("╰──────────────────────╯", None),
            ("╰─ ⧉ copy output ─ ⧉ copy diff ─╯", None),
            ("│ … 4 more lines · Enter to expand │", None),
            ("▌ hello world", Some("hello world")),
            ("│ let x = 1;", Some("let x = 1;")),
            ("  rust  ⧉ copy", Some("  rust")),
            (
                "◆ assistant          ⧉ copy · 14:32:00",
                Some("◆ assistant"),
            ),
            ("plain text   ", Some("plain text")),
            ("", None),
        ];
        for (input, want) in cases {
            assert_eq!(plain_line(input).as_deref(), want, "{input:?}");
        }
    }

    #[test]
    fn copies_are_plain_and_redacted() {
        let redactor = Arc::new(Redactor::new(["sk-supersecret-123".to_owned()]));
        let cb = Clipboard::osc52_only().with_redactor(redactor);
        let mut out = String::new();
        cb.copy("key=sk-supersecret-123 \u{1b}[31mx\u{1b}[0m", &mut |s| {
            out = s.to_owned();
            Ok(())
        })
        .unwrap();
        let prepared = cb.prepare("key=sk-supersecret-123 \u{1b}[31mx\u{1b}[0m");
        assert!(!prepared.contains("supersecret"), "{prepared}");
        assert!(prepared.ends_with(" x"), "{prepared}");
        assert_eq!(out, osc52(&prepared));
    }

    #[test]
    fn detects_tools_from_the_environment() {
        let all = |_: &str| true;
        let wl = SystemTool::detect(&env(&[("WAYLAND_DISPLAY", "wayland-0")]), &all).unwrap();
        assert_eq!(wl.name(), "wl-copy");
        let x = SystemTool::detect(&env(&[("DISPLAY", ":0")]), &all).unwrap();
        assert_eq!(x, SystemTool::new("xclip", &["-selection", "clipboard"]));
        let xsel = SystemTool::detect(&env(&[("DISPLAY", ":0")]), &|p| p == "xsel").unwrap();
        assert_eq!(xsel.name(), "xsel");
        let mac = SystemTool::detect(&env(&[]), &|p| p == "pbcopy").unwrap();
        assert_eq!(mac.name(), "pbcopy");
        assert_eq!(
            SystemTool::detect(&env(&[("DISPLAY", ":0")]), &|_| false),
            None
        );
        // osc52 never looks for a tool.
        let cb = Clipboard::from_env(ClipboardMode::Osc52, &env(&[("DISPLAY", ":0")]), &all);
        assert!(cb.tool().is_none());
    }

    fn sink_tool(dir: &Path) -> (SystemTool, std::path::PathBuf) {
        let out = dir.join("clip.txt");
        let tool = SystemTool {
            program: "sh".into(),
            args: vec!["-c".into(), format!("cat > '{}'", out.display())],
        };
        (tool, out)
    }

    #[test]
    fn system_mode_uses_the_tool_and_falls_back_to_osc52() {
        let tmp = tempfile::tempdir().unwrap();
        let (tool, out) = sink_tool(tmp.path());
        let cb =
            Clipboard::from_env(ClipboardMode::System, &env(&[]), &|_| false).with_tool(Some(tool));
        let mut osc = Vec::new();
        let c = cb
            .copy("line 1\nline 2 ü", &mut |s| {
                osc.push(s.to_owned());
                Ok(())
            })
            .unwrap();
        assert_eq!(std::fs::read_to_string(&out).unwrap(), "line 1\nline 2 ü");
        assert!(osc.is_empty(), "the tool was used: {osc:?}");
        assert_eq!(c.via, "sh");
        assert_eq!(c.summary(), "2 lines · sh");

        // No tool: OSC 52 with the reason.
        let cb = Clipboard::from_env(ClipboardMode::System, &env(&[]), &|_| false);
        let c = cb
            .copy("x", &mut |s| {
                osc.push(s.to_owned());
                Ok(())
            })
            .unwrap();
        assert_eq!(osc, vec![osc52("x")]);
        assert!(
            c.fallback.is_some() && c.summary().contains("OSC 52 (no wl-copy"),
            "{c:?}"
        );

        // A failing tool falls back too.
        let cb = cb.with_tool(Some(SystemTool::new("false", &[])));
        let c = cb.copy("y", &mut |_| Ok(())).unwrap();
        assert_eq!(c.via, "OSC 52");
        assert!(c.fallback.unwrap().contains("false exited"));
    }

    #[test]
    fn auto_uses_the_tool_locally_and_osc52_over_ssh() {
        let tmp = tempfile::tempdir().unwrap();
        let (tool, out) = sink_tool(tmp.path());
        let local = Clipboard::from_env(ClipboardMode::Auto, &env(&[]), &|_| false)
            .with_tool(Some(tool.clone()));
        let c = local.copy("local", &mut |_| Ok(())).unwrap();
        assert_eq!(c.via, "sh");
        assert_eq!(std::fs::read_to_string(&out).unwrap(), "local");
        let ssh = Clipboard::from_env(
            ClipboardMode::Auto,
            &env(&[("SSH_CONNECTION", "1.2.3.4 5 6.7.8.9 22")]),
            &|_| false,
        )
        .with_tool(Some(tool));
        let mut osc = String::new();
        let c = ssh
            .copy("remote", &mut |s| {
                osc = s.to_owned();
                Ok(())
            })
            .unwrap();
        assert_eq!(c.via, "OSC 52");
        assert_eq!(osc, osc52("remote"));
        // No tool at all: OSC 52 without a complaint.
        let none = Clipboard::from_env(ClipboardMode::Auto, &env(&[]), &|_| false);
        let c = none.copy("z", &mut |_| Ok(())).unwrap();
        assert_eq!((c.via.as_str(), c.fallback), ("OSC 52", None));
    }
}
