//! The `memory` tool as an agent tool, recording drive changes as
//! `memory` events.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use harness_agent::dispatch::AgentTool;
use harness_events::schema::{EventKind, NodeId};
use harness_events::store::{EventDraft, RunRecorder};
use harness_memory::tool::{MemoryArgs, MemoryTool};
use harness_tools::tool::{ToolContext, ToolError, ToolErrorKind, ToolOutput};
use serde_json::Value;

/// The node memory events are recorded on; the core moves it to each new
/// turn's node.
pub type NodeHandle = Arc<Mutex<Option<NodeId>>>;

/// [`MemoryTool`] behind the [`AgentTool`] interface. Every write or forget
/// that changed the drive is recorded as
/// `memory{action, memory_id: <commit sha>, summary}`, which the chat shows
/// as an "updated memory" card with undo.
pub struct MemoryAdapter {
    tool: MemoryTool,
    description: &'static str,
    recorder: RunRecorder,
    node: NodeHandle,
}

impl MemoryAdapter {
    /// Wraps `tool`; events go to `recorder` on the node in `node`.
    pub fn new(tool: MemoryTool, recorder: RunRecorder, node: NodeHandle) -> Self {
        let description = match tool.access() {
            harness_config::agents::MemoryAccess::ReadWrite => harness_memory::tool::DESCRIPTION,
            _ => harness_memory::tool::READ_ONLY_DESCRIPTION,
        };
        Self {
            tool,
            description,
            recorder,
            node,
        }
    }
}

#[async_trait]
impl AgentTool for MemoryAdapter {
    fn name(&self) -> &str {
        harness_memory::tool::NAME
    }

    fn description(&self) -> &str {
        self.description
    }

    fn schema(&self) -> Value {
        harness_memory::tool::schema(self.tool.access()).unwrap_or(Value::Null)
    }

    fn read_only(&self) -> bool {
        // Writes are serialised by the drive's publish lock.
        true
    }

    async fn call(&self, ctx: &ToolContext, args: Value) -> Result<ToolOutput, ToolError> {
        let args: MemoryArgs = serde_json::from_value(args).map_err(|e| {
            ToolError::new(
                ToolErrorKind::InvalidArgs,
                format!(
                    "invalid memory arguments: {e}. Use one of: {{\"action\":\"search\",\"query\":...}}, {{\"action\":\"read\",\"path\":...}}, {{\"action\":\"write\",\"path\":...,\"text\":...}}, {{\"action\":\"forget\",\"path\":...,\"text\":...}}."
                ),
            )
        })?;
        let outcome = self.tool.run(ctx, args).await?;
        if let Some(change) = &outcome.change {
            let kind = EventKind::Memory {
                action: change.action,
                memory_id: change.commit.commit.clone(),
                summary: change.summary(),
            };
            let node = self.node.lock().unwrap_or_else(|e| e.into_inner()).clone();
            let draft = match node {
                Some(n) => EventDraft::node(n, kind),
                None => EventDraft::run(kind),
            };
            if let Err(e) = self.recorder.emit(draft) {
                tracing::warn!(target: "harness::memory", error = %e, "could not record memory change");
            }
        }
        Ok(ToolOutput::Text(outcome.output))
    }
}
