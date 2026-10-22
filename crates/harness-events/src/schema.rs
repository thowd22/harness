//! Versioned event schema types.
//!
//! A run is stored as one [`RunHeader`] line followed by one [`RunEvent`] per
//! line (see [`crate::store`]). Every event is an envelope carrying the run id,
//! the node it belongs to, that node's parent, a monotonic sequence number and
//! a timestamp; the payload is an [`EventKind`].
//!
//! The JSON shape is part of the on-disk format. Changing it requires bumping
//! [`SCHEMA_VERSION`] and adding an explicit step to [`migrate_event`].

use std::fmt;

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use uuid::Uuid;

/// Current schema version written into every run header.
pub const SCHEMA_VERSION: u32 = 1;

/// Oldest schema version this build can read (via [`migrate_event`]).
pub const MIN_SUPPORTED_VERSION: u32 = 1;

/// Errors raised by schema migration.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum SchemaError {
    /// The run was written by a newer harness, or by one too old to migrate.
    #[error(
        "unsupported event schema version {found} (this build reads {MIN_SUPPORTED_VERSION}..={SCHEMA_VERSION})"
    )]
    UnsupportedVersion {
        /// Version found in the run header.
        found: u32,
    },
}

/// Upgrades one raw event written with schema `from` to [`SCHEMA_VERSION`].
///
/// Every supported older version must have an explicit arm here; anything
/// else is rejected rather than guessed at.
pub fn migrate_event(
    from: u32,
    event: serde_json::Value,
) -> Result<serde_json::Value, SchemaError> {
    match from {
        SCHEMA_VERSION => Ok(event),
        // Future: `1 => migrate_event(2, v1_to_v2(event))`, and so on.
        found => Err(SchemaError::UnsupportedVersion { found }),
    }
}

/// Unique id of a run (UUID v7, so ids sort by creation time).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RunId(pub Uuid);

impl RunId {
    /// A fresh, time-ordered run id.
    pub fn new() -> Self {
        Self(Uuid::now_v7())
    }
}

impl Default for RunId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for RunId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// Id of a node in a run: an agent, subagent, workflow step, shell or verify
/// step. Any unique string works; [`NodeId::random`] makes a UUID v7.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct NodeId(pub String);

impl NodeId {
    /// A fresh unique node id.
    pub fn random() -> Self {
        Self(Uuid::now_v7().to_string())
    }

    /// The id as a string slice.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<&str> for NodeId {
    fn from(s: &str) -> Self {
        Self(s.to_owned())
    }
}

impl From<String> for NodeId {
    fn from(s: String) -> Self {
        Self(s)
    }
}

impl fmt::Display for NodeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// First line of `events.jsonl`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunHeader {
    /// Schema version the events in this file were written with.
    pub schema_version: u32,
    /// The run.
    pub run_id: RunId,
    /// When the run was created.
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    /// Version of harness that wrote the run.
    pub harness_version: String,
    /// `harness_config::effective::config_hash` of the configuration and
    /// YAML the run used (absent when unknown).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config_hash: Option<String>,
}

impl RunHeader {
    /// A header for `run_id` with the current schema version and time.
    pub fn new(run_id: RunId) -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            run_id,
            created_at: OffsetDateTime::now_utc(),
            harness_version: env!("CARGO_PKG_VERSION").to_owned(),
            config_hash: None,
        }
    }
}

/// One persisted event: envelope plus [`EventKind`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunEvent {
    /// Monotonic sequence number within the run, starting at 0.
    pub seq: u64,
    /// Wall-clock time the event was recorded.
    #[serde(with = "time::serde::rfc3339")]
    pub ts: OffsetDateTime,
    /// The run this event belongs to.
    pub run_id: RunId,
    /// Node the event belongs to; `None` for run-level events.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_id: Option<NodeId>,
    /// Parent of `node_id` (e.g. the agent that spawned a subagent).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_node_id: Option<NodeId>,
    /// What happened.
    #[serde(flatten)]
    pub kind: EventKind,
}

/// Token usage of a model call. All counts are additive.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    /// Uncached input tokens.
    #[serde(default)]
    pub input: u64,
    /// Input tokens served from the prompt cache.
    #[serde(default)]
    pub cached_input: u64,
    /// Output tokens.
    #[serde(default)]
    pub output: u64,
    /// Reasoning tokens (subset of or in addition to output, per provider).
    #[serde(default)]
    pub reasoning: u64,
}

impl Usage {
    /// Adds `other` into `self`.
    pub fn add(&mut self, other: &Usage) {
        self.input += other.input;
        self.cached_input += other.cached_input;
        self.output += other.output;
        self.reasoning += other.reasoning;
    }
}

/// How a blob's bytes should be interpreted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BlobEncoding {
    /// UTF-8 text (was a [`Payload::Text`]).
    Text,
    /// Serialized JSON (was a [`Payload::Json`]).
    Json,
}

/// Reference to content in the run's blob store ([`crate::blobs`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlobRef {
    /// Lowercase hex SHA-256 of the bytes.
    pub sha256: String,
    /// Size in bytes.
    pub size: u64,
    /// Original payload type.
    pub encoding: BlobEncoding,
}

/// A potentially large payload: inline text, inline JSON, or a blob reference.
///
/// The recorder moves inline payloads above its threshold into the blob store.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Payload {
    /// Inline text.
    Text(String),
    /// Inline JSON value.
    Json(serde_json::Value),
    /// Stored in the blob store.
    Blob(BlobRef),
}

impl Payload {
    /// Inline text payload.
    pub fn text(s: impl Into<String>) -> Self {
        Self::Text(s.into())
    }
}

/// Final status of a run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    /// Still running (no `run_finished` yet).
    Running,
    /// Finished successfully.
    Succeeded,
    /// Finished with an error.
    Failed,
    /// Cancelled by the user or a budget.
    Cancelled,
}

/// Status of a node, matching the map icons in doc-5.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeStatus {
    /// `○` not started.
    Queued,
    /// `◐` running.
    Running,
    /// `⏸` waiting for an approval.
    WaitingApproval,
    /// `✔` finished ok.
    Succeeded,
    /// `✗` failed.
    Failed,
    /// `⊘` skipped (e.g. a dependency failed).
    Skipped,
    /// `⊘` cancelled.
    Cancelled,
}

impl NodeStatus {
    /// True for statuses a node cannot leave.
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Succeeded | Self::Failed | Self::Skipped | Self::Cancelled
        )
    }
}

/// What kind of node a `node_started` event creates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeKind {
    /// Top-level agent session.
    Agent,
    /// Agent spawned by another agent (`spawn_agent`).
    Subagent,
    /// Workflow (or nested workflow) step container.
    Workflow,
    /// Shell step.
    Shell,
    /// Verify step.
    Verify,
    /// Approval gate.
    Approval,
}

/// The primary action offered with an [`EventKind::Error`]: a labelled link
/// the user can open to resolve the error.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorAction {
    /// Button label, such as `Manage usage`.
    pub label: String,
    /// The URL the button opens.
    pub url: String,
}

/// One task of a [`EventKind::Tasks`] snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskItem {
    /// Stable id.
    pub id: String,
    /// What to do.
    pub title: String,
    /// `pending`, `in_progress`, `blocked`, `done`, `failed` or `cancelled`.
    pub status: String,
    /// The agent node working on it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<NodeId>,
    /// That agent's name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
}

/// Output stream of a [`EventKind::ToolOutput`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutputStream {
    /// Standard output.
    Stdout,
    /// Standard error.
    Stderr,
}

/// Which stream a [`EventKind::Delta`] belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeltaChannel {
    /// Visible output text.
    Text,
    /// Reasoning / thinking text.
    Reasoning,
    /// Streaming tool-call arguments.
    ToolArgs,
}

/// Decision on an approval request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalDecision {
    /// Allowed once.
    Approved,
    /// Allowed and remembered.
    ApprovedAlways,
    /// Refused.
    Denied,
}

/// Kind of file change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileChangeKind {
    /// New file.
    Created,
    /// Existing file changed.
    Modified,
    /// File removed.
    Deleted,
}

/// What happened to a memory entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryAction {
    /// Memory was injected into context.
    Recalled,
    /// New memory written.
    Written,
    /// Existing memory updated.
    Updated,
    /// Memory deleted or forgotten.
    Deleted,
}

/// Event payloads. Serialized with a `"type"` tag in snake_case, flattened
/// into the [`RunEvent`] envelope.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[non_exhaustive]
pub enum EventKind {
    /// The run began.
    RunStarted {
        /// Human-readable name (workflow or agent name).
        name: String,
        /// Workflow name when the run is a workflow.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        workflow: Option<String>,
        /// Host the run executes on (`None` = local).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        host: Option<String>,
    },
    /// The run ended.
    RunFinished {
        /// Final status.
        status: RunStatus,
        /// Error message on failure.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    /// A node (agent, subagent, step) was created. The envelope's
    /// `parent_node_id` links subagents to their parent.
    NodeStarted {
        /// Node kind.
        kind: NodeKind,
        /// Display name (step id or agent name).
        name: String,
        /// Agent profile, if any.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        agent: Option<String>,
        /// Model, if any.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model: Option<String>,
        /// Host badge (`None` = local).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        host: Option<String>,
        /// Workflow dependencies (`needs`).
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        needs: Vec<NodeId>,
        /// True when the node is only queued and has not begun executing.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        queued: bool,
    },
    /// A queued node began executing.
    NodeRunning,
    /// A node is being retried (or a loop iterates).
    NodeRetry {
        /// 1-based attempt number now starting.
        attempt: u32,
        /// Why it is retried.
        reason: String,
    },
    /// A node ended.
    NodeFinished {
        /// Terminal status.
        status: NodeStatus,
        /// Error message on failure.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    /// A model request was sent.
    ModelCallStarted {
        /// Id correlating deltas and the finish event.
        call_id: String,
        /// Provider name.
        provider: String,
        /// Model id.
        model: String,
        /// Request payload as sent (redacted).
        request: Payload,
    },
    /// Streaming chunk of a model response.
    Delta {
        /// Model call this chunk belongs to.
        call_id: String,
        /// Output, reasoning or tool arguments.
        channel: DeltaChannel,
        /// The chunk text.
        text: String,
    },
    /// A model call finished.
    ModelCallFinished {
        /// Model call id.
        call_id: String,
        /// Token usage.
        usage: Usage,
        /// Provider stop reason.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stop_reason: Option<String>,
        /// Raw response, if recorded.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        response: Option<Payload>,
        /// Wall time of the call in milliseconds.
        duration_ms: u64,
    },
    /// The model asked to call a tool.
    ToolCall {
        /// Tool call id.
        call_id: String,
        /// Tool name.
        tool: String,
        /// Arguments.
        args: Payload,
    },
    /// A tool finished.
    ToolResult {
        /// Tool call id.
        call_id: String,
        /// Whether the tool succeeded.
        ok: bool,
        /// Output as given to the model (redacted).
        output: Payload,
        /// Wall time in milliseconds.
        duration_ms: u64,
    },
    /// An approval prompt was raised.
    ApprovalRequested {
        /// Approval id.
        approval_id: String,
        /// Short description of the action.
        action: String,
        /// Preview shown to the user (diff, command, ...).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        preview: Option<Payload>,
    },
    /// An approval prompt was answered.
    ApprovalResolved {
        /// Approval id.
        approval_id: String,
        /// The decision.
        decision: ApprovalDecision,
        /// Who decided (`user`, `policy`, ...).
        by: String,
    },
    /// A file in the workspace changed.
    FileChanged {
        /// Workspace-relative path.
        path: String,
        /// Kind of change.
        change: FileChangeKind,
        /// Unified diff, if available.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        diff: Option<Payload>,
    },
    /// A verify command started.
    VerifyStarted {
        /// The command line.
        command: String,
    },
    /// A verify command finished.
    VerifyFinished {
        /// The command line.
        command: String,
        /// Whether verification passed.
        passed: bool,
        /// Process exit code, if it exited.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        exit_code: Option<i32>,
        /// Wall time in milliseconds.
        duration_ms: u64,
        /// Captured output.
        output: Payload,
    },
    /// Memory was recalled or changed.
    Memory {
        /// What happened.
        action: MemoryAction,
        /// Memory entry id.
        memory_id: String,
        /// One-line summary.
        summary: String,
    },
    /// Cost incurred (added to the node's and run's totals).
    Cost {
        /// Cost in US dollars.
        usd: f64,
    },
    /// An error worth recording (node failures use `node_finished`).
    Error {
        /// Message.
        message: String,
        /// Whether the run continued.
        recoverable: bool,
        /// The primary way out of the error, if there is one (for example
        /// "Manage usage" when a ChatGPT plan limit is hit). Views show it
        /// as the error's primary button.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        action: Option<ErrorAction>,
    },
    /// The user sent a message to the agent on this node.
    UserMessage {
        /// The message text (pastes expanded).
        text: String,
    },
    /// Live output of a running tool call (bash stdout/stderr). The final
    /// `tool_result` still carries the complete (possibly spilled) output.
    ToolOutput {
        /// The tool call.
        call_id: String,
        /// Which stream the text came from.
        stream: OutputStream,
        /// The output text (stream-redacted like deltas).
        text: String,
    },
    /// The policy engine decided a tool call.
    PolicyDecision {
        /// The tool call.
        call_id: String,
        /// Tool name.
        tool: String,
        /// `allow`, `ask` or `deny`.
        decision: String,
        /// Layer of the deciding rule (`built_in`, `ini`, `yaml`, `session`, `mode`).
        layer: String,
        /// Where the rule came from (e.g. `harness.ini [policy]`).
        origin: String,
        /// The matching rule (pattern or description).
        rule: String,
        /// Why the rule applies.
        reason: String,
    },
    /// An operation on a remote host (audit trail).
    RemoteOp {
        /// `[remote.<name>]` host name.
        host: String,
        /// Operation (`fs.read`, `exec.start`, ...).
        op: String,
        /// What it acted on (path, command).
        target: String,
        /// Whether it succeeded.
        ok: bool,
        /// Error text on failure.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
        /// Duration in milliseconds.
        duration_ms: u64,
        /// Bytes not transferred because the other side already held the
        /// content (remote content-hash cache); omitted when 0.
        #[serde(default, skip_serializing_if = "is_zero")]
        bytes_saved: u64,
    },
    /// The run's task list (goal bar) after a change: a full snapshot,
    /// so the latest event alone shows the current state.
    Tasks {
        /// Goal line.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        goal: Option<String>,
        /// Every task in creation order.
        tasks: Vec<TaskItem>,
        /// Board version (strictly increasing).
        version: u64,
    },
    /// The connection to a remote host changed state.
    RemoteConnection {
        /// `[remote.<name>]` host name.
        host: String,
        /// `connecting`, `connected`, `reconnecting`, `disconnected`, `failed`, `job_lost`.
        state: String,
        /// Details (reason, attempt).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
    },
}

impl EventKind {
    /// Mutable references to every [`Payload`] in this event.
    pub fn payloads_mut(&mut self) -> Vec<&mut Payload> {
        match self {
            Self::ModelCallStarted { request, .. } => vec![request],
            Self::ModelCallFinished { response, .. } => response.iter_mut().collect(),
            Self::ToolCall { args, .. } => vec![args],
            Self::ToolResult { output, .. } => vec![output],
            Self::ApprovalRequested { preview, .. } => preview.iter_mut().collect(),
            Self::FileChanged { diff, .. } => diff.iter_mut().collect(),
            Self::VerifyFinished { output, .. } => vec![output],
            _ => Vec::new(),
        }
    }

    /// The snake_case `type` tag of this event.
    pub fn type_name(&self) -> &'static str {
        match self {
            Self::RunStarted { .. } => "run_started",
            Self::RunFinished { .. } => "run_finished",
            Self::NodeStarted { .. } => "node_started",
            Self::NodeRunning => "node_running",
            Self::NodeRetry { .. } => "node_retry",
            Self::NodeFinished { .. } => "node_finished",
            Self::ModelCallStarted { .. } => "model_call_started",
            Self::Delta { .. } => "delta",
            Self::ModelCallFinished { .. } => "model_call_finished",
            Self::ToolCall { .. } => "tool_call",
            Self::ToolResult { .. } => "tool_result",
            Self::ApprovalRequested { .. } => "approval_requested",
            Self::ApprovalResolved { .. } => "approval_resolved",
            Self::FileChanged { .. } => "file_changed",
            Self::VerifyStarted { .. } => "verify_started",
            Self::VerifyFinished { .. } => "verify_finished",
            Self::Memory { .. } => "memory",
            Self::Cost { .. } => "cost",
            Self::Error { .. } => "error",
            Self::UserMessage { .. } => "user_message",
            Self::ToolOutput { .. } => "tool_output",
            Self::PolicyDecision { .. } => "policy_decision",
            Self::RemoteOp { .. } => "remote_op",
            Self::RemoteConnection { .. } => "remote_connection",
            Self::Tasks { .. } => "tasks",
        }
    }
}

#[allow(clippy::trivially_copy_pass_by_ref)] // serde's skip_serializing_if passes a reference
fn is_zero(n: &u64) -> bool {
    *n == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(kind: EventKind) -> RunEvent {
        RunEvent {
            seq: 3,
            ts: time::macros::datetime!(2026-10-22 12:00:00 UTC),
            run_id: RunId(Uuid::nil()),
            node_id: Some("n1".into()),
            parent_node_id: Some("root".into()),
            kind,
        }
    }

    fn all_kinds() -> Vec<EventKind> {
        vec![
            EventKind::RunStarted {
                name: "r".into(),
                workflow: Some("w".into()),
                host: None,
            },
            EventKind::RunFinished {
                status: RunStatus::Succeeded,
                error: None,
            },
            EventKind::NodeStarted {
                kind: NodeKind::Subagent,
                name: "n".into(),
                agent: Some("researcher".into()),
                model: Some("haiku".into()),
                host: Some("gpu".into()),
                needs: vec!["a".into()],
                queued: true,
            },
            EventKind::NodeRunning,
            EventKind::NodeRetry {
                attempt: 2,
                reason: "flaky".into(),
            },
            EventKind::NodeFinished {
                status: NodeStatus::Failed,
                error: Some("x".into()),
            },
            EventKind::ModelCallStarted {
                call_id: "c".into(),
                provider: "fake".into(),
                model: "m".into(),
                request: Payload::Json(serde_json::json!({"messages": []})),
            },
            EventKind::Delta {
                call_id: "c".into(),
                channel: DeltaChannel::Reasoning,
                text: "hm".into(),
            },
            EventKind::ModelCallFinished {
                call_id: "c".into(),
                usage: Usage {
                    input: 1,
                    cached_input: 2,
                    output: 3,
                    reasoning: 4,
                },
                stop_reason: Some("end_turn".into()),
                response: None,
                duration_ms: 5,
            },
            EventKind::ToolCall {
                call_id: "t".into(),
                tool: "bash".into(),
                args: Payload::text("ls"),
            },
            EventKind::ToolResult {
                call_id: "t".into(),
                ok: true,
                output: Payload::Blob(BlobRef {
                    sha256: "ab".into(),
                    size: 9,
                    encoding: BlobEncoding::Text,
                }),
                duration_ms: 1,
            },
            EventKind::ApprovalRequested {
                approval_id: "a".into(),
                action: "rm".into(),
                preview: None,
            },
            EventKind::ApprovalResolved {
                approval_id: "a".into(),
                decision: ApprovalDecision::Denied,
                by: "user".into(),
            },
            EventKind::FileChanged {
                path: "src/x.rs".into(),
                change: FileChangeKind::Modified,
                diff: None,
            },
            EventKind::VerifyStarted {
                command: "cargo test".into(),
            },
            EventKind::VerifyFinished {
                command: "cargo test".into(),
                passed: false,
                exit_code: Some(101),
                duration_ms: 8,
                output: Payload::text("2 failed"),
            },
            EventKind::Memory {
                action: MemoryAction::Written,
                memory_id: "m1".into(),
                summary: "s".into(),
            },
            EventKind::Cost { usd: 0.25 },
            EventKind::Error {
                message: "e".into(),
                recoverable: true,
                action: None,
            },
            EventKind::Error {
                message: "usage limit".into(),
                recoverable: false,
                action: Some(ErrorAction {
                    label: "Manage usage".into(),
                    url: "https://example.com/usage".into(),
                }),
            },
            EventKind::UserMessage { text: "hi".into() },
            EventKind::ToolOutput {
                call_id: "t".into(),
                stream: OutputStream::Stderr,
                text: "warning".into(),
            },
            EventKind::PolicyDecision {
                call_id: "t".into(),
                tool: "bash".into(),
                decision: "ask".into(),
                layer: "ini".into(),
                origin: "harness.ini [policy]".into(),
                rule: "git push*".into(),
                reason: "bash.ask".into(),
            },
            EventKind::RemoteOp {
                host: "gpu".into(),
                op: "fs.read".into(),
                target: "src/main.rs".into(),
                ok: false,
                error: Some("not found".into()),
                duration_ms: 3,
                bytes_saved: 0,
            },
            EventKind::RemoteOp {
                host: "gpu".into(),
                op: "fs.read".into(),
                target: "src/lib.rs".into(),
                ok: true,
                error: None,
                duration_ms: 1,
                bytes_saved: 4096,
            },
            EventKind::RemoteConnection {
                host: "gpu".into(),
                state: "reconnecting".into(),
                detail: Some("attempt 1/3".into()),
            },
            EventKind::Tasks {
                goal: Some("ship".into()),
                tasks: vec![TaskItem {
                    id: "t1".into(),
                    title: "write tests".into(),
                    status: "in_progress".into(),
                    owner: Some("n1".into()),
                    agent: Some("coder".into()),
                }],
                version: 3,
            },
        ]
    }

    #[test]
    fn every_kind_round_trips_with_type_tag() {
        for kind in all_kinds() {
            let event = ev(kind);
            let json = serde_json::to_string(&event).unwrap();
            let value: serde_json::Value = serde_json::from_str(&json).unwrap();
            assert_eq!(value["type"], event.kind.type_name(), "{json}");
            assert_eq!(value["seq"], 3);
            assert_eq!(value["node_id"], "n1");
            assert_eq!(value["parent_node_id"], "root");
            let back: RunEvent = serde_json::from_str(&json).unwrap();
            assert_eq!(back, event);
        }
    }

    #[test]
    fn error_action_is_optional_on_the_wire() {
        // Events written before `action` existed still parse.
        let old: EventKind =
            serde_json::from_str(r#"{"type":"error","message":"m","recoverable":false}"#).unwrap();
        assert_eq!(
            old,
            EventKind::Error {
                message: "m".into(),
                recoverable: false,
                action: None,
            }
        );
        let json = serde_json::to_string(&old).unwrap();
        assert!(!json.contains("action"), "{json}");
    }

    #[test]
    fn run_level_event_omits_node_fields() {
        let mut event = ev(EventKind::Cost { usd: 1.0 });
        event.node_id = None;
        event.parent_node_id = None;
        let json = serde_json::to_string(&event).unwrap();
        assert!(!json.contains("node_id"), "{json}");
    }

    #[test]
    fn migrate_accepts_current_and_rejects_others() {
        let v = serde_json::json!({"x": 1});
        assert_eq!(migrate_event(SCHEMA_VERSION, v.clone()), Ok(v.clone()));
        assert_eq!(
            migrate_event(SCHEMA_VERSION + 1, v.clone()),
            Err(SchemaError::UnsupportedVersion {
                found: SCHEMA_VERSION + 1
            })
        );
        assert!(migrate_event(0, v).is_err());
    }
}
