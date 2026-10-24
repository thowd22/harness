//! Session start and later providers (subagents, session summaries,
//! dreaming) against one keep-alive server: the catalog comes from the
//! on-disk cache once it exists, and every provider for the base URL
//! reuses one warm connection.

use std::sync::{Arc, Mutex};

use futures::StreamExt;
use harness_providers::model::{ChatRequest, ContentBlock, Message, Role};
use harness_providers::provider::Provider;
use harness_providers::registry::Registry;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_util::sync::CancellationToken;

use super::{provider, provider_sync};

const CATALOG: &str = r#"{"data": [{"id": "anthropic/claude-opus-5.5", "context_length": 200000, "supported_parameters": ["tools", "reasoning"]}, {"id": "x/no-tools", "supported_parameters": ["temperature"]}]}"#;

const CHAT: &str = "data: {\"id\":\"g\",\"choices\":[{\"delta\":{\"content\":\"hi\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n";

/// Accepted connections and the request paths, in order.
#[derive(Default)]
struct Seen {
    connections: usize,
    paths: Vec<String>,
}

async fn keep_alive_server() -> (String, Arc<Mutex<Seen>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/api/v1", listener.local_addr().unwrap());
    let seen = Arc::new(Mutex::new(Seen::default()));
    let s = seen.clone();
    tokio::spawn(async move {
        while let Ok((sock, _)) = listener.accept().await {
            s.lock().unwrap().connections += 1;
            tokio::spawn(serve(sock, s.clone()));
        }
    });
    (url, seen)
}

async fn serve(mut sock: TcpStream, seen: Arc<Mutex<Seen>>) {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 8192];
    loop {
        let end = loop {
            if let Some(p) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                break p + 4;
            }
            match sock.read(&mut tmp).await {
                Ok(0) | Err(_) => return,
                Ok(n) => buf.extend_from_slice(&tmp[..n]),
            }
        };
        let head = String::from_utf8_lossy(&buf[..end]).to_ascii_lowercase();
        let path = head.split(' ').nth(1).unwrap_or_default().to_owned();
        let len: usize = head
            .lines()
            .find_map(|l| l.strip_prefix("content-length:"))
            .map_or(0, |v| v.trim().parse().unwrap());
        while buf.len() < end + len {
            match sock.read(&mut tmp).await {
                Ok(0) | Err(_) => return,
                Ok(n) => buf.extend_from_slice(&tmp[..n]),
            }
        }
        buf.drain(..end + len);
        let (ctype, body) = if path.ends_with("/models") {
            ("application/json", CATALOG)
        } else {
            ("text/event-stream", CHAT)
        };
        seen.lock().unwrap().paths.push(path);
        let reply = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: {ctype}\r\ncontent-length: {}\r\n\r\n{body}",
            body.len()
        );
        if sock.write_all(reply.as_bytes()).await.is_err() {
            return;
        }
    }
}

async fn chat(p: &Arc<dyn Provider>) {
    let request = ChatRequest {
        model: "anthropic/claude-opus-5.5".into(),
        messages: vec![Message {
            role: Role::User,
            content: vec![ContentBlock::text("hi")],
        }],
        ..ChatRequest::default()
    };
    let mut s = p.stream(request, CancellationToken::new()).await.unwrap();
    while let Some(e) = s.next().await {
        e.unwrap();
    }
}

#[tokio::test]
async fn cached_catalog_and_one_warm_connection_for_every_provider() {
    let (url, seen) = keep_alive_server().await;
    let tmp = tempfile::tempdir().unwrap();
    let ini = format!(
        "[general]\ndata_dir = {}\n[provider.openrouter]\napi_key = or-test-key\nbase_url = {url}\n",
        tmp.path().display()
    );
    let cfg = harness_config::ini::load_str(&ini, None).unwrap();
    let registry = Arc::new(Registry::builtin());
    let resolved = registry
        .resolve("openrouter/anthropic/claude-opus-5.5")
        .unwrap();

    // First session start: no cached list yet, so the catalog is
    // downloaded (and cached), then the main agent calls the model.
    let main = provider(&cfg, &registry, &resolved, true).await.unwrap();
    chat(&main).await;
    // A subagent, session summary or dreaming provider (built without
    // network checks) reuses the main provider's connection.
    for _ in 0..3 {
        let later = provider_sync(&cfg, &registry, &resolved).unwrap();
        chat(&later).await;
    }
    // The next session start reads the cached list: no download.
    let next = provider(&cfg, &registry, &resolved, true).await.unwrap();
    chat(&next).await;

    let seen = seen.lock().unwrap();
    let models = seen.paths.iter().filter(|p| p.ends_with("/models")).count();
    assert_eq!(models, 1, "{:?}", seen.paths);
    assert_eq!(seen.paths.len(), 6, "{:?}", seen.paths);
    assert_eq!(seen.connections, 1, "{:?}", seen.paths);
}

#[tokio::test]
async fn tool_less_models_are_still_rejected_with_a_cached_catalog() {
    let (url, _seen) = keep_alive_server().await;
    let tmp = tempfile::tempdir().unwrap();
    let ini = format!(
        "[general]\ndata_dir = {}\n[provider.openrouter]\napi_key = k\nbase_url = {url}\n",
        tmp.path().display()
    );
    let cfg = harness_config::ini::load_str(&ini, None).unwrap();
    let registry = Arc::new(Registry::builtin());
    let ok = registry
        .resolve("openrouter/anthropic/claude-opus-5.5")
        .unwrap();
    provider(&cfg, &registry, &ok, true).await.unwrap();
    let bad = registry.resolve("openrouter/x/no-tools").unwrap();
    let err = provider(&cfg, &registry, &bad, true).await.err().unwrap();
    assert!(err.contains("does not support tool calling"), "{err}");
    assert!(provider(&cfg, &registry, &bad, false).await.is_ok());
}
