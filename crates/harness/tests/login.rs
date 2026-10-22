//! `harness login` / `logout` end to end: the real binary against a local
//! mock provider. On a pseudo-terminal the typed key must never be echoed;
//! piped stdin works for scripts; the saved file is mode 0600.

mod common;

use std::fs::File;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::os::unix::fs::PermissionsExt;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use common::*;
use nix::pty::{Winsize, openpty};

const KEY: &str = "sk-or-v1-pty-secret-0123456789";

/// A mock OpenRouter answering `GET /key`: 200 for [`KEY`], 401 otherwise.
/// Returns the base URL and the request heads it saw.
fn mock_openrouter() -> (String, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = seen.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { break };
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut head = String::new();
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                    break;
                }
                head.push_str(&line);
            }
            let ok = head.to_ascii_lowercase().contains(&format!(
                "authorization: bearer {}",
                KEY.to_ascii_lowercase()
            ));
            log.lock().unwrap().push(head);
            let (status, body) = if ok {
                ("200 OK", r#"{"data":{"label":"sk-or-v1-pty...789"}}"#)
            } else {
                (
                    "401 Unauthorized",
                    r#"{"error":{"message":"No auth credentials found"}}"#,
                )
            };
            let _ = write!(
                stream,
                "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
        }
    });
    (base, seen)
}

fn env_with_mock(base: &str) -> Env {
    Env::raw(&format!(
        "; user comment\n[provider.openrouter]\nbase_url = {base}\n"
    ))
}

fn ini(env: &Env) -> String {
    std::fs::read_to_string(env.root.join("harness.ini")).unwrap()
}

fn mode(env: &Env) -> u32 {
    std::fs::metadata(env.root.join("harness.ini"))
        .unwrap()
        .permissions()
        .mode()
        & 0o777
}

#[test]
fn piped_key_is_checked_and_saved_with_mode_0600() {
    let (base, seen) = mock_openrouter();
    let env = env_with_mock(&base);
    std::fs::set_permissions(
        env.root.join("harness.ini"),
        std::fs::Permissions::from_mode(0o644),
    )
    .unwrap();
    let mut child = env
        .cmd(&["login", "openrouter"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    writeln!(child.stdin.take().unwrap(), "{KEY}").unwrap();
    let out = child.wait_with_output().unwrap();
    let (so, se) = (stdout(&out), stderr(&out));
    assert!(out.status.success(), "{so}{se}");
    assert!(so.contains("OpenRouter accepted the key"), "{so}");
    assert!(
        !so.contains(KEY) && !se.contains(KEY),
        "key leaked:\n{so}{se}"
    );
    assert!(seen.lock().unwrap()[0].starts_with("GET /key "));
    let text = ini(&env);
    assert!(text.starts_with("[general]\n"), "{text}");
    assert!(text.contains("; user comment\n[provider.openrouter]\nbase_url = "));
    assert!(
        text.contains(&format!("base_url = {base}\napi_key = {KEY}\n")),
        "{text}"
    );
    assert_eq!(mode(&env), 0o600);
    // The saved file loads (inline key + 0600 passes the permission check).
    let check = env.cmd(&["config", "check"]).output().unwrap();
    assert!(check.status.success(), "{}", stderr(&check));
}

#[test]
fn wrong_key_is_rejected_and_nothing_is_saved() {
    let (base, _) = mock_openrouter();
    let env = env_with_mock(&base);
    let before = ini(&env);
    let mut child = env
        .cmd(&["login", "openrouter"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    writeln!(child.stdin.take().unwrap(), "sk-or-v1-wrong-key").unwrap();
    let out = child.wait_with_output().unwrap();
    let se = stderr(&out);
    assert_eq!(out.status.code(), Some(1), "{se}");
    assert!(
        se.contains("OpenRouter rejected the key (No auth credentials found)"),
        "{se}"
    );
    assert!(se.contains("Nothing was saved"), "{se}");
    assert!(!se.contains("sk-or-v1-wrong-key"));
    assert_eq!(ini(&env), before);
}

#[test]
fn logout_needs_yes_without_a_terminal_then_removes_the_key() {
    let env = Env::raw("[provider.openai]\napi_key = sk-logout-me\nbase_url = http://x\n");
    let out = env
        .cmd(&["logout", "openai"])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert!(stderr(&out).contains("--yes"));
    assert!(ini(&env).contains("sk-logout-me"));
    let out = env.cmd(&["logout", "openai", "--yes"]).output().unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(stdout(&out).contains("removed api_key from [provider.openai]"));
    let text = ini(&env);
    assert!(!text.contains("sk-logout-me"));
    assert!(text.contains("base_url = http://x"));
}

#[test]
fn typed_key_is_masked_on_a_terminal() {
    let (base, _) = mock_openrouter();
    let env = env_with_mock(&base);
    let size = Winsize {
        ws_row: 24,
        ws_col: 100,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    let pty = openpty(Some(&size), None).unwrap();
    let mut cmd = env.cmd(&["login", "openrouter"]);
    cmd.stdin(Stdio::from(pty.slave.try_clone().unwrap()))
        .stdout(Stdio::from(pty.slave.try_clone().unwrap()))
        .stderr(Stdio::from(pty.slave))
        .env("TERM", "xterm-256color");
    let mut child = cmd.spawn().unwrap();
    drop(cmd);
    let mut master = File::from(pty.master);
    let out = Arc::new(Mutex::new(Vec::new()));
    let mut reader = master.try_clone().unwrap();
    let sink = out.clone();
    std::thread::spawn(move || {
        let mut buf = [0u8; 4096];
        while let Ok(n) = reader.read(&mut buf) {
            if n == 0 {
                break;
            }
            sink.lock().unwrap().extend_from_slice(&buf[..n]);
        }
    });
    let text = || String::from_utf8_lossy(&out.lock().unwrap()).into_owned();
    let wait = |needle: &str| {
        let end = Instant::now() + Duration::from_secs(30);
        while Instant::now() < end {
            if text().contains(needle) {
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        panic!("{needle:?} never appeared:\n{}", text());
    };
    wait("Choose 1-3 [1]: ");
    master.write_all(b"1\r").unwrap();
    wait("(input is hidden): ");
    // Give the child time to enter raw mode before typing.
    std::thread::sleep(Duration::from_millis(300));
    master.write_all(KEY.as_bytes()).unwrap();
    master.write_all(b"x\x7f").unwrap(); // a typo, then backspace
    master.write_all(b"\r").unwrap();
    wait("accepted the key");
    let status = child.wait().unwrap();
    let screen = text();
    assert!(status.success(), "{screen}");
    assert!(!screen.contains(KEY), "key echoed:\n{screen}");
    assert!(!screen.contains("pty-secret"), "key echoed:\n{screen}");
    assert!(
        screen.contains(&"•".repeat(KEY.chars().count())),
        "{screen}"
    );
    assert!(ini(&env).contains(&format!("api_key = {KEY}\n")));
    assert_eq!(mode(&env), 0o600);
}
