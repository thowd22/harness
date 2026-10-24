//! `[experimental] cache_prewarm` end to end (TASK-161.4): the real binary
//! (`harness bench`, which starts a session as the TUI does) talks to a
//! local mock Messages API. With the setting on, the first request is the
//! non-streaming `max_tokens: 0` pre-warm with the real call's tools,
//! system and thinking, and the first real call follows it; the answers
//! are the same with the setting off, where no pre-warm is sent.

mod common;

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process::Command;
use std::sync::{Arc, Mutex};

use common::*;
use serde_json::{Value, json};

fn sse(text: &str) -> String {
    [
        json!({"type": "message_start", "message": {"id": "msg_1", "model": "claude-opus-5-5",
            "usage": {"input_tokens": 10, "output_tokens": 1, "cache_read_input_tokens": 3000,
                      "cache_creation_input_tokens": 0}}}),
        json!({"type": "content_block_start", "index": 0, "content_block": {"type": "text", "text": ""}}),
        json!({"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": text}}),
        json!({"type": "content_block_stop", "index": 0}),
        json!({"type": "message_delta", "delta": {"stop_reason": "end_turn"}, "usage": {"output_tokens": 3}}),
        json!({"type": "message_stop"}),
    ]
    .iter()
    .map(|e| format!("event: {}\ndata: {e}\n\n", e["type"].as_str().unwrap()))
    .collect()
}

/// Serves every request on its own connection; records the bodies.
fn mock() -> (String, Arc<Mutex<Vec<Value>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let seen = Arc::new(Mutex::new(Vec::new()));
    let rec = seen.clone();
    std::thread::spawn(move || {
        for sock in listener.incoming() {
            let Ok(sock) = sock else { return };
            let rec = rec.clone();
            std::thread::spawn(move || serve(sock, &rec));
        }
    });
    (url, seen)
}

fn serve(sock: TcpStream, rec: &Mutex<Vec<Value>>) {
    let mut reader = BufReader::new(sock.try_clone().unwrap());
    let mut len = 0usize;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).unwrap_or(0) == 0 {
            return;
        }
        let l = line.to_ascii_lowercase();
        if let Some(v) = l.strip_prefix("content-length:") {
            len = v.trim().parse().unwrap_or(0);
        }
        if line == "\r\n" {
            break;
        }
    }
    let mut body = vec![0u8; len];
    if reader.read_exact(&mut body).is_err() {
        return;
    }
    let body: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let streaming = body["stream"] == true;
    rec.lock().unwrap().push(body);
    let mut sock = sock;
    let out = if streaming {
        format!(
            "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\n{}",
            sse("All done.")
        )
    } else {
        let j = json!({"id": "msg_w", "type": "message", "role": "assistant", "content": [],
            "stop_reason": "max_tokens", "usage": {"input_tokens": 2, "output_tokens": 0,
            "cache_creation_input_tokens": 3000, "cache_read_input_tokens": 0}})
        .to_string();
        format!(
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{j}",
            j.len()
        )
    };
    let _ = sock.write_all(out.as_bytes());
}

/// Every event of the run logs under `dir` (the kept bench data dirs).
fn run_events(dir: &std::path::Path) -> Vec<Value> {
    let mut out = Vec::new();
    for e in std::fs::read_dir(dir).unwrap().flatten() {
        let p = e.path();
        if p.is_dir() {
            out.extend(run_events(&p));
        } else if p.file_name().is_some_and(|n| n == "events.jsonl") {
            let text = std::fs::read_to_string(&p).unwrap();
            out.extend(
                text.lines()
                    .map(|l| serde_json::from_str::<Value>(l).unwrap()),
            );
        }
    }
    out
}

fn run(prewarm: bool) -> (Vec<Value>, Value, Vec<Value>) {
    let (url, seen) = mock();
    let root = hermetic_root();
    let tmp = root.path().join("tmp");
    std::fs::create_dir_all(&tmp).unwrap();
    std::fs::write(
        root.path().join("harness.ini"),
        format!(
            "[general]\ndata_dir = {}\n\n[provider.anthropic]\napi_key_env = HARNESS_TEST_ANTHROPIC_KEY\nbase_url = {url}\n\n[experimental]\ncache_prewarm = {prewarm}\n",
            root.path().join("data").display()
        ),
    )
    .unwrap();
    let mut c = Command::new(env!("CARGO_BIN_EXE_harness"));
    hermetic(&mut c, root.path());
    let o = c
        .env("TMPDIR", &tmp)
        .env("HARNESS_TEST_ANTHROPIC_KEY", "sk-ant-test")
        .current_dir(root.path())
        .args([
            "bench",
            "--model",
            "anthropic/claude-opus-5-5",
            "--runs",
            "1",
            "--json",
            "-",
            "--keep",
        ])
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(0), "stderr: {}", stderr(&o));
    let report: Value = serde_json::from_str(&stdout(&o)).unwrap();
    let seen = seen.lock().unwrap().clone();
    (seen, report, run_events(&tmp))
}

#[test]
fn the_prewarm_goes_first_and_changes_nothing_else() {
    let (warm, warm_report, warm_events) = run(true);
    let (plain, plain_report, _) = run(false);
    assert!(
        plain.iter().all(|b| b["stream"] == true),
        "off: no pre-warm"
    );
    assert_eq!(warm.len(), plain.len() + 1, "exactly one extra call");
    let (w, real) = (&warm[0], &warm[1]);
    assert_eq!(w["stream"], false);
    assert_eq!(w["max_tokens"], 0);
    for k in ["model", "tools", "system", "thinking", "output_config"] {
        assert_eq!(w[k], real[k], "{k} must match the first real call");
    }
    assert_eq!(
        w["messages"].as_array().unwrap().len(),
        1,
        "a new session shares no messages: only the placeholder"
    );
    assert!(
        w["messages"][0]["content"][0]
            .get("cache_control")
            .is_none()
    );
    // The real requests are the same with and without the pre-warm (the
    // system prompt names the run's temporary repository, so it differs).
    let without_system = |bodies: &[Value]| -> Vec<Value> {
        bodies
            .iter()
            .map(|b| {
                let mut b = b.clone();
                b.as_object_mut().unwrap().remove("system");
                b
            })
            .collect()
    };
    assert_eq!(without_system(&warm[1..]), without_system(&plain));
    // And so are the runs: same calls, same answers.
    let calls = |r: &Value| r["runs"][0]["calls"].as_array().unwrap().len();
    assert_eq!(calls(&warm_report), calls(&plain_report));
    // The pre-warm node hangs under the turn it warms (a real node, not a
    // made-up parent), so the map shows it there.
    let started = |e: &&Value| e["type"] == "node_started";
    let warm_node = warm_events
        .iter()
        .filter(started)
        .find(|e| e["name"] == "cache-prewarm")
        .unwrap_or_else(|| panic!("no pre-warm node: {warm_events:#?}"));
    let turn = warm_events
        .iter()
        .filter(started)
        .find(|e| e["parent_node_id"].is_null())
        .expect("the turn's root node");
    assert_eq!(warm_node["parent_node_id"], turn["node_id"]);
}
