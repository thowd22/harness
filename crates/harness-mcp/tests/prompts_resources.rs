//! MCP prompts and resources (TASK-146.4) against the bundled test server
//! over stdio (both eras) and HTTP, plus the status states the TUI shows:
//! starting, disabled for the session and restart.

mod common;

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use common::{alive, handle, stdio, wait_gone};
use harness_config::ini::McpServer;
use harness_mcp::prompts::PromptPart;
use harness_mcp::test_server::{HttpServer, Options, ServerEra};
use harness_mcp::{McpError, Status};

fn args(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect()
}

async fn check_prompts_and_resources(h: &harness_mcp::ServerHandle) {
    let prompts = h.list_prompts().await.unwrap();
    let names: Vec<&str> = prompts.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(names, ["review", "greet"]);
    let review = &prompts[0];
    assert_eq!(review.arguments.len(), 2);
    assert!(review.arguments[0].required);
    assert_eq!(review.arguments[0].name, "pr");

    let got = h
        .get_prompt("review", &args(&[("pr", "42"), ("focus", "tests")]))
        .await
        .unwrap();
    let parts = got.parts();
    assert_eq!(
        parts[0].1,
        PromptPart::Text("Review pull request #42, focusing on tests.".into())
    );
    assert!(matches!(&parts[1].1, PromptPart::Resource { uri, .. } if uri == "test://diff"));
    // A missing required argument is the server's error, passed on.
    let e = h.get_prompt("review", &args(&[])).await.unwrap_err();
    assert_eq!(e.rpc_error().map(|r| r.code), Some(-32602), "{e}");

    let resources = h.list_resources().await.unwrap();
    let uris: Vec<&str> = resources.iter().map(|r| r.uri.as_str()).collect();
    assert_eq!(uris, ["test://readme", "test://logo"]);
    assert_eq!(resources[0].title.as_deref(), Some("Test readme"));
    let readme = h.read_resource("test://readme").await.unwrap();
    assert!(readme.text().starts_with("# Readme"), "{}", readme.text());
    let logo = h.read_resource("test://logo").await.unwrap();
    assert!(logo.text().contains("binary content (image/png"));
    assert!(h.read_resource("test://nope").await.is_err());
}

#[tokio::test]
async fn stdio_prompts_and_resources_in_both_eras() {
    for era in ["modern", "legacy"] {
        let h = handle("t", stdio(&["--era", era]));
        check_prompts_and_resources(&h).await;
        h.shutdown().await;
    }
}

#[tokio::test]
async fn http_prompts_and_resources_send_the_name_header() {
    let srv = HttpServer::start(Options::default()).await;
    let mut cfg = McpServer::http(&srv.url);
    cfg.timeout = Duration::from_secs(20);
    let h = handle("web", cfg);
    // The test server rejects prompts/get and resources/read whose
    // Mcp-Name does not match the body.
    check_prompts_and_resources(&h).await;
    let reqs = srv.requests();
    let get = reqs
        .iter()
        .find(|r| r.rpc_method().as_deref() == Some("prompts/get"))
        .unwrap();
    assert_eq!(get.header("mcp-name"), Some("review"));
    let read = reqs
        .iter()
        .find(|r| r.rpc_method().as_deref() == Some("resources/read"))
        .unwrap();
    assert_eq!(read.header("mcp-name"), Some("test://readme"));
    h.shutdown().await;

    let legacy = HttpServer::start(Options {
        era: ServerEra::Legacy,
        ..Options::default()
    })
    .await;
    let mut cfg = McpServer::http(&legacy.url);
    cfg.timeout = Duration::from_secs(20);
    let h = handle("old", cfg);
    check_prompts_and_resources(&h).await;
    h.shutdown().await;
}

#[tokio::test]
async fn status_reports_starting_without_waiting_for_the_start() {
    let h = std::sync::Arc::new(handle("slow", stdio(&["--startup-delay-ms", "1500"])));
    let mut changes = h.watch_status();
    let starter = {
        let h = h.clone();
        tokio::spawn(async move { h.start().await })
    };
    // Wait until the start is under way, then status answers at once.
    tokio::time::timeout(Duration::from_secs(10), changes.changed())
        .await
        .unwrap()
        .unwrap();
    let asked = Instant::now();
    assert_eq!(h.status().await, Status::Starting);
    assert!(asked.elapsed() < Duration::from_secs(1));
    starter.await.unwrap().unwrap();
    assert!(matches!(h.status().await, Status::Running(_)));
    assert_eq!(h.connect_count(), 1);
    h.shutdown().await;
}

#[tokio::test]
async fn disable_for_session_stops_the_server_and_enable_restores_it() {
    let h = handle("t", stdio(&[]));
    h.start().await.unwrap();
    let pid = h.pgid().await.unwrap();
    let before = h.status_generation();
    h.disable_for_session().await;
    assert!(h.status_generation() > before, "the change is announced");
    assert_eq!(h.status().await, Status::DisabledForSession);
    assert!(wait_gone(pid).await, "the server was stopped");
    let e = h.list_tools().await.unwrap_err();
    assert!(matches!(e, McpError::DisabledForSession(_)), "{e}");
    assert!(e.to_string().contains("enable it again in /mcp"), "{e}");
    h.enable_for_session().await;
    assert_eq!(h.status().await, Status::Idle);
    assert!(!h.list_tools().await.unwrap().is_empty());
    h.shutdown().await;
}

#[tokio::test]
async fn restart_replaces_the_process_and_revives_a_given_up_server() {
    let h = handle("t", stdio(&[]));
    h.start().await.unwrap();
    let first = h.pgid().await.unwrap();
    h.restart().await.unwrap();
    let second = h.pgid().await.unwrap();
    assert_ne!(first, second);
    assert!(wait_gone(first).await);
    assert!(alive(second));
    assert!(matches!(h.status().await, Status::Running(_)));
    h.shutdown().await;
    assert!(wait_gone(second).await);

    // A server that gave up gets another chance.
    let bad = handle(
        "bad",
        McpServer::stdio("/nonexistent/harness-mcp-server", Vec::new()),
    );
    for _ in 0..4 {
        let _ = bad.ping().await;
    }
    assert!(matches!(bad.status().await, Status::GaveUp { .. }));
    let e = bad.restart().await.unwrap_err();
    assert!(matches!(e, McpError::Spawn { .. }), "{e}");
    assert!(matches!(
        bad.status().await,
        Status::Failing { failures: 1, .. }
    ));
}
