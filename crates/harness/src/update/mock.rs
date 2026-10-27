//! A local stand-in for the GitHub releases API: answers every request
//! with one status and body, optionally only after [`MockApi::release`],
//! and records the request heads.

use std::sync::{Arc, Mutex};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::Semaphore;

/// A running mock API.
pub(crate) struct MockApi {
    /// Its base URL (`http://127.0.0.1:<port>`).
    pub(crate) url: String,
    heads: Arc<Mutex<Vec<String>>>,
    gate: Arc<Semaphore>,
}

impl MockApi {
    /// Answers right away.
    pub(crate) async fn start(status: u16, body: &str) -> MockApi {
        let m = Self::held(status, body).await;
        m.gate.add_permits(1_000);
        m
    }

    /// Holds every answer until [`MockApi::release`].
    pub(crate) async fn held(status: u16, body: &str) -> MockApi {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let heads = Arc::new(Mutex::new(Vec::new()));
        let gate = Arc::new(Semaphore::new(0));
        let (h, g, body) = (heads.clone(), gate.clone(), body.to_owned());
        tokio::spawn(async move {
            while let Ok((mut sock, _)) = listener.accept().await {
                let (h, g, body) = (h.clone(), g.clone(), body.clone());
                tokio::spawn(async move {
                    let mut buf = Vec::new();
                    let mut tmp = [0u8; 4096];
                    while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
                        match sock.read(&mut tmp).await {
                            Ok(0) | Err(_) => return,
                            Ok(n) => buf.extend_from_slice(&tmp[..n]),
                        }
                    }
                    h.lock()
                        .unwrap()
                        .push(String::from_utf8_lossy(&buf).into_owned());
                    let Ok(_permit) = g.acquire().await else {
                        return;
                    };
                    let resp = format!(
                        "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    let _ = sock.write_all(resp.as_bytes()).await;
                });
            }
        });
        MockApi { url, heads, gate }
    }

    /// Lets held answers go.
    pub(crate) fn release(&self) {
        self.gate.add_permits(1_000);
    }

    /// The request heads received so far.
    pub(crate) fn heads(&self) -> Vec<String> {
        self.heads.lock().unwrap().clone()
    }
}

/// A releases listing with `(tag, prerelease)` entries.
pub(crate) fn listing(releases: &[(&str, bool)]) -> String {
    let v: Vec<_> = releases
        .iter()
        .map(|(tag, pre)| {
            serde_json::json!({
                "tag_name": tag,
                "html_url": format!("https://github.com/thowd22/harness/releases/tag/{tag}"),
                "prerelease": pre,
                "draft": false,
            })
        })
        .collect();
    serde_json::to_string(&v).unwrap()
}
