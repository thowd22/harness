//! Stdio servers get the same environment scrubbing as the bash tool.
//!
//! The test needs credentials in the *inherited* environment, which a test
//! cannot set on itself (`set_var` is unsafe), so it re-runs this test
//! binary with them set and does the real check in that child.

mod common;

use std::sync::Arc;

use common::{fast_opts, inline, stdio};
use harness_mcp::ServerHandle;
use serde_json::json;

const CHILD: &str = "HARNESS_MCP_ENV_SCRUB_CHILD_TEST";
const MARK: &str = "MCP_ENV_SCRUB_CHILD";
const RESOLVED: &str = "sk-resolved-provider-key-0123456789";

#[tokio::test]
async fn inherited_credentials_do_not_reach_stdio_servers() {
    if std::env::var_os(MARK).is_some() {
        return child().await;
    }
    let home = tempfile::tempdir().unwrap();
    let h = home.path();
    let out = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "inherited_credentials_do_not_reach_stdio_servers",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(MARK, "1")
        .env("ANTHROPIC_API_KEY", "sk-ant-parent-env")
        .env("OPENAI_API_KEY", "sk-openai-parent-env")
        .env(CHILD, "harness-internal")
        .env("MY_PROVIDER_KEY", "configured-api-key-env")
        .env("COPY_OF_KEY", format!("Bearer {RESOLVED}"))
        .env("MCP_HARMLESS", "visible")
        .env("HARNESS_CONFIG", h.join("harness.ini"))
        .env("HOME", h)
        .env("XDG_CONFIG_HOME", h.join("config"))
        .env("XDG_DATA_HOME", h.join("data"))
        .env("XDG_CACHE_HOME", h.join("cache"))
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "child test failed:\n{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    // The child really ran the check (and was not filtered out).
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("1 passed"),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
}

async fn child() {
    let mut cfg = stdio(&[]);
    // A server's own env.* values are added after scrubbing, even under a
    // scrubbed name.
    cfg.env
        .insert("OPENAI_API_KEY".into(), inline("server-own-value"));
    let mut opts = fast_opts();
    opts.scrub_env = vec!["MY_PROVIDER_KEY".into()];
    opts.known_secrets = vec![RESOLVED.into()];
    let srv = ServerHandle::new("scrub", cfg, Arc::new(opts));
    let get = |name: &'static str| {
        let srv = &srv;
        async move {
            srv.call_tool("env", json!({ "name": name }))
                .await
                .unwrap()
                .text()
        }
    };
    assert_eq!(get("ANTHROPIC_API_KEY").await, "<unset>");
    assert_eq!(get(CHILD).await, "<unset>", "HARNESS_* is scrubbed");
    assert_eq!(get("HARNESS_CONFIG").await, "<unset>");
    assert_eq!(get("MY_PROVIDER_KEY").await, "<unset>");
    assert_eq!(get("COPY_OF_KEY").await, "<unset>", "value matching");
    assert_eq!(get("OPENAI_API_KEY").await, "server-own-value");
    assert_eq!(get("MCP_HARMLESS").await, "visible");
    srv.shutdown().await;
}
