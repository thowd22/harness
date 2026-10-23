//! MCP end to end (TASK-146 AC#2): `harness run` drives an agent against
//! the bundled MCP test server, over stdio (the `harness-mcp-test-server`
//! binary) and over streamable HTTP (the in-process `HttpServer`), each in
//! the modern and the legacy protocol generation. For every transport the
//! test checks the whole path:
//!
//! 1. the server's tools are listed and offered to the model,
//! 2. the model calls them and the calls are policy-checked as
//!    `mcp:<server>/<tool>` (one allowed, one asking and denied headless),
//! 3. the calls are recorded in the run log on disk (`tool_call`,
//!    `mcp_call`, `policy_decision`, `tool_result`),
//! 4. folding that run log into the TUI chat view shows each call as an
//!    `mcp <server> › <tool>` tool card that expands by click and by key.
//!
//! Hermetic: config, data and HOME are temp dirs (see `common::hermetic`).

mod common;

use std::path::Path;
use std::time::Instant;

use common::*;
use harness_events::schema::{EventKind, Payload, RunEvent};
use harness_events::store::read_run;
use harness_mcp::test_server::{HttpServer, Options, ServerEra};
use harness_policy::rules::Mode;
use harness_tui::chat::view::{ChatConfig, ChatView};
use harness_tui::interaction::testing::{buffer_text, click, find_text, key};
use harness_tui::status::StatusLine;
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use serde_json::Value;

const SCRIPT: &str = "responses:\n  - content:\n      - tool_use: { name: mcp__t__echo, input: { text: \"from the server\\nline 2\\nline 3\\nline 4\\nline 5\\nline 6\\nline 7\\nline 8\\nline 9\\nline 10\" } }\n  - content:\n      - tool_use: { name: mcp__t__write_note, input: { text: \"note\" } }\n  - content:\n      - text: \"done\"\n";

/// `[mcp.t]` plus a policy that allows `echo`; `write_note` keeps the
/// outward-action default (ask), which headless runs deny.
fn env_for(server_section: &str) -> Env {
    let env = Env::new(&format!(
        "[mcp.t]\n{server_section}\n\n[policy]\nmcp.t.allow = echo\n"
    ));
    env.global_file(
        "agents/mcp-agent.yaml",
        "name: mcp-agent\nprompt: coder\ntools: [read, mcp:t/echo, mcp:t/write_note]\n",
    );
    env
}

fn stdio_section(era: &str) -> String {
    format!(
        "command = {}\nargs = --agent-tools, --era, {era}",
        harness_mcp::test_server::binary_path().display()
    )
}

/// Runs the scripted agent; blocking, so HTTP tests call it off the
/// runtime that serves the test server.
fn run_agent(env: &Env) -> std::process::Output {
    let script = env.script("s.yaml", SCRIPT);
    env.run(
        &script,
        &["--json", "--agent", "mcp-agent", "--mode", "auto", "go"],
    )
}

fn json_payload(p: &Payload) -> Value {
    match p {
        Payload::Json(v) => v.clone(),
        Payload::Text(t) => serde_json::from_str(t).unwrap_or(Value::String(t.clone())),
        other => panic!("unexpected payload {other:?}"),
    }
}

fn payload_text(p: &Payload) -> String {
    match p {
        Payload::Text(t) => t.clone(),
        other => serde_json::to_string(other).unwrap(),
    }
}

/// Checks the headless output and the run log, then renders the run log
/// as the TUI does.
fn check_run(env: &Env, o: &std::process::Output) {
    assert_eq!(o.status.code(), Some(0), "stderr: {}", stderr(o));
    // The streamed JSON and the run log agree on the MCP events.
    let streamed = events(o);
    let streamed_calls = streamed.iter().filter(|e| e["type"] == "mcp_call").count();
    assert_eq!(streamed_calls, 2, "{streamed:?}");

    let runs = env.runs();
    assert_eq!(runs.len(), 1, "{runs:?}");
    let log: Vec<RunEvent> = read_run(&runs[0]).unwrap().events;
    check_listed(&log);
    check_recorded(&log);
    check_cards(&log, env.project.as_path());
}

/// 1. The server's tools were listed and offered under their MCP names,
///    with the server's descriptions and schemas.
fn check_listed(log: &[RunEvent]) {
    let first = log
        .iter()
        .find_map(|e| match &e.kind {
            EventKind::ModelCallStarted { request, .. } => Some(json_payload(request)),
            _ => None,
        })
        .expect("a model call");
    let tools = first["tools"].as_array().expect("request tools");
    let names: Vec<&str> = tools.iter().filter_map(|t| t["name"].as_str()).collect();
    assert!(names.contains(&"mcp__t__echo"), "{names:?}");
    assert!(names.contains(&"mcp__t__write_note"), "{names:?}");
    // Only the selected tools: nothing else the server offers.
    assert!(
        !names.iter().any(|n| n.starts_with("mcp__t__")
            && !n.ends_with("echo")
            && !n.ends_with("write_note")),
        "{names:?}"
    );
    let echo = tools.iter().find(|t| t["name"] == "mcp__t__echo").unwrap();
    assert!(
        echo.to_string().contains("\"text\""),
        "echo schema from the server: {echo}"
    );
}

/// 2 and 3. Calls, policy decisions and results are in the run log.
fn check_recorded(log: &[RunEvent]) {
    let calls: Vec<(String, String)> = log
        .iter()
        .filter_map(|e| match &e.kind {
            EventKind::ToolCall { call_id, tool, .. } => Some((call_id.clone(), tool.clone())),
            _ => None,
        })
        .collect();
    let names: Vec<&str> = calls.iter().map(|c| c.1.as_str()).collect();
    assert_eq!(names, ["mcp__t__echo", "mcp__t__write_note"]);

    let mcp: Vec<(String, String, String)> = log
        .iter()
        .filter_map(|e| match &e.kind {
            EventKind::McpCall {
                call_id,
                server,
                tool,
            } => Some((call_id.clone(), server.clone(), tool.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(
        mcp,
        [
            (calls[0].0.clone(), "t".to_owned(), "echo".to_owned()),
            (calls[1].0.clone(), "t".to_owned(), "write_note".to_owned()),
        ]
    );

    let decisions: Vec<(String, String)> = log
        .iter()
        .filter_map(|e| match &e.kind {
            EventKind::PolicyDecision { tool, decision, .. } => {
                Some((tool.clone(), decision.clone()))
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        decisions,
        [
            ("mcp:t/echo".to_owned(), "allow".to_owned()),
            // An outward action asks even in auto mode; headless denies it.
            ("mcp:t/write_note".to_owned(), "ask".to_owned()),
        ]
    );

    let results: Vec<(String, bool, String)> = log
        .iter()
        .filter_map(|e| match &e.kind {
            EventKind::ToolResult {
                call_id,
                ok,
                output,
                ..
            } => Some((call_id.clone(), *ok, payload_text(output))),
            _ => None,
        })
        .collect();
    assert_eq!(results.len(), 2, "{results:?}");
    assert_eq!(
        (results[0].0.as_str(), results[0].1),
        (calls[0].0.as_str(), true)
    );
    assert!(results[0].2.contains("from the server"), "{}", results[0].2);
    assert!(
        results[0].2.contains("origin=\"mcp server\""),
        "{}",
        results[0].2
    );
    assert_eq!(
        (results[1].0.as_str(), results[1].1),
        (calls[1].0.as_str(), false)
    );
}

/// 4. The run log, folded into the chat view, shows MCP tool cards that a
///    click and the keyboard expand.
fn check_cards(log: &[RunEvent], cwd: &Path) {
    let now = Instant::now();
    let cfg = ChatConfig::default();
    let status = StatusLine::new(cwd, None, Mode::Auto);
    let mut view = ChatView::new(cfg, status, now);
    for ev in log {
        view.apply_run_event(ev, now);
    }
    let mut term = Terminal::new(TestBackend::new(120, 40)).unwrap();
    let mut draw = |view: &mut ChatView| {
        term.draw(|f| view.render(f, f.area(), now)).unwrap();
        term.backend().buffer().clone()
    };
    let buf = draw(&mut view);
    let text = buffer_text(&buf);
    assert!(
        text.contains("✔ mcp t › echo  text=from the server"),
        "{text}"
    );
    assert!(text.contains("✗ mcp t › write_note"), "{text}");
    assert!(text.contains("from the server"), "{text}");
    assert!(!text.contains("<tool_result"), "{text}");
    assert!(text.contains("more lines"), "collapsed preview: {text}");

    // Mouse: clicking the card header toggles it.
    let wall = time::OffsetDateTime::now_utc();
    let (x, y) = find_text(&buf, "mcp t › echo").unwrap();
    view.handle(&click(x, y), now, wall);
    let expanded = buffer_text(&draw(&mut view));
    assert!(expanded.contains("Enter to collapse"), "{expanded}");
    // Keyboard: Enter on the focused card collapses it again.
    view.handle(&key("enter"), now, wall);
    let collapsed = buffer_text(&draw(&mut view));
    assert!(!collapsed.contains("Enter to collapse"), "{collapsed}");
    assert!(collapsed.contains("more lines"), "{collapsed}");
}

#[test]
fn stdio_modern_server_end_to_end() {
    let env = env_for(&stdio_section("modern"));
    let o = run_agent(&env);
    check_run(&env, &o);
}

#[test]
fn stdio_legacy_server_end_to_end() {
    let env = env_for(&stdio_section("legacy"));
    let o = run_agent(&env);
    check_run(&env, &o);
}

async fn http_end_to_end(opts: Options) {
    let server = HttpServer::start(Options {
        agent_tools: true,
        ..opts
    })
    .await;
    let env = env_for(&format!("url = {}", server.url));
    let (env, o) = tokio::task::spawn_blocking(move || {
        let o = run_agent(&env);
        (env, o)
    })
    .await
    .unwrap();
    check_run(&env, &o);
    // The server saw the listing and exactly the allowed call.
    let methods: Vec<String> = server
        .requests()
        .iter()
        .filter_map(|r| r.rpc_method())
        .collect();
    assert!(methods.iter().any(|m| m == "tools/list"), "{methods:?}");
    let calls = methods.iter().filter(|m| *m == "tools/call").count();
    assert_eq!(
        calls, 1,
        "only the allowed call reaches the server: {methods:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn http_modern_server_end_to_end() {
    http_end_to_end(Options::default()).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn http_legacy_sse_server_end_to_end() {
    http_end_to_end(Options {
        era: ServerEra::Legacy,
        sse: true,
        ..Options::default()
    })
    .await;
}
