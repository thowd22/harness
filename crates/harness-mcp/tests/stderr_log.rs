//! Secrets in `env.*` resolve like provider keys, reach the server, and
//! are redacted from the captured stderr log. Alone in its test binary:
//! a thread-local tracing subscriber is only reliable when no other test
//! in the process touches the same callsites.

mod common;

use std::io::Write;
use std::sync::{Arc, Mutex};

use common::{fast_opts, server_bin};
use harness_config::ini::load_str;
use harness_mcp::{ClientOptions, Servers};
use serde_json::json;

const SECRET: &str = "s3cr3t-token-value-42";

#[derive(Clone, Default)]
struct Buf(Arc<Mutex<Vec<u8>>>);

impl Write for Buf {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(b);
        Ok(b.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Buf {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.0.lock().unwrap()).into_owned()
    }
}

fn ini(extra: &str) -> String {
    format!(
        "[mcp.demo]\ncommand = {}\nargs = --stderr-env, MCP_TEST_TOKEN\nenv.MCP_TEST_TOKEN_cmd = printf {SECRET}\n{extra}",
        server_bin()
    )
}

// current_thread: the server's stderr reader runs on this thread, so the
// thread-local subscriber below sees its log lines.
#[tokio::test(flavor = "current_thread")]
async fn env_secrets_reach_the_server_and_stderr_is_logged_redacted() {
    let buf = Buf::default();
    let sink = buf.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(move || sink.clone())
        .with_ansi(false)
        .with_max_level(tracing::Level::DEBUG)
        .finish();
    let _guard = tracing::subscriber::set_default(subscriber);

    let cfg = load_str(&ini("env.FROM_PARENT_env = PATH\n"), None).unwrap();
    let servers = Servers::from_config(
        &cfg,
        ClientOptions {
            scrub_env: vec!["HOME".into()],
            ..fast_opts()
        },
    );
    let demo = servers.get("demo").unwrap();
    // The server received the resolved value...
    let got = demo
        .call_tool("env", json!({"name": "MCP_TEST_TOKEN"}))
        .await
        .unwrap();
    assert_eq!(got.text(), SECRET);
    // ...and `_env` values from the parent environment.
    let path = demo
        .call_tool("env", json!({"name": "FROM_PARENT"}))
        .await
        .unwrap();
    assert_eq!(path.text(), std::env::var("PATH").unwrap());
    // Scrubbed variables are not inherited.
    let home = demo
        .call_tool("env", json!({"name": "HOME"}))
        .await
        .unwrap();
    assert_eq!(home.text(), "<unset>");
    // Its stderr went to the log, redacted, and is kept for errors.
    demo.call_tool("stderr", json!({"text": "hello from stderr"}))
        .await
        .unwrap();
    servers.shutdown_all().await;
    let log = buf.text();
    assert!(log.contains("harness_mcp::stderr"), "{log}");
    assert!(log.contains("MCP_TEST_TOKEN=[REDACTED]"), "{log}");
    assert!(log.contains("hello from stderr"), "{log}");
    assert!(!log.contains(SECRET), "secret leaked into the log:\n{log}");
    let tail = demo.stderr_tail();
    assert!(
        tail.iter().any(|l| l == "MCP_TEST_TOKEN=[REDACTED]"),
        "{tail:?}"
    );
    assert!(tail.iter().all(|l| !l.contains(SECRET)));
}
