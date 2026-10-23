//! The coder is reminded to plan with `todo` when it works without a task
//! list (TASK-148), through `harness run` with the built-in coder. Hermetic:
//! config, data and HOME are temp dirs.

mod common;

use common::*;
use serde_json::Value;

const REMINDER: &str = "Reminder from harness";

fn script(calls: usize, todo_first: bool) -> String {
    let mut s = String::from("responses:\n");
    if todo_first {
        s.push_str("  - content:\n      - tool_use: { name: todo, input: { action: add, title: \"Map the crates\" } }\n");
    }
    for _ in 0..calls {
        s.push_str("  - content:\n      - tool_use: { name: read, input: { path: a.txt } }\n");
    }
    s.push_str("  - content:\n      - text: \"done\"\n");
    s
}

/// Which model requests carried the reminder, in order, from the run log
/// (the `--json` stream leaves request bodies out). `general` is extra
/// `[general]` settings.
fn reminded(general: &str, script_yaml: &str) -> Vec<bool> {
    let env = Env::new("");
    let ini = env.root.join("harness.ini");
    let text = std::fs::read_to_string(&ini).unwrap();
    std::fs::write(
        &ini,
        text.replacen("[general]\n", &format!("[general]\n{general}"), 1),
    )
    .unwrap();
    let script = env.script("s.yaml", script_yaml);
    let o = env.run(&script, &["--mode", "auto", "index it"]);
    assert_eq!(o.status.code(), Some(0), "stderr: {}", stderr(&o));
    let runs = env.runs();
    let log = std::fs::read_to_string(runs[0].join("events.jsonl")).unwrap();
    log.lines()
        .map(|l| serde_json::from_str::<Value>(l).unwrap())
        .filter(|e| e["type"] == "model_call_started")
        .map(|e| {
            e["request"]["json"]["messages"]
                .to_string()
                .contains(REMINDER)
        })
        .collect()
}

#[test]
fn the_coder_is_reminded_once_after_three_calls_without_a_todo_list() {
    assert_eq!(
        reminded("", &script(4, false)),
        [false, false, false, true, false]
    );
}

#[test]
fn no_reminder_once_the_coder_has_a_plan_or_when_turned_off() {
    assert!(reminded("", &script(4, true)).iter().all(|r| !r));
    assert!(
        reminded("todo_nudge_after = 0\n", &script(4, false))
            .iter()
            .all(|r| !r)
    );
}
