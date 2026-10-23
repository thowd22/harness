//! harness.ini `[mcp.*]` sections drive the client: secret failures,
//! eager and lazy startup.

mod common;

use std::time::Duration;

use common::{fast_opts, server_bin};
use harness_config::ini::load_str;
use harness_mcp::{Servers, Status};

const SECRET: &str = "s3cr3t-token-value-42";

fn ini(extra: &str) -> String {
    format!(
        "[mcp.demo]\ncommand = {}\nargs = --stderr-env, MCP_TEST_TOKEN\nenv.MCP_TEST_TOKEN_cmd = printf {SECRET}\n{extra}",
        server_bin()
    )
}

#[tokio::test]
async fn failing_secret_command_is_reported_without_the_value() {
    let text = format!(
        "[mcp.demo]\ncommand = {}\nenv.TOKEN_cmd = exit 3\n",
        server_bin()
    );
    let cfg = load_str(&text, None).unwrap();
    let servers = Servers::from_config(&cfg, fast_opts());
    let e = servers.get("demo").unwrap().start().await.unwrap_err();
    let msg = e.to_string();
    assert!(msg.starts_with("[mcp.demo] env.TOKEN:"), "{msg}");
    assert!(msg.contains("env.TOKEN_cmd failed"), "{msg}");
}

#[tokio::test]
async fn eager_servers_start_with_the_session() {
    let cfg = load_str(&ini("startup = eager\n"), None).unwrap();
    let mut lazy = cfg.clone();
    lazy.mcp.get_mut("demo").unwrap().startup = harness_config::ini::McpStartup::Lazy;

    let servers = Servers::from_config(&cfg, fast_opts());
    servers.start_eager();
    let demo = servers.get("demo").unwrap();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    while !matches!(demo.status().await, Status::Running(_)) {
        assert!(
            tokio::time::Instant::now() < deadline,
            "eager server never started"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    servers.shutdown_all().await;

    let servers = Servers::from_config(&lazy, fast_opts());
    servers.start_eager();
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(servers.get("demo").unwrap().status().await, Status::Idle);
}
