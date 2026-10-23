//! MCP tools end to end through `harness run` (TASK-146.2): an agent that
//! selects MCP tools calls them on the bundled test server; calls are
//! policy-checked as `mcp:<server>/<tool>`, recorded with server and tool
//! names, and labeled untrusted. Hermetic: config, data and HOME are temp
//! dirs.

mod common;

use common::*;
use serde_json::Value;

fn env_with_server(policy: &str) -> Env {
    let bin = harness_mcp::test_server::binary_path();
    let env = Env::new(&format!(
        "[mcp.t]\ncommand = {}\nargs = --agent-tools\n\n[policy]\n{policy}\n",
        bin.display()
    ));
    env.global_file(
        "agents/mcp-agent.yaml",
        "name: mcp-agent\nprompt: coder\ntools: [read, mcp:t/echo, mcp:t/write_note]\n",
    );
    env
}

const SCRIPT: &str = "responses:\n  - content:\n      - tool_use: { name: mcp__t__echo, input: { text: \"from the server\" } }\n  - content:\n      - tool_use: { name: mcp__t__write_note, input: { text: \"note\" } }\n  - content:\n      - text: \"done\"\n";

fn of_type<'a>(evs: &'a [Value], ty: &str) -> Vec<&'a Value> {
    evs.iter().filter(|e| e["type"] == ty).collect()
}

#[test]
fn run_calls_selected_mcp_tools_through_the_policy_engine() {
    let env = env_with_server("mcp.t.allow = echo");
    let script = env.script("s.yaml", SCRIPT);
    let o = env.run(
        &script,
        &["--json", "--agent", "mcp-agent", "--mode", "auto", "go"],
    );
    assert_eq!(o.status.code(), Some(0), "stderr: {}", stderr(&o));
    let evs = events(&o);
    let calls: Vec<(String, String)> = of_type(&evs, "mcp_call")
        .iter()
        .map(|e| {
            (
                e["server"].as_str().unwrap().to_owned(),
                e["tool"].as_str().unwrap().to_owned(),
            )
        })
        .collect();
    assert_eq!(
        calls,
        [
            ("t".to_owned(), "echo".to_owned()),
            ("t".to_owned(), "write_note".to_owned())
        ]
    );
    let decisions: Vec<(String, String)> = of_type(&evs, "policy_decision")
        .iter()
        .map(|e| {
            (
                e["tool"].as_str().unwrap().to_owned(),
                e["decision"].as_str().unwrap().to_owned(),
            )
        })
        .collect();
    assert_eq!(
        decisions,
        [
            ("mcp:t/echo".to_owned(), "allow".to_owned()),
            // Even in auto mode: an outward action, denied headless.
            ("mcp:t/write_note".to_owned(), "ask".to_owned())
        ]
    );
    let results = of_type(&evs, "tool_result");
    assert_eq!(results.len(), 2);
    assert_eq!(results[0]["ok"], true);
    let out = results[0]["output"].to_string();
    assert!(out.contains("from the server"), "{out}");
    assert!(out.contains("origin=\\\"mcp server\\\""), "{out}");
    assert_eq!(results[1]["ok"], false);
    // The tool calls are recorded under the names the model used.
    let names: Vec<&str> = of_type(&evs, "tool_call")
        .iter()
        .map(|e| e["tool"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["mcp__t__echo", "mcp__t__write_note"]);
}

#[test]
fn allow_patterns_grant_mcp_tools_in_allowlist_mode() {
    let env = env_with_server("");
    let script = env.script("s.yaml", SCRIPT);
    let o = env.run(
        &script,
        &[
            "--json",
            "--agent",
            "mcp-agent",
            "--approvals",
            "allowlist",
            "--allow",
            "mcp:t/*",
            "go",
        ],
    );
    assert_eq!(o.status.code(), Some(0), "stderr: {}", stderr(&o));
    let evs = events(&o);
    let ok: Vec<bool> = of_type(&evs, "tool_result")
        .iter()
        .map(|e| e["ok"].as_bool().unwrap())
        .collect();
    assert_eq!(ok, [true, true]);
}

#[test]
fn agents_naming_undefined_servers_are_reported() {
    let env = Env::new("");
    env.global_file(
        "agents/ghost.yaml",
        "name: ghost\nprompt: coder\ntools: [read, mcp:ghost/*]\n",
    );
    let script = env.script("s.yaml", "responses:\n  - content:\n      - text: \"hi\"\n");
    let o = env.run(&script, &["--agent", "ghost", "go"]);
    let err = stderr(&o);
    assert!(err.contains("[mcp.ghost]"), "{err}");
}

#[test]
fn workflow_agent_steps_label_mcp_results_untrusted() {
    let env = env_with_server("mcp.t.allow = echo");
    env.global_file(
        "agents/plain.yaml",
        "name: plain\nprompt: coder\ntools: [read]\n",
    );
    env.global_file(
        "workflows/mcp-step.yaml",
        "name: mcp-step\nsteps:\n  - id: ask\n    agent: plain\n    prompt: \"echo something\"\n    tools: [mcp:t/echo]\n",
    );
    let script = env.script(
        "s.yaml",
        "responses:\n  - content:\n      - tool_use: { name: mcp__t__echo, input: { text: \"from the server\" } }\n  - content:\n      - text: \"done\"\n",
    );
    let o = env
        .cmd(&[
            "wf",
            "run",
            "mcp-step",
            "--mode",
            "auto",
            "--json",
            "--fake-script",
            script.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(0), "stderr: {}", stderr(&o));
    let evs = events(&o);
    let calls = of_type(&evs, "mcp_call");
    assert_eq!(calls.len(), 1, "{evs:?}");
    assert_eq!(calls[0]["server"], "t");
    assert_eq!(calls[0]["tool"], "echo");
    let results = of_type(&evs, "tool_result");
    assert_eq!(results.len(), 1, "{evs:?}");
    assert_eq!(results[0]["ok"], true);
    let out = results[0]["output"].to_string();
    assert!(out.contains("from the server"), "{out}");
    assert!(out.contains("trust=\\\"untrusted\\\""), "{out}");
    assert!(out.contains("origin=\\\"mcp server\\\""), "{out}");
}
