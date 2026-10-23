//! MCP over stdio against the bundled test server: lifecycle, process
//! groups, crash restarts, timeouts and malformed messages.

mod common;

use std::time::{Duration, Instant};

use common::{alive, fast_opts, handle, stdio, wait_gone};
use harness_mcp::protocol::{INVALID_REQUEST, MODERN_PROTOCOL_VERSION};
use harness_mcp::{Era, McpError, Status};
use serde_json::json;

#[tokio::test]
async fn modern_server_handshake_tools_and_ping() {
    let h = handle("t", stdio(&["--page-size", "5"]));
    let n = h.start().await.unwrap();
    assert_eq!(n.era, Era::Modern);
    assert_eq!(n.version, MODERN_PROTOCOL_VERSION);
    assert_eq!(n.server_info.name, "harness-mcp-test-server");
    assert!(n.capabilities.get("tools").is_some());
    assert_eq!(n.instructions.as_deref(), Some("Test server."));
    // Three pages of five, four and three: all twelve tools arrive.
    let tools = h.list_tools().await.unwrap();
    assert_eq!(tools.len(), 12, "{tools:?}");
    assert!(tools.iter().any(|t| t.name == "echo"));
    h.ping().await.unwrap();
    let r = h.call_tool("echo", json!({"text": "hi"})).await.unwrap();
    assert_eq!(r.text(), "hi");
    assert!(!r.is_error);
    // Notifications before the response are skipped.
    assert_eq!(
        h.call_tool("progress", json!({})).await.unwrap().text(),
        "done"
    );
    h.shutdown().await;
}

#[tokio::test]
async fn legacy_server_gets_the_initialize_handshake() {
    let h = handle("legacy", stdio(&["--era", "legacy"]));
    let n = h.start().await.unwrap();
    assert_eq!(n.era, Era::Legacy);
    assert_eq!(n.version, "2025-11-25");
    assert_eq!(n.server_info.name, "harness-mcp-test-server");
    h.ping().await.unwrap();
    assert_eq!(h.list_tools().await.unwrap().len(), 12);
    let r = h.call_tool("echo", json!({"text": "old"})).await.unwrap();
    assert_eq!(r.text(), "old");
    h.shutdown().await;
}

#[tokio::test]
async fn silent_legacy_server_falls_back_after_the_probe_timeout() {
    let mut opts = fast_opts();
    opts.probe_timeout = Duration::from_millis(300);
    let h = harness_mcp::ServerHandle::new(
        "silent",
        stdio(&["--era", "legacy", "--silent-discover"]),
        std::sync::Arc::new(opts),
    );
    let n = h.start().await.unwrap();
    assert_eq!(n.era, Era::Legacy);
    assert_eq!(
        h.call_tool("echo", json!({"text": "x"}))
            .await
            .unwrap()
            .text(),
        "x"
    );
    h.shutdown().await;
}

#[tokio::test]
async fn dual_era_server_is_used_in_the_modern_era() {
    let h = handle("dual", stdio(&["--era", "dual"]));
    assert_eq!(h.start().await.unwrap().era, Era::Modern);
    h.shutdown().await;
}

#[tokio::test]
async fn servers_start_lazily_in_their_own_session_and_group() {
    let h = handle("lazy", stdio(&[]));
    assert_eq!(h.status().await, Status::Idle);
    assert_eq!(h.pgid().await, None, "nothing runs before first use");
    h.list_tools().await.unwrap();
    assert!(matches!(h.status().await, Status::Running(_)));
    let pid = h.pgid().await.unwrap();
    let p = nix::unistd::Pid::from_raw(pid);
    assert_eq!(
        nix::unistd::getpgid(Some(p)).unwrap(),
        p,
        "leads its own group"
    );
    assert_eq!(
        nix::unistd::getsid(Some(p)).unwrap(),
        p,
        "leads its own session"
    );
    assert_ne!(nix::unistd::getsid(None).unwrap(), p);
    h.shutdown().await;
    assert!(wait_gone(pid).await);
}

#[tokio::test]
async fn crashed_server_is_restarted_with_backoff() {
    let h = handle("crashy", stdio(&[]));
    h.start().await.unwrap();
    let first = h.pgid().await.unwrap();
    let e = h.call_tool("crash", json!({})).await.unwrap_err();
    assert!(matches!(e, McpError::Exited { .. }), "{e}");
    assert!(e.to_string().contains("exit code 3"), "{e}");
    assert!(e.to_string().contains("restarted on next use"), "{e}");
    assert!(wait_gone(first).await);

    let started = Instant::now();
    let r = h.call_tool("echo", json!({"text": "back"})).await.unwrap();
    assert_eq!(r.text(), "back");
    assert!(
        started.elapsed() >= Duration::from_millis(50),
        "restart waits for the backoff"
    );
    let second = h.pgid().await.unwrap();
    assert_ne!(first, second, "a new process");

    // A second crash in a row doubles the delay.
    h.call_tool("crash", json!({})).await.unwrap_err();
    // The successful echo reset the count, so this is failure 1 again.
    let started = Instant::now();
    h.ping().await.unwrap();
    assert!(started.elapsed() >= Duration::from_millis(50));
    h.shutdown().await;
}

#[tokio::test]
async fn backoff_doubles_up_to_the_cap() {
    let o = fast_opts();
    assert_eq!(o.backoff(1), Duration::from_millis(50));
    assert_eq!(o.backoff(2), Duration::from_millis(100));
    assert_eq!(o.backoff(3), Duration::from_millis(200));
    assert_eq!(o.backoff(4), Duration::from_millis(400));
    assert_eq!(o.backoff(30), Duration::from_millis(400));
}

#[tokio::test]
async fn a_server_that_keeps_failing_is_given_up() {
    // An unknown flag makes the server print an error and exit at once.
    let h = handle("broken", stdio(&["--era", "bogus"]));
    for _ in 0..4 {
        let e = h.start().await.unwrap_err();
        assert!(e.is_connection_lost(), "{e}");
        assert!(
            e.to_string().contains("unknown era"),
            "stderr is in the error: {e}"
        );
    }
    let e = h.start().await.unwrap_err();
    assert!(matches!(e, McpError::GaveUp { failures: 4, .. }), "{e}");
    assert!(matches!(h.status().await, Status::GaveUp { .. }));
}

#[tokio::test]
async fn missing_command_is_an_actionable_error() {
    let mut cfg = stdio(&[]);
    cfg.transport = harness_config::ini::McpTransport::Stdio {
        command: "/nonexistent/mcp-server".into(),
        args: vec![],
        cwd: None,
    };
    let e = handle("nope", cfg).start().await.unwrap_err();
    let msg = e.to_string();
    assert!(
        msg.contains("could not start `/nonexistent/mcp-server`"),
        "{msg}"
    );
    assert!(msg.contains("[mcp.nope]"), "{msg}");
}

#[tokio::test]
async fn slow_requests_time_out_and_the_server_keeps_working() {
    let mut cfg = stdio(&[]);
    cfg.timeout = Duration::from_millis(300);
    let h = handle("slow", cfg);
    h.start().await.unwrap();
    let started = Instant::now();
    let e = h
        .call_tool("sleep", json!({"ms": 30_000}))
        .await
        .unwrap_err();
    assert!(matches!(e, McpError::Timeout { .. }), "{e}");
    assert!(
        e.to_string().contains("raise `timeout` in [mcp.slow]"),
        "{e}"
    );
    assert!(started.elapsed() < Duration::from_secs(10));
    // The same process still answers other requests.
    let pid = h.pgid().await.unwrap();
    assert_eq!(
        h.call_tool("echo", json!({"text": "ok"}))
            .await
            .unwrap()
            .text(),
        "ok"
    );
    assert_eq!(h.pgid().await, Some(pid));
    h.shutdown().await;
}

#[tokio::test]
async fn hung_startup_times_out_and_leaves_nothing_running() {
    let mut cfg = stdio(&["--startup-delay-ms", "60000"]);
    cfg.startup_timeout = Duration::from_millis(500);
    let h = handle("hung", cfg);
    let e = h.start().await.unwrap_err();
    assert!(matches!(e, McpError::StartupTimeout { .. }), "{e}");
    assert!(e.to_string().contains("startup_timeout"), "{e}");
    assert_eq!(h.pgid().await, None);
}

#[tokio::test]
async fn malformed_messages_are_skipped_or_reported() {
    let h = handle("garbage", stdio(&[]));
    // Garbage lines and invalid messages before the answer are ignored.
    let r = h.call_tool("malformed", json!({})).await.unwrap();
    assert_eq!(r.text(), "survived");
    // A response with neither result nor error fails just that request.
    let e = h.call_tool("bad_response", json!({})).await.unwrap_err();
    assert_eq!(e.rpc_error().map(|r| r.code), Some(INVALID_REQUEST), "{e}");
    // The connection is still fine.
    assert_eq!(
        h.call_tool("echo", json!({"text": "fine"}))
            .await
            .unwrap()
            .text(),
        "fine"
    );
    // An input request is reported, not misread as a result.
    let e = h.call_tool("input_required", json!({})).await.unwrap_err();
    assert!(e.to_string().contains("asked for more input"), "{e}");
    h.shutdown().await;
}

#[tokio::test]
async fn shutdown_kills_the_whole_group() {
    let h = handle("group", stdio(&[]));
    let r = h.call_tool("spawn_grandchild", json!({})).await.unwrap();
    let grandchild: i32 = r.text().parse().unwrap();
    let server = h.pgid().await.unwrap();
    assert!(alive(grandchild));
    h.shutdown().await;
    assert!(wait_gone(server).await, "server exited");
    assert!(
        wait_gone(grandchild).await,
        "grandchild killed with the group"
    );
}

#[tokio::test]
async fn dropping_a_handle_leaves_no_orphans() {
    let h = handle("dropped", stdio(&[]));
    let r = h.call_tool("spawn_grandchild", json!({})).await.unwrap();
    let grandchild: i32 = r.text().parse().unwrap();
    let server = h.pgid().await.unwrap();
    drop(h);
    assert!(wait_gone(server).await, "server killed on drop");
    assert!(wait_gone(grandchild).await, "grandchild killed on drop");
}

#[tokio::test]
async fn stubborn_server_is_escalated_to_sigkill() {
    let mut opts = fast_opts();
    opts.shutdown_grace = Duration::from_millis(300);
    let h = harness_mcp::ServerHandle::new(
        "stubborn",
        stdio(&["--ignore-stdin-eof", "--ignore-sigterm"]),
        std::sync::Arc::new(opts),
    );
    h.start().await.unwrap();
    let pid = h.pgid().await.unwrap();
    let started = Instant::now();
    h.shutdown().await;
    assert!(wait_gone(pid).await);
    assert!(started.elapsed() < Duration::from_secs(10));
}

#[tokio::test]
async fn disabled_servers_never_start() {
    let mut cfg = stdio(&[]);
    cfg.enabled = false;
    let h = handle("off", cfg);
    let e = h.list_tools().await.unwrap_err();
    assert!(
        e.to_string().contains("set `enabled = true` in [mcp.off]"),
        "{e}"
    );
    assert_eq!(h.status().await, Status::Disabled);
}
