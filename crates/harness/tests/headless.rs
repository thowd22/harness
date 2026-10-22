//! `harness run` end to end: the real binary against the scripted fake
//! provider, with a temp harness.ini, data dir and project.

mod common;

use std::process::{Command, Stdio};
use std::time::Duration;

use common::*;
use serde_json::Value;

const TEXT: &str = "responses:\n  - content:\n      - text: \"All done.\"\n    usage: { input_tokens: 10, output_tokens: 5 }\n";

#[test]
fn run_executes_the_default_agent_in_the_current_directory() {
    let env = Env::new("");
    let script = env.script(
        "s.yaml",
        "responses:\n  - content:\n      - tool_use: { name: write, input: { path: out.txt, content: \"hi\\n\" } }\n  - content:\n      - text: \"Wrote it.\"\n",
    );
    let o = env.run(&script, &["write", "a", "file"]);
    assert_eq!(o.status.code(), Some(0), "stderr: {}", stderr(&o));
    assert_eq!(stdout(&o).trim(), "Wrote it.");
    assert_eq!(
        std::fs::read_to_string(env.project.join("out.txt")).unwrap(),
        "hi\n"
    );
    assert!(
        stderr(&o).contains("done after 2 turn(s)"),
        "{}",
        stderr(&o)
    );

    let list = env.cmd(&["sessions"]).output().unwrap();
    assert_eq!(list.status.code(), Some(0));
    let text = stdout(&list);
    assert!(text.contains("write a file"), "{text}");
    assert!(
        text.contains(env.project.canonicalize().unwrap().to_str().unwrap()),
        "{text}"
    );
}

#[test]
fn json_streams_redacted_events() {
    let env = Env::new("");
    let script = env.script(
        "s.yaml",
        &format!(
            "responses:\n  - content:\n      - text: \"the key is {SECRET}\"\n      - tool_use: {{ name: read, input: {{ path: secret.txt }} }}\n  - content:\n      - text: \"ok\"\n"
        ),
    );
    let o = env.run(&script, &["--json", "show", "the", "secret"]);
    assert_eq!(o.status.code(), Some(0), "stderr: {}", stderr(&o));
    let out = stdout(&o);
    assert!(!out.contains(SECRET), "secret leaked into --json output");
    assert!(out.contains("[REDACTED]"));
    let evs = events(&o);
    let types: Vec<&str> = evs.iter().map(|e| e["type"].as_str().unwrap()).collect();
    assert_eq!(types.first(), Some(&"run_started"));
    assert_eq!(types.last(), Some(&"run_finished"));
    for t in [
        "node_started",
        "model_call_started",
        "delta",
        "tool_call",
        "tool_result",
        "node_finished",
    ] {
        assert!(types.contains(&t), "missing {t} in {types:?}");
    }
    let seqs: Vec<u64> = evs.iter().map(|e| e["seq"].as_u64().unwrap()).collect();
    assert!(seqs.windows(2).all(|w| w[0] < w[1]));
}

#[test]
fn exit_codes_distinguish_outcomes() {
    let env = Env::new("");
    let looping = env.script(
        "loop.yaml",
        "responses:\n  - content: [ { tool_use: { name: read, input: { path: secret.txt } } } ]\n    usage: { input_tokens: 100, output_tokens: 50 }\n  - content: [ { tool_use: { name: read, input: { path: secret.txt } } } ]\n    usage: { input_tokens: 100, output_tokens: 50 }\n",
    );
    let o = env.run(&looping, &["--max-turns", "1", "go"]);
    assert_eq!(o.status.code(), Some(5), "max turns: {}", stderr(&o));
    let o = env.run(&looping, &["--budget-tokens", "100", "go"]);
    assert_eq!(o.status.code(), Some(4), "budget: {}", stderr(&o));
    let failing = env.script(
        "err.yaml",
        "responses:\n  - error: { kind: auth, message: bad key }\n",
    );
    let o = env.run(&failing, &["go"]);
    assert_eq!(o.status.code(), Some(1), "error: {}", stderr(&o));
    assert!(stderr(&o).contains("bad key"));
    let o = env.cmd(&["run", "--bogus"]).output().unwrap();
    assert_eq!(o.status.code(), Some(2), "usage");
}

#[test]
fn verification_failure_has_its_own_exit_code() {
    // One answer plus one per fix attempt ([verify] max_fix_attempts = 3).
    let fail = Env::new("[verify]\ncommand = exit 7\n");
    let fixes = format!(
        "{TEXT}{}",
        TEXT.trim_start_matches("responses:\n").repeat(3)
    );
    let script = fail.script("s.yaml", &fixes);
    let o = fail.run(&script, &["--json", "go"]);
    assert_eq!(o.status.code(), Some(3), "{}", stderr(&o));
    assert!(stderr(&o).contains("verification failed"));
    let verifies = events(&o)
        .iter()
        .filter(|e| e["type"] == "verify_finished" && e["passed"] == false)
        .count();
    assert_eq!(
        verifies, 4,
        "verify runs once and after each of 3 fix attempts"
    );
    let o = fail.run(&script, &["--no-verify", "go"]);
    assert_eq!(o.status.code(), Some(0));

    let pass = Env::new("[verify]\ncommand = true\n");
    let script = pass.script("s.yaml", TEXT);
    let o = pass.run(&script, &["--json", "go"]);
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
    assert!(
        events(&o)
            .iter()
            .any(|e| e["type"] == "verify_finished" && e["passed"] == true)
    );
}

#[test]
fn sigint_cancels_with_exit_130() {
    let env = Env::new("");
    let script = env.script(
        "slow.yaml",
        "responses:\n  - delay_ms: 30000\n    content: [ { text: late } ]\n",
    );
    let child = env
        .cmd(&["run", "--fake-script", script.to_str().unwrap(), "go"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    std::thread::sleep(Duration::from_millis(700));
    let killed = Command::new("kill")
        .args(["-INT", &child.id().to_string()])
        .status()
        .unwrap();
    assert!(killed.success());
    let o = child.wait_with_output().unwrap();
    assert_eq!(o.status.code(), Some(130), "{}", stderr(&o));
    assert!(stderr(&o).contains("cancelled"));
}

const BASH: &str = "responses:\n  - content: [ { tool_use: { name: bash, input: { command: \"touch made.txt\" } } } ]\n  - content: [ { text: finished } ]\n";

#[test]
fn approvals_are_denied_by_default_in_headless_mode() {
    let env = Env::new("");
    let script = env.script("s.yaml", BASH);
    let o = env.run(&script, &["--json", "--mode", "ask", "make", "a", "file"]);
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
    assert!(!env.project.join("made.txt").exists(), "denied command ran");
    let evs = events(&o);
    assert!(
        evs.iter()
            .any(|e| e["type"] == "approval_resolved" && e["decision"] == "denied")
    );
    let result = evs.iter().find(|e| e["type"] == "tool_result").unwrap();
    assert_eq!(result["ok"], false);
    assert!(
        result["output"].to_string().contains("--approvals deny"),
        "{result}"
    );
}

#[test]
fn allowlist_mode_runs_only_allow_listed_commands() {
    let env = Env::new("");
    let script = env.script("s.yaml", BASH);
    let o = env.run(
        &script,
        &[
            "--mode",
            "ask",
            "--approvals",
            "allowlist",
            "--allow",
            "touch *",
            "make",
            "a",
            "file",
        ],
    );
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
    assert!(
        env.project.join("made.txt").exists(),
        "allow-listed command did not run"
    );

    let other = Env::new("");
    let script = other.script("s.yaml", BASH);
    let o = other.run(
        &script,
        &[
            "--json",
            "--mode",
            "ask",
            "--approvals",
            "allowlist",
            "--allow",
            "cargo *",
            "go",
        ],
    );
    assert!(!other.project.join("made.txt").exists());
    assert!(stdout(&o).contains("matches no --allow pattern"));
}

#[test]
fn resume_continues_a_saved_session() {
    let env = Env::new("");
    let first = env.script("a.yaml", "responses:\n  - content: [ { reasoning: { text: hmm, signature: sig-1 } }, { text: first } ]\n");
    let o = env.run(&first, &["start", "here"]);
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
    let second = env.script("b.yaml", "responses:\n  - content: [ { text: second } ]\n");
    let o = env.run(&second, &["--json", "--resume", "last", "and", "again"]);
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
    let request = events(&o)
        .into_iter()
        .find(|e| e["type"] == "model_call_started")
        .unwrap();
    // Payloads serialize externally tagged: {"json": {...}}.
    let payload = &request["request"]["json"];
    let messages = payload["messages"].as_array().unwrap();
    assert_eq!(payload["messages_from"], 0);
    assert_eq!(messages.len(), 3, "{messages:?}");
    assert!(
        messages[1].to_string().contains("sig-1"),
        "reasoning not restored"
    );
    let list = stdout(&env.cmd(&["sessions"]).output().unwrap());
    assert_eq!(list.lines().count(), 1, "resume reuses the session: {list}");
}

#[test]
fn config_check_and_schema_subcommands() {
    let env = Env::new("");
    let o = env.cmd(&["config", "check"]).output().unwrap();
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
    let bad = env.root.join("bad.ini");
    std::fs::write(&bad, "[nope]\nx = 1\n").unwrap();
    let o = env
        .cmd(&["config", "check", bad.to_str().unwrap()])
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(1));
    let o = env.cmd(&["schema", "agent"]).output().unwrap();
    assert_eq!(o.status.code(), Some(0));
    let _: Value = serde_json::from_slice(&o.stdout).unwrap();
    let o = env.cmd(&["schema", "bogus"]).output().unwrap();
    assert_eq!(o.status.code(), Some(2));
    let o = env.cmd(&["--version"]).output().unwrap();
    assert!(stdout(&o).starts_with("harness "));
}

#[test]
fn log_file_is_redacted_with_resolved_credentials() {
    let env = Env::new("");
    // A duplicate call id is logged as a warning; make the id the secret.
    let script = env.script(
        "s.yaml",
        &format!(
            "responses:\n  - content:\n      - tool_use: {{ id: {SECRET}, name: read, input: {{ path: secret.txt }} }}\n      - tool_use: {{ id: {SECRET}, name: read, input: {{ path: secret.txt }} }}\n  - content: [ {{ text: ok }} ]\n"
        ),
    );
    let o = env.run(&script, &["go"]);
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
    let logs = env.root.join("data").join("logs");
    let text: String = std::fs::read_dir(&logs)
        .unwrap()
        .map(|e| std::fs::read_to_string(e.unwrap().path()).unwrap_or_default())
        .collect();
    assert!(text.contains("duplicate tool call id"), "log: {text}");
    assert!(!text.contains(SECRET), "secret reached the log file");
    assert!(text.contains("[REDACTED]"));
}

#[test]
fn an_agent_mode_tightens_the_session_mode() {
    let env = Env::new("");
    std::fs::create_dir_all(env.root.join("xdg-config/harness/agents")).unwrap();
    std::fs::write(
        env.root.join("xdg-config/harness/agents/careful.yaml"),
        "name: careful\nprompt: coder\ntools: [bash]\nmode: ask\n",
    )
    .unwrap();
    let script = env.script("s.yaml", BASH);
    // `--mode auto` would run the command; the agent's `mode: ask` makes it
    // ask, and headless runs deny what asks.
    let o = env.run(
        &script,
        &["--json", "--mode", "auto", "--agent", "careful", "go"],
    );
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
    assert!(
        !env.project.join("made.txt").exists(),
        "the agent's mode applied"
    );
    assert!(
        events(&o)
            .iter()
            .any(|e| e["type"] == "approval_resolved" && e["decision"] == "denied")
    );
    // The same agent without `mode:` runs it.
    std::fs::write(
        env.root.join("xdg-config/harness/agents/careful.yaml"),
        "name: careful\nprompt: coder\ntools: [bash]\n",
    )
    .unwrap();
    let o = env.run(&script, &["--mode", "auto", "--agent", "careful", "go"]);
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
    assert!(env.project.join("made.txt").exists());
}

#[test]
fn files_changed_by_bash_reach_the_change_ledger() {
    let env = Env::new("");
    std::fs::write(env.project.join("old.txt"), "old\n").unwrap();
    let script = env.script(
        "s.yaml",
        "responses:\n  - content: [ { tool_use: { name: bash, input: { command: \"printf 'hi\\\\n' > made.txt && rm old.txt\" } } } ]\n  - content: [ { text: done } ]\n",
    );
    let o = env.run(&script, &["--json", "--mode", "auto", "go"]);
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
    assert!(env.project.join("made.txt").exists());
    let evs = events(&o);
    let node = evs.iter().find(|e| e["type"] == "node_started").unwrap()["node_id"].clone();
    let changes: Vec<(String, String)> = evs
        .iter()
        .filter(|e| e["type"] == "file_changed")
        .map(|e| {
            assert_eq!(e["node_id"], node, "on the agent's node");
            (
                e["path"].as_str().unwrap().to_owned(),
                e["change"].as_str().unwrap().to_owned(),
            )
        })
        .collect();
    assert!(
        changes.contains(&("made.txt".into(), "created".into())),
        "{changes:?}"
    );
    assert!(
        changes.contains(&("old.txt".into(), "deleted".into())),
        "{changes:?}"
    );
}

#[test]
fn memory_writes_are_recorded_with_their_commit() {
    let env = Env::new("[memory]\nheadless_access = true\n");
    let script = env.script(
        "s.yaml",
        "responses:\n  - content: [ { tool_use: { name: memory, input: { action: write, path: tools/cargo, text: \"Run cargo nextest instead of cargo test in this repo.\" } } } ]\n  - content: [ { text: saved } ]\n",
    );
    let o = env.run(&script, &["--json", "remember", "nextest"]);
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
    let evs = events(&o);
    let tools: Vec<&str> = evs
        .iter()
        .find(|e| e["type"] == "model_call_started")
        .unwrap()["request"]["json"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|t| t.as_str().or_else(|| t["name"].as_str()))
        .collect();
    assert!(tools.contains(&"memory"), "{tools:?}");
    let mem = evs
        .iter()
        .find(|e| e["type"] == "memory")
        .unwrap_or_else(|| panic!("no memory event: {}", stdout(&o)));
    assert_eq!(mem["action"], "written");
    let sha = mem["memory_id"].as_str().unwrap();
    assert_eq!(sha.len(), 40, "the commit id, for undo: {sha}");
    assert!(sha.chars().all(|c| c.is_ascii_hexdigit()));
    let summary = mem["summary"].as_str().unwrap();
    assert!(
        summary.contains("tools/cargo") && summary.contains("Run cargo nextest"),
        "{mem}"
    );
    let result = evs.iter().find(|e| e["type"] == "tool_result").unwrap();
    assert_eq!(result["ok"], true, "{result}");
}
