//! Errors from MCP servers and transports.
//!
//! Messages are short and say how to fix the problem; they never contain
//! credential values (header and env values are not part of any variant).

use std::time::Duration;

use crate::protocol::RpcError;

/// Result alias for this crate.
pub type Result<T, E = McpError> = std::result::Result<T, E>;

/// Something went wrong talking to an MCP server.
#[derive(Debug, Clone, thiserror::Error)]
#[non_exhaustive]
pub enum McpError {
    /// The server is disabled in harness.ini.
    #[error("MCP server `{0}` is disabled; set `enabled = true` in [mcp.{0}] to use it")]
    Disabled(String),
    /// The user disabled the server for this session (the TUI `/mcp`
    /// panel).
    #[error("MCP server `{0}` is disabled for this session; enable it again in /mcp (enable {0})")]
    DisabledForSession(String),
    /// A credential for `env.*` / `header.*` could not be resolved.
    #[error("[mcp.{server}] {key}: {message}")]
    Secret {
        /// Server name.
        server: String,
        /// `env.NAME` or `header.Name`.
        key: String,
        /// What failed (never the value).
        message: String,
    },
    /// The stdio server could not be started.
    #[error("could not start `{command}`: {message}; check `command` and `cwd` in [mcp.{server}]")]
    Spawn {
        /// Server name.
        server: String,
        /// The program.
        command: String,
        /// The OS error.
        message: String,
    },
    /// Starting or the handshake took longer than `startup_timeout`.
    #[error(
        "MCP server `{server}` did not finish starting within {limit:?}; raise `startup_timeout` in [mcp.{server}] or check the server log{stderr}"
    )]
    StartupTimeout {
        /// Server name.
        server: String,
        /// The limit.
        limit: Duration,
        /// Recent stderr, prefixed with `: `, or empty.
        stderr: String,
    },
    /// A request took longer than `timeout`.
    #[error(
        "MCP server `{server}` did not answer `{method}` within {limit:?}; the request was cancelled (raise `timeout` in [mcp.{server}] if the tool is slow)"
    )]
    Timeout {
        /// Server name.
        server: String,
        /// The JSON-RPC method.
        method: String,
        /// The limit.
        limit: Duration,
    },
    /// The server process exited or the connection closed.
    #[error("MCP server `{server}` exited ({status}){stderr}; it will be restarted on next use")]
    Exited {
        /// Server name.
        server: String,
        /// Exit status description.
        status: String,
        /// Recent stderr, prefixed with `; stderr: `, or empty.
        stderr: String,
    },
    /// The server kept crashing; restarts stopped.
    #[error(
        "MCP server `{server}` failed {failures} times in a row and will not be restarted this session: {last}"
    )]
    GaveUp {
        /// Server name.
        server: String,
        /// Consecutive failures.
        failures: u32,
        /// The last failure.
        last: String,
    },
    /// The server answered with a JSON-RPC error.
    #[error("MCP server `{server}` returned an error for `{method}`: {error}")]
    Rpc {
        /// Server name.
        server: String,
        /// The JSON-RPC method.
        method: String,
        /// The error.
        error: RpcError,
    },
    /// The server broke the protocol (malformed message, bad version, ...).
    #[error("MCP server `{server}` broke the protocol: {message}")]
    Protocol {
        /// Server name.
        server: String,
        /// What was wrong.
        message: String,
    },
    /// An HTTP request failed or returned an error status.
    #[error("MCP server `{server}`: {message}")]
    Http {
        /// Server name.
        server: String,
        /// HTTP status, if a response arrived.
        status: Option<u16>,
        /// JSON-RPC error in the response body, if any.
        rpc: Option<RpcError>,
        /// What failed.
        message: String,
    },
    /// A legacy HTTP session expired (404 with a session id).
    #[error("MCP server `{0}` ended the session")]
    SessionExpired(String),
    /// The server does not share a protocol version with harness.
    #[error(
        "MCP server `{server}` supports protocol versions {supported:?}, but harness supports {ours:?}; update the server or harness"
    )]
    NoCommonVersion {
        /// Server name.
        server: String,
        /// Versions the server listed.
        supported: Vec<String>,
        /// Versions harness supports.
        ours: Vec<String>,
    },
    /// OAuth authorization is missing, expired or was refused. The message
    /// says how to authorize (`harness login mcp <name>`); harness never
    /// opens a browser outside that command and the TUI `/login mcp`.
    #[error("MCP server `{server}`: {message}")]
    Auth {
        /// Server name.
        server: String,
        /// What is wrong and what to run.
        message: String,
    },
    /// The connection was shut down.
    #[error("MCP server `{0}` was shut down")]
    Closed(String),
}

impl McpError {
    /// The JSON-RPC error carried by this error, if any.
    pub fn rpc_error(&self) -> Option<&RpcError> {
        match self {
            McpError::Rpc { error, .. } => Some(error),
            McpError::Http { rpc, .. } => rpc.as_ref(),
            _ => None,
        }
    }

    /// True when the server process died or the connection dropped, so a
    /// restart may help.
    pub fn is_connection_lost(&self) -> bool {
        matches!(self, McpError::Exited { .. } | McpError::Closed(_))
    }
}
