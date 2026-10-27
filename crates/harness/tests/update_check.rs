//! The startup update check end to end: the real `harness` TUI on a
//! pseudo-terminal asks a local stand-in for the GitHub releases API in the
//! background, draws its first frame before the answer, and opens the
//! update dialog (versions, release-notes link, install command, copy
//! through OSC 52 by keyboard and mouse, closed only by Esc). The dismissed
//! version and the check time are cached in the data dir. With
//! `update_check = off`, and for the headless commands, nothing is asked.

mod common;

use std::fs::File;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::process::{Child, Stdio};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use common::*;
use nix::pty::{Winsize, openpty};
use serde_json::Value;

const SCRIPT: &str = "responses:\n  - content: [ { text: \"Done here.\" } ]\n";

const INSTALL: &str =
    "curl -fsSL https://raw.githubusercontent.com/thowd22/harness/main/install.sh | bash";

const NOTES: &str = "https://github.com/thowd22/harness/releases/tag/v99.0.0";

/// A local stand-in for the GitHub releases API. It records each request
/// head and answers with `body` once open.
struct MockGitHub {
    url: String,
    heads: Arc<Mutex<Vec<String>>>,
    gate: Arc<(Mutex<bool>, Condvar)>,
}

impl MockGitHub {
    fn new(body: &str, open: bool) -> MockGitHub {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let heads = Arc::new(Mutex::new(Vec::new()));
        let gate = Arc::new((Mutex::new(open), Condvar::new()));
        let (h, g, body) = (heads.clone(), gate.clone(), body.to_owned());
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut s) = stream else { continue };
                let (h, g, body) = (h.clone(), g.clone(), body.clone());
                std::thread::spawn(move || {
                    let mut buf = Vec::new();
                    let mut chunk = [0u8; 1024];
                    while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
                        match s.read(&mut chunk) {
                            Ok(0) | Err(_) => return,
                            Ok(n) => buf.extend_from_slice(&chunk[..n]),
                        }
                    }
                    h.lock()
                        .unwrap()
                        .push(String::from_utf8_lossy(&buf).into_owned());
                    let (lock, cv) = &*g;
                    let mut open = lock.lock().unwrap();
                    while !*open {
                        open = cv.wait(open).unwrap();
                    }
                    drop(open);
                    let _ = write!(
                        s,
                        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                        body.len()
                    );
                });
            }
        });
        MockGitHub { url, heads, gate }
    }

    /// Lets the held answers go.
    fn release(&self) {
        let (lock, cv) = &*self.gate;
        *lock.lock().unwrap() = true;
        cv.notify_all();
    }

    fn requests(&self) -> usize {
        self.heads.lock().unwrap().len()
    }
}

/// A releases listing whose newest stable release is v99.0.0.
fn releases() -> String {
    serde_json::json!([
        {"tag_name": "v99.1.0-rc.1", "prerelease": true, "draft": false,
         "html_url": "https://github.com/thowd22/harness/releases/tag/v99.1.0-rc.1"},
        {"tag_name": "v99.0.0", "prerelease": false, "draft": false, "html_url": NOTES},
        {"tag_name": "v0.1.0", "prerelease": false, "draft": false,
         "html_url": "https://github.com/thowd22/harness/releases/tag/v0.1.0"},
    ])
    .to_string()
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

fn wait_until(what: &str, mut cond: impl FnMut() -> bool) {
    let end = Instant::now() + Duration::from_secs(30);
    while Instant::now() < end {
        if cond() {
            return;
        }
        std::thread::sleep(Duration::from_millis(30));
    }
    panic!("{what} never happened");
}

struct Pty {
    child: Child,
    master: File,
    out: Arc<Mutex<Vec<u8>>>,
}

impl Pty {
    /// Starts the TUI with its update check pointed at `api`.
    fn spawn(env: &Env, args: &[&str], api: &str) -> Pty {
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
            .env("HARNESS_UPDATE_API", api)
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

impl Pty {
    fn count(&self, needle: &[u8]) -> usize {
        self.raw()
            .windows(needle.len())
            .filter(|w| *w == needle)
            .count()
    }
}

#[test]
fn the_tui_offers_a_newer_release_in_a_dialog_closed_only_by_esc() {
    let gh = MockGitHub::new(&releases(), false);
    let env = Env::raw(
        "[ui]\nmouse = true\nterminal_title = false\nclipboard = osc52\n\n[memory]\nenabled = false\n",
    );
    let script = env.script("script.yaml", SCRIPT);
    let s = script.to_str().unwrap();
    let mut pty = Pty::spawn(&env, &["--fake-script", s], &gh.url);

    // The first frame does not wait for GitHub: it is drawn while the
    // request is still unanswered.
    pty.wait_screen("Type a message");
    wait_until("the update request", || gh.requests() == 1);
    assert!(!pty.screen().contains("Update available"));
    gh.release();

    pty.wait_screen("Update available");
    let current = env!("CARGO_PKG_VERSION");
    pty.wait_screen(&format!("Current version  {current}"));
    // The newest stable release; the newer pre-release is ignored.
    pty.wait_screen("New version      99.0.0");
    assert!(!pty.screen().contains("99.1.0"), "{}", pty.screen());
    pty.wait_screen("https://github.com/thowd22/harness/releases/tag/v99.");
    pty.wait_screen("curl -fsSL https://raw.githubusercontent.com/thowd22/harnes");
    assert!(
        pty.contains(format!("\x1b]8;;{NOTES}\x1b\\").as_bytes()),
        "the release notes are an OSC 8 link"
    );

    // Keyboard: the copy button has the focus, Enter copies the command
    // with OSC 52; Tab moves to the link button.
    pty.send(b"\r");
    pty.wait_for(&osc52(INSTALL));
    pty.wait_screen("copied install command");
    pty.send(b"\t");
    pty.send(b"\r");
    pty.wait_for(&osc52(NOTES));
    // Mouse: a click on the button copies too.
    let (col, row) = pty.locate("[ copy install command ]");
    pty.click(col + 2, row);
    wait_until("a second copy", || pty.count(&osc52(INSTALL)) == 2);

    // Other keys and clicks outside do not close it.
    pty.send(b"q");
    pty.send(b" ");
    pty.click(1, 1);
    pty.click(120, 40);
    std::thread::sleep(Duration::from_millis(300));
    assert!(
        pty.screen().contains("Update available"),
        "{}",
        pty.screen()
    );

    // Esc closes it, and the version is remembered as dismissed.
    pty.send(b"\x1b");
    pty.wait_gone("Update available");
    pty.quit();
    let cache: Value = serde_json::from_str(
        &std::fs::read_to_string(env.root.join("data/update-check.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(cache["dismissed"], "99.0.0", "{cache}");
    assert!(cache["checked_at"].is_u64(), "{cache}");
    assert_eq!(cache["stable"]["version"], "99.0.0", "{cache}");

    // The next start is within the interval: no request, and the dismissed
    // version is not shown again.
    let pty = Pty::spawn(&env, &["--fake-script", s], &gh.url);
    pty.wait_screen("Type a message");
    std::thread::sleep(Duration::from_millis(1500));
    assert_eq!(gh.requests(), 1, "at most one check per interval");
    assert!(
        !pty.screen().contains("Update available"),
        "{}",
        pty.screen()
    );
    pty.quit();
}

#[test]
fn update_check_off_and_headless_commands_never_ask() {
    let gh = MockGitHub::new(&releases(), true);
    // `[general] update_check = off`: the TUI does not ask.
    let env = Env::raw(
        "update_check = off\n\n[ui]\nterminal_title = false\n\n[memory]\nenabled = false\n",
    );
    let script = env.script("script.yaml", SCRIPT);
    let s = script.to_str().unwrap();
    let pty = Pty::spawn(&env, &["--fake-script", s], &gh.url);
    pty.wait_screen("Type a message");
    std::thread::sleep(Duration::from_millis(1500));
    pty.quit();
    assert_eq!(gh.requests(), 0, "update_check = off");
    assert!(!env.root.join("data/update-check.json").exists());

    // Headless commands never ask, whatever the setting.
    let env = Env::new("");
    let script = env.script("script.yaml", SCRIPT);
    let s = script.to_str().unwrap();
    let wf = workspace_root()
        .join("crates/harness-workflow/evals/workflows/scripts/plan-approve-implement.yaml");
    let suite = workspace_root().join("crates/harness-workflow/evals/workflows/suite.yaml");
    let results = env.root.join("results.json");
    let commands: Vec<Vec<&str>> = vec![
        vec!["run", "--fake-script", s, "hello"],
        vec![
            "wf",
            "run",
            "plan",
            "--mode",
            "auto",
            "--approve-all",
            "--fake-script",
            wf.to_str().unwrap(),
            "--input",
            "test=true",
            "a task",
        ],
        vec!["bench", "--fake", "--runs", "1", "--json", "-"],
        vec![
            "eval",
            "run",
            suite.to_str().unwrap(),
            "--filter",
            "plan-approve",
            "--out",
            results.to_str().unwrap(),
        ],
    ];
    for args in commands {
        let o = env
            .cmd(&args)
            .env("HARNESS_UPDATE_API", &gh.url)
            .output()
            .unwrap();
        assert!(o.status.code().is_some(), "{args:?}: {}", stderr(&o));
        assert_eq!(gh.requests(), 0, "{args:?} asked GitHub");
    }
    assert!(!env.root.join("data/update-check.json").exists());
}
