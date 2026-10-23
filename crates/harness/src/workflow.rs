//! `harness wf`: list, describe and run workflows headlessly.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use harness_agent::dispatch::{PolicyGate, ToolGate};
use harness_agent::scheduler::{Scheduler, SchedulerConfig};
use harness_agent::subagents::{
    CatalogChildBuilder, ChildBuilder, ChildParts, SubagentLimits, SubagentRuntime,
};
use harness_agent::todo::{TASKS_FILE, TaskBoard};
use harness_config::discovery::Catalog;
use harness_config::ini::Config;
use harness_config::paths::DataDir;
use harness_config::prompts::PromptVars;
use harness_events::redact::Redactor;
use harness_events::schema::RunId;
use harness_events::store::{RecorderOptions, RunRecorder, run_dir};
use harness_hygiene::checkpoints::CheckpointStore;
use harness_policy::approvals::{ApprovalBroker, RecorderApprovalEvents};
use harness_policy::rules::{Mode, PolicyEngine};
use harness_tools::bash::EnvScrubber;
use harness_workflow::commands;
use harness_workflow::executor::{Executor, Runners, StepRunner};
use harness_workflow::gates::{ApprovalGate, HeadlessGates};
use harness_workflow::rerun::{ReusePlan, WorkflowStatus, plan_rerun, plan_resume};
use harness_workflow::runners::{
    AgentStepRunner, ProofStepRunner, ShellStepRunner, StoreCheckpointer, VerifyStepRunner,
};
use harness_workflow::schema::{CheckContext, Workflow, from_entry};
use tokio_util::sync::CancellationToken;

use crate::cli::{Approvals, WfArgs, exit};
use crate::core;
use crate::headless::{HeadlessUi, JsonPrinter, parse_mode, spawn_signal_cancel};
use crate::wiring;

/// Every valid workflow of the catalog, by name, plus load problems.
pub fn library(catalog: &Catalog, cfg: &Config) -> (BTreeMap<String, Workflow>, Vec<String>) {
    let ctx = CheckContext::from_catalog(catalog, Some(cfg));
    let mut out = BTreeMap::new();
    let mut errors = Vec::new();
    for entry in catalog.workflows.active() {
        match from_entry(entry, &ctx) {
            Ok(wf) => {
                out.insert(wf.name.clone(), wf);
            }
            Err(es) => errors.extend(es.iter().map(ToString::to_string)),
        }
    }
    (out, errors)
}

/// The workflow called `name`, or whose slash command is `name`.
pub fn find<'a>(lib: &'a BTreeMap<String, Workflow>, name: &str) -> Result<&'a Workflow, String> {
    let name = name.trim_start_matches('/');
    lib.get(name)
        .or_else(|| lib.values().find(|w| w.command == name))
        .ok_or_else(|| {
            let known: Vec<String> = lib
                .values()
                .map(|w| format!("{} (/{})", w.name, w.command))
                .collect();
            format!("unknown workflow `{name}`; known: {}", known.join(", "))
        })
}

/// Quotes a word for [`harness_config::skills::split_args`].
fn quote(word: &str) -> String {
    if !word.is_empty()
        && !word
            .chars()
            .any(|c| c.is_whitespace() || "'\"\\".contains(c))
    {
        return word.to_owned();
    }
    format!("'{}'", word.replace('\'', "'\\''"))
}

/// The validated inputs from slash-command arguments plus `--input` pairs.
pub fn inputs(
    wf: &Workflow,
    args: &WfArgs,
) -> Result<serde_json::Map<String, serde_json::Value>, String> {
    let line: Vec<String> = args.args.iter().map(|w| quote(w)).collect();
    let mut parsed = commands::parse_args(wf, &line.join(" "))?;
    let given: Vec<&str> = args.inputs.iter().map(|(k, _)| k.as_str()).collect();
    parsed.missing.retain(|m| !given.contains(&m.name.as_str()));
    if let Some(m) = parsed.missing.first() {
        return Err(format!(
            "missing input `{}` ({}); usage: {}",
            m.name,
            m.hint(),
            commands::command(wf).usage()
        ));
    }
    commands::complete(wf, &parsed, &args.inputs)
}

/// `harness wf list|help|run`.
pub async fn main(
    args: &[String],
    cfg: Config,
    data: DataDir,
    secrets: Vec<String>,
    redactor: Arc<Redactor>,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> u8 {
    let usage = |err: &mut dyn Write, msg: &str| {
        let _ = writeln!(err, "harness wf: {msg}\n\n{}", crate::cli::WF_USAGE);
        exit::USAGE
    };
    let (sub, rest) = match args.split_first() {
        Some((s, r)) => (s.as_str(), r),
        None => return usage(err, "a subcommand is required"),
    };
    if rest.iter().any(|a| a == "--help" || a == "-h") || sub == "--help" {
        let _ = writeln!(out, "{}", crate::cli::WF_USAGE);
        return exit::SUCCESS;
    }
    match sub {
        "list" | "help" => {
            let cwd = match wiring::project_dir(None) {
                Ok(c) => c,
                Err(e) => {
                    let _ = writeln!(err, "harness: {e}");
                    return exit::ERROR;
                }
            };
            let catalog = wiring::catalog(&cfg, &data, &cwd);
            let (lib, errors) = library(&catalog, &cfg);
            for e in errors {
                let _ = writeln!(err, "harness: warning: {e}");
            }
            if sub == "list" {
                for wf in lib.values() {
                    let c = commands::command(wf);
                    let _ = writeln!(out, "{:<28} {}", c.usage(), c.description);
                }
                return exit::SUCCESS;
            }
            let Some(name) = rest.first() else {
                return usage(err, "help needs a workflow name");
            };
            match find(&lib, name) {
                Ok(wf) => {
                    let _ = write!(out, "{}", commands::help(wf));
                    exit::SUCCESS
                }
                Err(e) => {
                    let _ = writeln!(err, "harness wf: {e}");
                    exit::ERROR
                }
            }
        }
        "run" => {
            let wf_args = match crate::cli::parse_wf(rest) {
                Ok(a) => a,
                Err(e) => return usage(err, &e),
            };
            match run(wf_args, cfg, data, secrets, redactor, err).await {
                Ok(code) => code,
                Err(e) => {
                    let _ = writeln!(err, "harness: {e}");
                    exit::ERROR
                }
            }
        }
        other => usage(err, &format!("unknown subcommand `{other}`")),
    }
}

/// Caps each child's turns at `max_turns` (`harness wf run --max-turns`).
struct Capped {
    inner: Arc<dyn ChildBuilder>,
    max_turns: u32,
}

#[async_trait]
impl ChildBuilder for Capped {
    async fn build(&self, agent: &str, model: Option<&str>) -> Result<ChildParts, String> {
        let mut parts = self.inner.build(agent, model).await?;
        parts.agent.max_turns = Some(
            parts
                .agent
                .max_turns
                .map_or(self.max_turns, |t| t.min(self.max_turns)),
        );
        Ok(parts)
    }

    fn describe(&self, agent: &str) -> Option<String> {
        self.inner.describe(agent)
    }
}

fn reuse_plan(data: &DataDir, wf: &Workflow, args: &WfArgs) -> Result<Option<ReusePlan>, String> {
    let dir = |id: &str| -> Result<std::path::PathBuf, String> {
        let run: RunId = harness_tui::history::show::resolve_run(data.root(), id)?;
        Ok(run_dir(data.root(), run))
    };
    if let Some(id) = &args.resume {
        return plan_resume(&dir(id)?, wf)
            .map(Some)
            .map_err(|e| e.to_string());
    }
    if let Some(id) = &args.rerun {
        return plan_rerun(&dir(id)?, wf, &args.from)
            .map(Some)
            .map_err(|e| e.to_string());
    }
    Ok(None)
}

/// Everything needed to build a workflow executor.
pub struct Setup<'a> {
    /// harness.ini.
    pub cfg: &'a Config,
    /// Data directory.
    pub data: &'a DataDir,
    /// Resolved credentials (scrubbed from step environments).
    pub secrets: Vec<String>,
    /// The run's redactor.
    pub redactor: Arc<Redactor>,
    /// Project directory.
    pub cwd: &'a Path,
    /// Discovered definitions.
    pub catalog: Arc<Catalog>,
    /// Workflows by name.
    pub library: BTreeMap<String, Workflow>,
    /// Who decides approval steps.
    pub gates: Arc<dyn ApprovalGate>,
    /// Who answers tool approvals of agent steps.
    pub approvals: Arc<dyn harness_policy::approvals::ApprovalUi>,
    /// Command patterns granted up front.
    pub allow: Vec<String>,
    /// Autonomy mode of agent steps.
    pub mode: Mode,
    /// Model for agent steps that do not name one.
    pub model: Option<String>,
    /// Turn cap for each agent step.
    pub max_turns: Option<u32>,
    /// Scripted fake provider for every agent step.
    pub fake: Option<Arc<dyn harness_providers::provider::Provider>>,
    /// Headless run (memory follows `[memory] headless_access`).
    pub headless: bool,
    /// The session's memory checkout (the TUI); headless runs open their
    /// own when `[memory] headless_access` allows.
    pub memory: Option<Arc<harness_memory::sync::SessionCheckout>>,
    /// The workflow's `memory:` cap.
    pub memory_cap: Option<harness_config::agents::MemoryAccess>,
}

/// A workflow executor and the run it records to.
pub struct Built {
    /// The executor.
    pub executor: Executor,
    /// Its run log.
    pub recorder: RunRecorder,
    /// Warnings (sandbox).
    pub warnings: Vec<String>,
}

/// Builds the executor: agent steps as subagents with the policy gate,
/// shell and verify steps with a scrubbed environment, bug-fix proofs and
/// step checkpoints over the shadow-git store, `runs_on` placement over
/// `[remote.*]`, with placed steps' tools running on their host.
pub async fn build(setup: Setup<'_>) -> Result<Built, String> {
    let Setup {
        cfg,
        data,
        secrets,
        redactor,
        cwd,
        catalog,
        library,
        gates,
        approvals,
        allow,
        mode,
        model,
        max_turns,
        fake,
        headless,
        memory,
        memory_cap,
    } = setup;
    let memory = match memory {
        Some(m) => Some(m),
        None if memory_cap != Some(harness_config::agents::MemoryAccess::Off)
            && cfg.memory.enabled
            && (!headless || cfg.memory.headless_access) =>
        {
            harness_memory::drive::Drive::from_config(cfg)
                .and_then(|d| harness_memory::sync::SessionCheckout::open(&d, "workflow"))
                .map(Arc::new)
                .map_err(|e| tracing::warn!(target: "harness::workflow", error = %e, "workflow runs without memory"))
                .ok()
        }
        None => None,
    };
    let scheduler = Scheduler::new(SchedulerConfig::from_config(cfg));
    let dream = crate::dream::DreamStepRunner::new(
        cfg.clone(),
        data.clone(),
        redactor.clone(),
        fake.clone(),
        headless,
    );
    let recorder = RunRecorder::create(
        data.root(),
        RunId::new(),
        redactor,
        RecorderOptions::default()
            .with_config_hash(harness_config::effective::config_hash(cfg, Some(&catalog))),
    )
    .await
    .map_err(|e| format!("run log: {e}"))?;
    let board = Arc::new(
        TaskBoard::open(recorder.dir().join(TASKS_FILE)).map_err(|e| format!("task board: {e}"))?,
    );
    harness_agent::todo::record_tasks(&board, recorder.clone());
    let mut warnings = Vec::new();
    let exec = core::executor(cfg, cwd, &mut warnings)?;
    let scrubber = EnvScrubber::new(cfg.secret_env_vars(), secrets.clone());
    let env = scrubber.scrub(std::env::vars());

    let engine = PolicyEngine::new(&core::policy_config(cfg, None), cwd)
        .map_err(|e| format!("[policy]: {e}"))?;
    for p in &allow {
        engine.grants().allow_command(p);
    }
    let grants = engine.grants().clone();
    let broker = Arc::new(
        ApprovalBroker::new(grants.clone(), approvals, cwd)
            .with_events(Arc::new(RecorderApprovalEvents(recorder.clone()))),
    );
    let gate =
        Arc::new(PolicyGate::new(engine, broker.clone(), mode).with_recorder(recorder.clone()));
    let default_model = model.unwrap_or_else(|| cfg.general.default_model.clone());
    let builder: Arc<dyn ChildBuilder> = Arc::new(
        CatalogChildBuilder::new(
            catalog.clone(),
            PromptVars::detect(cwd),
            default_model,
            core::resolver(cfg, fake.clone(), scheduler.clone()),
            core::tools_factory(
                cfg,
                cwd,
                exec.clone(),
                scrubber.clone(),
                &recorder,
                core::MemoryUse {
                    checkout: memory,
                    cap: memory_cap,
                    headless,
                },
            ),
            gate.clone() as Arc<dyn ToolGate>,
        )
        .with_gates(core::agent_gates(gate))
        .with_root(cwd)
        .with_cost(core::cost_resolver(cfg, fake.is_some())),
    );
    let builder = match max_turns {
        Some(max_turns) => Arc::new(Capped {
            inner: builder,
            max_turns,
        }),
        None => builder,
    };
    let runtime = SubagentRuntime::new(recorder.clone(), builder, SubagentLimits::from_config(cfg))
        .with_scheduler(scheduler.clone())
        .with_board(board.clone());

    let checkpoints = data.checkpoints();
    let store = {
        let root = cwd.to_path_buf();
        tokio::task::spawn_blocking(move || CheckpointStore::open(&checkpoints, &root))
            .await
            .map_err(|e| e.to_string())?
            .map_err(|e| format!("checkpoints: {e}"))?
    };
    let store = Arc::new(store);
    let verify = VerifyStepRunner::new(exec.clone(), cwd, recorder.clone()).with_env(env.clone());
    let agent = AgentStepRunner::new(runtime);
    let shell: Arc<dyn StepRunner> = Arc::new(
        ShellStepRunner::new(exec.clone(), cwd)
            .with_env(env.clone())
            .with_recorder(recorder.clone()),
    );
    let proof: Arc<dyn StepRunner> = Arc::new(ProofStepRunner::new(verify.clone(), store.clone()));
    let dream: Arc<dyn StepRunner> = Arc::new(dream);
    let verify: Arc<dyn StepRunner> = Arc::new(verify);
    let runners = Runners::new(gates).with_checkpoints(Arc::new(StoreCheckpointer(store)));
    // Steps placed with `runs_on` run their tools on the host.
    let runners = if cfg.remotes.is_empty() {
        runners
            .with_agent(Arc::new(agent))
            .with_shell(shell)
            .with_verify(verify)
            .with_proof(proof)
            .with_dream(dream)
    } else {
        let steps = crate::remote::steps(crate::remote::StepWiring {
            cfg,
            data_root: data.root(),
            cwd,
            secrets: &secrets,
            recorder: &recorder,
            scrubber: &scrubber,
            broker,
            grants,
            mode,
        })?;
        runners
            .with_agent(steps.agent(
                agent,
                harness_tools::config::ToolsConfig::default(),
                scrubber.clone(),
                Arc::new(harness_agent::dispatch::RunBlobSink(
                    recorder.blobs().clone(),
                )),
            ))
            // Placed shell/verify steps get the remote login environment,
            // never this process's (doc-6: no environment forwarding).
            .with_shell(steps.shell(shell, Vec::new()))
            .with_verify(steps.verify(verify, Vec::new()))
            .with_proof(steps.local_only("bugfix-proof", proof))
            .with_dream(steps.local_only("dream", dream))
    };
    let mut executor = Executor::new(recorder.clone(), runners, scheduler)
        .with_library(Arc::new(library))
        .with_board(board);
    if !cfg.remotes.is_empty() {
        let registry = harness_remote::hosts::HostRegistry::from_config(cfg);
        let cache = harness_remote::probe::ProbeCache::default();
        executor = executor.with_hosts(Arc::new(
            harness_workflow::placement::HostPool::from_registry(&registry, &cache),
        ));
    }
    Ok(Built {
        executor,
        recorder,
        warnings,
    })
}

/// Caps the workflow's cost at `usd` (the lower of it and its own budget).
pub fn cap_budget(wf: &mut Workflow, usd: f64) {
    let mut b = wf.budget.unwrap_or(harness_workflow::schema::Budget {
        usd: None,
        tokens: None,
        on_exceed: harness_workflow::schema::OnExceed::Stop,
        warn_at: harness_workflow::schema::DEFAULT_WARN_AT,
    });
    b.usd = Some(b.usd.map_or(usd, |u| u.min(usd)));
    wf.budget = Some(b);
}

async fn run(
    args: WfArgs,
    cfg: Config,
    data: DataDir,
    secrets: Vec<String>,
    redactor: Arc<Redactor>,
    err: &mut dyn Write,
) -> Result<u8, String> {
    let cwd = wiring::project_dir(args.cwd.as_ref())?;
    let catalog = Arc::new(wiring::catalog(&cfg, &data, &cwd));
    let (lib, errors) = library(&catalog, &cfg);
    for e in errors {
        let _ = writeln!(err, "harness: warning: {e}");
    }
    let mut wf = find(&lib, &args.name)?.clone();
    let inputs = inputs(&wf, &args)?;
    if let Some(usd) = args.budget_usd {
        cap_budget(&mut wf, usd);
    }
    let reuse = reuse_plan(&data, &wf, &args)?;
    let mode = args.mode.as_deref().map(parse_mode).transpose()?;
    let mode = mode.unwrap_or_else(|| Mode::from(cfg.general.mode));
    let fake = match &args.fake_script {
        Some(s) => Some(wiring::fake_provider(s)?),
        None => None,
    };
    let built = build(Setup {
        cfg: &cfg,
        data: &data,
        secrets,
        redactor,
        cwd: &cwd,
        catalog,
        library: lib,
        gates: Arc::new(HeadlessGates::from_flags(
            args.approve_all,
            args.approve.clone(),
            args.deny.clone(),
        )),
        approvals: Arc::new(HeadlessUi::new(args.approvals)),
        allow: if args.approvals == Approvals::Allowlist {
            args.allow.clone()
        } else {
            Vec::new()
        },
        mode,
        model: args.model.clone(),
        max_turns: args.max_turns,
        fake,
        headless: true,
        memory: None,
        memory_cap: wf.memory,
    })
    .await?;
    for w in &built.warnings {
        let _ = writeln!(err, "harness: warning: {w}");
    }
    let Built {
        executor, recorder, ..
    } = built;

    let printer = args.json.then(|| JsonPrinter::spawn(&recorder));
    let cancel = CancellationToken::new();
    spawn_signal_cancel(cancel.clone());
    let outcome = executor.run(&wf, &inputs, reuse, &cancel).await;
    let dir = recorder.dir().to_owned();
    if let Err(e) = recorder.finish().await {
        let _ = writeln!(err, "harness: warning: run log: {e}");
    }
    if let Some(p) = printer {
        p.finish().await;
    }
    summary(&wf, &outcome, &dir, err);
    Ok(match outcome.status {
        WorkflowStatus::Succeeded => exit::SUCCESS,
        WorkflowStatus::Cancelled => exit::CANCELLED,
        WorkflowStatus::Failed | WorkflowStatus::Running => exit::ERROR,
    })
}

fn summary(
    wf: &Workflow,
    outcome: &harness_workflow::executor::RunOutcome,
    dir: &Path,
    err: &mut dyn Write,
) {
    let status = match outcome.status {
        WorkflowStatus::Succeeded => "succeeded",
        WorkflowStatus::Failed => "failed",
        WorkflowStatus::Cancelled => "cancelled",
        WorkflowStatus::Running => "did not finish",
    };
    let _ = writeln!(
        err,
        "harness: workflow `{}` {status}{}",
        wf.name,
        outcome
            .error
            .as_deref()
            .map(|e| format!(": {e}"))
            .unwrap_or_default()
    );
    for step in wf.topo_order() {
        if let Some(r) = outcome.state.steps.get(&step.id) {
            let _ = writeln!(
                err,
                "  {:<10} {}{}",
                r.status.as_str(),
                step.id,
                r.error
                    .as_deref()
                    .map(|e| format!(" ({e})"))
                    .unwrap_or_default()
            );
        }
    }
    if let Some(badge) = outcome.state.budget.badge() {
        let _ = writeln!(err, "  budget: {badge}");
    }
    let _ = writeln!(err, "  run: {}", dir.display());
}
