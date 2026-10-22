//! Request-compression probe (TASK-128): offline against local mock servers
//! that either decode `Content-Encoding` or ignore it, plus the opt-in live
//! probe of OpenAI and OpenRouter.
//!
//! Live run (sends a few one-token requests, fractions of a cent):
//! `HARNESS_LIVE_TESTS=1 OPENROUTER_API_KEY=... cargo test -p harness-providers
//! --test compression_probe -- --ignored --nocapture`. `OPENAI_API_KEY`
//! adds OpenAI (a free token-count request).

use std::io::Read;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use harness_providers::probe::{
    Decision, Encoding, OPENROUTER_MODEL_ENV, OPENROUTER_PROBE_MODEL, ProbeTarget, Verdict, run,
};
use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

/// How the mock treats `Content-Encoding`.
#[derive(Clone, Copy)]
enum Server {
    /// Decodes gzip; listed encodings are understood, others parsed raw.
    Decodes(&'static [&'static str]),
    /// Answers every request with this status.
    Fails(u16),
}

#[derive(Debug, Clone)]
struct Seen {
    head: String,
    body_len: usize,
}

async fn mock(server: Server) -> (String, Arc<Mutex<Vec<Seen>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = seen.clone();
    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                return;
            };
            let log = log.clone();
            tokio::spawn(async move { serve(stream, server, log).await });
        }
    });
    (url, seen)
}

async fn serve(mut stream: TcpStream, server: Server, log: Arc<Mutex<Vec<Seen>>>) {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    let head_end = loop {
        let n = stream.read(&mut chunk).await.unwrap();
        if n == 0 {
            return;
        }
        buf.extend_from_slice(&chunk[..n]);
        if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break i + 4;
        }
    };
    let head = String::from_utf8_lossy(&buf[..head_end]).to_lowercase();
    let len: usize = head
        .lines()
        .find_map(|l| l.strip_prefix("content-length:"))
        .map(|v| v.trim().parse().unwrap())
        .unwrap_or(0);
    while buf.len() < head_end + len {
        let n = stream.read(&mut chunk).await.unwrap();
        buf.extend_from_slice(&chunk[..n]);
    }
    let body = buf[head_end..head_end + len].to_vec();
    let encoding = head
        .lines()
        .find_map(|l| l.strip_prefix("content-encoding:"))
        .map(|v| v.trim().to_string());
    log.lock().unwrap().push(Seen {
        head: head.clone(),
        body_len: body.len(),
    });

    let (status, reply) = match server {
        Server::Fails(s) => (s, r#"{"error":{"message":"denied"}}"#.to_string()),
        Server::Decodes(known) => {
            let decoded = match encoding.as_deref() {
                Some(e) if known.contains(&e) => decode(e, &body),
                _ => body,
            };
            match serde_json::from_slice::<Value>(&decoded) {
                Ok(_) => (200, r#"{"ok":true}"#.to_string()),
                Err(_) => (
                    400,
                    r#"{"error":{"message":"JSON parsing failed","code":400}}"#.to_string(),
                ),
            }
        }
    };
    let response = format!(
        "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{reply}",
        reply.len()
    );
    let _ = stream.write_all(response.as_bytes()).await;
    let _ = stream.shutdown().await;
}

fn decode(encoding: &str, body: &[u8]) -> Vec<u8> {
    match encoding {
        "gzip" => {
            let mut out = Vec::new();
            flate2::read::GzDecoder::new(body)
                .read_to_end(&mut out)
                .unwrap();
            out
        }
        "zstd" => zstd::decode_all(body).unwrap(),
        other => panic!("mock cannot decode {other}"),
    }
}

const TIMEOUT: Duration = Duration::from_secs(10);

#[tokio::test]
async fn server_that_ignores_encodings_leaves_compression_off() {
    let (url, seen) = mock(Server::Decodes(&[])).await;
    let target = ProbeTarget::openrouter("sk-test", &url, "m").unwrap();
    let report = run(&target, &[Encoding::Gzip, Encoding::Zstd], TIMEOUT).await;

    assert_eq!(report.provider, "openrouter");
    assert_eq!(
        report.result(Encoding::Identity).unwrap().verdict,
        Verdict::Accepted
    );
    for e in [Encoding::Gzip, Encoding::Zstd] {
        assert_eq!(
            report.result(e).unwrap().verdict,
            Verdict::Rejected {
                status: 400,
                message: "JSON parsing failed".into()
            }
        );
    }
    assert!(report.accepted().is_empty());
    assert_eq!(report.decision(), Decision::LeaveOff);
    assert!(!report.needs_follow_up());

    // Control first, then one request per encoding with the right header,
    // the auth header and the encoded body length.
    let seen = seen.lock().unwrap().clone();
    assert_eq!(seen.len(), 3);
    assert!(seen[0].head.starts_with("post /chat/completions "));
    assert!(!seen[0].head.contains("content-encoding"));
    assert!(seen[1].head.contains("\r\ncontent-encoding: gzip\r\n"));
    assert!(seen[2].head.contains("\r\ncontent-encoding: zstd\r\n"));
    for (s, r) in seen.iter().zip(&report.results) {
        assert!(s.head.contains("\r\nauthorization: bearer sk-test\r\n"));
        assert!(s.head.contains("\r\ncontent-type: application/json\r\n"));
        assert_eq!(s.body_len, r.wire_bytes);
    }
    assert_eq!(seen[0].body_len, report.raw_bytes);
}

#[tokio::test]
async fn server_that_decodes_gzip_is_recorded_and_needs_follow_up() {
    let (url, _) = mock(Server::Decodes(&["gzip"])).await;
    let target = ProbeTarget::openai("sk-test", &url).unwrap();
    let report = run(&target, &Encoding::ALL, TIMEOUT).await;

    assert_eq!(report.results.len(), 3, "identity is sent once");
    assert_eq!(report.accepted(), vec![Encoding::Gzip]);
    assert!(matches!(
        report.result(Encoding::Zstd).unwrap().verdict,
        Verdict::Rejected { status: 400, .. }
    ));
    assert_eq!(report.decision(), Decision::Enable(vec![Encoding::Gzip]));
    assert!(report.needs_follow_up());
    let text = report.render();
    assert!(text.contains("gzip") && text.contains("accepted"), "{text}");
    assert!(!text.contains("sk-test"));
}

#[tokio::test]
async fn server_that_decodes_both_accepts_both() {
    let (url, _) = mock(Server::Decodes(&["gzip", "zstd"])).await;
    let target = ProbeTarget::openai("sk-test", &url).unwrap();
    let report = run(&target, &[Encoding::Zstd, Encoding::Gzip], TIMEOUT).await;
    assert_eq!(
        report.results[0].encoding,
        Encoding::Identity,
        "control first"
    );
    assert_eq!(report.accepted(), vec![Encoding::Zstd, Encoding::Gzip]);
}

#[tokio::test]
async fn failed_control_makes_the_probe_inconclusive() {
    for status in [401, 429, 400] {
        let (url, _) = mock(Server::Fails(status)).await;
        let target = ProbeTarget::openrouter("sk-test", &url, "m").unwrap();
        let report = run(&target, &[Encoding::Gzip, Encoding::Zstd], TIMEOUT).await;
        assert_eq!(report.decision(), Decision::Inconclusive, "status {status}");
        assert!(report.results.iter().all(
            |r| matches!(r.verdict, Verdict::Inconclusive { status: Some(s), .. } if s == status)
        ));
        assert!(!report.needs_follow_up());
    }
}

#[tokio::test]
async fn unreachable_server_is_inconclusive() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    drop(listener);
    let target = ProbeTarget::openrouter("sk-test", &url, "m").unwrap();
    let report = run(&target, &[Encoding::Gzip], TIMEOUT).await;
    assert_eq!(report.decision(), Decision::Inconclusive);
    assert!(matches!(
        report.results[1].verdict,
        Verdict::Inconclusive { status: None, .. }
    ));
}

// ----------------------------------------------------------------- live

fn live_key(var: &str) -> Option<String> {
    if std::env::var("HARNESS_LIVE_TESTS").ok().as_deref() != Some("1") {
        eprintln!("skipping live probe: set HARNESS_LIVE_TESTS=1 to run it");
        return None;
    }
    let key = std::env::var(var).ok().filter(|k| !k.is_empty());
    if key.is_none() {
        eprintln!("skipping live probe: {var} is not set");
    }
    key
}

/// Live: one-token completions to OpenRouter, raw, gzip and zstd.
#[tokio::test]
#[ignore = "live network; set HARNESS_LIVE_TESTS=1 and OPENROUTER_API_KEY"]
async fn live_probe_openrouter() {
    let Some(key) = live_key("OPENROUTER_API_KEY") else {
        return;
    };
    let model =
        std::env::var(OPENROUTER_MODEL_ENV).unwrap_or_else(|_| OPENROUTER_PROBE_MODEL.into());
    let target = ProbeTarget::openrouter(
        &key,
        harness_providers::openrouter::DEFAULT_BASE_URL,
        &model,
    )
    .unwrap();
    let report = run(
        &target,
        &[Encoding::Gzip, Encoding::Zstd],
        Duration::from_secs(60),
    )
    .await;
    println!("{}", report.render());
    assert_eq!(
        report.result(Encoding::Identity).unwrap().verdict,
        Verdict::Accepted,
        "control request must succeed for the probe to mean anything"
    );
    assert_ne!(
        report.decision(),
        Decision::Inconclusive,
        "{}",
        report.render()
    );
}

/// Live: free input-token counts to OpenAI, raw, gzip and zstd.
#[tokio::test]
#[ignore = "live network; set HARNESS_LIVE_TESTS=1 and OPENAI_API_KEY"]
async fn live_probe_openai() {
    let Some(key) = live_key("OPENAI_API_KEY") else {
        return;
    };
    let target = ProbeTarget::openai(&key, harness_providers::openai::DEFAULT_BASE_URL).unwrap();
    let report = run(
        &target,
        &[Encoding::Gzip, Encoding::Zstd],
        Duration::from_secs(60),
    )
    .await;
    println!("{}", report.render());
    assert_eq!(
        report.result(Encoding::Identity).unwrap().verdict,
        Verdict::Accepted,
        "control request must succeed for the probe to mean anything"
    );
    assert_ne!(
        report.decision(),
        Decision::Inconclusive,
        "{}",
        report.render()
    );
}
