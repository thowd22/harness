//! The core agent loop: request, stream, tool calls, end turn.
//!
//! [`AgentLoop::run`] drives one agent node through an explicit state
//! machine ([`TurnState`]):
//!
//! ```text
//!  Request ──► Streaming ──► Dispatch ──► Submit ──► Request ...
//!     │            │            │                       │
//!     └────────────┴────────────┴──► Done(StopReason) ◄─┘
//! ```
//!
//! - **Request**: checks cancellation, `max_turns` and the budget, repairs
//!   the history (every `tool_use` has a result), builds the
//!   [`ChatRequest`] and records `model_call_started`.
//! - **Streaming**: forwards deltas as `delta` events, folds the stream into
//!   a response and records `model_call_finished` and `cost`.
//! - **Dispatch**: prepares the tool calls (ids, JSON repair, schema
//!   validation), records `tool_call`, runs them through the
//!   [`Dispatcher`] and records each `tool_result`.
//! - **Submit**: appends the tool results to the conversation.
//! - **Done**: optionally runs the [`Verifier`] and records `node_finished`
//!   with a status that identifies the [`StopReason`].
//!
//! Every event goes through the [`RunRecorder`], which redacts it. The
//! conversation keeps exactly what was recorded (redacted messages and
//! labeled tool results), so the model never receives raw secrets and a
//! session can be rebuilt byte-for-byte from the log
//! ([`crate::session::rebuild_conversation`]).
//!
//! Cancellation: the token is passed to [`Provider::stream`] and raced
//! against the stream and every tool call. A cancelled turn still leaves a
//! valid conversation (interrupted tool calls get an error result).
//!
//! Extension points:
//!
//! - [`RequestHook`]s (see [`hooks`]) run before every request. Context
//!   management ([`crate::compaction::ContextManager`]) and budgets
//!   ([`crate::budget::BudgetGuard`]) plug in here.
//! - Done-gating: when the model ends its turn and a [`Verifier`] is set,
//!   the harness runs it (always, or only after changes, see
//!   [`VerifyWhen`]). A failure is sent back to the model as a user message
//!   up to [`LoopConfig::max_fix_attempts`] times. The final
//!   [`Verification`] is `verified`, `unverified` or `failed`; a failed
//!   verification marks the node `failed` with the
//!   [`codes::VERIFY_FAILED`] code.

pub mod hooks;

pub use hooks::{CallReport, HookDecision, RequestContext, RequestHook};

use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;
use futures::StreamExt;
use harness_events::schema::{
    DeltaChannel, ErrorAction, EventKind, NodeId, NodeKind, NodeStatus, Payload, RunId,
    Usage as EventUsage,
};
use harness_events::store::{EventDraft, RunRecorder};
use harness_policy::provenance::Labeler;
use harness_providers::model::{
    ChatRequest, ContentBlock, Message, Role, StopKind, ThinkingConfig, ToolChoice, ToolResultPart,
    Usage,
};
use harness_providers::provider::{Provider, ProviderError, ResponseBuilder, StreamEvent};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio_util::sync::CancellationToken;

use crate::dispatch::{CallOutcome, Dispatcher, PreparedCall, prepare_calls};
use crate::robustness::{self, LoopDetector};

/// Why a loop run ended. Each reason maps to a distinct node status / error
/// code in the event log (see [`StopReason::code`]).
#[derive(Debug, Clone, PartialEq)]
pub enum StopReason {
    /// The model finished its turn.
    EndTurn,
    /// The `max_turns` limit was reached.
    MaxTurns {
        /// The limit.
        limit: u32,
    },
    /// A token or cost budget was exhausted.
    Budget {
        /// Which budget and by how much.
        reason: String,
    },
    /// The user cancelled.
    Cancelled,
    /// An unrecoverable error (provider failure, context overflow, ...).
    Error {
        /// What failed.
        message: String,
        /// The primary way out, when the failure has one (for example
        /// *Manage usage* for a ChatGPT plan usage limit). It is not part of
        /// `node_finished.error`, so [`StopReason::from_node_finished`]
        /// recovers it as `None`; the `error` event carries it.
        action: Option<ErrorAction>,
    },
}

/// Stable codes used as the prefix of `node_finished.error`.
pub mod codes {
    /// [`super::StopReason::MaxTurns`].
    pub const MAX_TURNS: &str = "max_turns";
    /// [`super::StopReason::Budget`].
    pub const BUDGET: &str = "budget";
    /// [`super::StopReason::Cancelled`].
    pub const CANCELLED: &str = "cancelled";
    /// [`super::StopReason::Error`].
    pub const ERROR: &str = "error";
    /// [`super::StopReason::EndTurn`].
    pub const END_TURN: &str = "end_turn";
    /// The model ended its turn but verification still failed after every
    /// fix attempt (node status `failed`, stop reason `EndTurn`).
    pub const VERIFY_FAILED: &str = "verify_failed";
}

impl StopReason {
    /// Stable snake_case code: `end_turn`, `max_turns`, `budget`,
    /// `cancelled` or `error`.
    pub fn code(&self) -> &'static str {
        match self {
            Self::EndTurn => codes::END_TURN,
            Self::MaxTurns { .. } => codes::MAX_TURNS,
            Self::Budget { .. } => codes::BUDGET,
            Self::Cancelled => codes::CANCELLED,
            Self::Error { .. } => codes::ERROR,
        }
    }

    /// The node status recorded for this reason.
    pub fn node_status(&self) -> NodeStatus {
        match self {
            Self::EndTurn => NodeStatus::Succeeded,
            Self::Cancelled => NodeStatus::Cancelled,
            Self::MaxTurns { .. } | Self::Budget { .. } | Self::Error { .. } => NodeStatus::Failed,
        }
    }

    /// The `node_finished.error` text: `<code>: <detail>`, or `None` for
    /// [`StopReason::EndTurn`].
    pub fn error_text(&self) -> Option<String> {
        let detail = match self {
            Self::EndTurn => return None,
            Self::MaxTurns { limit } => format!("stopped after {limit} model turns"),
            Self::Budget { reason } => reason.clone(),
            Self::Cancelled => "cancelled by the user".into(),
            Self::Error { message, .. } => message.clone(),
        };
        Some(format!("{}: {detail}", self.code()))
    }

    /// Recovers the reason from a recorded `node_finished` event.
    pub fn from_node_finished(status: NodeStatus, error: Option<&str>) -> Self {
        let (code, detail) = error
            .and_then(|e| e.split_once(": "))
            .unwrap_or((error.unwrap_or(""), ""));
        match (status, code) {
            (NodeStatus::Succeeded, _) => Self::EndTurn,
            (NodeStatus::Cancelled, _) => Self::Cancelled,
            (_, codes::VERIFY_FAILED) => Self::EndTurn,
            (_, codes::MAX_TURNS) => Self::MaxTurns {
                limit: detail
                    .split_whitespace()
                    .find_map(|w| w.parse().ok())
                    .unwrap_or(0),
            },
            (_, codes::BUDGET) => Self::Budget {
                reason: detail.to_owned(),
            },
            (_, codes::ERROR) => Self::Error {
                message: detail.to_owned(),
                action: None,
            },
            _ => Self::Error {
                message: error.unwrap_or("unknown failure").to_owned(),
                action: None,
            },
        }
    }
}

/// States of the turn state machine.
#[derive(Debug, Clone, PartialEq)]
pub enum TurnState {
    /// Build and send the next model request.
    Request,
    /// Consume the model stream.
    Streaming,
    /// Run the tool calls of the last response.
    Dispatch,
    /// Append tool results to the conversation.
    Submit,
    /// Stop.
    Done(StopReason),
}

/// The message history sent to the provider plus the system prompt.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Conversation {
    /// System prompt (stable for the session).
    pub system: Option<String>,
    /// Messages, oldest first.
    pub messages: Vec<Message>,
    /// Byte offsets in `system` where its later layers (memory index,
    /// skill listing) start; see [`crate::context::SessionPrefix`].
    pub system_layers: Vec<usize>,
    logged: Option<Logged>,
}

#[derive(Debug, Clone, PartialEq)]
struct Logged {
    run: RunId,
    messages: usize,
    system_sha: Option<String>,
    tools_sha: String,
}

impl Conversation {
    /// An empty conversation with `system`.
    pub fn new(system: Option<String>) -> Self {
        Self {
            system,
            messages: Vec::new(),
            system_layers: Vec::new(),
            logged: None,
        }
    }

    /// A conversation whose system prompt is a frozen session prefix: the
    /// layer boundaries travel with every request (cache breakpoints).
    pub fn from_prefix(prefix: &crate::context::SessionPrefix) -> Self {
        Self {
            system: Some(prefix.text().to_owned()),
            messages: Vec::new(),
            system_layers: prefix
                .layers()
                .iter()
                .skip(1)
                .map(|l| l.range.start)
                .collect(),
            logged: None,
        }
    }

    /// A conversation with existing messages (e.g. rebuilt on resume).
    pub fn with_messages(system: Option<String>, messages: Vec<Message>) -> Self {
        Self {
            system,
            messages,
            system_layers: Vec::new(),
            logged: None,
        }
    }

    /// Index of the first message not yet recorded in a `model_call_started`
    /// event (0 before the first request). Hooks may change messages from
    /// here on without invalidating the log or the prompt cache.
    pub fn unlogged_from(&self) -> usize {
        self.logged
            .as_ref()
            .map_or(0, |l| l.messages.min(self.messages.len()))
    }

    /// Appends a user message.
    pub fn push_user(&mut self, text: impl Into<String>) {
        self.messages.push(Message::user(text));
    }

    /// Marks `messages[index..]` as changed, so the next request records
    /// them again (`messages_from <= index`). Call this after rewriting
    /// earlier messages (elision, compaction); session resume then rebuilds
    /// the rewritten history.
    pub fn invalidate_log_from(&mut self, index: usize) {
        if let Some(l) = &mut self.logged {
            l.messages = l.messages.min(index);
        }
    }

    /// Text of the last assistant message.
    pub fn last_assistant_text(&self) -> String {
        self.messages
            .iter()
            .rev()
            .find(|m| m.role == Role::Assistant)
            .map(Message::text)
            .unwrap_or_default()
    }
}

/// Limits for one loop run. `None` means unlimited.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct LoopLimits {
    /// Maximum model calls.
    pub max_turns: Option<u32>,
    /// Maximum total tokens (input incl. cache + output).
    pub max_tokens: Option<u64>,
    /// Maximum cost in USD (only known costs count).
    pub max_usd: Option<f64>,
}

/// Computes the cost of one call from its usage; `None` = unknown.
pub type CostFn = Arc<dyn Fn(&Usage) -> Option<f64> + Send + Sync>;

/// Static settings of one agent loop.
#[derive(Debug, Clone, PartialEq)]
pub struct LoopConfig {
    /// Agent name (node name and `agent` field).
    pub agent: String,
    /// Model id sent to the provider.
    pub model: String,
    /// Node kind recorded in `node_started`.
    pub node_kind: NodeKind,
    /// `max_tokens` for each request.
    pub max_tokens: Option<u32>,
    /// Reasoning settings.
    pub thinking: Option<ThinkingConfig>,
    /// Tool choice (keep `Auto` for models that reject forced choices).
    pub tool_choice: ToolChoice,
    /// Turn, token and cost limits.
    pub limits: LoopLimits,
    /// When the [`Verifier`] runs. Default [`VerifyWhen::Always`].
    pub verify_when: VerifyWhen,
    /// How many times a verification failure is sent back to the model
    /// for a fix (`[verify] max_fix_attempts`). Default 0: verify once and
    /// report.
    pub max_fix_attempts: u32,
    /// Host the tools run on (`[remote.<name>]`), recorded in
    /// `node_started.host`; `None` is local.
    pub host: Option<String>,
}

/// When done-gating runs the [`Verifier`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum VerifyWhen {
    /// Every time the model ends its turn.
    #[default]
    Always,
    /// Only when the run made a successful mutating tool call (anything
    /// that is not read-only, including bash).
    OnChanges,
}

/// Final verification status of a loop run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verification {
    /// The verifier passed.
    Verified,
    /// No verifier ran (none configured, no changes, or the run did not end
    /// its turn).
    Unverified,
    /// The verifier still failed after every fix attempt.
    Failed,
}

impl Verification {
    /// From the last verifier result (`None` = did not run).
    pub fn from_result(verified: Option<bool>) -> Self {
        match verified {
            Some(true) => Self::Verified,
            Some(false) => Self::Failed,
            None => Self::Unverified,
        }
    }

    /// `verified`, `unverified` or `failed`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Verified => "verified",
            Self::Unverified => "unverified",
            Self::Failed => "failed",
        }
    }
}

impl std::fmt::Display for Verification {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl LoopConfig {
    /// Defaults for `agent` on `model`.
    pub fn new(agent: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            agent: agent.into(),
            model: model.into(),
            node_kind: NodeKind::Agent,
            max_tokens: None,
            thinking: None,
            tool_choice: ToolChoice::Auto,
            limits: LoopLimits::default(),
            verify_when: VerifyWhen::Always,
            max_fix_attempts: 0,
            host: None,
        }
    }
}

/// Result of a verify command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifyResult {
    /// True when verification passed.
    pub passed: bool,
    /// Exit code, if the process exited.
    pub exit_code: Option<i32>,
    /// Captured output.
    pub output: String,
}

/// Runs the project's verification after the model ends its turn.
#[async_trait]
pub trait Verifier: Send + Sync {
    /// The command line, for events and messages.
    fn command(&self) -> String;
    /// Runs it.
    async fn verify(&self, cancel: &CancellationToken) -> VerifyResult;
}

/// What a loop run produced.
#[derive(Debug, Clone, PartialEq)]
pub struct LoopOutcome {
    /// Why it stopped.
    pub stop: StopReason,
    /// The agent node.
    pub node: NodeId,
    /// Model calls made.
    pub turns: u32,
    /// Summed usage.
    pub usage: Usage,
    /// Summed known cost in USD.
    pub cost_usd: f64,
    /// True when some call's cost was unknown.
    pub cost_unknown: bool,
    /// Text of the final assistant message.
    pub final_text: String,
    /// Result of the last verifier run, when one ran.
    pub verified: Option<bool>,
    /// Final verification status.
    pub verification: Verification,
    /// Verification failures sent back to the model.
    pub fix_attempts: u32,
}

/// Drives one agent through the turn state machine.
pub struct AgentLoop {
    provider: Arc<dyn Provider>,
    dispatcher: Dispatcher,
    recorder: RunRecorder,
    config: LoopConfig,
    cost: Option<CostFn>,
    labeler: Option<Labeler>,
    verifier: Option<Arc<dyn Verifier>>,
    parent: Option<NodeId>,
    node: Option<NodeId>,
    loops: LoopDetector,
    hooks: Vec<Arc<dyn RequestHook>>,
}

impl std::fmt::Debug for AgentLoop {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentLoop")
            .field("provider", &self.provider.name())
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

struct RunState {
    node: NodeId,
    turns: u32,
    usage: Usage,
    cost: f64,
    cost_unknown: bool,
    pending: Option<(Message, Vec<PreparedCall>)>,
    stream: Option<(String, harness_providers::provider::EventStream, Instant)>,
    results: Vec<CallOutcome>,
    /// Messages carried by the request whose stream is open.
    sent_messages: usize,
    /// A mutating tool call succeeded during this run.
    changed: bool,
}

/// Longest verify output quoted back to the model, in chars.
const FEEDBACK_OUTPUT_CHARS: usize = 4000;

/// The user message sent after a failed verification.
/// How the verifier's fix feedback to the model starts (before any
/// provenance marker). Session descriptions skip such messages; they are
/// not user requests.
pub const VERIFY_FEEDBACK_PREFIX: &str = "Verification failed (fix attempt";

fn verify_feedback(command: &str, res: &VerifyResult, attempt: u32, max: u32) -> String {
    let status = match res.exit_code {
        Some(code) => format!("exited with code {code}"),
        None => "did not finish".to_owned(),
    };
    let chars: Vec<char> = res.output.chars().collect();
    let tail: String = chars[chars.len().saturating_sub(FEEDBACK_OUTPUT_CHARS)..]
        .iter()
        .collect();
    format!(
        "{VERIFY_FEEDBACK_PREFIX} {attempt} of {max}): `{command}` {status}.\n\n\
         Output (last {FEEDBACK_OUTPUT_CHARS} characters at most):\n```\n{tail}\n```\n\n\
         Fix the cause of the failure, then end your turn. The harness runs the \
         check again when you are done."
    )
}

fn event_usage(u: &Usage) -> EventUsage {
    EventUsage {
        input: u.input_tokens + u.cache_write_tokens,
        cached_input: u.cache_read_tokens,
        output: u.output_tokens,
        reasoning: u.reasoning_tokens,
        cache_write: u.cache_write_tokens,
    }
}

fn sha256_hex(text: &str) -> String {
    hex::encode(Sha256::digest(text.as_bytes()))
}

fn millis(start: Instant) -> u64 {
    u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX)
}

impl AgentLoop {
    /// A loop calling `provider`, running tools through `dispatcher` and
    /// recording to `recorder`.
    pub fn new(
        provider: Arc<dyn Provider>,
        dispatcher: Dispatcher,
        recorder: RunRecorder,
        config: LoopConfig,
    ) -> Self {
        Self {
            provider,
            dispatcher,
            recorder,
            config,
            cost: None,
            labeler: None,
            verifier: None,
            parent: None,
            node: None,
            loops: LoopDetector::new(),
            hooks: Vec::new(),
        }
    }

    /// Run `hook` before every request (after hooks added earlier).
    pub fn with_hook(mut self, hook: Arc<dyn RequestHook>) -> Self {
        self.hooks.push(hook);
        self
    }

    /// Price calls with `cost` (e.g. `Resolved::cost`). Provider-reported
    /// costs (OpenRouter) take precedence.
    pub fn with_cost(mut self, cost: CostFn) -> Self {
        self.cost = Some(cost);
        self
    }

    /// Label tool results with provenance tags before the model sees them.
    pub fn with_labeler(mut self, labeler: Labeler) -> Self {
        self.labeler = Some(labeler);
        self
    }

    /// Run `verifier` when the model ends its turn.
    pub fn with_verifier(mut self, verifier: Arc<dyn Verifier>) -> Self {
        self.verifier = Some(verifier);
        self
    }

    /// Record the agent node as a child of `parent`.
    pub fn with_parent(mut self, parent: NodeId) -> Self {
        self.parent = Some(parent);
        self
    }

    /// Use `node` as the agent's node id for the next run (default: random).
    pub fn with_node(mut self, node: NodeId) -> Self {
        self.node = Some(node);
        self
    }

    /// The settings.
    pub fn config(&self) -> &LoopConfig {
        &self.config
    }

    /// The recorder.
    pub fn recorder(&self) -> &RunRecorder {
        &self.recorder
    }

    fn emit(&self, node: &NodeId, kind: EventKind) {
        let mut draft = EventDraft::node(node.clone(), kind);
        if let (Some(p), EventKind::NodeStarted { .. }) = (&self.parent, &draft.kind) {
            draft = draft.with_parent(p.clone());
        }
        if let Err(e) = self.recorder.emit(draft) {
            tracing::warn!(target: "harness::agent", error = %e, "could not record event");
        }
    }

    /// Redacts a message exactly as the recorder will, so the history holds
    /// what the log holds.
    fn redact_message(&self, message: &Message) -> Message {
        let mut value = serde_json::to_value(message).unwrap_or(Value::Null);
        self.recorder.redactor().redact_json(&mut value);
        serde_json::from_value(value).unwrap_or_else(|_| message.clone())
    }

    /// Runs the loop until a [`StopReason`]. The caller adds the user's
    /// message to `conversation` first.
    pub async fn run(
        &mut self,
        conversation: &mut Conversation,
        cancel: &CancellationToken,
    ) -> LoopOutcome {
        let node = self.node.take().unwrap_or_else(NodeId::random);
        self.emit(
            &node,
            EventKind::NodeStarted {
                kind: self.config.node_kind,
                name: self.config.agent.clone(),
                agent: Some(self.config.agent.clone()),
                model: Some(self.config.model.clone()),
                host: self.config.host.clone(),
                needs: Vec::new(),
                queued: false,
            },
        );
        // User messages added since the last request are recorded with the
        // next request; redact them now so the history matches the log.
        let from = self.logged_from(conversation);
        for i in from..conversation.messages.len() {
            conversation.messages[i] = self.redact_message(&conversation.messages[i]);
        }
        // The user's new prompt(s): trailing user text messages, so the run
        // log (and replays) show what the user asked and when.
        let prompts = conversation.messages[from..]
            .iter()
            .rev()
            .take_while(|m| {
                m.role == Role::User
                    && !m
                        .content
                        .iter()
                        .any(|b| matches!(b, ContentBlock::ToolResult { .. }))
            })
            .map(Message::text)
            .filter(|t| !t.is_empty())
            .collect::<Vec<_>>();
        for text in prompts.into_iter().rev() {
            self.emit(&node, EventKind::UserMessage { text });
        }

        let mut st = RunState {
            node: node.clone(),
            turns: 0,
            usage: Usage::default(),
            cost: 0.0,
            cost_unknown: false,
            pending: None,
            stream: None,
            results: Vec::new(),
            sent_messages: 0,
            changed: false,
        };
        let mut fix_attempts = 0;
        let (stop, verified) = loop {
            let stop = self.drive(conversation, &mut st, cancel).await;
            let verifier = match (&stop, self.verifier.clone()) {
                (StopReason::EndTurn, Some(v))
                    if self.config.verify_when == VerifyWhen::Always || st.changed =>
                {
                    v
                }
                _ => break (stop, None),
            };
            let res = self.run_verifier(&node, verifier.as_ref(), cancel).await;
            if res.passed || fix_attempts >= self.config.max_fix_attempts || cancel.is_cancelled() {
                break (stop, Some(res.passed));
            }
            fix_attempts += 1;
            let text = verify_feedback(
                &verifier.command(),
                &res,
                fix_attempts,
                self.config.max_fix_attempts,
            );
            let text = match &self.labeler {
                Some(l) => l.tool_result("verify", &format!("verify-{fix_attempts}"), &text, true),
                None => text,
            };
            conversation
                .messages
                .push(self.redact_message(&Message::user(text)));
        };
        let verification = Verification::from_result(verified);
        if let StopReason::Error { message, action } = &stop {
            self.emit(
                &node,
                EventKind::Error {
                    message: message.clone(),
                    recoverable: false,
                    action: action.clone(),
                },
            );
        }
        let (status, error) = if verification == Verification::Failed {
            (
                NodeStatus::Failed,
                Some(format!(
                    "{}: verification failed after {fix_attempts} fix attempt(s)",
                    codes::VERIFY_FAILED
                )),
            )
        } else {
            (stop.node_status(), stop.error_text())
        };
        self.emit(&node, EventKind::NodeFinished { status, error });
        LoopOutcome {
            final_text: conversation.last_assistant_text(),
            stop,
            node,
            turns: st.turns,
            usage: st.usage,
            cost_usd: st.cost,
            cost_unknown: st.cost_unknown,
            verified,
            verification,
            fix_attempts,
        }
    }

    /// Runs the turn state machine until it reaches `Done`.
    async fn drive(
        &mut self,
        conversation: &mut Conversation,
        st: &mut RunState,
        cancel: &CancellationToken,
    ) -> StopReason {
        let mut state = TurnState::Request;
        loop {
            tracing::debug!(target: "harness::agent", node = %st.node.as_str(), ?state, "turn state");
            state = match state {
                TurnState::Request => self.request(conversation, st, cancel).await,
                TurnState::Streaming => self.streaming(conversation, st, cancel).await,
                TurnState::Dispatch => self.dispatch(st, cancel).await,
                TurnState::Submit => self.submit(conversation, st, cancel),
                TurnState::Done(reason) => return reason,
            };
        }
    }

    /// Runs `verifier` and records `verify_started` / `verify_finished`.
    async fn run_verifier(
        &self,
        node: &NodeId,
        verifier: &dyn Verifier,
        cancel: &CancellationToken,
    ) -> VerifyResult {
        let command = verifier.command();
        self.emit(
            node,
            EventKind::VerifyStarted {
                command: command.clone(),
            },
        );
        let start = Instant::now();
        let res = verifier.verify(cancel).await;
        self.emit(
            node,
            EventKind::VerifyFinished {
                command,
                passed: res.passed,
                exit_code: res.exit_code,
                duration_ms: millis(start),
                output: Payload::Text(res.output.clone()),
            },
        );
        res
    }

    /// Runs the request hooks; `Some(reason)` stops the loop.
    async fn run_hooks(
        &self,
        conv: &mut Conversation,
        st: &mut RunState,
        cancel: &CancellationToken,
    ) -> Option<StopReason> {
        if self.hooks.is_empty() {
            return None;
        }
        let tools = self.dispatcher.tools().specs();
        let mut ctx = RequestContext {
            conversation: conv,
            node: &st.node,
            recorder: &self.recorder,
            provider: self.provider.name(),
            model: &self.config.model,
            tools: &tools,
            turn: st.turns,
            run_usage: st.usage,
            run_cost: st.cost,
            cancel,
            charged_usage: Usage::default(),
            charged_cost: 0.0,
            charged_unknown: false,
        };
        let mut stop = None;
        for hook in &self.hooks {
            if let HookDecision::Stop(reason) = hook.before_request(&mut ctx).await {
                tracing::debug!(target: "harness::agent", hook = hook.name(), ?reason, "hook stopped the loop");
                stop = Some(reason);
                break;
            }
        }
        let (usage, cost, unknown) = (ctx.charged_usage, ctx.charged_cost, ctx.charged_unknown);
        st.usage += usage;
        st.cost += cost;
        st.cost_unknown |= unknown;
        stop
    }

    fn logged_from(&self, conversation: &Conversation) -> usize {
        match &conversation.logged {
            Some(l)
                if l.run == self.recorder.run_id() && l.messages <= conversation.messages.len() =>
            {
                l.messages
            }
            _ => 0,
        }
    }

    fn budget_exceeded(&self, st: &RunState) -> Option<String> {
        let limits = &self.config.limits;
        if let Some(max) = limits.max_tokens
            && st.usage.total() >= max
        {
            return Some(format!(
                "token budget of {max} exhausted ({} used)",
                st.usage.total()
            ));
        }
        if let Some(max) = limits.max_usd
            && st.cost >= max
        {
            return Some(format!(
                "cost budget of ${max:.2} exhausted (${:.4} spent)",
                st.cost
            ));
        }
        None
    }

    async fn request(
        &self,
        conv: &mut Conversation,
        st: &mut RunState,
        cancel: &CancellationToken,
    ) -> TurnState {
        if cancel.is_cancelled() {
            return TurnState::Done(StopReason::Cancelled);
        }
        if let Some(limit) = self.config.limits.max_turns
            && st.turns >= limit
        {
            return TurnState::Done(StopReason::MaxTurns { limit });
        }
        if let Some(reason) = self.budget_exceeded(st) {
            return TurnState::Done(StopReason::Budget { reason });
        }
        let added = robustness::ensure_tool_results(&mut conv.messages);
        if added > 0 {
            tracing::warn!(target: "harness::agent", added, "synthesized missing tool results");
        }
        if let Some(reason) = self.run_hooks(conv, st, cancel).await {
            return TurnState::Done(reason);
        }
        if cancel.is_cancelled() {
            return TurnState::Done(StopReason::Cancelled);
        }

        let request = ChatRequest {
            model: self.config.model.clone(),
            system: conv.system.clone(),
            system_layers: conv.system_layers.clone(),
            messages: conv.messages.clone(),
            tools: self.dispatcher.tools().specs(),
            tool_choice: self.config.tool_choice.clone(),
            max_tokens: self.config.max_tokens,
            temperature: None,
            thinking: self.config.thinking,
            stop_sequences: Vec::new(),
        };
        let from = self.logged_from(conv);
        let system_sha = conv.system.as_deref().map(sha256_hex);
        let system_changed =
            from == 0 || conv.logged.as_ref().map(|l| &l.system_sha) != Some(&system_sha);
        let tools_sha = sha256_hex(&serde_json::to_string(&request.tools).unwrap_or_default());
        let tools_changed =
            from == 0 || conv.logged.as_ref().map(|l| &l.tools_sha) != Some(&tools_sha);
        let call_id = format!("call_{}", uuid::Uuid::now_v7().simple());
        // Tools are logged in full (names, descriptions, schemas) when they
        // change, so the request inspector can rebuild the exact body.
        let tools = if tools_changed {
            json!(request.tools)
        } else {
            json!(self.dispatcher.tools().names())
        };
        let mut payload = json!({
            "provider": self.provider.name(),
            "model": request.model,
            "system_sha256": system_sha,
            "tools": tools,
            "tools_sha256": tools_sha,
            "tool_choice": request.tool_choice,
            "max_tokens": request.max_tokens,
            "temperature": request.temperature,
            "stop_sequences": request.stop_sequences,
            "thinking": request.thinking,
            "messages_from": from,
            "messages": &conv.messages[from..],
        });
        if system_changed {
            payload["system"] = json!(conv.system);
        }
        self.emit(
            &st.node,
            EventKind::ModelCallStarted {
                call_id: call_id.clone(),
                provider: self.provider.name().to_owned(),
                model: self.config.model.clone(),
                request: Payload::Json(payload),
            },
        );
        conv.logged = Some(Logged {
            run: self.recorder.run_id(),
            messages: conv.messages.len(),
            system_sha,
            tools_sha,
        });
        st.turns += 1;
        st.sent_messages = conv.messages.len();
        let start = Instant::now();
        let opened = tokio::select! {
            r = self.provider.stream(request, cancel.child_token()) => r,
            _ = cancel.cancelled() => Err(ProviderError::Cancelled),
        };
        match opened {
            Ok(stream) => {
                st.stream = Some((call_id, stream, start));
                TurnState::Streaming
            }
            Err(e) => self.call_failed(st, &call_id, start, e),
        }
    }

    fn call_failed(
        &self,
        st: &RunState,
        call_id: &str,
        start: Instant,
        e: ProviderError,
    ) -> TurnState {
        let cancelled = matches!(e.root(), ProviderError::Cancelled);
        self.emit(
            &st.node,
            EventKind::ModelCallFinished {
                call_id: call_id.to_owned(),
                usage: EventUsage::default(),
                stop_reason: Some(if cancelled {
                    "cancelled".into()
                } else {
                    "error".into()
                }),
                response: None,
                duration_ms: millis(start),
            },
        );
        if cancelled {
            TurnState::Done(StopReason::Cancelled)
        } else {
            TurnState::Done(StopReason::Error {
                message: format!("model call failed: {e}"),
                action: e.action().map(|a| ErrorAction {
                    label: a.label.to_owned(),
                    url: a.url,
                }),
            })
        }
    }

    async fn streaming(
        &self,
        conv: &mut Conversation,
        st: &mut RunState,
        cancel: &CancellationToken,
    ) -> TurnState {
        let Some((call_id, mut stream, start)) = st.stream.take() else {
            return TurnState::Done(StopReason::Error {
                message: "internal: no open stream".into(),
                action: None,
            });
        };
        let mut builder = ResponseBuilder::new();
        loop {
            let next = tokio::select! {
                ev = stream.next() => ev,
                _ = cancel.cancelled() => Some(Err(ProviderError::Cancelled)),
            };
            let event = match next {
                None => break,
                Some(Err(e)) => return self.call_failed(st, &call_id, start, e),
                Some(Ok(ev)) => ev,
            };
            let delta = match &event {
                StreamEvent::TextDelta { text, .. } => Some((DeltaChannel::Text, text.clone())),
                StreamEvent::ReasoningDelta { text, .. } => {
                    Some((DeltaChannel::Reasoning, text.clone()))
                }
                StreamEvent::ToolInputDelta { partial_json, .. } => {
                    Some((DeltaChannel::ToolArgs, partial_json.clone()))
                }
                StreamEvent::Retry(r) => {
                    self.emit(
                        &st.node,
                        EventKind::Error {
                            message: format!(
                                "retrying in {}s ({}, attempt {}/{}){}",
                                r.delay.as_secs(),
                                r.reason,
                                r.attempt,
                                r.max_retries,
                                if r.discard_partial {
                                    "; partial output discarded"
                                } else {
                                    ""
                                }
                            ),
                            recoverable: true,
                            action: None,
                        },
                    );
                    None
                }
                _ => None,
            };
            if let Some((channel, text)) = delta {
                self.emit(
                    &st.node,
                    EventKind::Delta {
                        call_id: call_id.clone(),
                        channel,
                        text,
                    },
                );
            }
            if let Err(e) = builder.apply(event) {
                return self.call_failed(st, &call_id, start, e);
            }
        }
        let response = match builder.finish() {
            Ok(r) => r,
            Err(e) => return self.call_failed(st, &call_id, start, e),
        };
        let mut message = response.message.clone();
        let calls = prepare_calls(&mut message, self.dispatcher.tools());
        let message = self.redact_message(&message);
        st.usage += response.usage;
        let cost = harness_providers::openai::notice::reported_cost(&response)
            .or_else(|| self.cost.as_ref().and_then(|f| f(&response.usage)));
        let stop_raw = response
            .stop
            .raw
            .clone()
            .unwrap_or_else(|| format!("{:?}", response.stop.kind).to_lowercase());
        let call_id_for_hooks = call_id.clone();
        self.emit(
            &st.node,
            EventKind::ModelCallFinished {
                call_id,
                usage: event_usage(&response.usage),
                stop_reason: Some(stop_raw),
                response: Some(Payload::Json(
                    serde_json::to_value(&message).unwrap_or(Value::Null),
                )),
                duration_ms: millis(start),
            },
        );
        match cost {
            Some(usd) => {
                st.cost += usd;
                self.emit(&st.node, EventKind::Cost { usd });
            }
            None => st.cost_unknown = true,
        }
        conv.messages.push(message.clone());
        if let Some(l) = &mut conv.logged {
            l.messages = conv.messages.len();
        }
        if !self.hooks.is_empty() {
            let report = CallReport {
                call_id: call_id_for_hooks,
                usage: response.usage,
                sent_messages: st.sent_messages,
                messages_after: conv.messages.len(),
            };
            for hook in &self.hooks {
                hook.after_call(&report);
            }
        }

        if calls.is_empty() {
            return match response.stop.kind {
                StopKind::ContextLimit => TurnState::Done(StopReason::Error {
                    message: "the conversation exceeds the model's context window".into(),
                    action: None,
                }),
                StopKind::Pause => TurnState::Request,
                _ => TurnState::Done(StopReason::EndTurn),
            };
        }
        if let Some(reason) = self.budget_exceeded(st) {
            // Do not run more tools; leave a valid history.
            st.results = calls
                .iter()
                .map(|c| CallOutcome::error(c, format!("Not run: the {reason}.")))
                .collect();
            for (c, o) in calls.iter().zip(&st.results) {
                self.record_call(&st.node, c);
                self.record_result(&st.node, o);
            }
            st.pending = Some((message, calls));
            let _ = self.submit(conv, st, cancel);
            return TurnState::Done(StopReason::Budget { reason });
        }
        st.pending = Some((message, calls));
        TurnState::Dispatch
    }

    fn record_call(&self, node: &NodeId, call: &PreparedCall) {
        self.emit(
            node,
            EventKind::ToolCall {
                call_id: call.id.clone(),
                tool: call.tool.clone(),
                args: Payload::Json(call.args.clone()),
            },
        );
        if let Some(target) = self
            .dispatcher
            .tools()
            .get(&call.tool)
            .and_then(|t| t.mcp_target(&call.args))
        {
            self.emit(
                node,
                EventKind::McpCall {
                    call_id: call.id.clone(),
                    server: target.server,
                    tool: target.tool,
                },
            );
        }
        if !call.repairs.is_empty() {
            let list: Vec<String> = call.repairs.iter().map(ToString::to_string).collect();
            self.emit(
                node,
                EventKind::Error {
                    message: format!(
                        "repaired arguments of `{}` call {}: {}",
                        call.tool,
                        call.id,
                        list.join("; ")
                    ),
                    recoverable: true,
                    action: None,
                },
            );
        }
        if let Some(original) = &call.duplicate_of {
            self.emit(
                node,
                EventKind::Error {
                    message: format!("duplicate tool call id {original} renamed to {}", call.id),
                    recoverable: true,
                    action: None,
                },
            );
        }
    }

    fn record_result(&self, node: &NodeId, outcome: &CallOutcome) {
        let output = match outcome.content.as_slice() {
            [ToolResultPart::Text { text }] => Payload::Text(text.clone()),
            parts => Payload::Json(serde_json::to_value(parts).unwrap_or(Value::Null)),
        };
        self.emit(
            node,
            EventKind::ToolResult {
                call_id: outcome.id.clone(),
                ok: !outcome.is_error,
                output,
                duration_ms: outcome.duration_ms,
            },
        );
    }

    /// Labels (or at least redacts) the text parts the model will see.
    fn finalize(&self, mut outcome: CallOutcome) -> CallOutcome {
        for part in &mut outcome.content {
            if let ToolResultPart::Text { text } = part {
                *text = match &self.labeler {
                    Some(l) => l.tool_result(&outcome.tool, &outcome.id, text, outcome.is_error),
                    None => self.recorder.redactor().redact(text).into_owned(),
                };
            }
        }
        outcome
    }

    async fn dispatch(&mut self, st: &mut RunState, cancel: &CancellationToken) -> TurnState {
        let Some((_, calls)) = &st.pending else {
            return TurnState::Done(StopReason::Error {
                message: "internal: nothing to dispatch".into(),
                action: None,
            });
        };
        let calls = calls.clone();
        for c in &calls {
            self.record_call(&st.node, c);
        }
        // Results are recorded in call order once the batch is done, after
        // loop detection and labeling.
        let outcomes = self
            .dispatcher
            .run(Some(&st.node), &calls, cancel, &|_, _| {})
            .await;
        let mut results = Vec::with_capacity(outcomes.len());
        for (call, outcome) in calls.iter().zip(outcomes) {
            let mut outcome = outcome;
            if outcome.is_error
                && !outcome.cancelled
                && let Some(hit) = self.loops.observe(&call.tool, &call.raw_input, true)
            {
                self.emit(
                    &st.node,
                    EventKind::Error {
                        message: format!(
                            "loop detected: `{}` failed {} times with identical arguments",
                            hit.tool, hit.failures
                        ),
                        recoverable: true,
                        action: None,
                    },
                );
                outcome
                    .content
                    .push(ToolResultPart::Text { text: hit.nudge });
            } else if !outcome.is_error {
                self.loops.observe(&call.tool, &call.raw_input, false);
            }
            if !outcome.is_error
                && matches!(
                    call.plan,
                    crate::dispatch::CallPlan::Run { read_only: false }
                )
            {
                st.changed = true;
            }
            let outcome = self.finalize(outcome);
            self.record_result(&st.node, &outcome);
            results.push(outcome);
        }
        st.results = results;
        TurnState::Submit
    }

    fn submit(
        &self,
        conv: &mut Conversation,
        st: &mut RunState,
        cancel: &CancellationToken,
    ) -> TurnState {
        let results = std::mem::take(&mut st.results);
        st.pending = None;
        let any_cancelled = results.iter().any(|r| r.cancelled);
        let message = Message {
            role: Role::User,
            content: results
                .iter()
                .map(CallOutcome::to_block)
                .collect::<Vec<ContentBlock>>(),
        };
        conv.messages.push(self.redact_message(&message));
        if any_cancelled || cancel.is_cancelled() {
            TurnState::Done(StopReason::Cancelled)
        } else {
            TurnState::Request
        }
    }
}

#[cfg(test)]
mod tests;
