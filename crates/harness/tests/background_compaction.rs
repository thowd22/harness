//! Background compaction through `harness run` (TASK-161.5): a summary
//! still pending when the session ends is stopped and recorded before the
//! run log closes. Hermetic: config, data and HOME are temp dirs.

mod common;

use common::*;
use serde_json::Value;

/// Three reads whose reported input crosses 0.7 of a 10k window before the
/// last agent call, then an answer. The handoff summary (matched by its
/// system prompt) takes far longer than the session, so it is still
/// pending when the session finishes.
const SCRIPT: &str = "capabilities:\n  tools: true\n  context_window: 10000\n  max_output_tokens: 2000\nresponses:\n  - match_system: \"You write handoff notes\"\n    delay_ms: 600000\n    content:\n      - text: \"## Goal\\nlate\"\n  - content:\n      - tool_use: { name: read, input: { path: a.txt } }\n    usage: { input_tokens: 3000, output_tokens: 10 }\n  - content:\n      - tool_use: { name: read, input: { path: a.txt } }\n    usage: { input_tokens: 5000, output_tokens: 10 }\n  - content:\n      - tool_use: { name: read, input: { path: a.txt } }\n    usage: { input_tokens: 7500, output_tokens: 10 }\n  - content:\n      - text: \"done\"\n    usage: { input_tokens: 7600, output_tokens: 5 }\n";

#[test]
fn a_summary_pending_at_the_end_of_the_session_is_recorded_before_the_log_closes() {
    let env = Env::new("[context]\nbackground_compaction = true\nkeep_recent_turns = 1\n");
    std::fs::write(env.project.join("a.txt"), "hello\n").unwrap();
    let script = env.script("s.yaml", SCRIPT);
    let o = env.run(&script, &["--mode", "auto", "read it"]);
    assert_eq!(o.status.code(), Some(0), "stderr: {}", stderr(&o));
    assert_eq!(stdout(&o).trim(), "done");

    let runs = env.runs();
    let log = std::fs::read_to_string(runs[0].join("events.jsonl")).unwrap();
    let events: Vec<Value> = log
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    let started: Vec<&Value> = events
        .iter()
        .filter(|e| e["type"] == "node_started" && e["name"] == "compaction")
        .collect();
    assert_eq!(started.len(), 1, "one background summary started:\n{log}");
    let node = &started[0]["node_id"];
    let finished: Vec<&Value> = events
        .iter()
        .filter(|e| e["type"] == "node_finished" && &e["node_id"] == node)
        .collect();
    assert_eq!(finished.len(), 1, "the compaction node ended:\n{log}");
    assert_eq!(finished[0]["status"], "cancelled", "{log}");
    let call_ended = events
        .iter()
        .any(|e| e["type"] == "model_call_finished" && &e["node_id"] == node);
    assert!(call_ended, "its model call ended:\n{log}");
    // Both are recorded before the run itself finishes.
    let end = events
        .iter()
        .position(|e| e["type"] == "run_finished")
        .expect("run_finished");
    let node_end = events
        .iter()
        .position(|e| e["type"] == "node_finished" && &e["node_id"] == node)
        .unwrap();
    assert!(node_end < end, "{log}");
}
