//! `harness doctor` checks for MCP servers: start, handshake, list tools.

use std::sync::Arc;
use std::time::{Duration, Instant};

use harness_config::ini::McpServer;

use crate::error::{McpError, Result};
use crate::protocol::Era;
use crate::server::{ClientOptions, ServerHandle};

/// What a healthy server reported.
#[derive(Debug, Clone, PartialEq)]
pub struct Report {
    /// Protocol generation.
    pub era: Era,
    /// Protocol version in use.
    pub version: String,
    /// Self-reported server name and version (`name version`).
    pub server: String,
    /// Tool names.
    pub tools: Vec<String>,
    /// Time to start, connect and list tools.
    pub elapsed: Duration,
}

impl Report {
    /// One line for the doctor report.
    pub fn summary(&self) -> String {
        let who = if self.server.trim().is_empty() {
            String::new()
        } else {
            format!("{}; ", self.server.trim())
        };
        format!(
            "{who}MCP {} ({}); {} tool(s); ready in {} ms",
            self.version,
            self.era.as_str(),
            self.tools.len(),
            self.elapsed.as_millis()
        )
    }
}

/// Starts `config`, completes the handshake, lists its tools and stops it.
/// The error says what to fix.
pub async fn check(name: &str, config: &McpServer, opts: ClientOptions) -> Result<Report> {
    if !config.enabled {
        return Err(McpError::Disabled(name.into()));
    }
    let started = Instant::now();
    let handle = ServerHandle::new(name, config.clone(), Arc::new(opts));
    let result = async {
        let negotiated = handle.start().await?;
        let tools = handle.list_tools().await?;
        Ok(Report {
            era: negotiated.era,
            version: negotiated.version,
            server: format!(
                "{} {}",
                negotiated.server_info.name, negotiated.server_info.version
            ),
            tools: tools.into_iter().map(|t| t.name).collect(),
            elapsed: started.elapsed(),
        })
    }
    .await;
    handle.shutdown().await;
    result
}

/// A suggested fix for a failed check.
pub fn fix_hint(name: &str, err: &McpError) -> String {
    match err {
        McpError::Disabled(_) => format!("set `enabled = true` in [mcp.{name}] to use it"),
        McpError::Spawn { .. } => format!(
            "install the server or fix `command` / `cwd` in [mcp.{name}]; run the command by hand to see why it fails"
        ),
        McpError::Secret { .. } => format!("fix the env.* / header.* credentials in [mcp.{name}]"),
        McpError::StartupTimeout { .. } => format!(
            "run the server by hand to see why it hangs, or raise `startup_timeout` in [mcp.{name}]"
        ),
        McpError::Timeout { .. } => format!("raise `timeout` in [mcp.{name}] or check the server"),
        McpError::Http { status: Some(401 | 403), .. } => format!(
            "check the header.* credentials (for example header.Authorization_env) in [mcp.{name}]"
        ),
        McpError::Http { .. } => format!("check `url` in [mcp.{name}] and that the server is reachable"),
        McpError::Auth { .. } => format!(
            "run `harness login mcp {name}` to authorize (it sets auth = oauth in [mcp.{name}])"
        ),
        McpError::NoCommonVersion { .. } => "update the server (or harness) to a common MCP version".into(),
        _ => "check the server's log lines (target harness_mcp::stderr) in the harness log, or run the server by hand; see docs/mcp.md".into(),
    }
}
