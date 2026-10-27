//! OAuth for remote MCP servers: pasted codes and tokens never reach the
//! logs, even at TRACE. A test binary of its own, so no other test thread
//! fixes `tracing`'s per-callsite interest before this subscriber is set.

use std::sync::Arc;
use std::time::Duration;

use harness_config::ini::{McpAuth, McpServer};
use harness_mcp::auth::mock::{MockBrowser, MockConfig, MockOAuthServer};
use harness_mcp::auth::{Authorize, McpTokenStore};
use harness_providers::signin::{Paste, PasteSender, paste_channel};

fn param(url: &str, k: &str) -> Option<String> {
    reqwest::Url::parse(url)
        .unwrap()
        .query_pairs()
        .find(|(n, _)| n == k)
        .map(|(_, v)| v.into_owned())
}

async fn paste(tx: &PasteSender, text: &str) -> Result<(), String> {
    let (p, reply) = Paste::new(text);
    tx.send(p).unwrap();
    reply.await.unwrap()
}

#[derive(Clone, Default)]
struct LogBuf(Arc<std::sync::Mutex<Vec<u8>>>);

impl std::io::Write for LogBuf {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(b);
        Ok(b.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[tokio::test]
async fn pasted_codes_and_tokens_never_reach_the_logs() {
    let buf = LogBuf::default();
    let writer = buf.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::TRACE)
        .with_writer(move || writer.clone())
        .with_ansi(false)
        .finish();
    let _guard = tracing::subscriber::set_default(subscriber);

    let mock = MockOAuthServer::start(MockConfig::default()).await;
    let dir = tempfile::tempdir().unwrap();
    let store = McpTokenStore::new(dir.path().join("auth/mcp"));
    let mut cfg = McpServer::http(mock.mcp_url());
    cfg.auth = McpAuth::OAuth;
    let browser = MockBrowser::remote();
    let (tx, mut rx) = paste_channel();
    let flow = Authorize::new("docs", &cfg, store.clone())
        .unwrap()
        .with_timeout(Duration::from_secs(20));
    let mut code = String::new();
    let user = async {
        let address = browser.next_redirect().await;
        code = param(&address, "code").unwrap();
        assert!(paste(&tx, "?code=wrong-attempt&state=nope").await.is_err());
        assert_eq!(paste(&tx, &address).await, Ok(()));
    };
    let (r, ()) = tokio::join!(flow.run_with(&browser, &mut rx), user);
    r.unwrap();
    let record = store.load("docs").unwrap().unwrap();
    let tokens = record.tokens.unwrap();
    let logs = String::from_utf8(buf.0.lock().unwrap().clone()).unwrap();
    assert!(
        logs.contains("MCP server authorized"),
        "logging was captured"
    );
    for secret in [
        code.as_str(),
        "wrong-attempt",
        tokens.access_token.expose(),
        tokens.refresh_token.as_ref().unwrap().expose(),
    ] {
        assert!(!logs.contains(secret), "{secret} leaked into the logs");
    }
}
