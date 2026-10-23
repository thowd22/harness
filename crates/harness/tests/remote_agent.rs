//! `harness remote-agent --stdio` speaks the helper protocol on stdio.

use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};

mod common;

#[test]
fn remote_agent_answers_hello_over_stdio() {
    let sock = tempfile::tempdir().unwrap();
    let root = common::hermetic_root();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_harness"));
    common::hermetic(&mut cmd, root.path());
    let mut child = cmd
        .args([
            "remote-agent",
            "--stdio",
            "--session",
            "bintest",
            "--grace",
            "1",
        ])
        .arg("--socket-dir")
        .arg(sock.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    writeln!(
        stdin,
        r#"{{"jsonrpc":"2.0","id":1,"method":"hello","params":{{"protocol":1,"version":"{}"}}}}"#,
        env!("CARGO_PKG_VERSION")
    )
    .unwrap();
    let mut line = String::new();
    BufReader::new(child.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    drop(stdin);
    let v: serde_json::Value = serde_json::from_str(&line).unwrap();
    assert_eq!(v["id"], 1);
    assert_eq!(v["result"]["protocol"], 1);
    assert_eq!(v["result"]["session"], "bintest");
    assert!(child.wait().unwrap().success(), "exits once stdin closes");
}

#[test]
fn remote_agent_requires_stdio() {
    let root = common::hermetic_root();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_harness"));
    common::hermetic(&mut cmd, root.path());
    let out = cmd.arg("remote-agent").output().unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("--stdio"));
}
