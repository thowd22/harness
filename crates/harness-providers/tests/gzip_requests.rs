//! Request-body gzip for the Anthropic provider (TASK-127), against local
//! mock servers that capture the exact bytes on the wire.
//!
//! Local servers are never "the public API", so these tests use
//! [`GzipRequests::Always`]; which base URLs `auto` compresses for is
//! covered by `gzip_eligible` tests and `auto_mode_never_compresses_*`.

use std::io::Read;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use harness_providers::anthropic::{
    AnthropicConfig, AnthropicProvider, COUNT_TOKENS_PATH, GzipRequests, MESSAGES_PATH,
};
use harness_providers::http::{BodyStats, GzipLatch};
use harness_providers::model::{ChatRequest, Message};
use harness_providers::openai::notice::request_body;
use harness_providers::provider::{Provider, ProviderError, collect};
use harness_providers::retry::RetryPolicy;
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_util::sync::CancellationToken;

/// One request as it arrived: lower-cased head and the body bytes exactly
/// as sent.
#[derive(Clone, Debug)]
struct Captured {
    head: String,
    body: Vec<u8>,
}

impl Captured {
    fn path(&self) -> &str {
        self.head.split_whitespace().nth(1).unwrap_or("")
    }

    fn gzipped(&self) -> bool {
        self.head.contains("\r\ncontent-encoding: gzip\r\n")
    }

    /// The body after undoing `Content-Encoding`.
    fn decoded(&self) -> Vec<u8> {
        if self.gzipped() {
            let mut out = Vec::new();
            flate2::read::GzDecoder::new(&self.body[..])
                .read_to_end(&mut out)
                .unwrap();
            out
        } else {
            self.body.clone()
        }
    }
}

#[derive(Clone)]
enum Reply {
    /// 200 with an SSE body.
    Sse(String),
    /// 200 with a JSON body.
    Json(String),
    /// An error status with extra headers and a body.
    Status(u16, Vec<(&'static str, &'static str)>, String),
    /// An error status whose chunked body sends `body` and then never
    /// ends; `closed` is set once the client closes the connection.
    Hanging(u16, String),
}

/// Scripted mock: the n-th request gets the n-th reply (the last repeats).
/// Every reply closes its connection except [`Reply::Hanging`].
struct Mock {
    url: String,
    captured: Arc<Mutex<Vec<Captured>>>,
    hanging_closed: Arc<AtomicBool>,
}

impl Mock {
    async fn start(replies: Vec<Reply>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let captured = Arc::new(Mutex::new(Vec::new()));
        let hanging_closed = Arc::new(AtomicBool::new(false));
        let (cap, closed) = (captured.clone(), hanging_closed.clone());
        tokio::spawn(async move {
            let mut n = 0;
            loop {
                let (sock, _) = listener.accept().await.unwrap();
                let reply = replies[n.min(replies.len() - 1)].clone();
                n += 1;
                let (cap, closed) = (cap.clone(), closed.clone());
                tokio::spawn(async move { serve(sock, reply, cap, closed).await });
            }
        });
        Self {
            url,
            captured,
            hanging_closed,
        }
    }

    fn captured(&self) -> Vec<Captured> {
        self.captured.lock().unwrap().clone()
    }
}

/// Reads one request (head + `content-length` body).
async fn read_request(sock: &mut TcpStream) -> Option<Captured> {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 16384];
    let header_end = loop {
        let n = sock.read(&mut tmp).await.unwrap_or(0);
        if n == 0 {
            return None;
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
    Some(Captured {
        head,
        body: buf[header_end..].to_vec(),
    })
}

async fn serve(
    mut sock: TcpStream,
    reply: Reply,
    cap: Arc<Mutex<Vec<Captured>>>,
    closed: Arc<AtomicBool>,
) {
    let Some(req) = read_request(&mut sock).await else {
        return;
    };
    cap.lock().unwrap().push(req);
    let out = match reply {
        Reply::Sse(body) => format!(
            "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\n{body}"
        ),
        Reply::Json(body) => format!(
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
            body.len()
        ),
        Reply::Status(code, headers, body) => {
            let extra: String = headers
                .iter()
                .map(|(k, v)| format!("{k}: {v}\r\n"))
                .collect();
            format!(
                "HTTP/1.1 {code} X\r\ncontent-type: application/json\r\n{extra}content-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            )
        }
        Reply::Hanging(code, body) => {
            let head = format!(
                "HTTP/1.1 {code} X\r\ncontent-type: application/json\r\ntransfer-encoding: chunked\r\n\r\n{:x}\r\n{body}\r\n",
                body.len()
            );
            let _ = sock.write_all(head.as_bytes()).await;
            // Never finish the body; wait for the client to hang up.
            let mut tmp = [0u8; 1024];
            loop {
                match tokio::time::timeout(Duration::from_secs(30), sock.read(&mut tmp)).await {
                    Ok(Ok(0)) | Ok(Err(_)) => {
                        closed.store(true, Ordering::SeqCst);
                        return;
                    }
                    Ok(Ok(_)) => {}
                    Err(_) => return,
                }
            }
        }
    };
    let _ = sock.write_all(out.as_bytes()).await;
}

fn sse_turn(text: &str) -> String {
    [
        json!({"type": "message_start", "message": {"id": "msg_1", "model": "claude-opus-5-5", "usage": {"input_tokens": 10, "output_tokens": 1}}}),
        json!({"type": "content_block_start", "index": 0, "content_block": {"type": "text", "text": ""}}),
        json!({"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": text}}),
        json!({"type": "content_block_stop", "index": 0}),
        json!({"type": "message_delta", "delta": {"stop_reason": "end_turn"}, "usage": {"output_tokens": 3}}),
        json!({"type": "message_stop"}),
    ]
    .iter()
    .map(|e| format!("event: {}\ndata: {}\n\n", e["type"].as_str().unwrap(), e))
    .collect()
}

/// A provider for `url` in `mode` with its own latch (so tests do not
/// share the process-wide one) and fast retries.
fn provider(url: &str, mode: GzipRequests) -> (AnthropicProvider, GzipLatch) {
    let latch = GzipLatch::new();
    let mut cfg = AnthropicConfig::new().with_base_url(url);
    cfg.gzip = mode;
    cfg.gzip_latch = latch.clone();
    cfg.http.retry = RetryPolicy {
        base_delay: Duration::from_millis(1),
        max_delay: Duration::from_millis(5),
        ..RetryPolicy::default()
    };
    (AnthropicProvider::new("sk-ant-test", cfg).unwrap(), latch)
}

/// A realistic coding conversation of roughly `bytes` serialized bytes:
/// file contents, tool output and chat, which compresses like real turns.
fn big_request(bytes: usize) -> ChatRequest {
    let mut req = ChatRequest::new("claude-opus-5-5");
    req.system = Some("You are a coding agent. Use the tools to read and edit files.".into());
    let mut i = 0usize;
    let mut size = 0;
    while size < bytes {
        let file: String = (0..40)
            .map(|l| {
                format!(
                    "    let value_{i}_{l} = compute(&input[{}..{}], {:#x}); // step {l}\n",
                    i * 7 + l,
                    i * 11 + l * 3,
                    (i * 2654435761 + l) & 0xffff
                )
            })
            .collect();
        size += file.len() + 64;
        req.messages.push(Message::user(format!(
            "Here is src/module_{i}.rs:\n```rust\nfn f{i}() {{\n{file}}}\n```\nWhat does it do?"
        )));
        req.messages.push(Message::assistant(format!(
            "Module {i} computes {} values over sliding windows of the input.",
            40
        )));
        i += 1;
    }
    req.messages
        .push(Message::user("Now refactor all of them."));
    req
}

fn expected_bytes(p: &AnthropicProvider, req: &ChatRequest) -> Vec<u8> {
    serde_json::to_vec(&p.build_body(req).unwrap()).unwrap()
}

#[tokio::test]
async fn large_messages_body_is_gzipped_and_decompresses_byte_identical() {
    let mock = Mock::start(vec![Reply::Sse(sse_turn("ok"))]).await;
    let (p, latch) = provider(&mock.url, GzipRequests::Always);
    let req = big_request(200 * 1024);
    let resp = collect(
        p.stream(req.clone(), CancellationToken::new())
            .await
            .unwrap(),
    )
    .await
    .unwrap();
    let cap = mock.captured();
    assert_eq!(cap.len(), 1);
    let c = &cap[0];
    assert_eq!(c.path(), MESSAGES_PATH);
    assert!(c.gzipped(), "{}", c.head);
    assert!(c.head.contains("content-type: application/json"));
    // A single gzip member with mtime 0.
    assert_eq!(&c.body[..3], &[0x1f, 0x8b, 8]);
    assert_eq!(&c.body[4..8], &[0, 0, 0, 0]);
    let mut single = flate2::bufread::GzDecoder::new(&c.body[..]);
    std::io::copy(&mut single, &mut std::io::sink()).unwrap();
    assert!(single.into_inner().is_empty(), "exactly one member");
    // Byte-identical to the uncompressed body, so snapshots and cache
    // keys are unchanged.
    let raw = expected_bytes(&p, &req);
    assert_eq!(c.decoded(), raw);
    assert!(
        c.body.len() * 4 < raw.len(),
        "{} vs {}",
        c.body.len(),
        raw.len()
    );
    // The response records both sizes.
    assert_eq!(
        request_body(&resp.message),
        Some(BodyStats {
            raw_bytes: raw.len() as u64,
            wire_bytes: c.body.len() as u64,
            encoding: Some("gzip".into()),
        })
    );
    assert_eq!(resp.message.text(), "ok");
    assert!(!latch.is_tripped());
}

#[tokio::test]
async fn uncompressed_wire_bytes_equal_the_decompressed_gzip_bytes() {
    let req = big_request(50 * 1024);
    let mut bodies = Vec::new();
    for mode in [GzipRequests::Off, GzipRequests::Always] {
        let mock = Mock::start(vec![Reply::Sse(sse_turn("ok"))]).await;
        let (p, _) = provider(&mock.url, mode);
        let resp = collect(
            p.stream(req.clone(), CancellationToken::new())
                .await
                .unwrap(),
        )
        .await
        .unwrap();
        let c = mock.captured().remove(0);
        assert_eq!(c.gzipped(), mode == GzipRequests::Always);
        let stats = request_body(&resp.message).unwrap();
        assert_eq!(stats.wire_bytes, c.body.len() as u64);
        bodies.push(c.decoded());
    }
    assert_eq!(bodies[0], bodies[1]);
}

#[tokio::test]
async fn bodies_under_4_kib_are_sent_raw() {
    let mock = Mock::start(vec![Reply::Sse(sse_turn("ok"))]).await;
    let (p, _) = provider(&mock.url, GzipRequests::Always);
    let mut req = ChatRequest::new("claude-opus-5-5");
    req.messages.push(Message::user("hi"));
    let resp = collect(
        p.stream(req.clone(), CancellationToken::new())
            .await
            .unwrap(),
    )
    .await
    .unwrap();
    let c = mock.captured().remove(0);
    assert!(!c.gzipped());
    assert_eq!(c.body, expected_bytes(&p, &req));
    let stats = request_body(&resp.message).unwrap();
    assert_eq!(stats, BodyStats::raw(c.body.len()));
}

#[tokio::test]
async fn count_tokens_body_is_gzipped() {
    let mock = Mock::start(vec![Reply::Json(r#"{"input_tokens":51234}"#.into())]).await;
    let (p, _) = provider(&mock.url, GzipRequests::Always);
    let req = big_request(20 * 1024);
    let n = p
        .count_tokens(&req, &CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(n, 51234);
    let c = mock.captured().remove(0);
    assert_eq!(c.path(), COUNT_TOKENS_PATH);
    assert!(c.gzipped());
    let body: Value = serde_json::from_slice(&c.decoded()).unwrap();
    assert_eq!(body, p.count_tokens_body(&req).unwrap());
    assert!(body.get("max_tokens").is_none() && body.get("stream").is_none());
    assert!(body["messages"].is_array());
}

#[tokio::test]
async fn auto_mode_never_compresses_for_a_custom_base_url() {
    let mock = Mock::start(vec![Reply::Sse(sse_turn("ok"))]).await;
    let (p, _) = provider(&mock.url, GzipRequests::Auto);
    assert!(p.http_config().compression.is_none());
    let req = big_request(64 * 1024);
    let resp = collect(p.stream(req, CancellationToken::new()).await.unwrap())
        .await
        .unwrap();
    assert!(!mock.captured()[0].gzipped());
    assert_eq!(request_body(&resp.message).unwrap().encoding, None);
}

#[tokio::test]
async fn off_mode_never_compresses() {
    let (p, _) = provider("http://127.0.0.1:1", GzipRequests::Off);
    assert!(p.http_config().compression.is_none());
    let mut cfg = AnthropicConfig::new();
    cfg.gzip = GzipRequests::Off;
    let p = AnthropicProvider::new("k", cfg).unwrap();
    assert!(p.http_config().compression.is_none());
}

/// Runs one streamed call and returns the result.
async fn call(p: &AnthropicProvider, req: &ChatRequest) -> Result<String, ProviderError> {
    let stream = p.stream(req.clone(), CancellationToken::new()).await?;
    collect(stream).await.map(|r| r.message.text())
}

/// Asserts that request 0 was gzipped and the remaining ones raw, all with
/// the same decoded body.
fn assert_fell_back(cap: &[Captured], expected: &[u8]) {
    assert!(cap[0].gzipped());
    for c in &cap[1..] {
        assert!(!c.gzipped(), "only the first request is compressed");
        assert_eq!(c.body, expected);
    }
    assert_eq!(cap[0].decoded(), expected);
}

async fn fallback_case(rejection: Reply) {
    let mock = Mock::start(vec![
        rejection,
        Reply::Sse(sse_turn("raw ok")),
        Reply::Sse(sse_turn("still raw")),
    ])
    .await;
    let (p, latch) = provider(&mock.url, GzipRequests::Always);
    let req = big_request(16 * 1024);
    assert_eq!(call(&p, &req).await.unwrap(), "raw ok");
    assert!(latch.is_tripped());
    // Gzip stays off for later calls.
    assert_eq!(call(&p, &req).await.unwrap(), "still raw");
    let cap = mock.captured();
    assert_eq!(cap.len(), 3);
    assert_fell_back(&cap, &expected_bytes(&p, &req));
}

#[tokio::test]
async fn unsupported_media_type_falls_back_to_raw_once_and_disables_gzip() {
    fallback_case(Reply::Status(
        415,
        vec![("request-id", "req_1")],
        r#"{"type":"error","error":{"type":"invalid_request_error","message":"Unsupported Content-Encoding"}}"#.into(),
    ))
    .await;
}

#[tokio::test]
async fn encoding_unsupported_400_falls_back() {
    fallback_case(Reply::Status(
        400,
        vec![("request-id", "req_1")],
        r#"{"type":"error","error":{"type":"request_body_encoding_unsupported","message":"zstd is not supported"}}"#.into(),
    ))
    .await;
}

#[tokio::test]
async fn invalid_json_400_falls_back() {
    fallback_case(Reply::Status(
        400,
        vec![("request-id", "req_1")],
        r#"{"type":"error","error":{"type":"invalid_request_error","message":"There was an error parsing the body as JSON"}}"#.into(),
    ))
    .await;
}

#[tokio::test]
async fn non_anthropic_hop_400_and_403_fall_back() {
    fallback_case(Reply::Status(
        400,
        vec![],
        "<html>400 Bad Request</html>".into(),
    ))
    .await;
    fallback_case(Reply::Status(
        403,
        vec![],
        "Forbidden by gateway policy".into(),
    ))
    .await;
    fallback_case(Reply::Status(415, vec![], String::new())).await;
}

#[tokio::test]
async fn ordinary_anthropic_errors_do_not_trigger_the_fallback() {
    let body = r#"{"type":"error","error":{"type":"invalid_request_error","message":"max_tokens: too large"}}"#;
    let mock = Mock::start(vec![Reply::Status(
        400,
        vec![("request-id", "req_1")],
        body.into(),
    )])
    .await;
    let (p, latch) = provider(&mock.url, GzipRequests::Always);
    let err = call(&p, &big_request(16 * 1024)).await.unwrap_err();
    assert!(err.to_string().contains("max_tokens: too large"), "{err}");
    assert_eq!(mock.captured().len(), 1);
    assert!(!latch.is_tripped());

    let mock = Mock::start(vec![Reply::Status(
        403,
        vec![("request-id", "req_2")],
        r#"{"type":"error","error":{"type":"permission_error","message":"no access"}}"#.into(),
    )])
    .await;
    let (p, latch) = provider(&mock.url, GzipRequests::Always);
    assert!(matches!(
        call(&p, &big_request(16 * 1024)).await,
        Err(ProviderError::Auth(_))
    ));
    assert_eq!(mock.captured().len(), 1);
    assert!(!latch.is_tripped());
}

#[tokio::test]
async fn a_second_rejection_is_returned_without_further_retries() {
    let mock = Mock::start(vec![Reply::Status(415, vec![], "unsupported".into())]).await;
    let (p, latch) = provider(&mock.url, GzipRequests::Always);
    let err = call(&p, &big_request(16 * 1024)).await.unwrap_err();
    assert!(
        matches!(err, ProviderError::Http { status: 415, .. }),
        "{err:?}"
    );
    let cap = mock.captured();
    assert_eq!(cap.len(), 2, "one gzip attempt and one raw resend");
    assert!(cap[0].gzipped() && !cap[1].gzipped());
    assert!(latch.is_tripped());
}

#[tokio::test]
async fn a_rejection_while_streaming_closes_the_response_before_resending() {
    let mock = Mock::start(vec![
        Reply::Hanging(400, "<html>gateway: bad request</html>".into()),
        Reply::Sse(sse_turn("raw ok")),
    ])
    .await;
    let (p, latch) = provider(&mock.url, GzipRequests::Always);
    let start = Instant::now();
    // The rejected body never ends: without closing it, this would hang
    // until the stall timeout instead of resending.
    assert!(p.http_config().stall_timeout >= Duration::from_secs(30));
    let req = big_request(16 * 1024);
    assert_eq!(call(&p, &req).await.unwrap(), "raw ok");
    assert!(latch.is_tripped());
    for _ in 0..200 {
        if mock.hanging_closed.load(Ordering::SeqCst) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(
        mock.hanging_closed.load(Ordering::SeqCst),
        "the rejected connection was closed"
    );
    assert!(start.elapsed() < Duration::from_secs(20));
    let cap = mock.captured();
    assert_eq!(cap.len(), 2);
    assert_fell_back(&cap, &expected_bytes(&p, &req));
}

/// A server that reads request bodies at `mbit` Mbit/s, then answers.
async fn throttled_server(mbit: f64) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        loop {
            let (mut sock, _) = listener.accept().await.unwrap();
            tokio::spawn(async move {
                let start = tokio::time::Instant::now();
                let mut buf = Vec::new();
                let mut tmp = [0u8; 8192];
                let mut want = usize::MAX;
                while buf.len() < want {
                    let n = sock.read(&mut tmp).await.unwrap_or(0);
                    if n == 0 {
                        return;
                    }
                    buf.extend_from_slice(&tmp[..n]);
                    let at = start + Duration::from_secs_f64(buf.len() as f64 * 8.0 / (mbit * 1e6));
                    tokio::time::sleep_until(at).await;
                    if want == usize::MAX
                        && let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n")
                    {
                        let head = String::from_utf8_lossy(&buf[..pos]).to_ascii_lowercase();
                        let len: usize = head
                            .lines()
                            .find_map(|l| {
                                l.strip_prefix("content-length:")
                                    .map(|v| v.trim().parse().unwrap())
                            })
                            .unwrap_or(0);
                        want = pos + 4 + len;
                    }
                }
                let body = sse_turn("ok");
                let out = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\n{body}"
                );
                let _ = sock.write_all(out.as_bytes()).await;
            });
        }
    });
    url
}

/// AC #8: time to send a large turn uncompressed and gzipped over
/// simulated 10, 100 and 1000 Mbit/s uplinks. Prints a table (run with
/// `--nocapture`); `HARNESS_GZIP_BENCH_KIB` sets the body size (default
/// 256 KiB, try 1024 for a long session).
#[tokio::test]
async fn upload_benchmark_reports_time_saved_at_simulated_link_speeds() {
    let kib: usize = std::env::var("HARNESS_GZIP_BENCH_KIB")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(256);
    let req = big_request(kib * 1024);
    let mut rows = Vec::new();
    for mbit in [10.0, 100.0, 1000.0] {
        let url = throttled_server(mbit).await;
        let mut times = Vec::new();
        let mut sizes = Vec::new();
        for mode in [GzipRequests::Off, GzipRequests::Always] {
            let (p, _) = provider(&url, mode);
            let start = Instant::now();
            let resp = collect(
                p.stream(req.clone(), CancellationToken::new())
                    .await
                    .unwrap(),
            )
            .await
            .unwrap();
            times.push(start.elapsed());
            sizes.push(request_body(&resp.message).unwrap());
        }
        rows.push((mbit, times, sizes));
    }
    println!("gzip upload benchmark, {kib} KiB request body (level 3)");
    println!(
        "{:>10} {:>12} {:>12} {:>12} {:>12} {:>10}",
        "link", "raw bytes", "gzip bytes", "raw ms", "gzip ms", "saved ms"
    );
    for (mbit, t, s) in &rows {
        println!(
            "{:>7} Mb {:>12} {:>12} {:>12.1} {:>12.1} {:>10.1}",
            mbit,
            s[0].wire_bytes,
            s[1].wire_bytes,
            t[0].as_secs_f64() * 1e3,
            t[1].as_secs_f64() * 1e3,
            (t[0].as_secs_f64() - t[1].as_secs_f64()) * 1e3
        );
        assert_eq!(s[0].raw_bytes, s[1].raw_bytes);
        assert!(s[1].wire_bytes * 4 < s[0].wire_bytes);
    }
    // On the slowest link the saving dwarfs compression time and jitter.
    let (_, t, _) = &rows[0];
    assert!(t[1] < t[0], "gzip should be faster at 10 Mbit/s: {t:?}");
}
