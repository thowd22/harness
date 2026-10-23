//! MCP servers and tools for a session or workflow run.
//!
//! [`wire`] builds, from `[mcp.<name>]` and `[policy] mcp.*` in
//! harness.ini, the servers (started lazily, `startup = eager` ones in the
//! background), the [`McpToolbox`] that gives agents the MCP tools they
//! select, and the policy rules for MCP calls. Agents that select nothing
//! start nothing.

use std::sync::Arc;

use harness_agent::dispatch::RunBlobSink;
use harness_config::ini::Config;
use harness_events::store::RunRecorder;
use harness_mcp::{ClientOptions, McpToolbox, Servers, ToolboxOptions};
use harness_policy::mcp::{McpRules, ReadOnlyTools};
use harness_tools::config::ToolsConfig;
use harness_tools::spill::Spiller;

/// MCP for one run.
#[derive(Clone)]
pub(crate) struct McpWiring {
    /// Agent tools from the configured servers.
    pub toolbox: McpToolbox,
    /// `[policy] mcp.*`, sharing read-only knowledge with the toolbox.
    pub rules: McpRules,
}

impl McpWiring {
    /// Stops every server.
    pub async fn shutdown(&self) {
        self.toolbox.servers().shutdown_all().await;
    }
}

/// MCP servers, tools and rules for a run recording to `recorder`.
/// `secrets` are the credentials harness already knows (redacted from
/// results and scrubbed from server environments).
pub(crate) fn wire(cfg: &Config, secrets: &[String], recorder: &RunRecorder) -> McpWiring {
    let read_only = Arc::new(ReadOnlyTools::default());
    let mut opts = ClientOptions::from_config(cfg);
    for s in secrets {
        if !opts.known_secrets.contains(s) {
            opts.known_secrets.push(s.clone());
        }
    }
    let servers = Arc::new(Servers::from_config(cfg, opts));
    if tokio::runtime::Handle::try_current().is_ok() {
        servers.start_eager();
    }
    let spill = Spiller::new(
        Arc::new(RunBlobSink(recorder.blobs().clone())),
        &ToolsConfig::default(),
    );
    let toolbox = McpToolbox::new(
        servers,
        ToolboxOptions::from_config(cfg, Some(spill)),
        read_only.clone(),
    );
    toolbox.set_recorder(recorder.clone());
    McpWiring {
        toolbox,
        rules: McpRules::new(cfg.policy.mcp.clone(), read_only),
    }
}

/// Grants an `--allow` pattern for the run: `mcp:<server>/<tool>` patterns
/// allow MCP tools, anything else is a command pattern.
pub(crate) fn grant(grants: &harness_policy::approvals::SessionGrants, pattern: &str) {
    if pattern.starts_with(harness_policy::mcp::MCP_ACTION_PREFIX) {
        grants.allow_tool(pattern);
    } else {
        grants.allow_command(pattern);
    }
}
