//! `[experimental] early_tool_start` end to end: `harness run` with the
//! setting on gives the same tool calls, results and event sequence as
//! with it off.

mod common;

use common::*;
use serde_json::Value;

const SCRIPT: &str = r#"responses:
  - chunk_size: 6
    chunk_delay_ms: 1
    content:
      - tool_use: { name: read, input: { path: a.txt } }
      - tool_use: { name: glob, input: { pattern: "*.txt" } }
      - tool_use: { name: read, input: { path: b.txt } }
      - text: "Read both files and listed them."
      - tool_use: { name: write, input: { path: c.txt, content: "c\n" } }
      - tool_use: { name: read, input: { path: c.txt } }
  - content:
      - text: "All done."
"#;

/// Event types, and for tool events the call id, tool, status and output.
fn projection(events: &[Value]) -> Vec<String> {
    events
        .iter()
        .map(|e| {
            let t = e["type"].as_str().unwrap_or_default();
            match t {
                "tool_call" => format!("{t} {} {}", e["call_id"], e["tool"]),
                "tool_result" => format!("{t} {} {} {}", e["call_id"], e["ok"], e["output"]),
                "file_changed" => format!("{t} {}", e["path"]),
                _ => t.to_owned(),
            }
        })
        .collect()
}

fn run(ini: &str) -> (Vec<String>, String) {
    let env = Env::new(ini);
    std::fs::write(env.project.join("a.txt"), "alpha\n").unwrap();
    std::fs::write(env.project.join("b.txt"), "beta\n").unwrap();
    let script = env.script("s.yaml", SCRIPT);
    let o = env.run(&script, &["--json", "read", "the", "files"]);
    assert_eq!(o.status.code(), Some(0), "stderr: {}", stderr(&o));
    assert_eq!(
        std::fs::read_to_string(env.project.join("c.txt")).unwrap(),
        "c\n"
    );
    (projection(&events(&o)), stderr(&o))
}

#[test]
fn early_tool_start_keeps_results_and_event_order() {
    let (off, _) = run("");
    let (on, err) = run("[experimental]\nearly_tool_start = true\n");
    assert!(!err.contains("unknown key"), "{err}");
    assert_eq!(on, off);
    let results: Vec<&String> = on.iter().filter(|e| e.starts_with("tool_result")).collect();
    assert_eq!(results.len(), 5, "{on:#?}");
    assert!(results[0].contains("alpha"), "{}", results[0]);
    assert!(results[2].contains("beta"), "{}", results[2]);
    // The read after the write sees the new file.
    assert!(results[4].contains("c"), "{}", results[4]);
}
