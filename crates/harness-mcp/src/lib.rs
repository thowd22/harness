//! MCP (Model Context Protocol) client for harness.
//!
//! harness connects to MCP servers that the user grants in `harness.ini`
//! (`[mcp.<name>]` sections; see [`harness_config::ini::McpServer`]).
//! This crate provides:
//!
//! - [`stdio`] and [`http`]: the stdio and streamable HTTP transports
//!   (JSON-RPC 2.0, newline-delimited on stdio; JSON or SSE responses over
//!   HTTP).
//! - [`client`]: one connection, with dual-era version negotiation: the
//!   modern stateless protocol (`2026-07-28`, per-request `_meta`,
//!   `server/discover`) and the legacy `initialize` handshake
//!   (`2025-11-25` and earlier), ping, `tools/list` and `tools/call`.
//! - [`server`]: managed servers with lazy start, restart with backoff on
//!   crash, per-request timeouts and clean shutdown.
//! - [`auth`]: OAuth for remote servers (`auth = oauth`): discovery,
//!   registration, the browser flow behind `harness login mcp <name>`, the
//!   protected token store and automatic refresh.
//! - [`tools`]: MCP tools for agents: explicit selection, the schema
//!   budget and the `mcp_tools` search-and-call tool, results (redacted,
//!   size-capped) and tool-list refresh.
//! - [`prompts`]: prompts (`prompts/list`, `prompts/get`) and resources
//!   (`resources/list`, `resources/read`), which the TUI offers as
//!   namespaced slash commands and attachable context.
//! - [`doctor`]: the `harness doctor` check.
//! - [`test_server`]: a minimal MCP server for tests over both transports
//!   (also built as the `harness-mcp-test-server` binary).
//!
//! Stdio servers run in their own session and process group; shutdown and
//! drop signal the whole group with `killpg` (never `kill` with a negative
//! id), and [`stdio::kill_all`] covers exit paths without destructors.
//! Server stderr goes to the log (target `harness_mcp::stderr`), redacted.

pub mod auth;
pub mod client;
pub mod doctor;
pub mod error;
pub mod headers;
pub mod http;
pub mod prompts;
pub mod protocol;
pub mod server;
pub mod stdio;
pub mod test_server;
pub mod tools;

pub use client::{Connection, Negotiated};
pub use error::{McpError, Result};
pub use protocol::{CallToolResult, Era, ServerInfo, Tool};
pub use server::{ClientOptions, ServerHandle, Servers, Status};
pub use stdio::kill_all;
pub use tools::{McpToolbox, ToolboxOptions};
