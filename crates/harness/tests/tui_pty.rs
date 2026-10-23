//! Copy and paste through a real pseudo-terminal: the `harness` TUI runs
//! on a pty with a scripted fake provider. Bracketed-paste sequences are
//! written to the terminal and the recorded `user_message` events must hold
//! the pasted text exactly; copy actions must emit the exact OSC 52
//! sequence. With `[ui] mouse = false` mouse capture is never enabled, so
//! the terminal's own selection keeps working.

mod common;

use std::fs::File;
use std::io::{Read, Write};
use std::process::{Child, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use common::*;
use nix::pty::{Winsize, openpty};
use serde_json::Value;

const SCRIPT: &str = r#"responses:
  - content:
      - text: "Here you go:\n\n```sh\ncargo test -p parse\n```\n"
  - content:
      - text: "Got it."
  - content:
      - text: "Again."
  - content:
      - text: "Once more."
"#;

/// `ESC [ ? 1000 h`: crossterm's first mouse-capture sequence.
const MOUSE_ON: &[u8] = b"\x1b[?1000h";
/// `ESC [ ? 2004 h`: bracketed paste on.
const PASTE_ON: &[u8] = b"\x1b[?2004h";

struct Pty {
    child: Child,
    master: File,
    out: Arc<Mutex<Vec<u8>>>,
}

impl Pty {
    fn spawn(env: &Env, args: &[&str]) -> Pty {
        let size = Winsize {
            ws_row: 40,
            ws_col: 120,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        let pty = openpty(Some(&size), None).expect("openpty");
        let mut cmd = env.cmd(args);
        cmd.stdin(Stdio::from(pty.slave.try_clone().unwrap()))
            .stdout(Stdio::from(pty.slave.try_clone().unwrap()))
            .stderr(Stdio::from(pty.slave))
            .env("TERM", "xterm-256color")
            .env_remove("TMUX")
            .env_remove("SSH_CONNECTION")
            .env_remove("SSH_TTY");
        let child = cmd.spawn().expect("spawn harness");
        drop(cmd);
        let master = File::from(pty.master);
        let out = Arc::new(Mutex::new(Vec::new()));
        let mut reader = master.try_clone().unwrap();
        let sink = out.clone();
        std::thread::spawn(move || {
            let mut buf = [0u8; 65536];
            while let Ok(n) = reader.read(&mut buf) {
                if n == 0 {
                    break;
                }
                sink.lock().unwrap().extend_from_slice(&buf[..n]);
            }
        });
        Pty { child, master, out }
    }

    fn raw(&self) -> Vec<u8> {
        self.out.lock().unwrap().clone()
    }

    fn contains(&self, needle: &[u8]) -> bool {
        self.raw().windows(needle.len()).any(|w| w == needle)
    }

    /// The screen as text, emulating the cursor moves ratatui writes.
    fn screen(&self) -> String {
        render_screen(&self.raw(), 40, 120)
    }

    /// Waits until `needle` is in the raw output.
    fn wait_for(&self, needle: &[u8]) {
        let end = Instant::now() + Duration::from_secs(30);
        while Instant::now() < end {
            if self.contains(needle) {
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        panic!(
            "{:?} never appeared; screen:\n{}",
            String::from_utf8_lossy(needle),
            self.screen()
        );
    }

    /// Waits until `text` is on the screen.
    fn wait_screen(&self, text: &str) {
        let end = Instant::now() + Duration::from_secs(30);
        while Instant::now() < end {
            if self.screen().contains(text) {
                return;
            }
            std::thread::sleep(Duration::from_millis(30));
        }
        panic!("{text:?} never appeared on screen:\n{}", self.screen());
    }

    fn send(&mut self, bytes: &[u8]) {
        self.master.write_all(bytes).unwrap();
        self.master.flush().unwrap();
        std::thread::sleep(Duration::from_millis(150));
    }

    fn paste(&mut self, text: &str) {
        let mut b = b"\x1b[200~".to_vec();
        b.extend_from_slice(text.as_bytes());
        b.extend_from_slice(b"\x1b[201~");
        self.send(&b);
    }

    fn quit(mut self) {
        self.send(b"\x11"); // ctrl+q
        let end = Instant::now() + Duration::from_secs(20);
        while Instant::now() < end {
            if let Ok(Some(_)) = self.child.try_wait() {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let _ = self.child.kill();
        panic!("harness did not quit");
    }
}

impl Drop for Pty {
    /// A test that fails half way must not leave its harness running (seen:
    /// TUI processes orphaned for hours after a failed pty test).
    fn drop(&mut self) {
        if let Ok(None) = self.child.try_wait() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

/// A minimal terminal: cursor positioning (`CSI row;col H`), text and
/// wide characters; other sequences are skipped. Enough for ratatui output.
fn render_screen(raw: &[u8], rows: usize, cols: usize) -> String {
    use unicode_width::UnicodeWidthChar;
    let text = String::from_utf8_lossy(raw);
    let mut grid = vec![vec![' '; cols]; rows];
    let (mut r, mut c) = (0usize, 0usize);
    let mut it = text.chars().peekable();
    while let Some(ch) = it.next() {
        match ch {
            '\u{1b}' => match it.next() {
                Some('[') => {
                    let mut params = String::new();
                    let mut fin = ' ';
                    for x in it.by_ref() {
                        if ('\u{40}'..='\u{7e}').contains(&x) {
                            fin = x;
                            break;
                        }
                        params.push(x);
                    }
                    if fin == 'H' {
                        let mut p = params.split(';').map(|n| n.parse::<usize>().unwrap_or(1));
                        r = p.next().unwrap_or(1).saturating_sub(1);
                        c = p.next().unwrap_or(1).saturating_sub(1);
                    } else if fin == 'J' && params == "2" {
                        grid = vec![vec![' '; cols]; rows];
                    }
                }
                Some(']') | Some('P') => {
                    while let Some(x) = it.next() {
                        if x == '\u{7}' {
                            break;
                        }
                        if x == '\u{1b}' && it.peek() == Some(&'\\') {
                            it.next();
                            break;
                        }
                    }
                }
                _ => {}
            },
            '\r' => c = 0,
            '\n' => r += 1,
            ch if ch.is_control() => {}
            ch => {
                let w = ch.width().unwrap_or(0);
                if r < rows && c < cols && w > 0 {
                    grid[r][c] = ch;
                    if w == 2 && c + 1 < cols {
                        grid[r][c + 1] = '\u{0}';
                    }
                }
                c += w;
            }
        }
    }
    grid.into_iter()
        .map(|row| {
            row.into_iter()
                .filter(|ch| *ch != '\u{0}')
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The OSC 52 sequence for `text` (standard base64).
fn osc52(text: &str) -> Vec<u8> {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut b64 = String::new();
    for c in text.as_bytes().chunks(3) {
        let n = (u32::from(c[0]) << 16)
            | (u32::from(*c.get(1).unwrap_or(&0)) << 8)
            | u32::from(*c.get(2).unwrap_or(&0));
        for (i, shift) in [18, 12, 6, 0].into_iter().enumerate() {
            b64.push(if i <= c.len() {
                T[(n >> shift) as usize & 63] as char
            } else {
                '='
            });
        }
    }
    format!("\x1b]52;c;{b64}\x07").into_bytes()
}

/// Texts of the recorded `user_message` events, oldest first.
fn user_messages(env: &Env) -> Vec<String> {
    let mut out = Vec::new();
    for run in env.runs() {
        let Ok(text) = std::fs::read_to_string(run.join("events.jsonl")) else {
            continue;
        };
        for line in text.lines() {
            let Ok(v) = serde_json::from_str::<Value>(line) else {
                continue;
            };
            if v["type"] == "user_message" {
                out.push(v["text"].as_str().unwrap_or_default().to_owned());
            }
        }
    }
    out
}

fn wait_messages(env: &Env, n: usize) -> Vec<String> {
    let end = Instant::now() + Duration::from_secs(30);
    loop {
        let m = user_messages(env);
        if m.len() >= n || Instant::now() > end {
            return m;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn session(mouse: bool) -> (Env, Pty) {
    let env = Env::raw(&format!(
        "[ui]\nmouse = {mouse}\nterminal_title = false\nclipboard = osc52\n\n[memory]\nenabled = false\n"
    ));
    let script = env.script("script.yaml", SCRIPT);
    let pty = Pty::spawn(&env, &["--fake-script", script.to_str().unwrap()]);
    pty.wait_screen("Type a message");
    (env, pty)
}

#[test]
fn bracketed_pastes_arrive_exactly_and_copy_emits_osc52() {
    let (env, mut pty) = session(true);
    assert!(pty.contains(PASTE_ON), "bracketed paste is enabled");
    assert!(pty.contains(MOUSE_ON), "mouse capture is on by default");

    // Multi-line text with tabs, CRLF, unicode and keys that would act if
    // typed (y, n, ctrl+p): nothing is sent before Enter, nothing opens.
    pty.paste("line one\n\tline two ünïcødé 日本 🚀\r\nline three y n \x10");
    pty.wait_screen("line two ünïcødé 日本 🚀");
    assert!(user_messages(&env).is_empty(), "a paste never sends");
    assert!(
        !pty.screen().contains("Command palette"),
        "no keybinding fired"
    );
    pty.send(b"\r");
    let msgs = wait_messages(&env, 1);
    assert_eq!(
        msgs,
        vec!["line one\n\tline two ünïcødé 日本 🚀\nline three y n \x10".to_owned()]
    );
    pty.wait_screen("Here you go");

    // A very large paste collapses to a placeholder and is sent in full.
    let big: String = (0..300).map(|i| format!("{i}\tlog line ü\n")).collect();
    pty.paste(&big);
    pty.wait_screen("[paste #1 · 300 lines]");
    assert_eq!(user_messages(&env).len(), 1);
    pty.send(b"\r");
    let msgs = wait_messages(&env, 2);
    assert_eq!(msgs.get(1), Some(&big));
    pty.wait_screen("Got it.");

    // Copy: ctrl+y copies the last assistant message as plain text.
    pty.send(b"\x19");
    pty.wait_for(&osc52("Got it."));
    pty.wait_screen("copied message");
    // Palette: the last code block, without fences.
    pty.send(b"\x10");
    pty.send(b"copy last code block");
    pty.send(b"\r");
    pty.wait_for(&osc52("cargo test -p parse"));
    // Keyboard selection: ctrl+t, mark the last lines, y copies them plain.
    pty.send(b"\x14");
    pty.wait_screen("SELECT");
    pty.send(b" ");
    pty.send(b"\x1b[A");
    pty.send(b"\x1b[A");
    pty.send(b"y");
    pty.wait_screen("copied selection");
    pty.quit();
}

#[test]
fn mouse_off_keeps_native_selection_and_paste_and_copy_work() {
    let (env, mut pty) = session(false);
    assert!(pty.contains(PASTE_ON));
    assert!(
        !pty.contains(MOUSE_ON),
        "`[ui] mouse = false` never enables mouse capture"
    );
    pty.paste("a\tb\nc");
    pty.send(b"\r");
    assert_eq!(wait_messages(&env, 1), vec!["a\tb\nc".to_owned()]);
    pty.wait_screen("Here you go");
    pty.send(b"\x19");
    pty.wait_for(&osc52("Here you go:\n\n```sh\ncargo test -p parse\n```\n"));
    assert!(!pty.contains(MOUSE_ON));
    pty.quit();
}
