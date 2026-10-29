//! The session core: everything needed to run agent turns in a project,
//! shared by `harness run`, `harness wf` and the TUI.
//!
//! [`Core::start`] resolves the agent and model, opens the run log and the
//! session, and wires the full tool stack:
//!
//! - standard tools (read, write, edit, bash) over the (optionally
//!   sandboxed) local executor, with secrets scrubbed from the environment;
//! - `todo` on the run's task board and `spawn_agent` for subagents
//!   (via [`SubagentRuntime`]);
//! - `memory` when the agent and `[memory]` allow it;
//! - the policy gate (staging guard, approvals, `policy_decision` events);
//! - request hooks: context management (elision and compaction) and
//!   budgets (session and daily limits);
//! - done-gating with `[verify]` (or an auto-detected command).
//!
//! [`Core::begin`] records `run_started`; each [`Core::turn`] runs one agent loop on a new node over the shared
//! conversation; [`Core::finish`] closes the run and saves the session.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use harness_agent::agent_loop::{
    AgentLoop, Conversation, CostFn, LoopConfig, LoopLimits, LoopOutcome, StopReason, VerifyWhen,
};
use harness_agent::budget::{BudgetApprover, BudgetGuard, BudgetMeter, BudgetScope, Budgets};
use harness_agent::compaction::{Compactor, ContextManager, ContextSettings};
use harness_agent::context::SessionPrefix;
use harness_agent::dispatch::{
    Dispatcher, PolicyGate, RecorderToolEvents, RunBlobSink, StandardTools, ToolGate, ToolSet,
};
use harness_agent::scheduler::{ScheduledProvider, Scheduler, SchedulerConfig};
use harness_agent::session::{Resumed, SessionHandle, SessionMeta, SessionStore};
use harness_agent::subagents::{
    AgentPlace, CatalogChildBuilder, ModelResolver, SubagentLimits, SubagentRuntime, ToolsFactory,
};
use harness_agent::todo::{TASKS_FILE, TaskBoard};
use harness_config::agents::{Agent, Effort, MemoryAccess};
use harness_config::discovery::Catalog;
use harness_config::ini::Config;
use harness_config::paths::DataDir;
use harness_config::prompts::PromptVars;
use harness_config::trust::{TrustStatus, TrustStore};
use harness_events::redact::Redactor;
use harness_events::schema::{EventKind, NodeId, RunId, RunStatus};
use harness_events::store::{EventDraft, RecorderOptions, RunRecorder, run_dir};
use harness_memory::sync::SessionCheckout;
use harness_memory::tool::{MemoryTool, MemoryToolConfig, effective_access};
use harness_policy::approvals::{ApprovalBroker, ApprovalUi, RecorderApprovalEvents};
use harness_policy::provenance::{Labeler, ProjectTrust};
use harness_policy::rules::{Mode, PolicyConfig, PolicyEngine, PolicyOverlay};
use harness_policy::sandbox::{Launcher, SandboxConfig, SandboxedExec};
use harness_providers::model::ReasoningEffort;
use harness_providers::provider::Provider;
use harness_providers::registry::ThinkingStyle;
use harness_tools::bash::EnvScrubber;
use harness_tools::config::ToolsConfig;
use harness_tools::exec::Exec;
use harness_tools::local::{LocalExec, LocalWorkspace};
use tokio_util::sync::CancellationToken;

use crate::memory::{MemoryAdapter, NodeHandle};
use crate::verify::ShellVerifier;
use crate::wiring;

/// How to start a session.
pub struct CoreOptions {
    /// Project directory (`None`: the resumed session's, else the current
    /// directory).
    pub cwd: Option<PathBuf>,
    /// Agent profile (default: the resumed session's, else `coder`).
    pub agent: Option<String>,
    /// Model alias or `provider/model`.
    pub model: Option<String>,
    /// Autonomy mode (default: `[general] mode`).
    pub mode: Option<Mode>,
    /// Scripted fake provider instead of a real model.
    pub fake_script: Option<PathBuf>,
    /// Turn limit (overrides the agent's).
    pub max_turns: Option<u32>,
    /// Token limit per turn (overrides the agent's).
    pub budget_tokens: Option<u64>,
    /// Cost limit per turn in USD (overrides the agent's and `[budget] run_usd`).
    pub budget_usd: Option<f64>,
    /// Run the verify command when the agent finishes.
    pub verify: bool,
    /// Headless (memory follows `[memory] headless_access`).
    pub headless: bool,
    /// Who answers approval prompts.
    pub approvals: Arc<dyn ApprovalUi>,
    /// Command patterns granted for the whole session.
    pub allow: Vec<String>,
    /// Who decides when a budget runs out (`[budget] on_exceed = ask`).
    pub budget_approver: Arc<dyn BudgetApprover>,
    /// Session to resume (id, id prefix or `last`).
    pub resume: Option<String>,
    /// First prompt (titles a new session).
    pub title: String,
    /// Snapshot the project before each turn (shadow git; `/undo`, diff
    /// revert). Interactive sessions turn this on.
    pub checkpoints: bool,
}

/// A resolved model ready to use.
#[derive(Clone)]
pub struct Model {
    /// The provider (scheduled).
    pub provider: Arc<dyn Provider>,
    /// `provider/model` reference.
    pub reference: String,
    /// The model id sent to the provider.
    pub id: String,
    /// Pricing (`None`: unknown or fake).
    pub cost: Option<CostFn>,
    /// Image input support.
    pub images: bool,
    /// Context window in tokens.
    pub context_window: u32,
    /// How the registry says the model reasons ([`ThinkingStyle::None`]
    /// for fake and unknown models; see [`Core::effort`]).
    pub thinking: ThinkingStyle,
    /// The effort levels the model accepts (empty: unknown).
    pub efforts: Vec<ReasoningEffort>,
}

/// Where the session's reasoning effort comes from (`/effort`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffortState {
    /// The `/effort` override for this session, if any.
    pub session: Option<Effort>,
    /// The agent YAML's `effort`, if any.
    pub agent: Option<Effort>,
    /// Whether the session's model reasons. When it does not, providers
    /// send no reasoning fields whatever the effort.
    pub reasons: bool,
    /// How the model's provider requests reasoning.
    pub style: ThinkingStyle,
    /// The levels the model accepts, least to most (empty: unknown).
    pub efforts: Vec<Effort>,
}

impl EffortState {
    /// The effort asked for (`None`: the model default).
    pub fn effective(&self) -> Option<Effort> {
        self.session.or(self.agent)
    }

    /// The level sent: the effective effort, or the nearest level the
    /// model accepts when it does not take that one.
    pub fn sent(&self) -> Option<Effort> {
        let e = self.effective()?;
        if self.efforts.is_empty() || self.efforts.contains(&e) {
            return Some(e);
        }
        self.efforts
            .iter()
            .copied()
            .filter(|x| *x < e)
            .max()
            .or_else(|| self.efforts.iter().copied().filter(|x| *x > e).min())
    }

    /// The effective effort's name, or `model default`.
    pub fn label(&self) -> &'static str {
        effort_name(self.effective())
    }

    /// `ultra (sent as max)` when the model does not accept the level,
    /// else [`EffortState::label`].
    pub fn sent_label(&self) -> String {
        match (self.effective(), self.sent()) {
            (Some(e), Some(s)) if e != s => {
                format!("{} (sent as {})", e.as_str(), s.as_str())
            }
            _ => self.label().to_owned(),
        }
    }

    /// Where the effective effort comes from.
    pub fn source(&self) -> &'static str {
        match (self.session, self.agent) {
            (Some(_), _) => "set with /effort for this session",
            (None, Some(_)) => "from the agent's YAML",
            (None, None) => "no effort is sent; the model decides",
        }
    }

    /// The status-line text: `effort low`, `no reasoning` when an effort
    /// is set on a model that does not reason, and `None` when no effort
    /// is set.
    pub fn status(&self) -> Option<String> {
        let e = self.effective()?;
        Some(if self.reasons {
            match self.sent() {
                Some(s) if s != e => format!("effort {}→{}", e.as_str(), s.as_str()),
                _ => format!("effort {}", e.as_str()),
            }
        } else {
            "no reasoning".into()
        })
    }
}

/// The level's name, or `model default` for `None`.
pub fn effort_name(effort: Option<Effort>) -> &'static str {
    effort.map_or("model default", Effort::as_str)
}

/// Longest a turn waits for a cache pre-warm still in flight.
pub const PREWARM_WAIT: std::time::Duration = std::time::Duration::from_secs(10);

/// One running session.
pub struct Core {
    /// harness.ini.
    pub cfg: Config,
    /// Data directory.
    pub data: DataDir,
    /// Project directory.
    pub cwd: PathBuf,
    /// Discovered definitions.
    pub catalog: Arc<Catalog>,
    /// The top-level agent.
    pub agent: Agent,
    /// Its model.
    pub model: Model,
    /// The run log (one run per session start).
    pub recorder: RunRecorder,
    /// The conversation shared by every turn.
    pub conversation: Conversation,
    /// Warnings for the user (missing tools, sandbox, resume drift).
    pub warnings: Vec<String>,
    /// Shared scheduler.
    pub scheduler: Scheduler,
    /// The session's memory checkout, when memory is on.
    pub memory: Option<Arc<SessionCheckout>>,
    /// Shadow-git checkpoints of the project, when enabled.
    pub checkpoints: Option<Arc<harness_hygiene::checkpoints::CheckpointStore>>,
    /// The checkpoint taken before each turn, oldest first.
    pub turn_checkpoints: Vec<(NodeId, u64)>,
    context: Arc<ContextManager>,
    session: SessionHandle,
    base_tools: ToolSet,
    runtime: SubagentRuntime,
    gate: Arc<PolicyGate>,
    labeler: Labeler,
    verifier: Option<Arc<ShellVerifier>>,
    hooks: Vec<Arc<dyn harness_agent::agent_loop::RequestHook>>,
    config: LoopConfig,
    memory_node: NodeHandle,
    last: Option<LoopOutcome>,
    /// `[general] session_summary_model`.
    summary: Option<crate::sessions::SummaryModel>,
    /// The summary of the last turn, running in the background.
    summary_task: Option<SummaryTask>,
    /// The session's shared spend meter (the budget hook charges it too).
    meter: BudgetMeter,
    redactor: Arc<Redactor>,
    /// MCP servers and the tools agents select from them.
    mcp: crate::mcp::McpWiring,
    /// The cache pre-warm in flight (`[experimental] cache_prewarm`).
    prewarm_task: Mutex<Option<tokio::task::JoinHandle<()>>>,
    /// Cancels pre-warms when the session ends.
    prewarm_cancel: CancellationToken,
    /// The node the next [`Core::turn`] runs on, allocated ahead so a
    /// pre-warm before the first turn hangs under it.
    next_turn_node: NodeId,
}

/// The background summary of the last turn ([`Core::spawn_summary`]).
struct SummaryTask {
    cancel: CancellationToken,
    handle: tokio::task::JoinHandle<Option<harness_agent::session::SummaryUpdate>>,
}

/// Resolves `name` to a model. The fake provider (when given) serves every
/// name; otherwise the registry picks the provider.
pub async fn resolve_model(
    cfg: &Config,
    fake: Option<&Arc<dyn Provider>>,
    name: &str,
    needs_tools: bool,
    scheduler: &Scheduler,
) -> Result<(Model, Vec<String>), String> {
    let (provider, model, warnings) = match fake {
        Some(fake) => {
            let id = name.strip_prefix("fake/").unwrap_or(name).to_owned();
            let caps = fake.capabilities(&id);
            let model = Model {
                provider: fake.clone(),
                reference: format!("fake/{id}"),
                id,
                cost: None,
                images: caps.images,
                context_window: caps.context_window,
                thinking: ThinkingStyle::None,
                efforts: Vec::new(),
            };
            (fake.clone(), model, Vec::new())
        }
        None => {
            let registry = wiring::registry(cfg)?;
            let resolved = registry.resolve(name).map_err(|e| e.to_string())?;
            let provider = wiring::provider(cfg, &registry, &resolved, needs_tools).await?;
            let caps = resolved.capabilities();
            let model = Model {
                provider: provider.clone(),
                reference: resolved.reference.to_string(),
                id: resolved.reference.model.clone(),
                cost: Some(wiring::cost_fn(&resolved)),
                images: caps.images,
                context_window: caps.context_window,
                thinking: resolved.info.thinking,
                efforts: if resolved.info.efforts.is_empty() {
                    harness_providers::registry::default_efforts(&resolved.reference, true)
                } else {
                    resolved.info.efforts.clone()
                },
            };
            (provider, model, resolved.warnings.clone())
        }
    };
    // `harness bench` times calls here; a no-op otherwise.
    let scheduled: Arc<dyn Provider> = harness_providers::timing::wrap(Arc::new(
        ScheduledProvider::new(provider, scheduler.clone()),
    ));
    Ok((
        Model {
            provider: scheduled,
            ..model
        },
        warnings,
    ))
}

/// The same as [`resolve_model`] without network checks, for subagents.
pub(crate) fn resolver(
    cfg: &Config,
    fake: Option<Arc<dyn Provider>>,
    scheduler: Scheduler,
) -> ModelResolver {
    let cfg = cfg.clone();
    Arc::new(move |name: &str| {
        let (provider, id) = match &fake {
            Some(f) => (
                f.clone(),
                name.strip_prefix("fake/").unwrap_or(name).to_owned(),
            ),
            None => {
                let registry = wiring::registry(&cfg)?;
                let resolved = registry.resolve(name).map_err(|e| e.to_string())?;
                (
                    wiring::provider_sync(&cfg, &registry, &resolved)?,
                    resolved.reference.model.clone(),
                )
            }
        };
        let scheduled: Arc<dyn Provider> = harness_providers::timing::wrap(Arc::new(
            ScheduledProvider::new(provider, scheduler.clone()),
        ));
        Ok((scheduled, id))
    })
}

/// The local executor, sandboxed when `[sandbox] enabled`.
pub fn executor(
    cfg: &Config,
    cwd: &Path,
    warnings: &mut Vec<String>,
) -> Result<Arc<dyn Exec>, String> {
    let local: Arc<dyn Exec> = Arc::new(LocalExec::new());
    if !cfg.sandbox.enabled {
        return Ok(local);
    }
    let launcher = Launcher::current_exe().map_err(|e| format!("sandbox launcher: {e}"))?;
    let sandboxed = SandboxedExec::new(local, launcher, SandboxConfig::from_ini(&cfg.sandbox, cwd));
    warnings.extend(sandboxed.status().warnings.iter().cloned());
    Ok(Arc::new(sandboxed))
}

impl Core {
    /// Starts (or resumes) a session and records `run_started`.
    pub async fn start(
        cfg: Config,
        data: DataDir,
        secrets: Vec<String>,
        redactor: Arc<Redactor>,
        opts: CoreOptions,
    ) -> Result<Core, String> {
        let mut warnings = Vec::new();
        let store = SessionStore::new(data.sessions());
        let resumed: Option<Resumed> = match &opts.resume {
            Some(id) => Some(store.resume(id, data.root()).map_err(|e| e.to_string())?),
            None => None,
        };
        let cwd = match (&opts.cwd, &resumed) {
            (None, Some(r)) => r.meta.project.clone(),
            _ => wiring::project_dir(opts.cwd.as_ref())?,
        };
        if let Some(r) = &resumed {
            warnings.extend(r.warnings.iter().cloned());
        }

        let trust = TrustStore::from_data_dir(&data);
        let roots = wiring::roots(&data, &cwd);
        warnings.extend(roots.notes.iter().cloned());
        let catalog = Arc::new(wiring::catalog_from(&cfg, &roots));
        for e in catalog.errors() {
            warnings.push(e.error.to_string());
        }
        let agent_name = opts
            .agent
            .clone()
            .or_else(|| resumed.as_ref().map(|r| r.meta.agent.clone()))
            .unwrap_or_else(|| wiring::DEFAULT_AGENT.into());
        let agent = wiring::agent(&catalog, &agent_name)?;

        // Model.
        let scheduler = Scheduler::new(SchedulerConfig::from_config(&cfg));
        let fake = match &opts.fake_script {
            Some(script) => Some(wiring::fake_provider(script)?),
            None => None,
        };
        let model_name = opts
            .model
            .clone()
            .or_else(|| resumed.as_ref().map(|r| r.meta.model.clone()))
            .or_else(|| agent.model.clone())
            .unwrap_or_else(|| cfg.general.default_model.clone());
        let (model, model_warnings) = resolve_model(
            &cfg,
            fake.as_ref(),
            &model_name,
            !agent.tools.is_empty(),
            &scheduler,
        )
        .await?;
        warnings.extend(model_warnings);
        if cfg.experimental.prewarm_connection {
            // Overlaps the TLS handshake with the rest of the startup.
            let provider = model.provider.clone();
            tokio::spawn(async move { provider.prewarm().await });
        }

        // Run log.
        let config_hash = harness_config::effective::config_hash(&cfg, Some(&catalog));
        let recorder = RunRecorder::create(
            data.root(),
            RunId::new(),
            redactor.clone(),
            RecorderOptions::default().with_config_hash(config_hash),
        )
        .await
        .map_err(|e| format!("run log: {e}"))?;
        let tasks = recorder.dir().join(TASKS_FILE);
        let board = match resumed.as_ref().and_then(|r| r.meta.runs.last()) {
            Some(prev) => {
                TaskBoard::continue_from(&run_dir(data.root(), *prev).join(TASKS_FILE), &tasks)
            }
            None => TaskBoard::open(&tasks),
        }
        .map(Arc::new)
        .map_err(|e| format!("task board: {e}"))?;
        harness_agent::todo::record_tasks(&board, recorder.clone());

        // Session.
        let meta = match &resumed {
            Some(r) => r.meta.clone(),
            None => SessionMeta::new(&cwd, &agent.name, &model.reference, &opts.title),
        };
        let mut session = SessionHandle::start(store, meta).map_err(|e| e.to_string())?;
        session
            .add_run(recorder.run_id())
            .map_err(|e| e.to_string())?;

        // Tools.
        let scrubber = EnvScrubber::new(cfg.secret_env_vars(), secrets.clone());
        let exec = executor(&cfg, &cwd, &mut warnings)?;
        let memory_node: NodeHandle = Arc::new(Mutex::new(None));
        let memory = open_memory(&cfg, &agent, &cwd, &session, opts.headless, &mut warnings);
        // One background-job manager for the session: the agent's bash and
        // every subagent's share it (ids, notices, /jobs, shutdown).
        let jobs = harness_tools::jobs::JobManager::new(
            harness_tools::jobs::JobLimits::from_config(&ToolsConfig::default()),
        );
        let tools_factory = tools_factory(
            &cfg,
            &cwd,
            exec.clone(),
            scrubber.clone(),
            &recorder,
            MemoryUse {
                checkout: memory.as_ref().map(|(c, _)| c.clone()),
                cap: None,
                headless: opts.headless,
            },
            jobs.clone(),
        );
        let mut base_tools = standard(&cwd, exec.clone(), scrubber.clone(), &recorder, &jobs);
        if let Some(bash) = base_tools.get(harness_tools::bash::NAME).cloned() {
            let ledger = Arc::new(
                harness_hygiene::ledger::Ledger::new(&cwd).with_sink(Arc::new(recorder.clone())),
            );
            base_tools.register(Arc::new(crate::ledger::LedgerBash::new(
                bash,
                ledger,
                memory_node.clone(),
            )));
        }
        if let Some((checkout, _)) = &memory
            && let Some(tool) =
                memory_tool(&cfg, &agent, opts.headless, None, checkout, &recorder, &cwd)
        {
            base_tools.register(Arc::new(MemoryAdapter::new(
                tool,
                recorder.clone(),
                memory_node.clone(),
            )));
        }

        // MCP servers (started on first use) and the tools agents select.
        let mcp = crate::mcp::wire(&cfg, &secrets, &recorder);

        // Policy and approvals.
        let engine = PolicyEngine::new(&policy_config(&cfg, memory.as_ref()), &cwd)
            .map_err(|e| format!("[policy]: {e}"))?
            .with_mcp(mcp.rules.clone())
            .with_overlay(
                format!("agent {}", agent.name),
                &PolicyOverlay::from(&agent.policy),
            )
            .map_err(|e| format!("agent {} policy: {e}", agent.name))?;
        for p in &opts.allow {
            crate::mcp::grant(engine.grants(), p);
        }
        let broker = ApprovalBroker::new(engine.grants().clone(), opts.approvals.clone(), &cwd)
            .with_events(Arc::new(RecorderApprovalEvents(recorder.clone())));
        // The agent's own `mode:` can only make the session stricter.
        let mode = harness_policy::approvals::ModeState::new(
            opts.mode.unwrap_or_else(|| Mode::from(cfg.general.mode)),
        )
        .effective(agent.mode.map(Mode::from));
        let bash_tools = base_tools.clone();
        let root = cwd.clone();
        let gate = Arc::new(
            PolicyGate::new(engine, Arc::new(broker), mode)
                .with_cwd(Arc::new(move || {
                    bash_tools.bash_cwd().unwrap_or_else(|| root.clone())
                }))
                .with_recorder(recorder.clone()),
        );

        // Tool results are labeled the same way for the agent and its
        // children (subagents, workflow agent steps).
        let project_trust = match trust.status(&cwd) {
            Ok(TrustStatus::Trusted { .. }) => ProjectTrust::Trusted,
            _ => ProjectTrust::Untrusted,
        };
        let labeler = Labeler::new(redactor.clone(), project_trust);
        // Subagents.
        let builder = CatalogChildBuilder::new(
            catalog.clone(),
            PromptVars::detect(&cwd),
            model_name.clone(),
            resolver(&cfg, fake.clone(), scheduler.clone()),
            tools_factory,
            gate.clone() as Arc<dyn ToolGate>,
        )
        .with_root(&cwd)
        .with_cost(cost_resolver(&cfg, fake.is_some()))
        .with_gates(agent_gates(gate.clone()));
        let mut runtime = SubagentRuntime::new(
            recorder.clone(),
            Arc::new(builder),
            SubagentLimits::from_config(&cfg),
        )
        .with_scheduler(scheduler.clone())
        .with_board(board.clone())
        .with_extra_tools(Arc::new(mcp.toolbox.clone()))
        .with_labeler(labeler.clone());
        let fan_out = cfg
            .experimental
            .stagger_subagents
            .then(|| harness_agent::stagger::FanOut::new(cfg.experimental.cache_prewarm));
        if let Some(gate) = &fan_out {
            runtime = runtime.with_fan_out(gate.clone());
        }
        // The agent's MCP tools are read now and fixed for the session.
        harness_agent::subagents::ExtraTools::prepare(&mcp.toolbox, &agent, &[]).await;
        warnings.extend(mcp.toolbox.take_warnings());
        let missing: Vec<String> = {
            let place = AgentPlace {
                node: NodeId::random(),
                depth: 0,
                cancel: CancellationToken::new(),
                slot: None,
            };
            let have = runtime.tools_for(&agent, &base_tools, &place);
            // Memory turned off by [memory] is a choice, not a missing tool.
            let memory_off =
                effective_access(agent.memory, &cfg.memory, opts.headless) == MemoryAccess::Off;
            agent
                .request_tools()
                .into_iter()
                .filter(|t| have.get(t).is_none())
                .filter(|t| !(memory_off && *t == harness_memory::tool::NAME))
                .map(str::to_owned)
                .collect()
        };
        for m in missing {
            warnings.push(format!(
                "agent `{}` lists tool `{m}`, which is not available here",
                agent.name
            ));
        }

        // Conversation: the frozen prefix (system prompt, memory index, skills).
        let conversation = match resumed {
            Some(r) => r.conversation,
            None => {
                let system = wiring::system_prompt(&catalog, &agent, &cwd)?;
                let skills = harness_config::skills::listing(
                    catalog.skills.active().iter().map(|e| &e.item),
                );
                let index = memory.as_ref().map(|(_, i)| i.text().to_owned());
                let prefix = SessionPrefix::freeze(
                    system,
                    index.as_deref().filter(|t| !t.is_empty()),
                    Some(skills.as_str()).filter(|t| !t.is_empty()),
                );
                Conversation::from_prefix(&prefix)
            }
        };

        // Loop settings and hooks.
        let mut config = LoopConfig::new(&agent.name, &model.id);
        config.thinking = wiring::thinking(&agent, session.meta().effort);
        config.limits = LoopLimits {
            max_turns: opts.max_turns.or(agent.max_turns),
            max_tokens: opts.budget_tokens.or(agent.budget.tokens),
            max_usd: opts.budget_usd.or(agent.budget.usd).or(cfg.budget.run_usd),
        };
        config.max_fix_attempts = cfg.verify.max_fix_attempts;
        config.early_tool_start = cfg.experimental.early_tool_start;
        let (context, mut hooks, meter) = hooks(
            &cfg,
            &data,
            &agent,
            &model,
            &recorder,
            opts.budget_approver.clone(),
        );
        // Fan-out pre-warms count against the session's budgets too.
        if let Some(gate) = &fan_out {
            gate.charge_to(meter.clone());
        }
        hooks.push(Arc::new(harness_agent::todo::TodoNudge::new(
            board.clone(),
            cfg.general.todo_nudge_after,
        )));
        // Ended background bash jobs are reported on the next request: the
        // agent's own, and those of subagents that have ended.
        if let Some(n) = harness_agent::job_notices::JobNotices::session(&base_tools) {
            hooks.push(Arc::new(n));
        }
        // Running jobs and long tool rounds ask for one-hour cache entries
        // (`[provider.anthropic] cache_ttl_auto_1h`); requests are unchanged.
        hooks.push(Arc::new(harness_agent::long_wait::LongWaitHook::new(
            base_tools.jobs(),
        )));
        let checkpoints = if opts.checkpoints {
            open_checkpoints(&data, &cwd, &mut warnings).await
        } else {
            None
        };
        // An explicit verify command runs whenever the agent finishes; an
        // auto-detected one only after changes.
        let spec = opts
            .verify
            .then(|| {
                harness_hygiene::verify::VerifySpec::resolve(
                    None,
                    agent.verify.as_deref().map(|c| (agent.name.as_str(), c)),
                    &cfg.verify,
                    &cwd,
                )
            })
            .flatten();
        if let Some(spec) = &spec {
            config.verify_when = match spec.source {
                harness_hygiene::verify::VerifySource::Detected(_) => VerifyWhen::OnChanges,
                _ => VerifyWhen::Always,
            };
        }
        let verifier = spec.map(|spec| {
            Arc::new(ShellVerifier::new(
                spec.command,
                cwd.clone(),
                Arc::new(LocalExec::new()),
                scrubber.scrub(std::env::vars()),
            ))
        });

        let cfg_summary = cfg.clone();
        let fake_summary = fake.clone();
        Ok(Core {
            cfg,
            data,
            cwd,
            catalog,
            agent,
            model,
            recorder,
            labeler,
            conversation,
            warnings,
            scheduler,
            memory: memory.as_ref().map(|(c, _)| c.clone()),
            checkpoints,
            turn_checkpoints: Vec::new(),
            context,
            session,
            base_tools,
            runtime,
            gate,
            verifier,
            hooks,
            config,
            memory_node,
            last: None,
            summary: crate::sessions::summary_model(&cfg_summary, fake_summary),
            summary_task: None,
            meter,
            redactor: redactor.clone(),
            mcp,
            prewarm_task: Mutex::new(None),
            prewarm_cancel: CancellationToken::new(),
            next_turn_node: NodeId::random(),
        })
    }

    /// Pre-warms the prompt cache for the next request in the background
    /// (`[experimental] cache_prewarm`; nothing happens when it is off or
    /// the provider has no pre-warm): the tools, system prompt and
    /// conversation so far, recorded as a
    /// [`harness_agent::prewarm::PREWARM_NODE`] node and charged to the
    /// budgets. Interactive sessions call it at start and resume (while
    /// the user types); [`Core::compact`] calls it after compacting. The
    /// next turn waits briefly for a pre-warm still in flight, so it reads
    /// the entry instead of writing it a second time.
    pub fn spawn_prewarm(&self, reason: &'static str) {
        if !self.cfg.experimental.cache_prewarm || self.meter.check().exceeded.is_some() {
            return;
        }
        let request = harness_agent::prewarm::prewarm_request(
            &self.config,
            &self.conversation,
            self.tool_specs(),
        );
        if !self.model.provider.can_prewarm_cache(&request) {
            return;
        }
        let provider = self.model.provider.clone();
        let recorder = self.recorder.clone();
        let cost = self.model.cost.clone();
        let meter = self.meter.clone();
        let cancel = self.prewarm_cancel.child_token();
        let parent = self.prewarm_parent();
        let handle = tokio::spawn(async move {
            let done = harness_agent::prewarm::prewarm(
                provider.as_ref(),
                request,
                &recorder,
                &parent,
                reason,
                cost.as_ref(),
                &cancel,
            )
            .await;
            if let Some(Ok(report)) = done
                && let Some(usd) = report.cost_usd
            {
                meter.charge(usd);
            }
        });
        *self.prewarm_task.lock().unwrap_or_else(|e| e.into_inner()) = Some(handle);
    }

    /// The parent of a pre-warm node: the latest turn's node, or before
    /// the first turn the node that turn will run on.
    fn prewarm_parent(&self) -> NodeId {
        self.memory_node
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
            .unwrap_or_else(|| self.next_turn_node.clone())
    }

    /// Waits up to [`PREWARM_WAIT`] for a pre-warm in flight.
    async fn settle_prewarm(&self) {
        let task = self
            .prewarm_task
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take();
        if let Some(t) = task {
            let _ = tokio::time::timeout(PREWARM_WAIT, t).await;
        }
    }

    /// The session's MCP servers (the TUI's `/mcp` panel, prompts and
    /// resources).
    pub(crate) fn mcp_servers(&self) -> Arc<harness_mcp::Servers> {
        self.mcp.toolbox.servers().clone()
    }

    /// Labels untrusted content for the model (attached MCP resources).
    pub(crate) fn labeler(&self) -> &Labeler {
        &self.labeler
    }

    /// Records `run_started`. Call it once, after subscribing to the
    /// recorder, before the first turn.
    pub fn begin(&self) {
        let _ = self.recorder.emit(EventDraft::run(EventKind::RunStarted {
            name: self.agent.name.clone(),
            workflow: None,
            host: None,
            title: None,
            agent_names: harness_config::agents::display_names(
                self.catalog
                    .agents
                    .active()
                    .iter()
                    .map(|e| &e.item)
                    .chain([&self.agent]),
            ),
        }));
    }

    /// The session id.
    pub fn session_id(&self) -> &str {
        &self.session.meta().id
    }

    /// The session metadata.
    pub fn session(&self) -> &SessionMeta {
        self.session.meta()
    }

    /// Renames the session (`/rename`). The name is kept from then on.
    /// A summary still running in the background is cancelled first.
    pub async fn rename(&mut self, name: &str) -> Result<String, String> {
        self.settle_summary(None).await;
        self.session.rename(name).map_err(|e| e.to_string())?;
        Ok(self.session.meta().name.clone())
    }

    /// Starts the optional cheap-model summary of the last turn
    /// (`[general] session_summary_model`, off by default) in the
    /// background, so it never delays the turn. It is skipped after a
    /// cancelled turn and while a budget is exhausted; its call is a
    /// [`crate::sessions::SUMMARY_NODE`] child node of `parent` and its
    /// cost is charged to the budgets. The task saves the name and
    /// description to the session file itself;
    /// [`settle_summary`](Self::settle_summary) mirrors them here.
    fn spawn_summary(&mut self, parent: NodeId) {
        let Some(summary) = self.summary.clone() else {
            return;
        };
        if let Some(exceeded) = self.meter.check().exceeded {
            tracing::info!(target: "harness::session", %exceeded, "session summary skipped");
            return;
        }
        let cancel = CancellationToken::new();
        let token = cancel.clone();
        let conversation = self.conversation.clone();
        let redactor = self.redactor.clone();
        let recorder = self.recorder.clone();
        let meter = self.meter.clone();
        let store = self.session.store().clone();
        let id = self.session.meta().id.clone();
        let handle = tokio::spawn(async move {
            let update = crate::sessions::summarize(
                &summary,
                &conversation,
                &redactor,
                &recorder,
                &parent,
                &token,
            )
            .await?;
            if let Some(usd) = update.cost_usd {
                meter.charge(usd);
            }
            if let Err(e) = store.update(&id, |m| m.apply_summary(&update)) {
                tracing::warn!(target: "harness::session", error = %e, "could not save the session summary");
            }
            Some(update)
        });
        self.summary_task = Some(SummaryTask { cancel, handle });
    }

    /// Ends the background summary, if any, and applies what it saved to
    /// the live session. With `wait`, it may finish (until `wait` fires);
    /// without, it is cancelled at once (a new turn, `/rename`, or the
    /// end of the session).
    async fn settle_summary(&mut self, wait: Option<&CancellationToken>) {
        let Some(SummaryTask { cancel, mut handle }) = self.summary_task.take() else {
            return;
        };
        let done = match wait {
            Some(w) => tokio::select! {
                r = &mut handle => Some(r),
                _ = w.cancelled() => None,
            },
            None => None,
        };
        let done = match done {
            Some(r) => r,
            None => {
                cancel.cancel();
                handle.await
            }
        };
        if let Ok(Some(update)) = done
            && let Err(e) = self.session.apply_summary(&update)
        {
            tracing::warn!(error = %e, "could not save the session summary");
        }
    }

    /// Waits for the background summary of the last turn (headless runs,
    /// which end right after their turn), unless `cancel` fires first.
    pub async fn wait_summary(&mut self, cancel: &CancellationToken) {
        self.settle_summary(Some(cancel)).await;
    }

    /// Shared handle to the effective autonomy mode.
    pub fn mode_handle(&self) -> Arc<Mutex<Mode>> {
        self.gate.mode_handle()
    }

    /// Runs one user turn on a new node and saves the session.
    pub async fn turn(&mut self, prompt: String, cancel: &CancellationToken) -> LoopOutcome {
        self.settle_summary(None).await;
        self.settle_prewarm().await;
        // Pick up MCP tool-list changes announced since the last turn.
        self.mcp.toolbox.refresh().await;
        let node = std::mem::replace(&mut self.next_turn_node, NodeId::random());
        self.checkpoint(&node).await;
        *self.memory_node.lock().unwrap_or_else(|e| e.into_inner()) = Some(node.clone());
        let place = AgentPlace {
            node: node.clone(),
            depth: 0,
            cancel: cancel.clone(),
            slot: None,
        };
        let tools = self
            .runtime
            .tools_for(&self.agent, &self.base_tools, &place);
        let dispatcher = Dispatcher::new(tools, self.gate.clone())
            .with_images(self.model.images)
            .with_events(Arc::new(RecorderToolEvents::new(
                self.recorder.clone(),
                node.clone(),
                &self.cwd,
            )));
        let mut agent_loop = AgentLoop::new(
            self.model.provider.clone(),
            dispatcher,
            self.recorder.clone(),
            self.config.clone(),
        )
        .with_node(node.clone())
        .with_labeler(self.labeler.clone());
        if let Some(cost) = &self.model.cost {
            agent_loop = agent_loop.with_cost(cost.clone());
        }
        if let Some(v) = &self.verifier {
            agent_loop = agent_loop.with_verifier(v.clone());
        }
        for h in &self.hooks {
            agent_loop = agent_loop.with_hook(harness_agent::timing::wrap_hook(h.clone()));
        }
        self.conversation.push_user(prompt);
        let outcome = agent_loop.run(&mut self.conversation, cancel).await;
        drop(agent_loop);
        if let Err(e) = self.session.record(&outcome, &self.conversation) {
            tracing::warn!(error = %e, "could not save the session");
            self.warnings
                .push(format!("could not save the session: {e}"));
        }
        if outcome.stop != StopReason::Cancelled && !cancel.is_cancelled() {
            self.spawn_summary(node);
        }
        self.last = Some(outcome.clone());
        outcome
    }

    /// Snapshots the project before a turn on `node`.
    async fn checkpoint(&mut self, node: &NodeId) {
        let Some(store) = self.checkpoints.clone() else {
            return;
        };
        let n = self.turn_checkpoints.len() as u32 + 1;
        let id = node.clone();
        let label = format!("before turn {n}");
        let done = tokio::task::spawn_blocking(move || {
            store.snapshot(
                harness_hygiene::checkpoints::CheckpointKind::Turn,
                Some(&id),
                Some(n),
                &label,
            )
        })
        .await;
        match done {
            Ok(Ok(cp)) => self.turn_checkpoints.push((node.clone(), cp.id)),
            Ok(Err(e)) => self.warnings.push(format!("checkpoint failed: {e}")),
            Err(e) => self.warnings.push(format!("checkpoint failed: {e}")),
        }
    }

    /// The session's background bash jobs (the TUI's `/jobs` panel).
    pub fn jobs(&self) -> Option<harness_tools::jobs::JobManager> {
        self.base_tools.jobs()
    }

    /// The tool definitions the top-level agent sends.
    pub fn tool_specs(&self) -> Vec<harness_providers::model::ToolSpec> {
        let place = AgentPlace {
            node: NodeId::random(),
            depth: 0,
            cancel: CancellationToken::new(),
            slot: None,
        };
        self.runtime
            .tools_for(&self.agent, &self.base_tools, &place)
            .specs()
    }

    /// Sets the effective autonomy mode for the next tool calls.
    pub fn set_mode(&self, mode: Mode) {
        let mode = harness_policy::approvals::ModeState::new(mode)
            .effective(self.agent.mode.map(Mode::from));
        *self
            .gate
            .mode_handle()
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = mode;
    }

    /// The context manager (`/compact`).
    pub fn context(&self) -> &Arc<ContextManager> {
        &self.context
    }

    /// Compacts the conversation now (`/compact` while idle).
    pub async fn compact(
        &mut self,
        focus: Option<&str>,
    ) -> Result<harness_agent::compaction::CompactionReport, String> {
        let node = NodeId::random();
        let report = self
            .context
            .compact_now(
                &mut self.conversation,
                &self.recorder,
                &node,
                focus,
                &CancellationToken::new(),
            )
            .await
            .map_err(|e| e.to_string())?;
        // The user types the next prompt now: write the compacted prefix.
        self.spawn_prewarm("compaction");
        Ok(report)
    }

    /// Switches the model for the next turns.
    pub async fn set_model(&mut self, name: &str) -> Result<Vec<String>, String> {
        let fake = self
            .model
            .reference
            .starts_with("fake/")
            .then(|| self.model.provider.clone());
        let (model, warnings) = resolve_model(
            &self.cfg,
            fake.as_ref(),
            name,
            !self.agent.tools.is_empty(),
            &self.scheduler,
        )
        .await?;
        self.config.model = model.id.clone();
        self.model = model;
        Ok(warnings)
    }

    /// The session's reasoning effort: the `/effort` override, the agent
    /// YAML's `effort` and whether the model reasons.
    pub fn effort(&self) -> EffortState {
        let caps = self.model.provider.capabilities(&self.model.id);
        EffortState {
            session: self.session.meta().effort,
            agent: self.agent.effort,
            reasons: caps.thinking || self.model.thinking != ThinkingStyle::None,
            style: self.model.thinking,
            efforts: self
                .model
                .efforts
                .iter()
                .filter_map(|e| Effort::parse(e.as_str()))
                .collect(),
        }
    }

    /// Sets the reasoning effort for the rest of the session (`/effort`):
    /// the next request carries it, and the session file keeps it for
    /// resume. `None` restores the agent YAML's `effort` or the model
    /// default.
    pub fn set_effort(&mut self, effort: Option<Effort>) -> Result<EffortState, String> {
        self.session
            .set_effort(effort)
            .map_err(|e| format!("session: {e}"))?;
        self.config.thinking = wiring::thinking(&self.agent, effort);
        Ok(self.effort())
    }

    /// Records `run_finished` (status from the last turn) and closes the log.
    pub async fn finish(mut self) -> Result<(), String> {
        self.settle_summary(None).await;
        // A background compaction summary still pending ends (recorded)
        // before the run log closes.
        self.context.stop_background().await;
        self.prewarm_cancel.cancel();
        self.settle_prewarm().await;
        self.mcp.shutdown().await;
        // Background jobs belong to the session: stop them (whole process
        // groups) before the run log closes, so their end is recorded.
        if let Some(jobs) = self.base_tools.jobs() {
            jobs.shutdown().await;
        }
        let (status, error) = match &self.last {
            Some(o) => (run_status(o), finished_error(o)),
            None => (RunStatus::Cancelled, None),
        };
        let _ = self
            .recorder
            .emit(EventDraft::run(EventKind::RunFinished { status, error }));
        self.recorder
            .finish()
            .await
            .map_err(|e| format!("run log: {e}"))
    }
}

/// The run status for a loop outcome.
pub fn run_status(outcome: &LoopOutcome) -> RunStatus {
    match outcome.stop {
        StopReason::EndTurn if outcome.verified != Some(false) => RunStatus::Succeeded,
        StopReason::Cancelled => RunStatus::Cancelled,
        _ => RunStatus::Failed,
    }
}

/// The error recorded for a loop outcome.
pub fn finished_error(outcome: &LoopOutcome) -> Option<String> {
    match (&outcome.stop, outcome.verified) {
        (StopReason::EndTurn, Some(false)) => Some("verification failed".to_owned()),
        (stop, _) => stop.error_text(),
    }
}

/// read, write, edit and bash in `cwd`, spilling to the run's blobs.
fn standard(
    cwd: &Path,
    exec: Arc<dyn Exec>,
    scrubber: EnvScrubber,
    recorder: &RunRecorder,
    jobs: &harness_tools::jobs::JobManager,
) -> ToolSet {
    ToolSet::standard_with(
        StandardTools {
            workspace: Arc::new(LocalWorkspace::new(cwd)),
            exec,
            cwd: cwd.to_path_buf(),
            config: ToolsConfig::default(),
            scrubber,
            spill: Arc::new(RunBlobSink(recorder.blobs().clone())),
        },
        harness_agent::dispatch::StandardOptions {
            base_env: None,
            jobs: Some(jobs.clone()),
        },
    )
}

pub(crate) type Memory = (Arc<SessionCheckout>, harness_memory::index::SessionIndex);

/// Opens the memory drive and this session's checkout when the agent may
/// use memory. Failures are warnings: the session runs without memory.
fn open_memory(
    cfg: &Config,
    agent: &Agent,
    cwd: &Path,
    session: &SessionHandle,
    headless: bool,
    warnings: &mut Vec<String>,
) -> Option<Memory> {
    if effective_access(agent.memory, &cfg.memory, headless) == MemoryAccess::Off {
        return None;
    }
    let open = || -> Result<Memory, String> {
        let drive = harness_memory::drive::Drive::from_config(cfg).map_err(|e| e.to_string())?;
        let checkout =
            SessionCheckout::open(&drive, &session.meta().id).map_err(|e| e.to_string())?;
        let key = harness_memory::drive::project::project_key(cwd);
        let index = harness_memory::index::SessionIndex::load(
            checkout.path(),
            Some(key.as_str()),
            cfg.memory.index_max_tokens,
        )
        .map_err(|e| e.to_string())?;
        Ok((Arc::new(checkout), index))
    };
    match open() {
        Ok(m) => Some(m),
        Err(e) => {
            warnings.push(format!("memory is unavailable this session: {e}"));
            None
        }
    }
}

/// The memory tool for `agent`, or `None` when it has no memory access.
fn memory_tool(
    cfg: &Config,
    agent: &Agent,
    headless: bool,
    cap: Option<MemoryAccess>,
    checkout: &Arc<SessionCheckout>,
    recorder: &RunRecorder,
    cwd: &Path,
) -> Option<MemoryTool> {
    let access = effective_access(agent.memory, &cfg.memory, headless);
    MemoryTool::for_agent(
        checkout.clone(),
        MemoryToolConfig {
            access: cap.map_or(access, |c| access.min(c)),
            run_id: recorder.run_id().to_string(),
            project_key: Some(harness_memory::drive::project::project_key(cwd).key),
            redactor: recorder.redactor().clone(),
            date: None,
        },
    )
}

/// The memory subagents may use.
pub(crate) struct MemoryUse {
    /// The checkout memory tools work in (`None`: no memory).
    pub checkout: Option<Arc<SessionCheckout>>,
    /// The most access any agent gets (a workflow's `memory:`).
    pub cap: Option<MemoryAccess>,
    /// Headless run (`[memory] headless_access` applies).
    pub headless: bool,
}

/// Builds each subagent's base tools. Agents get memory as their own
/// `memory:` allows, never more than `memory.cap`. Their bash starts
/// background jobs in `jobs` (the session's manager).
pub(crate) fn tools_factory(
    cfg: &Config,
    cwd: &Path,
    exec: Arc<dyn Exec>,
    scrubber: EnvScrubber,
    recorder: &RunRecorder,
    memory: MemoryUse,
    jobs: harness_tools::jobs::JobManager,
) -> ToolsFactory {
    let MemoryUse {
        checkout: memory,
        cap,
        headless,
    } = memory;
    let cfg = cfg.clone();
    let cwd = cwd.to_path_buf();
    let recorder = recorder.clone();
    Arc::new(move |agent: &Agent| {
        let mut set = standard(&cwd, exec.clone(), scrubber.clone(), &recorder, &jobs);
        if let Some(checkout) = &memory
            && let Some(tool) = memory_tool(&cfg, agent, headless, cap, checkout, &recorder, &cwd)
        {
            set.register(Arc::new(MemoryAdapter::new(
                tool,
                recorder.clone(),
                Arc::new(Mutex::new(None)),
            )));
        }
        set
    })
}

/// Each subagent's gate: the session's, capped at the agent's `mode:`.
pub(crate) fn agent_gates(gate: Arc<PolicyGate>) -> harness_agent::subagents::GateFactory {
    Arc::new(move |agent: &Agent| match agent.mode {
        Some(m) => Arc::new(harness_agent::dispatch::CappedGate::new(
            gate.clone(),
            Mode::from(m),
        )) as Arc<dyn ToolGate>,
        None => gate.clone() as Arc<dyn ToolGate>,
    })
}

/// Prices subagent calls by model name.
pub(crate) fn cost_resolver(cfg: &Config, fake: bool) -> harness_agent::subagents::CostResolver {
    let registry = (!fake).then(|| wiring::registry(cfg).ok()).flatten();
    Arc::new(move |name: &str| {
        let resolved = registry.as_ref()?.resolve(name).ok()?;
        Some(wiring::cost_fn(&resolved))
    })
}

/// `[policy]` plus the built-in protected paths and a custom memory drive.
pub(crate) fn policy_config(cfg: &Config, memory: Option<&Memory>) -> PolicyConfig {
    let mut p = PolicyConfig::from_config(cfg);
    if let Some((checkout, _)) = memory {
        p.protected_roots.extend(checkout.drive().protected_paths());
    }
    p
}

/// Context management and budget hooks for the top-level loop.
fn hooks(
    cfg: &Config,
    data: &DataDir,
    agent: &Agent,
    model: &Model,
    recorder: &RunRecorder,
    approver: Arc<dyn BudgetApprover>,
) -> (
    Arc<ContextManager>,
    Vec<Arc<dyn harness_agent::agent_loop::RequestHook>>,
    BudgetMeter,
) {
    let mut compactor = Compactor::new(
        model.provider.clone(),
        model.id.clone(),
        cfg.context.keep_recent_turns,
    );
    if let Some(cost) = &model.cost {
        compactor = compactor.with_cost(cost.clone());
    }
    let context = ContextManager::new(ContextSettings::from_config(
        &cfg.context,
        model.context_window,
    ))
    .with_elision(Arc::new(RunBlobSink(recorder.blobs().clone())))
    .with_compactor(compactor);
    let today = time::OffsetDateTime::now_utc().date();
    let spent_today = harness_agent::cost::daily_spend(data.root(), today)
        .map(|t| t.cost_usd)
        .unwrap_or(0.0);
    let meter = BudgetMeter::new(Budgets::from_config(&cfg.budget))
        .with_spent(BudgetScope::Day, spent_today);
    let budget = BudgetGuard::new(meter.clone(), approver).with_agent_limit(agent.budget.usd);
    let context = Arc::new(context);
    (context.clone(), vec![context, Arc::new(budget)], meter)
}

/// Opens the shadow-git checkpoint store of the project.
async fn open_checkpoints(
    data: &DataDir,
    cwd: &Path,
    warnings: &mut Vec<String>,
) -> Option<Arc<harness_hygiene::checkpoints::CheckpointStore>> {
    let dir = data.checkpoints();
    let root = cwd.to_path_buf();
    let opened = tokio::task::spawn_blocking(move || {
        harness_hygiene::checkpoints::CheckpointStore::open(&dir, &root)
    })
    .await;
    match opened {
        Ok(Ok(store)) => Some(Arc::new(store)),
        Ok(Err(e)) => {
            warnings.push(format!("checkpoints are off this session: {e}"));
            None
        }
        Err(e) => {
            warnings.push(format!("checkpoints are off this session: {e}"));
            None
        }
    }
}
