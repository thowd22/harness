//! MCP over streamable HTTP against the bundled in-process test server.

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use common::{cmd, handle, inline};
use harness_config::ini::McpServer;
use harness_mcp::protocol::{INVALID_REQUEST, MODERN_PROTOCOL_VERSION};
use harness_mcp::test_server::{HttpServer, Options, ServerEra};
use harness_mcp::{Era, McpError};
use serde_json::json;

fn opts(era: ServerEra, sse: bool) -> Options {
    Options {
        era,
        sse,
        ..Options::default()
    }
}

fn http(url: &str) -> McpServer {
    let mut s = McpServer::http(url);
    s.timeout = Duration::from_secs(20);
    s
}

#[tokio::test]
async fn modern_json_server_with_request_headers() {
    let srv = HttpServer::start(opts(ServerEra::Modern, false)).await;
    let h = handle("web", http(&srv.url));
    let n = h.start().await.unwrap();
    assert_eq!(n.era, Era::Modern);
    assert_eq!(n.version, MODERN_PROTOCOL_VERSION);
    let tools = h.list_tools().await.unwrap();
    // `bad_header` has an invalid x-mcp-header and is dropped over HTTP.
    assert_eq!(
        tools.len(),
        11,
        "{:?}",
        tools.iter().map(|t| &t.name).collect::<Vec<_>>()
    );
    assert!(!tools.iter().any(|t| t.name == "bad_header"));
    assert_eq!(
        h.call_tool("echo", json!({"text": "hi"}))
            .await
            .unwrap()
            .text(),
        "hi"
    );
    // The server rejects a call whose Mcp-Param-Region does not match.
    let r = h
        .call_tool("region", json!({"region": "Zürich west", "query": "q"}))
        .await
        .unwrap();
    assert_eq!(r.text(), "region=Zürich west");
    h.ping().await.unwrap();

    let reqs = srv.requests();
    let call = reqs
        .iter()
        .find(|r| r.body.contains("\"region\""))
        .expect("region call recorded");
    assert_eq!(
        call.header("mcp-protocol-version"),
        Some(MODERN_PROTOCOL_VERSION)
    );
    assert_eq!(call.header("mcp-method"), Some("tools/call"));
    assert_eq!(call.header("mcp-name"), Some("region"));
    assert!(
        call.header("mcp-param-region")
            .unwrap()
            .starts_with("=?base64?")
    );
    assert!(call.header("accept").unwrap().contains("text/event-stream"));
    assert!(
        reqs.iter().all(|r| r.header("mcp-session-id").is_none()),
        "no sessions in the modern era"
    );
    h.shutdown().await;
    assert!(srv.requests().iter().all(|r| r.method == "POST"));
}

#[tokio::test]
async fn modern_sse_server() {
    let srv = HttpServer::start(opts(ServerEra::Modern, true)).await;
    let h = handle("sse", http(&srv.url));
    assert_eq!(h.start().await.unwrap().era, Era::Modern);
    // A progress notification precedes the response on the stream.
    assert_eq!(
        h.call_tool("progress", json!({})).await.unwrap().text(),
        "done"
    );
    assert_eq!(
        h.call_tool("echo", json!({"text": "s"}))
            .await
            .unwrap()
            .text(),
        "s"
    );
    assert_eq!(h.list_tools().await.unwrap().len(), 11);
}

#[tokio::test]
async fn legacy_server_session_lifecycle() {
    let srv = HttpServer::start(opts(ServerEra::Legacy, false)).await;
    let h = handle("old", http(&srv.url));
    let n = h.start().await.unwrap();
    assert_eq!(n.era, Era::Legacy);
    assert_eq!(n.version, "2025-11-25");
    assert_eq!(srv.sessions(), 1);
    h.ping().await.unwrap();
    assert_eq!(
        h.call_tool("echo", json!({"text": "x"}))
            .await
            .unwrap()
            .text(),
        "x"
    );
    let reqs = srv.requests();
    let init = reqs
        .iter()
        .position(|r| r.rpc_method().as_deref() == Some("initialize"))
        .unwrap();
    let initialized = reqs
        .iter()
        .position(|r| r.rpc_method().as_deref() == Some("notifications/initialized"))
        .unwrap();
    assert!(init < initialized);
    let ping = reqs
        .iter()
        .find(|r| r.rpc_method().as_deref() == Some("ping"))
        .unwrap();
    assert_eq!(ping.header("mcp-session-id"), Some("s1"));
    assert_eq!(ping.header("mcp-protocol-version"), Some("2025-11-25"));
    // Shutdown ends the session with DELETE.
    h.shutdown().await;
    assert_eq!(srv.sessions(), 0);
    assert!(srv.requests().iter().any(|r| r.method == "DELETE"));
}

#[tokio::test]
async fn legacy_sse_server() {
    let srv = HttpServer::start(opts(ServerEra::Legacy, true)).await;
    let h = handle("old-sse", http(&srv.url));
    assert_eq!(h.start().await.unwrap().era, Era::Legacy);
    assert_eq!(
        h.call_tool("echo", json!({"text": "y"}))
            .await
            .unwrap()
            .text(),
        "y"
    );
    h.shutdown().await;
}

#[tokio::test]
async fn expired_legacy_session_is_re_established() {
    let srv = HttpServer::start(opts(ServerEra::Legacy, false)).await;
    let h = handle("expiry", http(&srv.url));
    h.start().await.unwrap();
    srv.expire_sessions();
    assert_eq!(
        h.call_tool("echo", json!({"text": "z"}))
            .await
            .unwrap()
            .text(),
        "z"
    );
    let inits = srv
        .requests()
        .iter()
        .filter(|r| r.rpc_method().as_deref() == Some("initialize"))
        .count();
    assert_eq!(inits, 2);
}

#[tokio::test]
async fn configured_headers_authenticate() {
    let srv = HttpServer::start_with(
        opts(ServerEra::Modern, false),
        Some(("Authorization", "Bearer tok-123456")),
    )
    .await;
    let mut cfg = http(&srv.url);
    cfg.headers
        .insert("Authorization".into(), cmd("printf 'Bearer tok-123456'"));
    let h = handle("auth", cfg);
    assert_eq!(
        h.call_tool("echo", json!({"text": "in"}))
            .await
            .unwrap()
            .text(),
        "in"
    );

    let mut bad = http(&srv.url);
    bad.headers
        .insert("Authorization".into(), inline("Bearer wrong-token-999"));
    let e = handle("auth", bad).start().await.unwrap_err();
    let msg = e.to_string();
    assert!(msg.contains("401"), "{msg}");
    assert!(msg.contains("header.* credentials"), "{msg}");
    assert!(!msg.contains("wrong-token-999"), "{msg}");
    assert!(matches!(
        e,
        McpError::Http {
            status: Some(401),
            ..
        }
    ));
}

/// A listener that answers every request with `status` and `Location:
/// location`, counting the requests it saw.
async fn redirector(status: u16, location: String) -> (String, Arc<AtomicUsize>) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/mcp", listener.local_addr().unwrap());
    let seen = Arc::new(AtomicUsize::new(0));
    let count = seen.clone();
    tokio::spawn(async move {
        while let Ok((mut sock, _)) = listener.accept().await {
            count.fetch_add(1, Ordering::SeqCst);
            let location = location.clone();
            tokio::spawn(async move {
                let mut buf = [0u8; 4096];
                let _ = sock.read(&mut buf).await;
                let reply = format!(
                    "HTTP/1.1 {status} Redirect\r\nLocation: {location}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                );
                let _ = sock.write_all(reply.as_bytes()).await;
                let _ = sock.shutdown().await;
            });
        }
    });
    (url, seen)
}

#[tokio::test]
async fn redirects_are_not_followed_so_credentials_stay_put() {
    // The target would accept the request; it must never see it (nor the
    // custom credential header, which reqwest would forward cross-origin).
    let target = HttpServer::start(opts(ServerEra::Modern, false)).await;
    for status in [302, 307, 308] {
        let (url, seen) = redirector(status, target.url.clone()).await;
        let mut cfg = http(&url);
        cfg.headers
            .insert("X-API-Key".into(), inline("custom-cred-424242"));
        let e = handle("redir", cfg).start().await.unwrap_err();
        let msg = e.to_string();
        assert!(seen.load(Ordering::SeqCst) >= 1);
        assert!(
            matches!(e, McpError::Http { status: Some(s), .. } if s == status),
            "{msg}"
        );
        assert!(msg.contains("redirected to"), "{msg}");
        assert!(msg.contains(&target.url), "{msg}");
        assert!(msg.contains("[mcp.redir]"), "{msg}");
        assert!(!msg.contains("custom-cred-424242"), "{msg}");
    }
    assert!(
        target.requests().is_empty(),
        "the redirect target received {:?}",
        target.requests()
    );
}

#[tokio::test]
async fn invalid_header_value_is_reported() {
    let mut cfg = http("http://127.0.0.1:9/mcp");
    cfg.headers.insert("X-Key".into(), inline("bad\nvalue"));
    let e = handle("hdr", cfg).start().await.unwrap_err();
    assert!(e.to_string().contains("header.X-Key"), "{e}");
    assert!(!e.to_string().contains("bad\nvalue"));
}

#[tokio::test]
async fn slow_requests_time_out() {
    let srv = HttpServer::start(opts(ServerEra::Modern, false)).await;
    let mut cfg = http(&srv.url);
    cfg.timeout = Duration::from_millis(300);
    let h = handle("slow", cfg);
    h.start().await.unwrap();
    let started = Instant::now();
    let e = h
        .call_tool("sleep", json!({"ms": 30_000}))
        .await
        .unwrap_err();
    assert!(matches!(e, McpError::Timeout { .. }), "{e}");
    assert!(started.elapsed() < Duration::from_secs(10));
    assert_eq!(
        h.call_tool("echo", json!({"text": "ok"}))
            .await
            .unwrap()
            .text(),
        "ok"
    );
}

#[tokio::test]
async fn dropped_connections_and_malformed_responses() {
    for sse in [false, true] {
        let srv = HttpServer::start(opts(ServerEra::Modern, sse)).await;
        let h = handle("bad", http(&srv.url));
        let e = h.call_tool("crash", json!({})).await.unwrap_err();
        assert!(matches!(e, McpError::Http { .. }), "{e}");
        let e = h.call_tool("bad_response", json!({})).await.unwrap_err();
        assert_eq!(e.rpc_error().map(|r| r.code), Some(INVALID_REQUEST), "{e}");
        let r = h.call_tool("malformed", json!({})).await.unwrap();
        assert_eq!(r.text(), "survived", "sse={sse}");
        let e = h.call_tool("input_required", json!({})).await.unwrap_err();
        assert!(e.to_string().contains("asked for more input"), "{e}");
        assert_eq!(
            h.call_tool("echo", json!({"text": "fine"}))
                .await
                .unwrap()
                .text(),
            "fine"
        );
    }
}

#[tokio::test]
async fn unreachable_server_is_an_actionable_error() {
    // Bind and drop a listener to find a closed port.
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let e = handle("gone", http(&format!("http://127.0.0.1:{port}/mcp")))
        .start()
        .await
        .unwrap_err();
    assert!(e.to_string().contains("cannot reach"), "{e}");
}

#[tokio::test]
async fn modern_only_server_rejects_legacy_clients_but_we_stay_modern() {
    // A dual-era server is used in the modern era over HTTP too.
    let srv = HttpServer::start(opts(ServerEra::Dual, false)).await;
    let h = handle("dual", http(&srv.url));
    assert_eq!(h.start().await.unwrap().era, Era::Modern);
    assert!(
        !srv.requests()
            .iter()
            .any(|r| r.rpc_method().as_deref() == Some("initialize")),
        "no fallback for a modern server"
    );
}
