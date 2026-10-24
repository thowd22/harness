//! Shared connection pool: every `HttpClient` for one base URL reuses the
//! same connections, while each sends only its own credentials.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use harness_providers::http::{HttpClient, HttpConfig};
use reqwest::header::{AUTHORIZATION, HeaderMap, HeaderValue};
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_util::sync::CancellationToken;

/// An HTTP/1.1 keep-alive server answering every request with `{}` and
/// recording the number of accepted connections and each request's
/// `authorization` header.
struct KeepAlive {
    url: String,
    connections: Arc<Mutex<usize>>,
    auth: Arc<Mutex<Vec<Option<String>>>>,
}

impl KeepAlive {
    async fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let connections = Arc::new(Mutex::new(0));
        let auth = Arc::new(Mutex::new(Vec::new()));
        let (c, a) = (connections.clone(), auth.clone());
        tokio::spawn(async move {
            while let Ok((sock, _)) = listener.accept().await {
                *c.lock().unwrap() += 1;
                tokio::spawn(serve(sock, a.clone()));
            }
        });
        Self {
            url,
            connections,
            auth,
        }
    }

    fn connections(&self) -> usize {
        *self.connections.lock().unwrap()
    }

    fn auth(&self) -> Vec<Option<String>> {
        self.auth.lock().unwrap().clone()
    }
}

async fn serve(mut sock: TcpStream, auth: Arc<Mutex<Vec<Option<String>>>>) {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 4096];
    loop {
        let header_end = loop {
            if let Some(p) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                break p + 4;
            }
            match sock.read(&mut tmp).await {
                Ok(0) | Err(_) => return,
                Ok(n) => buf.extend_from_slice(&tmp[..n]),
            }
        };
        let head = String::from_utf8_lossy(&buf[..header_end]).to_string();
        let header = |name: &str| {
            head.lines().find_map(|l| {
                let (k, v) = l.split_once(':')?;
                k.eq_ignore_ascii_case(name).then(|| v.trim().to_owned())
            })
        };
        let len: usize = header("content-length").map_or(0, |v| v.parse().unwrap());
        auth.lock().unwrap().push(header("authorization"));
        while buf.len() < header_end + len {
            match sock.read(&mut tmp).await {
                Ok(0) | Err(_) => return,
                Ok(n) => buf.extend_from_slice(&tmp[..n]),
            }
        }
        buf.drain(..header_end + len);
        let reply =
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: 2\r\n\r\n{}";
        if sock.write_all(reply.as_bytes()).await.is_err() {
            return;
        }
    }
}

fn client(url: &str, key: &str) -> HttpClient {
    let mut cfg = HttpConfig::new(url);
    let mut v = HeaderValue::from_str(&format!("Bearer {key}")).unwrap();
    v.set_sensitive(true);
    cfg.default_headers.insert(AUTHORIZATION, v);
    cfg.request_timeout = Duration::from_secs(10);
    HttpClient::new(cfg).unwrap()
}

async fn post(c: &HttpClient) {
    c.post_json(
        "chat",
        &HeaderMap::new(),
        &json!({}),
        &CancellationToken::new(),
        |_| {},
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn clients_for_one_base_url_share_connections_but_not_credentials() {
    let server = KeepAlive::start().await;
    let main = client(&server.url, "key-main");
    post(&main).await;
    // A provider built later (subagent, summary, dreaming) with another
    // key reuses the warm connection and sends its own key.
    let sub = client(&format!("{}/", server.url), "key-sub");
    assert!(main.shares_connections_with(&sub));
    post(&sub).await;
    post(&main).await;
    let (_, _) = sub.get_text("models").await.unwrap();
    assert_eq!(server.connections(), 1, "one connection for all requests");
    let bearer = |k: &str| Some(format!("Bearer {k}"));
    assert_eq!(
        server.auth(),
        vec![
            bearer("key-main"),
            bearer("key-sub"),
            bearer("key-main"),
            bearer("key-sub")
        ]
    );
    // A client without credentials sends none.
    let anon = HttpClient::new(HttpConfig::new(server.url.clone())).unwrap();
    post(&anon).await;
    assert_eq!(server.auth().last(), Some(&None));
    assert_eq!(server.connections(), 1);
}

#[tokio::test]
async fn other_base_urls_and_isolated_clients_open_their_own_connections() {
    let (a, b) = (KeepAlive::start().await, KeepAlive::start().await);
    let ca = client(&a.url, "k");
    let cb = client(&b.url, "k");
    assert!(!ca.shares_connections_with(&cb));
    post(&ca).await;
    post(&cb).await;
    assert_eq!((a.connections(), b.connections()), (1, 1));
    let iso = HttpClient::isolated(HttpConfig::new(a.url.clone())).unwrap();
    assert!(!iso.shares_connections_with(&ca));
    post(&iso).await;
    assert_eq!(a.connections(), 2, "an isolated client opens its own");
    post(&ca).await;
    assert_eq!(a.connections(), 2);
}
