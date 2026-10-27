//! `login::verify_api_key` against a local mock server, plus an env-gated
//! live OpenRouter check.

use std::time::Duration;

use harness_providers::login::{VERIFY_TIMEOUT, verify_api_key};
use harness_providers::provider::ProviderError;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// Serves one request with `status` and `body`; returns the base URL and a
/// handle yielding the raw request head.
async fn serve_once(status: u16, body: &'static str) -> (String, tokio::task::JoinHandle<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        let (mut sock, _) = listener.accept().await.unwrap();
        let mut buf = Vec::new();
        let mut chunk = [0u8; 1024];
        while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
            let n = sock.read(&mut chunk).await.unwrap();
            if n == 0 {
                break;
            }
            buf.extend_from_slice(&chunk[..n]);
        }
        let resp = format!(
            "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
            body.len()
        );
        sock.write_all(resp.as_bytes()).await.unwrap();
        String::from_utf8_lossy(&buf).into_owned()
    });
    (base, task)
}

#[tokio::test]
async fn anthropic_check_uses_x_api_key_and_lists_one_model() {
    let (base, req) = serve_once(200, r#"{"data":[{"id":"claude-x"}],"has_more":true}"#).await;
    let client = reqwest::Client::new();
    let ok = verify_api_key(
        &client,
        "anthropic",
        " sk-ant-good ",
        Some(&base),
        VERIFY_TIMEOUT,
    )
    .await
    .unwrap();
    assert_eq!(ok.detail.as_deref(), Some("1 model visible"));
    let head = req.await.unwrap().to_ascii_lowercase();
    assert!(head.starts_with("get /v1/models?limit=1 "), "{head}");
    assert!(head.contains("x-api-key: sk-ant-good\r\n"), "{head}");
    assert!(head.contains("anthropic-version:"), "{head}");
}

#[tokio::test]
async fn openai_and_openrouter_use_bearer_tokens() {
    let (base, req) = serve_once(200, r#"{"data":[]}"#).await;
    let client = reqwest::Client::new();
    verify_api_key(
        &client,
        "openai",
        "sk-oa",
        Some(&format!("{base}/v1")),
        VERIFY_TIMEOUT,
    )
    .await
    .unwrap();
    let head = req.await.unwrap().to_ascii_lowercase();
    assert!(head.starts_with("get /v1/models "), "{head}");
    assert!(head.contains("authorization: bearer sk-oa\r\n"), "{head}");

    let (base, req) = serve_once(200, r#"{"data":{"label":"sk-or-v1-abc...xyz"}}"#).await;
    let ok = verify_api_key(
        &client,
        "openrouter",
        "sk-or-test-key",
        Some(&base),
        VERIFY_TIMEOUT,
    )
    .await
    .unwrap();
    assert_eq!(ok.detail, None, "the label is part of the key");
    assert!(req.await.unwrap().starts_with("GET /key "));
}

#[tokio::test]
async fn rejected_key_is_an_auth_error_without_the_key() {
    let (base, _req) = serve_once(
        401,
        r#"{"error":{"message":"Incorrect API key provided: sk-bad-key-123"}}"#,
    )
    .await;
    let client = reqwest::Client::new();
    let err = verify_api_key(
        &client,
        "openai",
        "sk-bad-key-123",
        Some(&base),
        VERIFY_TIMEOUT,
    )
    .await
    .unwrap_err();
    assert!(matches!(err, ProviderError::Auth(_)), "{err:?}");
    let text = err.to_string();
    assert!(!text.contains("sk-bad-key-123"), "{text}");
    assert!(text.contains("Incorrect API key"), "{text}");
}

#[tokio::test]
async fn unreachable_server_is_a_connection_error() {
    // A privileged port no test can bind (a freed ephemeral port can be
    // reused by another test's mock server, which then answers).
    let base = "http://127.0.0.1:1".to_owned();
    let client = reqwest::Client::new();
    let err = verify_api_key(
        &client,
        "openrouter",
        "k",
        Some(&base),
        Duration::from_secs(5),
    )
    .await
    .unwrap_err();
    assert!(matches!(err, ProviderError::Connection(_)), "{err:?}");
    assert!(matches!(
        verify_api_key(&client, "gemini", "k", None, VERIFY_TIMEOUT).await,
        Err(ProviderError::InvalidRequest(_))
    ));
}

/// Live: `OPENROUTER_API_KEY` is accepted and a bogus key is rejected.
#[tokio::test]
#[ignore = "live network; needs OPENROUTER_API_KEY"]
async fn live_openrouter_key_check() {
    let Ok(key) = std::env::var("OPENROUTER_API_KEY") else {
        eprintln!("OPENROUTER_API_KEY not set; skipping");
        return;
    };
    let client = reqwest::Client::new();
    verify_api_key(&client, "openrouter", &key, None, VERIFY_TIMEOUT)
        .await
        .unwrap();
    let err = verify_api_key(
        &client,
        "openrouter",
        "sk-or-v1-not-a-real-key",
        None,
        VERIFY_TIMEOUT,
    )
    .await
    .unwrap_err();
    assert!(matches!(err, ProviderError::Auth(_)), "{err:?}");
}

/// Live: `ANTHROPIC_API_KEY` is accepted and a bogus key is rejected.
#[tokio::test]
#[ignore = "live network; needs ANTHROPIC_API_KEY"]
async fn live_anthropic_key_check() {
    let Ok(key) = std::env::var("ANTHROPIC_API_KEY") else {
        eprintln!("ANTHROPIC_API_KEY not set; skipping");
        return;
    };
    let client = reqwest::Client::new();
    verify_api_key(&client, "anthropic", &key, None, VERIFY_TIMEOUT)
        .await
        .unwrap();
    let err = verify_api_key(
        &client,
        "anthropic",
        "sk-ant-api03-not-a-real-key",
        None,
        VERIFY_TIMEOUT,
    )
    .await
    .unwrap_err();
    assert!(matches!(err, ProviderError::Auth(_)), "{err:?}");
}
