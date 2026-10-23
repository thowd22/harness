//! Shared helpers for the MCP client tests. Everything runs offline
//! against the bundled test server.
#![allow(dead_code)]

use std::sync::Arc;
use std::time::{Duration, Instant};

use harness_config::ini::{McpSecret, McpServer};
use harness_config::secrets::Secret;
use harness_mcp::{ClientOptions, ServerHandle};

/// The bundled stdio test server binary.
pub fn server_bin() -> String {
    env!("CARGO_BIN_EXE_harness-mcp-test-server").to_owned()
}

/// A stdio server config running the test server with `args`.
pub fn stdio(args: &[&str]) -> McpServer {
    let mut s = McpServer::stdio(server_bin(), args.iter().map(|a| (*a).to_owned()).collect());
    s.timeout = Duration::from_secs(20);
    s.startup_timeout = Duration::from_secs(20);
    s
}

/// Client options with short delays for tests.
pub fn fast_opts() -> ClientOptions {
    ClientOptions {
        probe_timeout: Duration::from_secs(5),
        backoff_base: Duration::from_millis(50),
        backoff_max: Duration::from_millis(400),
        max_failures: 4,
        shutdown_grace: Duration::from_millis(500),
        ..ClientOptions::default()
    }
}

/// A handle with [`fast_opts`].
pub fn handle(name: &str, cfg: McpServer) -> ServerHandle {
    ServerHandle::new(name, cfg, Arc::new(fast_opts()))
}

/// An inline secret source.
pub fn inline(v: &str) -> McpSecret {
    McpSecret {
        inline: Some(Secret::new(v.to_owned())),
        ..McpSecret::default()
    }
}

/// A command secret source.
pub fn cmd(c: &str) -> McpSecret {
    McpSecret {
        cmd: Some(c.to_owned()),
        ..McpSecret::default()
    }
}

/// True while process `pid` exists (zombies count as gone once reaped).
pub fn alive(pid: i32) -> bool {
    pid > 1 && nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), None).is_ok()
}

/// Waits (generously, for slow CI) until `pid` is gone.
pub async fn wait_gone(pid: i32) -> bool {
    let deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < deadline {
        if !alive(pid) {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    false
}
