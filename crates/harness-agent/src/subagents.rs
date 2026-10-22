//! Subagents: the `spawn_agent` tool and child agent runs.
//!
//! A parent agent whose YAML lists `subagents:` gets a `spawn_agent` tool
//! whose `agent` argument is an enum of exactly those names. A child:
//!
//! - gets **its own** system prompt (from its agent definition) and a
//!   single user message built from the parent's `brief` plus an optional
//!   `files` list ([`brief_message`]); it never sees the parent transcript;
//! - runs as a child node (`node_kind: subagent`, `parent_node_id` = the
//!   parent) in the same run, so the map shows it hanging off its parent;
//! - waits for a [`Scheduler`] slot (`[general] max_concurrent_agents`),
//!   showing as a queued node while it waits;
//! - is limited in depth by `[general] max_subagent_depth`: `spawn_agent`
//!   is not offered to agents at the limit and refuses deeper spawns;
//! - is cancelled with its parent (its token is a child of the parent's,
//!   and dropping the parent's tool call cancels it);
//! - returns its final text as a summary, plus a structured output when
//!   the agent declares `output_schema` ([`check_output`]); an invalid
//!   structured output gets one correction turn.
//!
//! [`SubagentRuntime`] holds what every child needs (recorder, scheduler,
//! task board, a [`ChildBuilder`] that turns an agent name into a model,
//! prompt and tools). [`SubagentRuntime::run_child`] is also used by the
//! workflow executor for agent steps.

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use harness_config::agents::{Agent, Effort, PromptRef, SPAWN_AGENT_TOOL};
use harness_config::discovery::Catalog;
use harness_config::ini::Config;
use harness_config::prompts::PromptVars;
use harness_events::schema::{EventKind, NodeId, NodeKind};
use harness_events::store::{EventDraft, RunRecorder};
use harness_providers::model::{ReasoningEffort, ThinkingConfig};
use harness_providers::provider::Provider;
use harness_tools::tool::{ToolContext, ToolError, ToolErrorKind, ToolOutput};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use crate::agent_loop::{
    AgentLoop, Conversation, CostFn, LoopConfig, LoopLimits, LoopOutcome, StopReason,
};
use crate::dispatch::{AgentTool, Dispatcher, RecorderToolEvents, ToolGate, ToolSet};
use crate::robustness;
use crate::scheduler::{AgentSlot, Scheduler, SchedulerConfig};
use crate::todo::{self, Actor, TaskBoard, TodoTool};

/// Tool name (same as [`harness_config::agents::SPAWN_AGENT_TOOL`]).
pub const NAME: &str = SPAWN_AGENT_TOOL;
/// Tool description sent to the model.
pub const DESCRIPTION: &str = "Delegate a self-contained task to a subagent. It sees only your brief \
and the listed files, not this conversation, and returns its final summary.";
/// Longest text searched for a structured output.
pub const MAX_STRUCTURED_SCAN: usize = 64 * 1024;

/// Depth and concurrency limits for subagents.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SubagentLimits {
    /// Deepest allowed child (the top agent is depth 0).
    pub max_depth: u32,
    /// Concurrent agent slots.
    pub max_concurrent: usize,
}

impl Default for SubagentLimits {
    fn default() -> Self {
        Self {
            max_depth: 2,
            max_concurrent: 4,
        }
    }
}

impl SubagentLimits {
    /// `[general] max_subagent_depth` and `max_concurrent_agents`.
    pub fn from_config(cfg: &Config) -> Self {
        Self {
            max_depth: cfg.general.max_subagent_depth,
            max_concurrent: usize::try_from(cfg.general.max_concurrent_agents.max(1))
                .unwrap_or(usize::MAX),
        }
    }
}

/// Everything needed to run one child agent.
pub struct ChildParts {
    /// The agent definition.
    pub agent: Agent,
    /// Its rendered system prompt.
    pub system: String,
    /// Provider for its model.
    pub provider: Arc<dyn Provider>,
    /// Model id sent to the provider.
    pub model: String,
    /// Its base tools (read/write/edit/bash as the agent lists them).
    /// `todo` and `spawn_agent` are added by the runtime.
    pub tools: ToolSet,
    /// Policy gate for its tool calls.
    pub gate: Arc<dyn ToolGate>,
    /// Pricing.
    pub cost: Option<CostFn>,
    /// Workspace root for file-change events, if any.
    pub root: Option<PathBuf>,
}

impl std::fmt::Debug for ChildParts {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChildParts")
            .field("agent", &self.agent.name)
            .field("model", &self.model)
            .field("tools", &self.tools)
            .finish_non_exhaustive()
    }
}

/// Turns an agent name into a runnable child.
#[async_trait]
pub trait ChildBuilder: Send + Sync {
    /// Builds the parts for `agent` (on `model` instead of the agent's own
    /// model when given), or explains why it cannot run.
    async fn build(&self, agent: &str, model: Option<&str>) -> Result<ChildParts, String>;
    /// The agent's description (shown to the parent), if known.
    fn describe(&self, agent: &str) -> Option<String>;
}

/// Maps the agent `effort` onto the provider-neutral thinking setting.
pub fn thinking_for(effort: Option<Effort>) -> Option<ThinkingConfig> {
    effort.map(|e| ThinkingConfig {
        budget_tokens: None,
        effort: Some(match e {
            Effort::Low => ReasoningEffort::Low,
            Effort::Medium => ReasoningEffort::Medium,
            Effort::High => ReasoningEffort::High,
        }),
    })
}

/// Loop limits from the agent's `max_turns` and `budget`.
pub fn limits_for(agent: &Agent) -> LoopLimits {
    LoopLimits {
        max_turns: agent.max_turns,
        max_tokens: agent.budget.tokens,
        max_usd: agent.budget.usd,
    }
}

/// Combines two sets of limits: for each limit the lower value wins.
pub fn combine_limits(a: LoopLimits, b: Option<LoopLimits>) -> LoopLimits {
    let Some(b) = b else { return a };
    fn min<T: PartialOrd + Copy>(x: Option<T>, y: Option<T>) -> Option<T> {
        match (x, y) {
            (Some(x), Some(y)) => Some(if y < x { y } else { x }),
            (x, None) => x,
            (None, y) => y,
        }
    }
    LoopLimits {
        max_turns: min(a.max_turns, b.max_turns),
        max_tokens: min(a.max_tokens, b.max_tokens),
        max_usd: min(a.max_usd, b.max_usd),
    }
}

/// Resolves a model name (alias or `provider/model`) to a provider and the
/// model id to send.
pub type ModelResolver =
    Arc<dyn Fn(&str) -> Result<(Arc<dyn Provider>, String), String> + Send + Sync>;
/// Prices calls for a model name (`None` = unknown price).
pub type CostResolver = Arc<dyn Fn(&str) -> Option<CostFn> + Send + Sync>;
/// Builds an agent's base tool set.
pub type ToolsFactory = Arc<dyn Fn(&Agent) -> ToolSet + Send + Sync>;

/// A [`ChildBuilder`] over the discovered [`Catalog`].
pub struct CatalogChildBuilder {
    catalog: Arc<Catalog>,
    vars: PromptVars,
    default_model: String,
    resolve: ModelResolver,
    tools: ToolsFactory,
    gate: Arc<dyn ToolGate>,
    gates: Option<GateFactory>,
    cost: Option<CostResolver>,
    root: Option<PathBuf>,
}

/// Picks the gate for an agent (e.g. one capped at its `mode:`).
pub type GateFactory = Arc<dyn Fn(&Agent) -> Arc<dyn ToolGate> + Send + Sync>;

impl CatalogChildBuilder {
    /// Children come from `catalog`; prompts render with `vars`; agents
    /// without a `model` use `default_model`.
    pub fn new(
        catalog: Arc<Catalog>,
        vars: PromptVars,
        default_model: impl Into<String>,
        resolve: ModelResolver,
        tools: ToolsFactory,
        gate: Arc<dyn ToolGate>,
    ) -> Self {
        Self {
            catalog,
            vars,
            default_model: default_model.into(),
            resolve,
            tools,
            gate,
            gates: None,
            cost: None,
            root: None,
        }
    }

    /// Chooses each child's gate with `gates` instead of the shared one.
    pub fn with_gates(mut self, gates: GateFactory) -> Self {
        self.gates = Some(gates);
        self
    }

    /// Prices each child's calls with `cost(model_name)`.
    pub fn with_cost(mut self, cost: CostResolver) -> Self {
        self.cost = Some(cost);
        self
    }

    /// Records file changes relative to `root`.
    pub fn with_root(mut self, root: impl Into<PathBuf>) -> Self {
        self.root = Some(root.into());
        self
    }

    /// The system prompt of `agent`.
    pub fn system_prompt(&self, agent: &Agent) -> Result<String, String> {
        match &agent.prompt {
            PromptRef::Inline(text) => Ok(text.clone()),
            PromptRef::Named(name) => {
                let p = self.catalog.prompts.get(name).ok_or_else(|| {
                    format!("agent `{}` uses unknown prompt `{name}`", agent.name)
                })?;
                p.render(&self.vars).map_err(|e| e.to_string())
            }
        }
    }
}

#[async_trait]
impl ChildBuilder for CatalogChildBuilder {
    async fn build(&self, name: &str, model: Option<&str>) -> Result<ChildParts, String> {
        let agent = self
            .catalog
            .agents
            .get(name)
            .cloned()
            .ok_or_else(|| format!("unknown agent `{name}`"))?;
        let system = self.system_prompt(&agent)?;
        let model_name = model
            .map(str::to_owned)
            .or_else(|| agent.model.clone())
            .unwrap_or_else(|| self.default_model.clone());
        let (provider, model) = (self.resolve)(&model_name)?;
        let gate = match &self.gates {
            Some(f) => f(&agent),
            None => self.gate.clone(),
        };
        Ok(ChildParts {
            tools: (self.tools)(&agent),
            cost: self.cost.as_ref().and_then(|c| c(&model_name)),
            agent,
            system,
            provider,
            model,
            gate,
            root: self.root.clone(),
        })
    }

    fn describe(&self, agent: &str) -> Option<String> {
        self.catalog.agents.get(agent)?.description.clone()
    }
}

/// Replaces a child's base tools with ones that act elsewhere: it gets the
/// base set and returns the set to offer (same tool names).
pub type HostTools = Arc<dyn Fn(&Agent, ToolSet) -> ToolSet + Send + Sync>;

/// Runs a child's file and process tools on another host (`runs_on`). The
/// model loop, policy gate and event log stay local; only the tools change.
#[derive(Clone)]
pub struct ChildHost {
    /// `[remote.<name>]` name, recorded as the child's `node_started.host`.
    pub name: String,
    /// The workspace root on that host (file-change paths are relative to it).
    pub root: PathBuf,
    /// Swaps the base tools for ones on that host.
    pub tools: HostTools,
    /// The host's gate (e.g. the policy with the host's tighten-only
    /// overlay); `None` keeps the builder's gate.
    pub gate: Option<GateFactory>,
}

impl std::fmt::Debug for ChildHost {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChildHost")
            .field("name", &self.name)
            .field("root", &self.root)
            .finish_non_exhaustive()
    }
}

/// One child to run.
#[derive(Debug, Clone)]
pub struct ChildRequest {
    /// Agent name.
    pub agent: String,
    /// Task description from the parent.
    pub brief: String,
    /// Files the child should look at first.
    pub files: Vec<String>,
    /// Parent node.
    pub parent: NodeId,
    /// Node id for the child (random when `None`).
    pub node: Option<NodeId>,
    /// The child's depth (the top agent is 0).
    pub depth: u32,
    /// Node kind recorded for the child.
    pub node_kind: NodeKind,
    /// Cancellation for the child.
    pub cancel: CancellationToken,
    /// Model override (alias or `provider/model`).
    pub model: Option<String>,
    /// Extra limits; each is combined with the agent's own (the lower
    /// one wins).
    pub limits: Option<LoopLimits>,
    /// Output schema to use when the agent declares none.
    pub output_schema: Option<Value>,
    /// A scheduler slot the caller already holds (no second slot is
    /// taken).
    pub slot: Option<Arc<AgentSlot>>,
    /// Run the child's tools on this host instead of the builder's
    /// workspace. Its own subagents inherit it.
    pub host: Option<ChildHost>,
}

impl ChildRequest {
    /// A subagent request for `agent` below `parent`.
    pub fn new(
        agent: impl Into<String>,
        brief: impl Into<String>,
        parent: NodeId,
        depth: u32,
        cancel: CancellationToken,
    ) -> Self {
        Self {
            agent: agent.into(),
            brief: brief.into(),
            files: Vec::new(),
            parent,
            node: None,
            depth,
            node_kind: NodeKind::Subagent,
            cancel,
            model: None,
            limits: None,
            output_schema: None,
            slot: None,
            host: None,
        }
    }
}

/// What a child produced.
#[derive(Debug, Clone, PartialEq)]
pub struct ChildResult {
    /// Agent name.
    pub agent: String,
    /// Child node.
    pub node: NodeId,
    /// The loop outcome (absent if the child could not start).
    pub outcome: Option<LoopOutcome>,
    /// Structured output, validated against `output_schema`.
    pub structured: Option<Value>,
    /// Why the child failed, if it did.
    pub error: Option<String>,
}

impl ChildResult {
    /// True when the child ended its turn and its output is valid.
    pub fn succeeded(&self) -> bool {
        self.error.is_none()
            && self
                .outcome
                .as_ref()
                .is_some_and(|o| o.stop == StopReason::EndTurn)
    }

    /// The final summary text.
    pub fn summary(&self) -> &str {
        self.outcome.as_ref().map_or("", |o| o.final_text.as_str())
    }

    /// The text returned to the parent model.
    pub fn tool_text(&self) -> String {
        let status = match (&self.error, &self.outcome) {
            (Some(e), _) => format!("failed: {e}"),
            (None, Some(o)) => o.stop.code().to_owned(),
            (None, None) => "not started".to_owned(),
        };
        let mut s = format!(
            "Subagent `{}` (node {}) finished: {status}\n\n{}",
            self.agent,
            self.node.as_str(),
            self.summary().trim()
        );
        if let Some(v) = &self.structured {
            s.push_str("\n\nStructured output:\n");
            s.push_str(&serde_json::to_string_pretty(v).unwrap_or_default());
        }
        s
    }
}

/// The first user message of a child: the brief, the files to read and,
/// with an `output_schema`, how to format the result.
pub fn brief_message(brief: &str, files: &[String], output_schema: Option<&Value>) -> String {
    let mut s = brief.trim().to_owned();
    if !files.is_empty() {
        s.push_str("\n\nRelevant files:\n");
        for f in files {
            s.push_str("- ");
            s.push_str(f);
            s.push('\n');
        }
        s = s.trim_end().to_owned();
    }
    if let Some(schema) = output_schema {
        s.push_str(
            "\n\nWhen you are done, end your reply with a JSON object matching this schema:\n```json\n",
        );
        s.push_str(&serde_json::to_string(schema).unwrap_or_default());
        s.push_str("\n```");
    }
    s
}

/// Finds the structured output in `text`: the last ```` ```json ```` block,
/// or else a JSON object that ends the text.
pub fn extract_json(text: &str) -> Option<Value> {
    let text = if text.len() > MAX_STRUCTURED_SCAN {
        let mut start = text.len() - MAX_STRUCTURED_SCAN;
        while !text.is_char_boundary(start) {
            start += 1;
        }
        &text[start..]
    } else {
        text
    };
    if let Some(open) = text.rfind("```json") {
        let body = &text[open + "```json".len()..];
        let body = body.find("```").map_or(body, |end| &body[..end]);
        if let Ok(v) = serde_json::from_str::<Value>(body.trim()) {
            return Some(v);
        }
    }
    let trimmed = text.trim_end();
    trimmed
        .char_indices()
        .rev()
        .filter(|&(_, c)| c == '{')
        .find_map(|(i, _)| serde_json::from_str::<Value>(&trimmed[i..]).ok())
}

/// Extracts and validates the structured output of `text` against
/// `schema`. The error says what is wrong, for a correction turn.
pub fn check_output(schema: &Value, text: &str) -> Result<Value, String> {
    let mut value = extract_json(text)
        .ok_or_else(|| "no JSON object found at the end of the reply".to_owned())?;
    let mut repairs = Vec::new();
    robustness::validate(schema, &mut value, &mut repairs).map_err(|issues| {
        issues
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("; ")
    })?;
    Ok(value)
}

/// Shared state for running children.
#[derive(Clone)]
pub struct SubagentRuntime {
    recorder: RunRecorder,
    builder: Arc<dyn ChildBuilder>,
    limits: SubagentLimits,
    scheduler: Scheduler,
    board: Option<Arc<TaskBoard>>,
}

impl std::fmt::Debug for SubagentRuntime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SubagentRuntime")
            .field("limits", &self.limits)
            .field("scheduler", &self.scheduler)
            .finish_non_exhaustive()
    }
}

/// Where an agent sits, for building its tools.
#[derive(Debug, Clone)]
pub struct AgentPlace {
    /// Its node (the loop must run with this node id).
    pub node: NodeId,
    /// Its depth (top agent 0).
    pub depth: u32,
    /// The token its loop runs with; children derive theirs from it.
    pub cancel: CancellationToken,
    /// Its scheduler slot, lent to children while it waits for them.
    pub slot: Option<Arc<AgentSlot>>,
}

impl SubagentRuntime {
    /// Children record to `recorder` and are built by `builder`. The
    /// scheduler defaults to `limits.max_concurrent` slots.
    pub fn new(
        recorder: RunRecorder,
        builder: Arc<dyn ChildBuilder>,
        limits: SubagentLimits,
    ) -> Self {
        let scheduler = Scheduler::new(SchedulerConfig {
            max_concurrent_agents: limits.max_concurrent,
            ..SchedulerConfig::default()
        });
        Self {
            recorder,
            builder,
            limits,
            scheduler,
            board: None,
        }
    }

    /// Shares `scheduler` (e.g. with the workflow executor).
    pub fn with_scheduler(mut self, scheduler: Scheduler) -> Self {
        self.scheduler = scheduler;
        self
    }

    /// Gives agents that list `todo` the run's task board.
    pub fn with_board(mut self, board: Arc<TaskBoard>) -> Self {
        self.board = Some(board);
        self
    }

    /// The limits.
    pub fn limits(&self) -> SubagentLimits {
        self.limits
    }

    /// The scheduler.
    pub fn scheduler(&self) -> &Scheduler {
        &self.scheduler
    }

    /// The recorder.
    pub fn recorder(&self) -> &RunRecorder {
        &self.recorder
    }

    /// The task board, if any.
    pub fn board(&self) -> Option<&Arc<TaskBoard>> {
        self.board.as_ref()
    }

    /// `base` plus the tools the runtime provides for `agent` at `place`:
    /// `todo` when its `tools:` list it (and a board exists), and
    /// `spawn_agent` restricted to its `subagents:` while below the depth
    /// limit. Tools the agent does not list are removed, so the request
    /// carries exactly [`Agent::request_tools`] (minus unavailable ones).
    pub fn tools_for(&self, agent: &Agent, base: &ToolSet, place: &AgentPlace) -> ToolSet {
        self.tools_on(agent, base, place, None)
    }

    /// [`SubagentRuntime::tools_for`] for an agent whose tools run on
    /// `host`: the base tools are swapped for the host's, and children it
    /// spawns run there too.
    fn tools_on(
        &self,
        agent: &Agent,
        base: &ToolSet,
        place: &AgentPlace,
        host: Option<&ChildHost>,
    ) -> ToolSet {
        let mut set = match host {
            Some(h) => (h.tools)(agent, base.clone()),
            None => base.clone(),
        };
        if agent.tools.iter().any(|t| t == todo::NAME)
            && let Some(board) = &self.board
        {
            set.register(Arc::new(TodoTool::new(
                board.clone(),
                Actor::new(place.node.clone(), agent.name.clone()),
            )));
        }
        if !agent.subagents.is_empty() && place.depth < self.limits.max_depth {
            set.register(Arc::new(SpawnAgentTool {
                runtime: self.clone(),
                allowed: agent
                    .subagents
                    .iter()
                    .map(|n| (n.clone(), self.builder.describe(n)))
                    .collect(),
                place: place.clone(),
                host: host.cloned(),
            }));
        }
        let wanted = agent.request_tools();
        set.select(&wanted).0
    }

    fn emit(&self, draft: EventDraft) {
        if let Err(e) = self.recorder.emit(draft) {
            tracing::warn!(target: "harness::subagents", error = %e, "could not record event");
        }
    }

    /// Runs one child to completion (waiting for a slot first).
    pub async fn run_child(&self, req: ChildRequest) -> ChildResult {
        let node = req.node.clone().unwrap_or_else(NodeId::random);
        let fail = |error: String| ChildResult {
            agent: req.agent.clone(),
            node: node.clone(),
            outcome: None,
            structured: None,
            error: Some(error),
        };
        if req.depth > self.limits.max_depth {
            return fail(format!(
                "subagent depth {} exceeds [general] max_subagent_depth = {}",
                req.depth, self.limits.max_depth
            ));
        }
        let parts = match self.builder.build(&req.agent, req.model.as_deref()).await {
            Ok(p) => p,
            Err(e) => return fail(e),
        };
        let started = |queued: bool| {
            EventDraft::node(
                node.clone(),
                EventKind::NodeStarted {
                    kind: req.node_kind,
                    name: parts.agent.name.clone(),
                    agent: Some(parts.agent.name.clone()),
                    model: Some(parts.model.clone()),
                    host: req.host.as_ref().map(|h| h.name.clone()),
                    needs: Vec::new(),
                    queued,
                },
            )
            .with_parent(req.parent.clone())
        };
        let slot = if let Some(slot) = &req.slot {
            slot.clone()
        } else {
            match self
                .scheduler
                .acquire_agent(&req.cancel, || self.emit(started(true)))
                .await
            {
                Ok(s) => Arc::new(s),
                Err(_) => {
                    // Only a queued wait can be cancelled, so the node exists.
                    self.emit(EventDraft::node(
                        node.clone(),
                        EventKind::NodeFinished {
                            status: harness_events::schema::NodeStatus::Cancelled,
                            error: Some(StopReason::Cancelled.error_text().unwrap_or_default()),
                        },
                    ));
                    return fail("cancelled before it started".into());
                }
            }
        };

        let place = AgentPlace {
            node: node.clone(),
            depth: req.depth,
            cancel: req.cancel.clone(),
            slot: Some(slot.clone()),
        };
        let tools = self.tools_on(&parts.agent, &parts.tools, &place, req.host.as_ref());
        let gate = match req.host.as_ref().and_then(|h| h.gate.as_ref()) {
            Some(f) => f(&parts.agent),
            None => parts.gate.clone(),
        };
        let mut dispatcher = Dispatcher::new(tools, gate);
        let root = match &req.host {
            Some(h) => Some(&h.root),
            None => parts.root.as_ref(),
        };
        if let Some(root) = root {
            dispatcher = dispatcher.with_events(Arc::new(RecorderToolEvents::new(
                self.recorder.clone(),
                node.clone(),
                root.clone(),
            )));
        }
        let mut config = LoopConfig::new(parts.agent.name.clone(), parts.model.clone());
        config.node_kind = req.node_kind;
        config.host = req.host.as_ref().map(|h| h.name.clone());
        config.thinking = thinking_for(parts.agent.effort);
        config.limits = combine_limits(limits_for(&parts.agent), req.limits);
        let mut lp = AgentLoop::new(
            parts.provider.clone(),
            dispatcher,
            self.recorder.clone(),
            config,
        )
        .with_parent(req.parent.clone())
        .with_node(node.clone());
        if let Some(c) = &parts.cost {
            lp = lp.with_cost(c.clone());
        }
        let schema = parts
            .agent
            .output_schema
            .clone()
            .or_else(|| req.output_schema.clone());
        let mut conv = Conversation::new(Some(parts.system.clone()));
        conv.push_user(brief_message(&req.brief, &req.files, schema.as_ref()));
        let mut outcome = lp.run(&mut conv, &req.cancel).await;

        let mut structured = None;
        let mut error = None;
        if let (Some(schema), StopReason::EndTurn) = (&schema, &outcome.stop) {
            match check_output(schema, &outcome.final_text) {
                Ok(v) => structured = Some(v),
                Err(why) => {
                    conv.push_user(format!(
                        "Your structured output is invalid: {why}. Reply with only the corrected JSON object."
                    ));
                    let mut lp = lp.with_node(node.clone());
                    let again = lp.run(&mut conv, &req.cancel).await;
                    let turns = outcome.turns + again.turns;
                    let usage = outcome.usage + again.usage;
                    let cost = outcome.cost_usd + again.cost_usd;
                    let unknown = outcome.cost_unknown || again.cost_unknown;
                    outcome = LoopOutcome {
                        turns,
                        usage,
                        cost_usd: cost,
                        cost_unknown: unknown,
                        ..again
                    };
                    match (&outcome.stop, check_output(schema, &outcome.final_text)) {
                        (StopReason::EndTurn, Ok(v)) => structured = Some(v),
                        (StopReason::EndTurn, Err(why)) => {
                            error = Some(format!(
                                "structured output does not match output_schema: {why}"
                            ));
                        }
                        _ => {}
                    }
                }
            }
        }
        drop(slot);
        ChildResult {
            agent: parts.agent.name.clone(),
            node,
            outcome: Some(outcome),
            structured,
            error,
        }
    }
}

/// The `spawn_agent` tool of one parent agent.
pub struct SpawnAgentTool {
    runtime: SubagentRuntime,
    allowed: Vec<(String, Option<String>)>,
    place: AgentPlace,
    host: Option<ChildHost>,
}

impl std::fmt::Debug for SpawnAgentTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SpawnAgentTool")
            .field("allowed", &self.allowed)
            .field("parent", &self.place.node)
            .field("depth", &self.place.depth)
            .finish()
    }
}

/// The argument schema for a parent allowed to spawn `allowed`.
pub fn schema(allowed: &[(String, Option<String>)]) -> Value {
    let names: Vec<&str> = allowed.iter().map(|(n, _)| n.as_str()).collect();
    let help: Vec<String> = allowed
        .iter()
        .map(|(n, d)| match d {
            Some(d) => format!("{n}: {d}"),
            None => n.clone(),
        })
        .collect();
    json!({
        "type": "object",
        "properties": {
            "agent": {"enum": names, "description": help.join("; ")},
            "brief": {"type": "string", "description": "Self-contained task: goal, context, expected result."},
            "files": {"type": "array", "items": {"type": "string"}}
        },
        "required": ["agent", "brief"],
        "additionalProperties": false
    })
}

#[derive(Deserialize)]
struct SpawnArgs {
    agent: String,
    brief: String,
    #[serde(default)]
    files: Vec<String>,
}

impl SpawnAgentTool {
    /// The agents this parent may spawn.
    pub fn allowed(&self) -> Vec<&str> {
        self.allowed.iter().map(|(n, _)| n.as_str()).collect()
    }
}

#[async_trait]
impl AgentTool for SpawnAgentTool {
    fn name(&self) -> &str {
        NAME
    }
    fn description(&self) -> &str {
        DESCRIPTION
    }
    fn schema(&self) -> Value {
        schema(&self.allowed)
    }
    fn read_only(&self) -> bool {
        // The tool itself changes nothing; children's tool calls go through
        // their own gate. This lets parallel spawns run concurrently.
        true
    }
    async fn call(&self, _ctx: &ToolContext, args: Value) -> Result<ToolOutput, ToolError> {
        let a: SpawnArgs = serde_json::from_value(args).map_err(|e| {
            ToolError::invalid_args(format!(
                "bad spawn_agent arguments: {e}. Fix: pass agent and brief."
            ))
        })?;
        if !self.allowed.iter().any(|(n, _)| *n == a.agent) {
            return Err(ToolError::invalid_args(format!(
                "agent `{}` is not one of your subagents. Fix: use one of: {}.",
                a.agent,
                self.allowed().join(", ")
            )));
        }
        let depth = self.place.depth + 1;
        if depth > self.runtime.limits.max_depth {
            return Err(ToolError::new(
                ToolErrorKind::Unsupported,
                format!(
                    "subagent depth limit ({}) reached. Fix: do the task yourself.",
                    self.runtime.limits.max_depth
                ),
            ));
        }
        let token = self.place.cancel.child_token();
        let guard = token.clone().drop_guard();
        let mut req = ChildRequest::new(a.agent, a.brief, self.place.node.clone(), depth, token);
        req.files = a.files;
        req.host.clone_from(&self.host);
        if let Some(slot) = &self.place.slot {
            slot.suspend();
        }
        let rt = self.runtime.clone();
        // A task, so that a dropped call (parent cancelled) still lets the
        // child record its cancellation.
        let joined = tokio::spawn(async move { rt.run_child(req).await }).await;
        if let Some(slot) = &self.place.slot {
            slot.resume().await;
        }
        guard.disarm();
        let result = joined.map_err(|e| {
            ToolError::new(
                ToolErrorKind::Exec,
                format!("subagent crashed: {e}. Fix: retry or do it yourself."),
            )
        })?;
        if result.succeeded() {
            Ok(ToolOutput::Text(result.tool_text()))
        } else {
            Err(ToolError::new(ToolErrorKind::Exec, result.tool_text()))
        }
    }
}

#[cfg(test)]
mod tests;
