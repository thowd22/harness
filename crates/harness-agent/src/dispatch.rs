//! Tool-call dispatch: the tool registry, argument preparation, the policy
//! gate and execution order.
//!
//! One model turn may contain several `tool_use` blocks. The dispatcher:
//!
//! 1. [`prepare_calls`]: gives every call a unique id (duplicate ids are
//!    renamed in the assistant message), rejects unknown tools, parses and
//!    repairs the JSON arguments and validates them against the tool schema
//!    ([`crate::robustness`]). Rejected calls never run; their result is an
//!    actionable error.
//! 2. Asks the [`ToolGate`] about every runnable call of the turn at once, so
//!    parallel calls produce a single approval prompt.
//! 3. Runs consecutive read-only calls in parallel and mutating calls one by
//!    one, in the order the model gave them ([`Dispatcher::run`]).
//!
//! Every call is raced against the cancellation token. Dropping a tool's
//! future cancels it (the bash tool kills its process group on drop).

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use async_trait::async_trait;
use futures::future::join_all;
use harness_events::blobs::BlobStore;
use harness_events::schema::{
    BlobEncoding, EventKind, FileChangeKind, NodeId, OutputStream, Payload,
};
use harness_events::store::{EventDraft, RunRecorder};
use harness_policy::approvals::{ApprovalBroker, PendingApproval};
use harness_policy::rules::{
    Action, Decision, DecisionSink, Mode, NoDecisions, PolicyEngine, Verdict,
};
use harness_policy::staging::StagingGuard;
use harness_providers::model::{ContentBlock, ImageSource, Message, ToolResultPart, ToolSpec};
use harness_tools::bash::{self, BashTool, EnvScrubber};
use harness_tools::config::ToolsConfig;
use harness_tools::edit::{self, EditTool};
use harness_tools::exec::{Exec, StreamKind};
use harness_tools::read::{self, ReadTool};
use harness_tools::spill::{BlobSink, Spiller};
use harness_tools::tool::{
    ChangeKind, ToolContext, ToolError, ToolEvent, ToolEventSink, ToolOutput,
};
use harness_tools::tracker::ReadTracker;
use harness_tools::workspace::Workspace;
use harness_tools::write::{self, WriteTool};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::robustness::{self, Repair, ToolErrorMessage};

/// The MCP server tool behind a call (see [`AgentTool::mcp_target`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpTarget {
    /// `[mcp.<server>]` name.
    pub server: String,
    /// The tool's name on the server.
    pub tool: String,
}

impl McpTarget {
    /// The name the policy engine decides: `mcp:<server>/<tool>`.
    pub fn action_name(&self) -> String {
        harness_policy::mcp::action_name(&self.server, &self.tool)
    }
}

/// A tool the agent can offer to the model.
#[async_trait]
pub trait AgentTool: Send + Sync {
    /// Name as offered to the model.
    fn name(&self) -> &str;
    /// One-line description for the model.
    fn description(&self) -> &str;
    /// JSON schema of the arguments.
    fn schema(&self) -> Value;
    /// True when the tool never changes anything, so calls may run in
    /// parallel with other read-only calls.
    fn read_only(&self) -> bool;
    /// Runs the tool.
    async fn call(&self, ctx: &ToolContext, args: Value) -> Result<ToolOutput, ToolError>;

    /// For tools that forward to an MCP server: the server tool a call
    /// with `args` reaches. The policy gate then decides
    /// `mcp:<server>/<tool>` instead of the tool's own name, and the agent
    /// loop records an `mcp_call` event. `None` (the default) for
    /// everything else.
    fn mcp_target(&self, _args: &Value) -> Option<McpTarget> {
        None
    }

    /// The spec sent to the provider.
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: self.name().to_owned(),
            description: self.description().to_owned(),
            input_schema: self.schema(),
        }
    }
}

macro_rules! adapter {
    ($(#[$doc:meta])* $name:ident, $inner:ty, $module:ident, $read_only:expr) => {
        $(#[$doc])*
        pub struct $name(pub $inner);

        #[async_trait]
        impl AgentTool for $name {
            fn name(&self) -> &str {
                $module::NAME
            }
            fn description(&self) -> &str {
                $module::DESCRIPTION
            }
            fn schema(&self) -> Value {
                $module::schema()
            }
            fn read_only(&self) -> bool {
                $read_only
            }
            async fn call(&self, ctx: &ToolContext, args: Value) -> Result<ToolOutput, ToolError> {
                self.0.call(ctx, args).await
            }
        }
    };
}

adapter!(
    /// [`AgentTool`] adapter for the `read` tool (read-only).
    ReadAdapter, ReadTool, read, true
);
adapter!(
    /// [`AgentTool`] adapter for the `write` tool.
    WriteAdapter, WriteTool, write, false
);
adapter!(
    /// [`AgentTool`] adapter for the `edit` tool.
    EditAdapter, EditTool, edit, false
);

/// [`AgentTool`] adapter for the `bash` tool (always treated as mutating).
pub struct BashAdapter(pub Arc<BashTool>);

#[async_trait]
impl AgentTool for BashAdapter {
    fn name(&self) -> &str {
        bash::NAME
    }
    fn description(&self) -> &str {
        bash::DESCRIPTION
    }
    fn schema(&self) -> Value {
        bash::schema()
    }
    fn read_only(&self) -> bool {
        false
    }
    async fn call(&self, ctx: &ToolContext, args: Value) -> Result<ToolOutput, ToolError> {
        self.0.call(ctx, args).await
    }
}

/// Everything needed to build the standard tool set of one session.
pub struct StandardTools {
    /// Workspace the file tools operate on.
    pub workspace: Arc<dyn Workspace>,
    /// Process runner for bash (e.g. a sandboxed exec).
    pub exec: Arc<dyn Exec>,
    /// Starting directory of bash.
    pub cwd: PathBuf,
    /// `[tools]` limits.
    pub config: ToolsConfig,
    /// Removes secrets from the bash environment.
    pub scrubber: EnvScrubber,
    /// Where spilled output goes.
    pub spill: Arc<dyn BlobSink>,
}

/// The tools offered in a session, in offer order.
#[derive(Clone, Default)]
pub struct ToolSet {
    tools: Vec<Arc<dyn AgentTool>>,
    bash: Option<Arc<BashTool>>,
}

impl std::fmt::Debug for ToolSet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ToolSet")
            .field("tools", &self.names())
            .finish()
    }
}

impl ToolSet {
    /// An empty set.
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a tool (replacing one with the same name).
    pub fn with(mut self, tool: Arc<dyn AgentTool>) -> Self {
        self.register(tool);
        self
    }

    /// Adds a tool (replacing one with the same name).
    pub fn register(&mut self, tool: Arc<dyn AgentTool>) {
        self.tools.retain(|t| t.name() != tool.name());
        self.tools.push(tool);
    }

    /// read, write, edit and bash sharing one [`ReadTracker`] (one per
    /// session, so stale-read checks span the tools).
    pub fn standard(parts: StandardTools) -> Self {
        Self::standard_inner(parts, None)
    }

    /// Like [`ToolSet::standard`], but bash starts from `base_env` instead
    /// of this process's environment (still scrubbed, plus the
    /// non-interactive variables). Tools on a remote host pass an empty
    /// one so the local environment is never sent there.
    pub fn standard_with_bash_env(parts: StandardTools, base_env: Vec<(String, String)>) -> Self {
        Self::standard_inner(parts, Some(base_env))
    }

    fn standard_inner(parts: StandardTools, base_env: Option<Vec<(String, String)>>) -> Self {
        let tracker = Arc::new(ReadTracker::default());
        let ws = parts.workspace;
        let mut bash = BashTool::new(
            parts.exec,
            parts.cwd,
            parts.config.clone(),
            parts.scrubber,
            Spiller::new(parts.spill.clone(), &parts.config),
        );
        if let Some(env) = base_env {
            bash = bash.with_base_env(env);
        }
        let bash = Arc::new(bash);
        let mut set = Self::new()
            .with(Arc::new(ReadAdapter(
                ReadTool::new(ws.clone(), tracker.clone(), parts.config).with_spill(parts.spill),
            )))
            .with(Arc::new(WriteAdapter(WriteTool::new(
                ws.clone(),
                tracker.clone(),
            ))))
            .with(Arc::new(EditAdapter(EditTool::new(ws, tracker))))
            .with(Arc::new(BashAdapter(bash.clone())));
        set.bash = Some(bash);
        set
    }

    /// Adds every tool of `other` this set lacks (tools of the same name
    /// stay as they are in `self`). Use it to swap the standard tools of a
    /// base set for ones on another host while keeping its extra tools.
    pub fn fill_from(mut self, other: &ToolSet) -> ToolSet {
        for t in &other.tools {
            if self.get(t.name()).is_none() {
                self.tools.push(t.clone());
            }
        }
        if self.bash.is_none() {
            self.bash.clone_from(&other.bash);
        }
        self
    }

    /// Keeps only the tools named in `names`, in that order. Returns the
    /// names that are not available.
    pub fn select(&self, names: &[&str]) -> (ToolSet, Vec<String>) {
        let mut out = ToolSet {
            tools: Vec::new(),
            bash: self.bash.clone(),
        };
        let mut missing = Vec::new();
        for name in names {
            match self.get(name) {
                Some(t) => out.tools.push(t.clone()),
                None => missing.push((*name).to_owned()),
            }
        }
        (out, missing)
    }

    /// The tool called `name`.
    pub fn get(&self, name: &str) -> Option<&Arc<dyn AgentTool>> {
        self.tools.iter().find(|t| t.name() == name)
    }

    /// Tool names in offer order.
    pub fn names(&self) -> Vec<&str> {
        self.tools.iter().map(|t| t.name()).collect()
    }

    /// Specs for the provider request.
    pub fn specs(&self) -> Vec<ToolSpec> {
        self.tools.iter().map(|t| t.spec()).collect()
    }

    /// True when the set is empty.
    pub fn is_empty(&self) -> bool {
        self.tools.is_empty()
    }

    /// The bash tool's persisted working directory, if bash is in the set.
    pub fn bash_cwd(&self) -> Option<PathBuf> {
        self.bash.as_ref().map(|b| b.cwd())
    }
}

/// What will happen to a prepared call.
#[derive(Debug, Clone, PartialEq)]
pub enum CallPlan {
    /// Run the tool.
    Run {
        /// From [`AgentTool::read_only`].
        read_only: bool,
    },
    /// Do not run; send this error to the model.
    Reject {
        /// The model-facing error.
        message: String,
    },
}

/// A tool call after id, name and argument checks.
#[derive(Debug, Clone, PartialEq)]
pub struct PreparedCall {
    /// Unique call id (renamed when the model reused an id).
    pub id: String,
    /// Tool name as the model wrote it.
    pub tool: String,
    /// Arguments after repair and coercion (`{}` when unparseable).
    pub args: Value,
    /// The raw `input` the model sent.
    pub raw_input: Value,
    /// Run or reject.
    pub plan: CallPlan,
    /// Repairs applied to the arguments (logged and recorded).
    pub repairs: Vec<Repair>,
    /// Set when the id was a duplicate: the original id.
    pub duplicate_of: Option<String>,
}

/// Prepares every `tool_use` block of `message`.
///
/// `message` is updated in place so the conversation stays valid for the
/// provider: duplicate ids are renamed (`<id>__2`) and each block's input is
/// replaced by the repaired argument object (`{}` when it could not be
/// parsed).
pub fn prepare_calls(message: &mut Message, tools: &ToolSet) -> Vec<PreparedCall> {
    let mut seen: HashSet<String> = HashSet::new();
    let names = tools.names();
    let mut out = Vec::new();
    for block in &mut message.content {
        let ContentBlock::ToolUse { id, name, input } = block else {
            continue;
        };
        let mut duplicate_of = None;
        if !seen.insert(id.clone()) {
            let original = id.clone();
            let mut n = 2;
            while seen.contains(&format!("{original}__{n}")) {
                n += 1;
            }
            *id = format!("{original}__{n}");
            seen.insert(id.clone());
            tracing::warn!(target: "harness::tools", id = %original, renamed = %id, "duplicate tool call id");
            duplicate_of = Some(original);
        }
        let raw_input = input.clone();
        let mut repairs = Vec::new();
        let (args, plan) = match tools.get(name) {
            None => (
                raw_input.clone(),
                CallPlan::Reject {
                    message: ToolErrorMessage::unknown_tool(name, &names).to_string(),
                },
            ),
            Some(tool) => {
                let schema = tool.schema();
                match robustness::parse_arguments(input) {
                    Err(reason) => (
                        Value::Object(Default::default()),
                        CallPlan::Reject {
                            message: ToolErrorMessage::bad_json(name, &reason, &schema).to_string(),
                        },
                    ),
                    Ok(parsed) => {
                        repairs = parsed.repairs;
                        let mut args = parsed.value;
                        match robustness::validate(&schema, &mut args, &mut repairs) {
                            Ok(()) => (
                                args,
                                CallPlan::Run {
                                    read_only: tool.read_only(),
                                },
                            ),
                            Err(issues) => {
                                let message =
                                    ToolErrorMessage::invalid_args(name, &issues, &schema)
                                        .to_string();
                                (args, CallPlan::Reject { message })
                            }
                        }
                    }
                }
            }
        };
        if !repairs.is_empty() {
            tracing::info!(target: "harness::tools", tool = %name, id = %id, repairs = ?repairs, "repaired tool arguments");
        }
        if args.is_object() {
            *input = args.clone();
        } else {
            *input = Value::Object(Default::default());
        }
        out.push(PreparedCall {
            id: id.clone(),
            tool: name.clone(),
            args,
            raw_input,
            plan,
            repairs,
            duplicate_of,
        });
    }
    out
}

/// The gate's answer for one call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GateDecision {
    /// Run it.
    Allow,
    /// Refuse it; `reason` goes to the model.
    Deny {
        /// Model-facing reason.
        reason: String,
    },
}

/// A call as shown to the gate.
#[derive(Debug, Clone, PartialEq)]
pub struct GateCall {
    /// Call id.
    pub call_id: String,
    /// Tool name.
    pub tool: String,
    /// Validated arguments.
    pub args: Value,
}

/// Decides whether calls may run (policy and approvals).
#[async_trait]
pub trait ToolGate: Send + Sync {
    /// One decision per call, in order. All runnable calls of a turn are
    /// passed together so approvals can be batched into one prompt.
    async fn review(&self, node: Option<&NodeId>, calls: &[GateCall]) -> Vec<GateDecision>;
}

/// A gate that allows everything (tests, trusted automation).
#[derive(Debug, Clone, Copy, Default)]
pub struct AllowAll;

#[async_trait]
impl ToolGate for AllowAll {
    async fn review(&self, _node: Option<&NodeId>, calls: &[GateCall]) -> Vec<GateDecision> {
        vec![GateDecision::Allow; calls.len()]
    }
}

/// The policy engine plus the approval broker.
///
/// `allow` verdicts run, `deny` verdicts return the rule's reason to the
/// model, and every `ask` verdict of a turn goes to the broker in one batch.
pub struct PolicyGate {
    engine: PolicyEngine,
    broker: Arc<ApprovalBroker>,
    mode: Arc<Mutex<Mode>>,
    cwd: Arc<dyn Fn() -> PathBuf + Send + Sync>,
    staging: StagingGuard,
    decisions: Arc<dyn DecisionSink>,
    recorder: Option<RunRecorder>,
}

impl PolicyGate {
    /// A gate deciding with `engine` in `mode`; `ask` verdicts go to
    /// `broker` (which must share `engine.grants()`).
    pub fn new(engine: PolicyEngine, broker: Arc<ApprovalBroker>, mode: Mode) -> Self {
        let root = engine.root().to_path_buf();
        Self {
            engine,
            broker,
            mode: Arc::new(Mutex::new(mode)),
            cwd: Arc::new(move || root.clone()),
            staging: StagingGuard::default(),
            decisions: Arc::new(NoDecisions),
            recorder: None,
        }
    }

    /// Records every decision as a `policy_decision` event on the calling
    /// node, with the rule that matched.
    pub fn with_recorder(mut self, recorder: RunRecorder) -> Self {
        self.recorder = Some(recorder);
        self
    }

    /// Also passes every final decision to `sink`.
    pub fn with_decisions(mut self, sink: Arc<dyn DecisionSink>) -> Self {
        self.decisions = sink;
        self
    }

    /// Replaces the git staging guard (default: ask before `git add -A`
    /// and `git commit -a`).
    pub fn with_staging(mut self, staging: StagingGuard) -> Self {
        self.staging = staging;
        self
    }

    fn record(&self, node: Option<&NodeId>, call: &GateCall, verdict: &Verdict) {
        let Some(recorder) = &self.recorder else {
            return;
        };
        let name = |v: serde_json::Result<Value>| {
            v.ok()
                .and_then(|v| v.as_str().map(str::to_owned))
                .unwrap_or_default()
        };
        let kind = EventKind::PolicyDecision {
            call_id: call.call_id.clone(),
            tool: call.tool.clone(),
            decision: name(serde_json::to_value(verdict.decision)),
            layer: name(serde_json::to_value(verdict.rule.layer)),
            origin: verdict.rule.origin.clone(),
            rule: verdict.rule.rule.clone(),
            reason: verdict.rule.reason.clone(),
        };
        let draft = match node {
            Some(n) => EventDraft::node(n.clone(), kind),
            None => EventDraft::run(kind),
        };
        if let Err(e) = recorder.emit(draft) {
            tracing::warn!(target: "harness::policy", error = %e, "could not record policy decision");
        }
    }

    /// Resolve bash commands relative to `cwd()` (e.g. [`ToolSet::bash_cwd`]).
    pub fn with_cwd(mut self, cwd: Arc<dyn Fn() -> PathBuf + Send + Sync>) -> Self {
        self.cwd = cwd;
        self
    }

    /// Shared handle to the effective mode (the palette can change it).
    pub fn mode_handle(&self) -> Arc<Mutex<Mode>> {
        self.mode.clone()
    }
}

fn preview(tool: &str, args: &Value) -> Option<String> {
    let text = match tool {
        "bash" => args.get("command")?.as_str()?.to_owned(),
        // The MCP search-and-call tool: show the arguments sent to the server.
        t if t.starts_with(harness_policy::mcp::MCP_ACTION_PREFIX)
            && args.get("call").is_some() =>
        {
            serde_json::to_string_pretty(args.get("arguments").unwrap_or(&Value::Null)).ok()?
        }
        _ => serde_json::to_string_pretty(args).ok()?,
    };
    Some(robustness::truncate_chars(&text, 4000))
}

#[async_trait]
impl ToolGate for PolicyGate {
    async fn review(&self, node: Option<&NodeId>, calls: &[GateCall]) -> Vec<GateDecision> {
        let mode = *self.mode.lock().unwrap_or_else(|e| e.into_inner());
        self.review_in(node, calls, mode).await
    }
}

/// A [`PolicyGate`] whose mode never gets looser than `cap` (an agent's
/// `mode:`). The session mode still applies when it is stricter.
pub struct CappedGate {
    gate: Arc<PolicyGate>,
    cap: Mode,
}

impl CappedGate {
    /// Decides with `gate` in the stricter of its mode and `cap`.
    pub fn new(gate: Arc<PolicyGate>, cap: Mode) -> Self {
        Self { gate, cap }
    }
}

#[async_trait]
impl ToolGate for CappedGate {
    async fn review(&self, node: Option<&NodeId>, calls: &[GateCall]) -> Vec<GateDecision> {
        let session = *self.gate.mode.lock().unwrap_or_else(|e| e.into_inner());
        // `Ask < AutoEdit < Auto`: the stricter mode is the smaller one.
        self.gate
            .review_in(node, calls, session.min(self.cap))
            .await
    }
}

impl PolicyGate {
    /// Reviews `calls` as [`ToolGate::review`] does, in `mode`.
    pub async fn review_in(
        &self,
        node: Option<&NodeId>,
        calls: &[GateCall],
        mode: Mode,
    ) -> Vec<GateDecision> {
        let cwd = (self.cwd)();
        let mut out = Vec::with_capacity(calls.len());
        let mut pending = Vec::new();
        for (i, call) in calls.iter().enumerate() {
            let action = Action::from_tool_call(&call.tool, &call.args);
            let verdict =
                self.staging
                    .decide(&self.engine, &action, mode, &cwd, self.decisions.as_ref());
            self.record(node, call, &verdict);
            match verdict.decision {
                Decision::Allow => out.push(GateDecision::Allow),
                Decision::Deny => out.push(GateDecision::Deny {
                    reason: format!(
                        "blocked by policy ({}: `{}`): {}",
                        verdict.rule.origin, verdict.rule.rule, verdict.rule.reason
                    ),
                }),
                Decision::Ask => {
                    out.push(GateDecision::Allow);
                    pending.push((
                        i,
                        PendingApproval {
                            call_id: call.call_id.clone(),
                            node_id: node.cloned(),
                            action,
                            preview: preview(&call.tool, &call.args),
                            verdict,
                        },
                    ));
                }
            }
        }
        if !pending.is_empty() {
            let (idx, reqs): (Vec<usize>, Vec<PendingApproval>) = pending.into_iter().unzip();
            let resolutions = self.broker.request(reqs).await;
            for (i, res) in idx.into_iter().zip(resolutions) {
                if !res.approved {
                    let why = res.reason.unwrap_or_else(|| "no reason given".into());
                    out[i] = GateDecision::Deny {
                        reason: format!("the approval was denied ({}: {why})", res.by),
                    };
                }
            }
        }
        out
    }
}

/// The result of one call, ready to be sent to the model.
#[derive(Debug, Clone, PartialEq)]
pub struct CallOutcome {
    /// Call id.
    pub id: String,
    /// Tool name.
    pub tool: String,
    /// Result parts (text and images).
    pub content: Vec<ToolResultPart>,
    /// True on failure, rejection, denial or cancellation.
    pub is_error: bool,
    /// Wall time in milliseconds.
    pub duration_ms: u64,
    /// True when the call was cancelled before it finished.
    pub cancelled: bool,
}

impl CallOutcome {
    /// An error outcome that never ran.
    pub fn error(call: &PreparedCall, message: impl Into<String>) -> Self {
        Self {
            id: call.id.clone(),
            tool: call.tool.clone(),
            content: vec![ToolResultPart::Text {
                text: message.into(),
            }],
            is_error: true,
            duration_ms: 0,
            cancelled: false,
        }
    }

    /// Concatenated text of the result parts.
    pub fn text(&self) -> String {
        let parts: Vec<&str> = self
            .content
            .iter()
            .filter_map(|p| match p {
                ToolResultPart::Text { text } => Some(text.as_str()),
                ToolResultPart::Image { .. } => None,
            })
            .collect();
        parts.join("\n")
    }

    /// The `tool_result` block for the next request.
    pub fn to_block(&self) -> ContentBlock {
        ContentBlock::ToolResult {
            tool_use_id: self.id.clone(),
            content: self.content.clone(),
            is_error: self.is_error,
        }
    }
}

/// Message used for calls interrupted by cancellation.
pub const CANCELLED_RESULT: &str =
    "Cancelled by the user before the tool finished. Do not assume it took effect.";

/// Runs prepared calls through the gate and the tools.
pub struct Dispatcher {
    tools: ToolSet,
    gate: Arc<dyn ToolGate>,
    events: Arc<dyn ToolEventSink>,
    supports_images: bool,
}

impl Dispatcher {
    /// A dispatcher over `tools` guarded by `gate`.
    pub fn new(tools: ToolSet, gate: Arc<dyn ToolGate>) -> Self {
        Self {
            tools,
            gate,
            events: Arc::new(harness_tools::tool::NoEvents),
            supports_images: false,
        }
    }

    /// Send tool side events (file changes, ...) to `events`.
    pub fn with_events(mut self, events: Arc<dyn ToolEventSink>) -> Self {
        self.events = events;
        self
    }

    /// Whether the model accepts image results.
    pub fn with_images(mut self, supports_images: bool) -> Self {
        self.supports_images = supports_images;
        self
    }

    /// The tool set.
    pub fn tools(&self) -> &ToolSet {
        &self.tools
    }

    /// Runs `calls` and returns one outcome per call, in order.
    ///
    /// Consecutive read-only calls run concurrently; every mutating call
    /// runs alone, after everything before it finished. `on_done` is called
    /// as each call finishes (for live events). After cancellation, calls
    /// that did not finish get a [`CANCELLED_RESULT`] outcome.
    pub async fn run(
        &self,
        node: Option<&NodeId>,
        calls: &[PreparedCall],
        cancel: &CancellationToken,
        on_done: &(dyn Fn(&PreparedCall, &CallOutcome) + Send + Sync),
    ) -> Vec<CallOutcome> {
        let runnable: Vec<usize> = calls
            .iter()
            .enumerate()
            .filter(|(_, c)| matches!(c.plan, CallPlan::Run { .. }))
            .map(|(i, _)| i)
            .collect();
        let gate_calls: Vec<GateCall> = runnable
            .iter()
            .map(|&i| GateCall {
                call_id: calls[i].id.clone(),
                // MCP calls are decided as `mcp:<server>/<tool>`.
                tool: self
                    .tools
                    .get(&calls[i].tool)
                    .and_then(|t| t.mcp_target(&calls[i].args))
                    .map_or_else(|| calls[i].tool.clone(), |m| m.action_name()),
                args: calls[i].args.clone(),
            })
            .collect();
        let decisions = if gate_calls.is_empty() {
            Vec::new()
        } else {
            tokio::select! {
                d = self.gate.review(node, &gate_calls) => d,
                _ = cancel.cancelled() => vec![GateDecision::Deny { reason: CANCELLED_RESULT.into() }; gate_calls.len()],
            }
        };
        let mut allowed = vec![false; calls.len()];
        let mut outcomes: Vec<Option<CallOutcome>> = vec![None; calls.len()];
        for (k, &i) in runnable.iter().enumerate() {
            match decisions.get(k) {
                Some(GateDecision::Allow) => allowed[i] = true,
                Some(GateDecision::Deny { reason }) => {
                    let c = &calls[i];
                    let msg = ToolErrorMessage::new(
                        &c.tool,
                        format!("not run: {reason}"),
                        "Do not retry the same call. Choose an allowed alternative or ask the user.",
                    );
                    outcomes[i] = Some(CallOutcome::error(c, msg.to_string()));
                }
                None => {
                    outcomes[i] = Some(CallOutcome::error(
                        &calls[i],
                        "Error: the call was not reviewed. Fix: retry it.",
                    ));
                }
            }
        }
        for (i, c) in calls.iter().enumerate() {
            if let CallPlan::Reject { message } = &c.plan {
                outcomes[i] = Some(CallOutcome::error(c, message.clone()));
            }
        }
        // Report the calls that will never run first, in order.
        for (i, c) in calls.iter().enumerate() {
            if let Some(o) = &outcomes[i] {
                on_done(c, o);
            }
        }

        let mut i = 0;
        while i < calls.len() {
            if !allowed[i] {
                i += 1;
                continue;
            }
            let read_only = matches!(calls[i].plan, CallPlan::Run { read_only: true });
            let mut group = vec![i];
            if read_only {
                let mut j = i + 1;
                while j < calls.len() {
                    match (&calls[j].plan, allowed[j]) {
                        (CallPlan::Run { read_only: true }, true) => group.push(j),
                        (_, false) => {}
                        _ => break,
                    }
                    j += 1;
                }
            }
            let last = *group.last().unwrap_or(&i);
            let futures = group.iter().map(|&k| async move {
                let o = self.run_one(&calls[k], cancel).await;
                on_done(&calls[k], &o);
                (k, o)
            });
            for (k, o) in join_all(futures).await {
                outcomes[k] = Some(o);
            }
            i = last + 1;
        }
        outcomes
            .into_iter()
            .zip(calls)
            .map(|(o, c)| o.unwrap_or_else(|| CallOutcome::error(c, CANCELLED_RESULT)))
            .collect()
    }

    async fn run_one(&self, call: &PreparedCall, cancel: &CancellationToken) -> CallOutcome {
        let start = Instant::now();
        let mut outcome = CallOutcome::error(call, CANCELLED_RESULT);
        outcome.cancelled = true;
        if cancel.is_cancelled() {
            return outcome;
        }
        let Some(tool) = self.tools.get(&call.tool) else {
            return CallOutcome::error(
                call,
                ToolErrorMessage::unknown_tool(&call.tool, &self.tools.names()).to_string(),
            );
        };
        let ctx = ToolContext::new(call.id.clone())
            .with_images(self.supports_images)
            .with_events(self.events.clone());
        let result = tokio::select! {
            r = tool.call(&ctx, call.args.clone()) => Some(r),
            _ = cancel.cancelled() => None,
        };
        outcome.duration_ms = u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX);
        match result {
            None => outcome,
            Some(Ok(out)) => {
                outcome.cancelled = false;
                outcome.is_error = false;
                outcome.content = output_parts(out);
                outcome
            }
            Some(Err(e)) => {
                outcome.cancelled = false;
                outcome.content = vec![ToolResultPart::Text {
                    text: robustness::truncate_chars(
                        &format!("Error in `{}`: {}", call.tool, e.message),
                        robustness::MAX_ERROR_CHARS * 4,
                    ),
                }];
                outcome
            }
        }
    }
}

fn output_parts(out: ToolOutput) -> Vec<ToolResultPart> {
    match out {
        ToolOutput::Text(text) => vec![ToolResultPart::Text { text }],
        ToolOutput::Image {
            media_type,
            data,
            caption,
        } => vec![
            ToolResultPart::Text { text: caption },
            ToolResultPart::Image {
                source: ImageSource::Base64 {
                    media_type,
                    data: base64_encode(&data),
                },
            },
        ],
    }
}

/// Standard base64 with padding (for image results).
pub fn base64_encode(data: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for (k, shift) in [18, 12, 6, 0].into_iter().enumerate() {
            if k <= chunk.len() {
                out.push(ALPHABET[((n >> shift) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// Maps [`ToolEvent`]s onto the run's event log: file changes become
/// `file_changed` events and live process output becomes `tool_output`
/// events on the agent's node (split UTF-8 sequences are held until they
/// complete). Spill records are only traced.
pub struct RecorderToolEvents {
    recorder: RunRecorder,
    node: NodeId,
    root: PathBuf,
    partial: Mutex<std::collections::HashMap<(String, OutputStream), Vec<u8>>>,
}

impl RecorderToolEvents {
    /// Events for `node`; paths are made relative to `root`.
    pub fn new(recorder: RunRecorder, node: NodeId, root: impl Into<PathBuf>) -> Self {
        Self {
            recorder,
            node,
            root: root.into(),
            partial: Mutex::new(std::collections::HashMap::new()),
        }
    }

    /// Decodes `data` after any incomplete UTF-8 tail held from the last
    /// chunk of the same stream.
    fn decode(&self, call_id: &str, stream: OutputStream, data: &[u8]) -> String {
        let mut held = self.partial.lock().unwrap_or_else(|e| e.into_inner());
        let buf = held.entry((call_id.to_owned(), stream)).or_default();
        buf.extend_from_slice(data);
        let keep = match std::str::from_utf8(buf) {
            Ok(_) => 0,
            // An incomplete sequence at the end: wait for the rest.
            Err(e) if e.error_len().is_none() => buf.len() - e.valid_up_to(),
            Err(_) => 0,
        };
        let tail = buf.split_off(buf.len() - keep);
        let text = String::from_utf8_lossy(buf).into_owned();
        *buf = tail;
        text
    }
}

fn relative(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .display()
        .to_string()
}

impl ToolEventSink for RecorderToolEvents {
    fn emit(&self, call_id: &str, event: ToolEvent) {
        match event {
            ToolEvent::FileChanged(change) => {
                let kind = EventKind::FileChanged {
                    path: relative(&self.root, &change.path),
                    change: match change.kind {
                        ChangeKind::Created => FileChangeKind::Created,
                        ChangeKind::Modified => FileChangeKind::Modified,
                    },
                    diff: Some(Payload::Text(change.diff)),
                };
                if let Err(e) = self
                    .recorder
                    .emit(EventDraft::node(self.node.clone(), kind))
                {
                    tracing::warn!(target: "harness::tools", error = %e, "could not record file change");
                }
            }
            ToolEvent::OutputSpilled(rec) => {
                tracing::debug!(target: "harness::tools", call_id, blob = %rec.blob_id, saved = rec.tokens_saved(), "tool output spilled");
            }
            ToolEvent::Output { stream, data } => {
                let stream = match stream {
                    StreamKind::Stdout => OutputStream::Stdout,
                    StreamKind::Stderr => OutputStream::Stderr,
                };
                let text = self.decode(call_id, stream, &data);
                if text.is_empty() {
                    return;
                }
                let kind = EventKind::ToolOutput {
                    call_id: call_id.to_owned(),
                    stream,
                    text,
                };
                if let Err(e) = self
                    .recorder
                    .emit(EventDraft::node(self.node.clone(), kind))
                {
                    tracing::warn!(target: "harness::tools", error = %e, "could not record tool output");
                }
            }
        }
    }
}

/// [`BlobSink`] over a run's [`BlobStore`], so spilled tool output lands in
/// the run's `blobs/` directory. Ids are the full sha256.
#[derive(Debug, Clone)]
pub struct RunBlobSink(pub BlobStore);

impl BlobSink for RunBlobSink {
    fn put(&self, data: &[u8]) -> std::io::Result<String> {
        Ok(self.0.put(data, BlobEncoding::Text)?.sha256)
    }

    fn get(&self, id: &str) -> std::io::Result<Option<Vec<u8>>> {
        if id.len() != 64 || !id.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Ok(None);
        }
        match std::fs::read(self.0.path_for(id)) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }
}

#[cfg(test)]
mod tests;
