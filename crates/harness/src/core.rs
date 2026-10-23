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
use harness_config::agents::{Agent, MemoryAccess};
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
use harness_providers::provider::Provider;
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
}

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
            };
            (provider, model, resolved.warnings.clone())
        }
    };
    let scheduled: Arc<dyn Provider> =
        Arc::new(ScheduledProvider::new(provider, scheduler.clone()));
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
        let scheduled: Arc<dyn Provider> =
            Arc::new(ScheduledProvider::new(provider, scheduler.clone()));
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
        );
        let mut base_tools = standard(&cwd, exec.clone(), scrubber.clone(), &recorder);
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

        // Policy and approvals.
        let engine = PolicyEngine::new(&policy_config(&cfg, memory.as_ref()), &cwd)
            .map_err(|e| format!("[policy]: {e}"))?
            .with_overlay(
                format!("agent {}", agent.name),
                &PolicyOverlay::from(&agent.policy),
            )
            .map_err(|e| format!("agent {} policy: {e}", agent.name))?;
        for p in &opts.allow {
            engine.grants().allow_command(p);
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
        let runtime = SubagentRuntime::new(
            recorder.clone(),
            Arc::new(builder),
            SubagentLimits::from_config(&cfg),
        )
        .with_scheduler(scheduler.clone())
        .with_board(board.clone());
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
        config.thinking = wiring::thinking(&agent);
        config.limits = LoopLimits {
            max_turns: opts.max_turns.or(agent.max_turns),
            max_tokens: opts.budget_tokens.or(agent.budget.tokens),
            max_usd: opts.budget_usd.or(agent.budget.usd).or(cfg.budget.run_usd),
        };
        config.max_fix_attempts = cfg.verify.max_fix_attempts;
        let project_trust = match trust.status(&cwd) {
            Ok(TrustStatus::Trusted { .. }) => ProjectTrust::Trusted,
            _ => ProjectTrust::Untrusted,
        };
        let (context, hooks) = hooks(
            &cfg,
            &data,
            &agent,
            &model,
            &recorder,
            opts.budget_approver.clone(),
        );
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

        Ok(Core {
            cfg,
            data,
            cwd,
            catalog,
            agent,
            model,
            recorder,
            labeler: Labeler::new(redactor.clone(), project_trust),
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
        })
    }

    /// Records `run_started`. Call it once, after subscribing to the
    /// recorder, before the first turn.
    pub fn begin(&self) {
        let _ = self.recorder.emit(EventDraft::run(EventKind::RunStarted {
            name: self.agent.name.clone(),
            workflow: None,
            host: None,
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

    /// Shared handle to the effective autonomy mode.
    pub fn mode_handle(&self) -> Arc<Mutex<Mode>> {
        self.gate.mode_handle()
    }

    /// Runs one user turn on a new node and saves the session.
    pub async fn turn(&mut self, prompt: String, cancel: &CancellationToken) -> LoopOutcome {
        let node = NodeId::random();
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
        .with_node(node)
        .with_labeler(self.labeler.clone());
        if let Some(cost) = &self.model.cost {
            agent_loop = agent_loop.with_cost(cost.clone());
        }
        if let Some(v) = &self.verifier {
            agent_loop = agent_loop.with_verifier(v.clone());
        }
        for h in &self.hooks {
            agent_loop = agent_loop.with_hook(h.clone());
        }
        self.conversation.push_user(prompt);
        let outcome = agent_loop.run(&mut self.conversation, cancel).await;
        drop(agent_loop);
        if let Err(e) = self.session.record(&outcome, &self.conversation) {
            tracing::warn!(error = %e, "could not save the session");
            self.warnings
                .push(format!("could not save the session: {e}"));
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
        self.context
            .compact_now(
                &mut self.conversation,
                &self.recorder,
                &node,
                focus,
                &CancellationToken::new(),
            )
            .await
            .map_err(|e| e.to_string())
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

    /// Records `run_finished` (status from the last turn) and closes the log.
    pub async fn finish(self) -> Result<(), String> {
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
) -> ToolSet {
    ToolSet::standard(StandardTools {
        workspace: Arc::new(LocalWorkspace::new(cwd)),
        exec,
        cwd: cwd.to_path_buf(),
        config: ToolsConfig::default(),
        scrubber,
        spill: Arc::new(RunBlobSink(recorder.blobs().clone())),
    })
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
/// `memory:` allows, never more than `memory.cap`.
pub(crate) fn tools_factory(
    cfg: &Config,
    cwd: &Path,
    exec: Arc<dyn Exec>,
    scrubber: EnvScrubber,
    recorder: &RunRecorder,
    memory: MemoryUse,
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
        let mut set = standard(&cwd, exec.clone(), scrubber.clone(), &recorder);
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
    let budget = BudgetGuard::new(meter, approver).with_agent_limit(agent.budget.usd);
    let context = Arc::new(context);
    (context.clone(), vec![context, Arc::new(budget)])
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
