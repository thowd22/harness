//! The `/resume` picker through a real pseudo-terminal: sessions saved by
//! `harness run` are listed by name with their last activity and
//! description, filtered by typing, chosen with the keyboard and with a
//! mouse click, and the chosen conversation comes back.

mod common;

use std::fs::File;
use std::io::{Read, Write};
use std::process::{Child, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use common::*;
use nix::pty::{Winsize, openpty};

const SCRIPT: &str = "responses:\n  - content: [ { text: \"Done here.\" } ]\n  - content: [ { text: \"Again.\" } ]\n";

struct Pty {
    child: Child,
    master: File,
    out: Arc<Mutex<Vec<u8>>>,
}

#[allow(dead_code)]
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

impl Pty {
    /// The (1-based column, row) of the first cell of `text` on screen.
    fn locate(&self, text: &str) -> (usize, usize) {
        let screen = self.screen();
        for (r, line) in screen.lines().enumerate() {
            if let Some(i) = line.find(text) {
                return (line[..i].chars().count() + 1, r + 1);
            }
        }
        panic!("{text:?} not on screen:\n{screen}");
    }

    /// `/resume` with no argument: the first Enter completes the command
    /// (it takes an argument), the second sends it.
    fn open_picker(&mut self) {
        self.send(b"/resume");
        self.send(b"\r");
        self.send(b"\r");
        self.wait_screen("Resume session");
    }

    /// Waits until `text` is no longer on the screen.
    fn wait_gone(&self, text: &str) {
        let end = Instant::now() + Duration::from_secs(30);
        while Instant::now() < end {
            if !self.screen().contains(text) {
                return;
            }
            std::thread::sleep(Duration::from_millis(30));
        }
        panic!("{text:?} stayed on screen:\n{}", self.screen());
    }

    /// A left click (SGR mouse encoding) at a 1-based cell.
    fn click(&mut self, col: usize, row: usize) {
        self.send(format!("\x1b[<0;{col};{row}M\x1b[<0;{col};{row}m").as_bytes());
    }
}

#[test]
fn resume_picker_lists_filters_and_resumes_by_keyboard_and_mouse() {
    let env = Env::raw("[ui]\nmouse = true\nterminal_title = false\n\n[memory]\nenabled = false\n");
    let script = env.script("script.yaml", SCRIPT);
    let s = script.to_str().unwrap();
    for prompt in [
        ["please", "fix", "the", "header", "parser"].as_slice(),
        &["write", "the", "release", "notes"],
    ] {
        let mut args = vec!["run", "--fake-script", s];
        args.extend_from_slice(prompt);
        let o = env.cmd(&args).output().unwrap();
        assert_eq!(
            o.status.code(),
            Some(0),
            "{}",
            String::from_utf8_lossy(&o.stderr)
        );
    }

    let mut pty = Pty::spawn(&env, &["--fake-script", s]);
    pty.wait_screen("Type a message");
    pty.open_picker();
    pty.wait_screen("2 of 2 sessions");
    pty.wait_screen("Write release notes");
    pty.wait_screen("Fix header parser");
    pty.wait_screen("please fix the header parser");
    pty.wait_screen("today");
    pty.wait_screen("2 msgs");

    // Typing filters by name and description; Enter resumes.
    pty.send(b"header");
    pty.wait_screen("1 of 2 sessions");
    pty.send(b"\r");
    pty.wait_gone("Resume session");
    pty.wait_screen("resumed");
    pty.wait_screen("please fix the header parser");

    // Arrows move the selection; Esc closes without resuming.
    pty.open_picker();
    pty.wait_screen("2 of 2 sessions");
    pty.send(b"\x1b[B");
    pty.send(b"\x1b");
    pty.wait_gone("2 of 2 sessions");

    // A click on a session's description row resumes it.
    pty.open_picker();
    pty.wait_screen("2 of 2 sessions");
    let (col, row) = pty.locate("write the release notes");
    pty.click(col, row);
    pty.wait_screen("Write release notes");
    pty.wait_gone("2 of 2 sessions");
    pty.wait_screen("write the release notes");
    pty.quit();
}
