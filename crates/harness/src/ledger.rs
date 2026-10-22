//! Bash-made file changes for the change ledger.

use std::sync::Arc;

use async_trait::async_trait;
use harness_agent::dispatch::AgentTool;
use harness_hygiene::ledger::Ledger;
use harness_tools::tool::{ToolContext, ToolError, ToolOutput};
use serde_json::Value;

use crate::memory::NodeHandle;

/// Wraps the `bash` tool: the worktree is compared before and after each
/// command and every file the command created, modified or deleted is
/// recorded by the [`Ledger`] as a `file_changed` event on the current node
/// (edits by write/edit are recorded by the tools themselves).
pub struct LedgerBash {
    inner: Arc<dyn AgentTool>,
    ledger: Arc<Ledger>,
    node: NodeHandle,
}

impl LedgerBash {
    /// Watches `inner` with `ledger`; changes go on the node in `node`.
    pub fn new(inner: Arc<dyn AgentTool>, ledger: Arc<Ledger>, node: NodeHandle) -> Self {
        Self {
            inner,
            ledger,
            node,
        }
    }
}

#[async_trait]
impl AgentTool for LedgerBash {
    fn name(&self) -> &str {
        self.inner.name()
    }

    fn description(&self) -> &str {
        self.inner.description()
    }

    fn schema(&self) -> Value {
        self.inner.schema()
    }

    fn read_only(&self) -> bool {
        self.inner.read_only()
    }

    async fn call(&self, ctx: &ToolContext, args: Value) -> Result<ToolOutput, ToolError> {
        let command = args
            .get("command")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let ledger = self.ledger.clone();
        let watch = tokio::task::spawn_blocking(move || ledger.begin_command())
            .await
            .ok()
            .and_then(|r| {
                r.map_err(|e| tracing::warn!(target: "harness::ledger", error = %e, "worktree snapshot failed"))
                    .ok()
            });
        let out = self.inner.call(ctx, args).await;
        if let Some(watch) = watch {
            let ledger = self.ledger.clone();
            let node = self.node.lock().unwrap_or_else(|e| e.into_inner()).clone();
            let done = tokio::task::spawn_blocking(move || {
                ledger.end_command(watch, node.as_ref(), &command)
            })
            .await;
            if let Ok(Err(e)) = done {
                tracing::warn!(target: "harness::ledger", error = %e, "worktree snapshot failed");
            }
        }
        out
    }
}
