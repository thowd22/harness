//! Transport tests against a local mock HTTP server, one per failure mode.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures::StreamExt;
use harness_providers::http::{HttpClient, HttpConfig};
use harness_providers::provider::ProviderError;
use harness_providers::retry::{Attempt, RetryPolicy, RetryReason};
use harness_providers::sse::SseEvent;
use reqwest::header::HeaderMap;
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_util::sync::CancellationToken;

/// A scripted reply for one connection.
#[derive(Clone)]
enum Reply {
    /// A complete response with status, extra headers and body.
    Status(u16, Vec<(&'static str, &'static str)>, &'static str),
    /// A 200 SSE response written as these chunks with a delay before each,
    /// then optionally hanging forever instead of closing.
    Sse(Vec<(u64, &'static str)>, bool),
    /// Accept, read the request and never answer.
    Hang,
}

/// Minimal HTTP/1.1 server: the n-th connection gets the n-th reply (the
/// last reply repeats). Records each request body.
struct Mock {
    url: String,
    bodies: Arc<Mutex<Vec<String>>>,
}

impl Mock {
    async fn start(replies: Vec<Reply>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let bodies = Arc::new(Mutex::new(Vec::new()));
        let recorded = bodies.clone();
        tokio::spawn(async move {
            let mut n = 0;
            loop {
                let (sock, _) = listener.accept().await.unwrap();
                let reply = replies[n.min(replies.len() - 1)].clone();
                n += 1;
                let recorded = recorded.clone();
                tokio::spawn(async move { serve(sock, reply, recorded).await });
            }
        });
        Self { url, bodies }
    }

    fn requests(&self) -> usize {
        self.bodies.lock().unwrap().len()
    }
}

async fn serve(mut sock: TcpStream, reply: Reply, bodies: Arc<Mutex<Vec<String>>>) {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 4096];
    let header_end = loop {
        let n = sock.read(&mut tmp).await.unwrap_or(0);
        if n == 0 {
            return;
        }
        buf.extend_from_slice(&tmp[..n]);
        if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break pos + 4;
        }
    };
    let head = String::from_utf8_lossy(&buf[..header_end]).to_ascii_lowercase();
    let len: usize = head
        .lines()
        .find_map(|l| {
            l.strip_prefix("content-length:")
                .map(|v| v.trim().parse().unwrap())
        })
        .unwrap_or(0);
    while buf.len() < header_end + len {
        let n = sock.read(&mut tmp).await.unwrap_or(0);
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&tmp[..n]);
    }
    bodies
        .lock()
        .unwrap()
        .push(String::from_utf8_lossy(&buf[header_end..]).into_owned());
    match reply {
        Reply::Status(code, headers, body) => {
            let mut out = format!(
                "HTTP/1.1 {code} X\r\ncontent-length: {}\r\nconnection: close\r\n",
                body.len()
            );
            for (k, v) in headers {
                out.push_str(&format!("{k}: {v}\r\n"));
            }
            out.push_str("\r\n");
            out.push_str(body);
            let _ = sock.write_all(out.as_bytes()).await;
        }
        Reply::Sse(chunks, hang) => {
            let _ = sock
                .write_all(b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\n")
                .await;
            for (delay, chunk) in chunks {
                tokio::time::sleep(Duration::from_millis(delay)).await;
                if sock.write_all(chunk.as_bytes()).await.is_err() || sock.flush().await.is_err() {
                    return;
                }
            }
            if hang {
                tokio::time::sleep(Duration::from_secs(3600)).await;
            }
        }
        Reply::Hang => tokio::time::sleep(Duration::from_secs(3600)).await,
    }
    let _ = sock.shutdown().await;
}

fn client(url: &str) -> HttpClient {
    let mut cfg = HttpConfig::new(url);
    cfg.stall_timeout = Duration::from_millis(300);
    cfg.request_timeout = Duration::from_secs(5);
    cfg.retry = RetryPolicy {
        max_retries: 3,
        base_delay: Duration::from_millis(10),
        max_delay: Duration::from_millis(40),
        jitter: 0.5,
        max_retry_after: Duration::from_secs(5),
    };
    HttpClient::new(cfg).unwrap()
}

const OK_SSE: &str = "event: a\ndata: 1\n\nevent: b\ndata: 2\n\n";

async fn run_sse(
    c: &HttpClient,
    cancel: CancellationToken,
) -> Vec<Result<Attempt<SseEvent>, ProviderError>> {
    c.post_sse("/v1/stream", HeaderMap::new(), json!({"q": 1}), cancel)
        .collect()
        .await
}

fn data(items: &[Result<Attempt<SseEvent>, ProviderError>]) -> Vec<String> {
    items
        .iter()
        .filter_map(|i| match i {
            Ok(Attempt::Item(e)) => Some(e.data.clone()),
            _ => None,
        })
        .collect()
}

fn retries(items: &[Result<Attempt<SseEvent>, ProviderError>]) -> Vec<(RetryReason, bool)> {
    items
        .iter()
        .filter_map(|i| match i {
            Ok(Attempt::Retry(e)) => Some((e.reason, e.discard_partial)),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn sse_success_with_partial_chunks_comments_and_multiline_data() {
    let mock = Mock::start(vec![Reply::Sse(
        vec![
            (0, ": keep-alive\n\nevent: a\nda"),
            (20, "ta: line1\ndata: line2\n"),
            (20, "\n: ping\n\nevent: b\ndata: 2\n\n"),
        ],
        false,
    )])
    .await;
    let out = run_sse(&client(&mock.url), CancellationToken::new()).await;
    assert_eq!(data(&out), vec!["line1\nline2", "2"]);
    assert!(retries(&out).is_empty());
    assert_eq!(mock.bodies.lock().unwrap()[0], r#"{"q":1}"#);
}

#[tokio::test]
async fn rate_limit_honours_retry_after_then_succeeds() {
    let mock = Mock::start(vec![
        Reply::Status(
            429,
            vec![("retry-after", "1")],
            r#"{"error":{"message":"slow down"}}"#,
        ),
        Reply::Sse(vec![(0, OK_SSE)], false),
    ])
    .await;
    let start = Instant::now();
    let out = run_sse(&client(&mock.url), CancellationToken::new()).await;
    assert!(
        start.elapsed() >= Duration::from_millis(950),
        "retry-after not honoured: {:?}",
        start.elapsed()
    );
    assert_eq!(retries(&out), vec![(RetryReason::RateLimited, false)]);
    match &out[0] {
        Ok(Attempt::Retry(e)) => {
            assert_eq!(e.delay, Duration::from_secs(1));
            assert!(e.error.contains("slow down"));
        }
        o => panic!("{o:?}"),
    }
    assert_eq!(data(&out), vec!["1", "2"]);
}

#[tokio::test]
async fn server_errors_and_overload_are_retried() {
    let mock = Mock::start(vec![
        Reply::Status(500, vec![], "boom"),
        Reply::Status(
            529,
            vec![],
            r#"{"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}"#,
        ),
        Reply::Status(503, vec![("retry-after-ms", "20")], ""),
        Reply::Sse(vec![(0, OK_SSE)], false),
    ])
    .await;
    let out = run_sse(&client(&mock.url), CancellationToken::new()).await;
    assert_eq!(
        retries(&out),
        vec![
            (RetryReason::Server(500), false),
            (RetryReason::Overloaded, false),
            (RetryReason::Server(503), false)
        ]
    );
    assert_eq!(data(&out), vec!["1", "2"]);
    assert_eq!(mock.requests(), 4);
}

#[tokio::test]
async fn retry_cap_is_enforced() {
    let mock = Mock::start(vec![Reply::Status(502, vec![], "bad gateway")]).await;
    let out = run_sse(&client(&mock.url), CancellationToken::new()).await;
    assert_eq!(retries(&out).len(), 3);
    match out.last().unwrap() {
        Err(ProviderError::RetriesExhausted { attempts: 4, last }) => {
            assert!(matches!(**last, ProviderError::Http { status: 502, .. }))
        }
        o => panic!("{o:?}"),
    }
    assert_eq!(mock.requests(), 4);
}

#[tokio::test]
async fn permanent_errors_are_not_retried() {
    let mock = Mock::start(vec![Reply::Status(
        400,
        vec![],
        r#"{"error":{"message":"bad field"}}"#,
    )])
    .await;
    let out = run_sse(&client(&mock.url), CancellationToken::new()).await;
    assert_eq!(out.len(), 1);
    assert!(
        matches!(&out[0], Err(ProviderError::Http { status: 400, message, .. }) if message == "bad field")
    );
    let mock = Mock::start(vec![Reply::Status(401, vec![], "nope")]).await;
    let out = run_sse(&client(&mock.url), CancellationToken::new()).await;
    assert!(matches!(&out[..], [Err(ProviderError::Auth(_))]));
}

#[tokio::test]
async fn stall_mid_stream_aborts_and_retries_discarding_partial_output() {
    let mock = Mock::start(vec![
        Reply::Sse(vec![(0, "data: partial\n\n")], true),
        Reply::Sse(vec![(0, OK_SSE)], false),
    ])
    .await;
    let out = run_sse(&client(&mock.url), CancellationToken::new()).await;
    assert_eq!(retries(&out), vec![(RetryReason::Stalled, true)]);
    assert_eq!(data(&out), vec!["partial", "1", "2"]);
}

#[tokio::test]
async fn stall_before_headers_is_retried() {
    let mock = Mock::start(vec![Reply::Hang, Reply::Sse(vec![(0, OK_SSE)], false)]).await;
    let out = run_sse(&client(&mock.url), CancellationToken::new()).await;
    assert_eq!(retries(&out), vec![(RetryReason::Stalled, false)]);
    assert_eq!(data(&out), vec!["1", "2"]);
}

#[tokio::test]
async fn connection_refused_is_retried_as_connection_error() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    drop(listener);
    let out = run_sse(&client(&url), CancellationToken::new()).await;
    assert_eq!(retries(&out), vec![(RetryReason::Connection, false); 3]);
    assert!(matches!(
        out.last(),
        Some(Err(ProviderError::RetriesExhausted { .. }))
    ));
}

#[tokio::test]
async fn cancel_aborts_a_hanging_stream_promptly() {
    let mock = Mock::start(vec![Reply::Sse(vec![(0, "data: x\n\n")], true)]).await;
    let mut c = client(&mock.url);
    c = HttpClient::new(HttpConfig {
        stall_timeout: Duration::from_secs(60),
        ..c.config().clone()
    })
    .unwrap();
    let cancel = CancellationToken::new();
    let canceller = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(200)).await;
        canceller.cancel();
    });
    let start = Instant::now();
    let out = run_sse(&c, cancel).await;
    assert!(start.elapsed() < Duration::from_secs(5));
    assert_eq!(data(&out), vec!["x"]);
    assert_eq!(out.last(), Some(&Err(ProviderError::Cancelled)));
}

#[tokio::test]
async fn post_json_retries_and_reports_events() {
    let mock = Mock::start(vec![
        Reply::Status(429, vec![("retry-after-ms", "10")], "{}"),
        Reply::Status(
            200,
            vec![("content-type", "application/json")],
            r#"{"ok":true}"#,
        ),
    ])
    .await;
    let c = client(&mock.url);
    let mut events = Vec::new();
    let v = c
        .post_json(
            "/v1/x",
            &HeaderMap::new(),
            &json!({}),
            &CancellationToken::new(),
            |e| events.push(e.clone()),
        )
        .await
        .unwrap();
    assert_eq!(v, json!({"ok": true}));
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].delay, Duration::from_millis(10));
}

#[tokio::test]
async fn request_timeout_applies_to_post_json() {
    let mock = Mock::start(vec![Reply::Sse(vec![(0, "{")], true)]).await;
    let cfg = HttpConfig {
        request_timeout: Duration::from_millis(200),
        stall_timeout: Duration::from_secs(60),
        retry: RetryPolicy::none(),
        ..HttpConfig::new(&mock.url)
    };
    let c = HttpClient::new(cfg).unwrap();
    let e = c
        .post_json(
            "/",
            &HeaderMap::new(),
            &json!({}),
            &CancellationToken::new(),
            |_| {},
        )
        .await
        .unwrap_err();
    assert_eq!(e, ProviderError::Timeout(Duration::from_millis(200)));
}

#[tokio::test]
async fn default_headers_and_base_url_path_are_sent() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/api/", listener.local_addr().unwrap());
    let head = tokio::spawn(async move {
        let (mut sock, _) = listener.accept().await.unwrap();
        let mut buf = vec![0u8; 8192];
        let n = sock.read(&mut buf).await.unwrap();
        let _ = sock
            .write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\nconnection: close\r\n\r\n{}")
            .await;
        String::from_utf8_lossy(&buf[..n]).to_ascii_lowercase()
    });
    let mut cfg = HttpConfig::new(url);
    cfg.default_headers
        .insert("x-api-key", "secret".parse().unwrap());
    let c = HttpClient::new(cfg).unwrap();
    c.post_json(
        "/v1/messages",
        &HeaderMap::new(),
        &json!({}),
        &CancellationToken::new(),
        |_| {},
    )
    .await
    .unwrap();
    let head = head.await.unwrap();
    assert!(head.starts_with("post /api/v1/messages http/1.1"), "{head}");
    assert!(head.contains("x-api-key: secret"));
    assert!(head.contains("content-type: application/json"));
}
