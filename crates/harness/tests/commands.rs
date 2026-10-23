//! The binary's inspection and workflow subcommands end to end: lint,
//! doctor, config show, runs show, wf, eval and the built-in defaults.

mod common;

use std::net::TcpListener;

use common::*;
use serde_json::Value;

const TEXT: &str = "responses:\n  - content:\n      - text: \"All done.\"\n    usage: { input_tokens: 10, output_tokens: 5 }\n";

#[test]
fn default_agent_is_the_builtin_coder_with_todo() {
    let env = Env::new("");
    let script = env.script("s.yaml", TEXT);
    let o = env.run(&script, &["--json", "hello"]);
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
    assert!(
        !stderr(&o).contains("not available"),
        "every tool of the built-in coder is wired: {}",
        stderr(&o)
    );
    let evs = events(&o);
    let started = evs.iter().find(|e| e["type"] == "node_started").unwrap();
    assert_eq!(started["agent"], "coder");
    let user = evs.iter().find(|e| e["type"] == "user_message").unwrap();
    assert_eq!(user["text"], "hello");
    let request = &evs
        .iter()
        .find(|e| e["type"] == "model_call_started")
        .unwrap()["request"]["json"];
    let system = request["system"].as_str().unwrap();
    // Text from the built-in prompt layers (prompts/base.md, prompts/context.md).
    assert!(system.contains("coding agent"), "{system}");
    assert!(system.contains("The harness manages the conversation's length"));
    let tools: Vec<&str> = request["tools"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|t| t.as_str().or_else(|| t["name"].as_str()))
        .collect();
    for t in ["read", "write", "edit", "bash", "todo"] {
        assert!(tools.contains(&t), "{t} missing from {tools:?}");
    }
}

#[test]
fn every_run_is_stamped_with_the_config_hash() {
    let env = Env::new("");
    let script = env.script("s.yaml", TEXT);
    for _ in 0..2 {
        assert_eq!(env.run(&script, &["go"]).status.code(), Some(0));
    }
    let header = |dir: &std::path::Path| -> Value {
        let text = std::fs::read_to_string(dir.join("events.jsonl")).unwrap();
        serde_json::from_str(text.lines().next().unwrap()).unwrap()
    };
    let runs = env.runs();
    assert_eq!(runs.len(), 2);
    let a = header(&runs[0])["config_hash"].as_str().unwrap().to_owned();
    let b = header(&runs[1])["config_hash"].as_str().unwrap().to_owned();
    assert_eq!(a.len(), 64);
    assert_eq!(a, b, "same config, same hash");
    // Changing loaded YAML changes the hash.
    env.global_file(
        "skills/greet.md",
        "---\ndescription: Say hello\n---\nSay hello to $ARGUMENTS.\n",
    );
    assert_eq!(env.run(&script, &["go"]).status.code(), Some(0));
    let c = header(&env.runs()[2])["config_hash"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_ne!(a, c);
}

#[test]
fn lint_exits_nonzero_on_errors() {
    let env = Env::new("");
    let o = env.cmd(&["lint"]).output().unwrap();
    assert_eq!(o.status.code(), Some(0), "{}{}", stdout(&o), stderr(&o));
    assert!(stdout(&o).contains("0 errors"), "{}", stdout(&o));

    env.global_file(
        "agents/broken.yaml",
        "name: broken\nprompt_text: hi\ntools: [read, teleport]\n",
    );
    env.global_file(
        "prompts/strict.yaml",
        "name: strict\nlayers:\n  - text: Never modify tests or config files.\n",
    );
    let o = env.cmd(&["lint"]).output().unwrap();
    let out = stdout(&o);
    assert_eq!(o.status.code(), Some(1), "{out}");
    assert!(out.contains("broken.yaml"), "{out}");
    assert!(out.contains("teleport"), "{out}");
    assert!(
        out.contains("strict.yaml"),
        "blanket prohibition reported: {out}"
    );
}

#[test]
fn doctor_checks_every_area_without_the_network() {
    // A local listener stands in for the provider endpoint.
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let env = Env::raw(&format!(
        "[provider.anthropic]\napi_key = {SECRET}\nbase_url = http://127.0.0.1:{port}\n"
    ));
    env.global_file(
        "prompts/strict.yaml",
        "name: strict\nlayers:\n  - text: Never modify tests or config files.\n",
    );
    let o = env.cmd(&["doctor"]).output().unwrap();
    let out = stdout(&o);
    for area in [
        "config",
        "credentials",
        "provider anthropic",
        "sandbox",
        "lint",
        "definitions",
    ] {
        assert!(out.contains(area), "doctor misses `{area}`:\n{out}");
    }
    assert!(out.contains("reachable"), "{out}");
    assert!(
        out.contains("strict.yaml"),
        "lint warnings appear in doctor: {out}"
    );
    assert!(!out.contains(SECRET));
    let failed = out
        .lines()
        .any(|l| l.contains("FAIL") || l.contains("fail"));
    assert_eq!(o.status.code(), Some(i32::from(failed)), "{out}");
}

#[test]
fn config_show_prints_sources_and_redacts() {
    let env = Env::new("[ui]\nmouse = false\n");
    let o = env.cmd(&["config", "show"]).output().unwrap();
    let out = stdout(&o);
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
    assert!(!out.contains(SECRET), "secret printed: {out}");
    assert!(out.to_lowercase().contains("redacted"), "{out}");
    assert!(out.contains("mouse"), "{out}");
    assert!(out.contains("harness.ini"), "sources are shown: {out}");
    let bad = env.root.join("bad.ini");
    std::fs::write(&bad, "[ui]\nmouse = sometimes\n").unwrap();
    let o = env
        .cmd(&["config", "show", bad.to_str().unwrap()])
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(1));
}

#[test]
fn runs_show_prints_the_tree_and_json_state() {
    let env = Env::new("");
    let script = env.script(
        "s.yaml",
        "responses:\n  - content:\n      - tool_use: { name: write, input: { path: out.txt, content: \"hi\\n\" } }\n  - content:\n      - text: \"Wrote it.\"\n",
    );
    assert_eq!(env.run(&script, &["write"]).status.code(), Some(0));
    let o = env.cmd(&["runs", "show", "last"]).output().unwrap();
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
    let tree = stdout(&o);
    // Titled after the prompt, with the agent's display name.
    assert!(tree.starts_with("Coder · write  succeeded"), "{tree}");
    let o = env
        .cmd(&["runs", "show", "last", "--json"])
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(0));
    let state: Value = serde_json::from_slice(&o.stdout).unwrap();
    assert!(state.to_string().contains("succeeded"), "{state}");
    let o = env.cmd(&["runs", "show", "nope"]).output().unwrap();
    assert_eq!(o.status.code(), Some(1));
    let o = env.cmd(&["runs"]).output().unwrap();
    assert_eq!(o.status.code(), Some(2));
}

fn calc_fixture(env: &Env) {
    let fixtures = workspace_root().join("crates/harness-workflow/evals/workflows/fixtures/calc");
    copy_dir(&fixtures, &env.project);
    std::fs::write(
        env.project.join("tests/test_double.sh"),
        ". ./src/calc.sh\n. ./tests/lib.sh\ncheck double \"$(double 4)\" 8\nfinish\n",
    )
    .unwrap();
}

fn wf_script() -> String {
    workspace_root()
        .join("crates/harness-workflow/evals/workflows/scripts/plan-approve-implement.yaml")
        .display()
        .to_string()
}

#[test]
fn wf_lists_describes_and_runs_bundled_workflows() {
    let env = Env::new("");
    let o = env.cmd(&["wf", "list"]).output().unwrap();
    let out = stdout(&o);
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
    for c in ["/fix", "/ivr", "/plan"] {
        assert!(out.contains(c), "{c} missing: {out}");
    }
    let o = env.cmd(&["wf", "help", "fix"]).output().unwrap();
    assert_eq!(o.status.code(), Some(0));
    assert!(stdout(&o).contains("Steps:"), "{}", stdout(&o));

    calc_fixture(&env);
    let task = "Add a `double` function to src/calc.sh (double 4 prints 8).";
    let base = [
        "wf",
        "run",
        "plan",
        "--mode",
        "auto",
        "--fake-script",
        &wf_script(),
    ];
    // The plan gate is denied unless approved.
    let mut args = base.to_vec();
    args.extend(["--input", "test=sh run_tests.sh", task]);
    let o = env.cmd(&args).output().unwrap();
    assert_eq!(o.status.code(), Some(1), "{}", stderr(&o));
    assert!(
        !std::fs::read_to_string(env.project.join("src/calc.sh"))
            .unwrap()
            .contains("double()"),
        "nothing changes before the plan is approved"
    );

    let mut args = base.to_vec();
    args.extend([
        "--json",
        "--approve-all",
        "--input",
        "test=sh run_tests.sh",
        task,
    ]);
    let o = env.cmd(&args).output().unwrap();
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
    assert!(stderr(&o).contains("succeeded"), "{}", stderr(&o));
    assert!(
        std::fs::read_to_string(env.project.join("src/calc.sh"))
            .unwrap()
            .contains("double()")
    );
    let evs = events(&o);
    assert_eq!(evs.first().unwrap()["type"], "run_started");
    assert_eq!(evs.first().unwrap()["workflow"], "plan-approve-implement");
    assert!(evs.iter().any(|e| e["type"] == "approval_resolved"));

    let o = env.cmd(&["wf", "run", "nope"]).output().unwrap();
    assert_eq!(o.status.code(), Some(1));
    assert!(stderr(&o).contains("unknown workflow"));
    let o = env.cmd(&["wf", "run", "plan"]).output().unwrap();
    assert_eq!(o.status.code(), Some(1));
    assert!(stderr(&o).contains("missing input"), "{}", stderr(&o));
    let o = env.cmd(&["wf"]).output().unwrap();
    assert_eq!(o.status.code(), Some(2));
}

#[test]
fn eval_runs_a_suite_through_the_binary() {
    let env = Env::new("");
    let suite = workspace_root().join("crates/harness-workflow/evals/workflows/suite.yaml");
    let out = env.root.join("results.json");
    let o = env
        .cmd(&[
            "eval",
            "run",
            suite.to_str().unwrap(),
            "--filter",
            "plan-approve",
            "--out",
            out.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(0), "{}\n{}", stdout(&o), stderr(&o));
    let results: Value = serde_json::from_str(&std::fs::read_to_string(&out).unwrap()).unwrap();
    assert_eq!(results["harness_version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(results["config_hash"].as_str().unwrap().len(), 64);
    assert_eq!(results["summary"]["passed"], 1, "{results}");
    let o = env.cmd(&["eval"]).output().unwrap();
    assert_eq!(o.status.code(), Some(2));
}

/// Serves `body` as JSON to every request (a stand-in for a provider API).
fn json_server(body: &'static str) -> u16 {
    use std::io::{Read, Write};
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for stream in listener.incoming().take(8) {
            let Ok(mut s) = stream else { continue };
            let mut buf = Vec::new();
            let mut chunk = [0u8; 1024];
            while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
                match s.read(&mut chunk) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => buf.extend_from_slice(&chunk[..n]),
                }
            }
            let _ = write!(
                s,
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
        }
    });
    port
}

#[test]
fn openrouter_models_without_tools_are_rejected_at_agent_load() {
    let port = json_server(
        r#"{"data":[{"id":"acme/chat-only","context_length":8000,"supported_parameters":["temperature"]}]}"#,
    );
    let env = Env::raw(&format!(
        "[provider.openrouter]\napi_key = {SECRET}\nbase_url = http://127.0.0.1:{port}/api/v1\n"
    ));
    let o = env
        .cmd(&["run", "--model", "openrouter/acme/chat-only", "hello"])
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(1), "{}", stderr(&o));
    let err = stderr(&o);
    assert!(err.contains("does not support tool calling"), "{err}");
    assert!(err.contains("acme/chat-only"), "{err}");
    assert!(env.runs().is_empty(), "nothing ran");
}

#[test]
fn the_dream_workflow_loads_and_runs_headless_only_with_headless_access() {
    let env = Env::new("");
    let o = env.cmd(&["wf", "list"]).output().unwrap();
    assert!(stdout(&o).contains("/dream"), "{}", stdout(&o));
    assert!(
        !stderr(&o).contains("invalid step type"),
        "the built-in dream workflow loads: {}",
        stderr(&o)
    );
    let o = env.cmd(&["wf", "run", "dream"]).output().unwrap();
    assert_eq!(o.status.code(), Some(1), "{}", stderr(&o));
    assert!(stderr(&o).contains("headless_access"), "{}", stderr(&o));

    let env = Env::new("[memory]\nheadless_access = true\n");
    let o = env.cmd(&["wf", "run", "dream", "--json"]).output().unwrap();
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
    let evs = events(&o);
    assert!(
        evs.iter()
            .any(|e| e["type"] == "node_finished" && e["status"] == "succeeded"),
        "{}",
        stdout(&o)
    );
    // The pass wrote its log in the drive.
    let log = env.root.join("data/memory/.harness/dream-log.jsonl");
    assert!(log.exists(), "{}", log.display());
}

#[test]
fn long_task_evals_pass_through_the_binary() {
    let env = Env::new("");
    let suite = workspace_root().join("crates/harness-eval/suites/long-tasks/suite.yaml");
    let out = env.root.join("long.json");
    let o = env
        .cmd(&[
            "eval",
            "run",
            suite.to_str().unwrap(),
            "--out",
            out.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(0), "{}\n{}", stdout(&o), stderr(&o));
    let results: Value = serde_json::from_str(&std::fs::read_to_string(&out).unwrap()).unwrap();
    assert_eq!(results["summary"]["passed"], 2, "{results}");
    assert_eq!(results["summary"]["premature_stops"], 0);
}

#[test]
fn workflows_can_turn_memory_off() {
    let env = Env::new("[memory]\nheadless_access = true\n");
    let script = env.script("s.yaml", TEXT);
    let tools_of = |wf: &str| -> Vec<String> {
        env.global_file(
            "workflows/note.yaml",
            &format!(
                "name: note\n{wf}steps:\n  - id: a\n    agent: coder\n    prompt: \"say hi\"\n"
            ),
        );
        let o = env
            .cmd(&[
                "wf",
                "run",
                "note",
                "--json",
                "--fake-script",
                script.to_str().unwrap(),
            ])
            .output()
            .unwrap();
        assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
        let evs = events(&o);
        let call = evs
            .iter()
            .find(|e| e["type"] == "model_call_started")
            .unwrap_or_else(|| panic!("{}", stdout(&o)));
        call["request"]["json"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|t| t.as_str().or_else(|| t["name"].as_str()).map(str::to_owned))
            .collect()
    };
    // The built-in coder has read-write memory; the workflow gives it.
    assert!(tools_of("").contains(&"memory".to_owned()));
    assert!(!tools_of("memory: off\n").contains(&"memory".to_owned()));
}
