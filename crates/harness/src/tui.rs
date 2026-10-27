//! The interactive TUI: `harness` with no command.
//!
//! The UI ([`harness_tui::app`]) and the core run side by side. The UI sends
//! [`UiRequest`]s; this driver runs agent turns on the session [`Core`]
//! (one at a time; messages typed meanwhile are queued), carries out slash
//! commands and palette entries ([`commands`]), and answers with run events
//! (forwarded from the recorder) and [`CoreEvent`]s.
//!
//! Approval prompts of tool calls, workflow gates and budget overruns all
//! reach the chat's approval queue through one [`ChannelUi`].

mod commands;
pub mod demo;
mod effort;
mod jobs;
mod mcp;
mod mcp_login;
mod signin;

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use harness_agent::agent_loop::LoopOutcome;
use harness_agent::budget::{BudgetApprover, BudgetExceeded};
use harness_config::ini::Config;
use harness_config::paths::DataDir;
use harness_events::redact::Redactor;
use harness_events::schema::{NodeId, RunEvent};
use harness_policy::approvals::{
    ApprovalAnswer, ApprovalPrompt, ApprovalRequest, ApprovalUi, ChannelUi, PromptEnvelope,
};
use harness_policy::rules::{Action, Decision, Layer, MatchedRule, Mode, Verdict};
use harness_tui::app::{
    self, App, AppConfig, AppEvent, CoreEvent, UiRequest, event_channel, forward_run_events,
};
use harness_tui::notify::Level;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::cli::exit;
use crate::core::{Core, CoreOptions};
use crate::headless::parse_mode;

/// Options of the interactive TUI.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TuiOptions {
    /// Session to resume (id, prefix or `last`).
    pub resume: Option<String>,
    /// Agent profile.
    pub agent: Option<String>,
    /// Model alias or `provider/model`.
    pub model: Option<String>,
    /// Autonomy mode.
    pub mode: Option<Mode>,
    /// Project directory.
    pub cwd: Option<PathBuf>,
    /// Scripted fake provider (demos, tests).
    pub fake_script: Option<PathBuf>,
    /// A first message sent when the session starts (`ui-demo`).
    pub prompt: Option<String>,
}

/// Parses `harness [--resume ID] [--agent NAME] [--model REF] [--mode M]
/// [--cwd DIR] [--fake-script FILE]`.
pub fn parse(args: &[String]) -> Result<TuiOptions, String> {
    let mut o = TuiOptions::default();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let (flag, inline) = match a.split_once('=') {
            Some((f, v)) if f.starts_with("--") => (f, Some(v.to_owned())),
            _ => (a.as_str(), None),
        };
        let mut take = || {
            inline
                .clone()
                .or_else(|| it.next().cloned())
                .ok_or_else(|| format!("{flag} needs a value"))
        };
        match flag {
            "--resume" => o.resume = Some(take()?),
            "--agent" => o.agent = Some(take()?),
            "--model" => o.model = Some(take()?),
            "--mode" => o.mode = Some(parse_mode(&take()?)?),
            "--cwd" => o.cwd = Some(PathBuf::from(take()?)),
            "--fake-script" => o.fake_script = Some(PathBuf::from(take()?)),
            other => return Err(format!("unknown option `{other}`")),
        }
    }
    Ok(o)
}

/// Asks the user in the chat when a budget runs out (`on_exceed = ask`).
pub struct ChatBudgetApprover(pub ChannelUi);

#[async_trait]
impl BudgetApprover for ChatBudgetApprover {
    async fn approve(&self, exceeded: &BudgetExceeded) -> bool {
        let request = approval_request(
            "budget",
            "budget",
            None,
            Action::Tool {
                name: "budget".into(),
            },
            &format!("{exceeded}; continue and raise the limit?"),
        );
        let answers = self
            .0
            .ask(ApprovalPrompt {
                batch: 0,
                requests: vec![request],
            })
            .await;
        matches!(answers.first(), Some((_, ApprovalAnswer::Approve(_))))
    }
}

/// An approval request that is not a policy decision (budget, workflow
/// gate): shown in the chat like a tool approval.
pub fn approval_request(
    id: &str,
    call_id: &str,
    node: Option<NodeId>,
    action: Action,
    reason: &str,
) -> ApprovalRequest {
    ApprovalRequest {
        approval_id: id.to_owned(),
        call_id: call_id.to_owned(),
        node_id: node,
        action,
        verdict: Verdict {
            decision: Decision::Ask,
            rule: MatchedRule {
                layer: Layer::Session,
                origin: "harness".into(),
                rule: String::new(),
                reason: reason.to_owned(),
            },
            command: None,
            outward: false,
        },
        preview: Some(reason.to_owned()),
        suggested_pattern: None,
    }
}

/// Parts of the session that slash commands need while a turn runs.
#[derive(Clone)]
struct Handles {
    catalog: Arc<harness_config::discovery::Catalog>,
    cwd: PathBuf,
    mode: Arc<Mutex<Mode>>,
    context: Arc<harness_agent::compaction::ContextManager>,
    checkpoints: Option<Arc<harness_hygiene::checkpoints::CheckpointStore>>,
    turn_checkpoints: Vec<(NodeId, u64)>,
    memory: Option<Arc<harness_memory::sync::SessionCheckout>>,
    runs: Vec<harness_events::schema::RunId>,
    run_dir: PathBuf,
    blobs: harness_events::blobs::BlobStore,
    context_window: u32,
    tools: Vec<harness_providers::model::ToolSpec>,
    system: Option<String>,
    /// The session's background bash jobs (`/jobs`), also during a turn.
    jobs: Option<harness_tools::jobs::JobManager>,
}

impl Handles {
    fn of(core: &Core) -> Handles {
        Handles {
            catalog: core.catalog.clone(),
            cwd: core.cwd.clone(),
            mode: core.mode_handle(),
            context: core.context().clone(),
            checkpoints: core.checkpoints.clone(),
            turn_checkpoints: core.turn_checkpoints.clone(),
            memory: core.memory.clone(),
            runs: core.session().runs.clone(),
            run_dir: core.recorder.dir().to_path_buf(),
            blobs: core.recorder.blobs().clone(),
            context_window: core.model.context_window,
            tools: core.tool_specs(),
            system: core.conversation.system.clone(),
            jobs: core.jobs(),
        }
    }
}

/// The session driver: owns the core between turns.
struct Driver {
    h: Option<Handles>,
    /// A workflow command waiting for the palette's answers.
    pending_workflow: Option<(String, harness_workflow::commands::ParsedArgs)>,
    cfg: Config,
    data: DataDir,
    secrets: Vec<String>,
    redactor: Arc<Redactor>,
    opts: TuiOptions,
    core: Option<Core>,
    turn: Option<JoinHandle<(Core, LoopOutcome)>>,
    cancel: Option<CancellationToken>,
    queue: VecDeque<String>,
    tx: mpsc::Sender<AppEvent>,
    approvals: ChannelUi,
    events: Arc<Mutex<Vec<RunEvent>>>,
    forward: Option<JoinHandle<()>>,
    registry: harness_agent::commands::CommandRegistry,
    background: Vec<JoinHandle<()>>,
    /// Remote host health, shared by `/remote` and its test action.
    probes: harness_remote::probe::ProbeCache,
    /// The session's provider, for the request inspector's exact bodies.
    provider: Arc<Mutex<Option<Arc<dyn harness_providers::provider::Provider>>>>,
    /// Run directories of the workflows started in this TUI (artifacts).
    workflow_runs: Vec<PathBuf>,
    /// The model catalog behind `/model`, created on first use.
    models: Option<harness_providers::catalog::CatalogService>,
    /// The `harness.ini` that `/login` edits.
    login_path: PathBuf,
    /// Checks `/login` keys with the provider.
    login_verifier: Arc<dyn crate::login::Verifier>,
    /// Opens links, such as an error's *Manage usage* action (the system
    /// browser; tests replace it).
    browser: Arc<dyn harness_providers::chatgpt::Browser>,
    /// The action of the last stopped turn, run by the panel's primary
    /// button ([`commands::ERROR_ACTION`]).
    error_action: Option<harness_events::schema::ErrorAction>,
    /// MCP servers of the session: `/mcp`, prompts, resources.
    mcp: mcp::McpUi,
    /// Running browser sign-ins (ChatGPT, MCP OAuth).
    sign_ins: signin::SignIns,
}

impl Driver {
    fn new(
        cfg: Config,
        data: DataDir,
        secrets: Vec<String>,
        redactor: Arc<Redactor>,
        tx: mpsc::Sender<AppEvent>,
        approvals: ChannelUi,
    ) -> Driver {
        let probes = crate::remote::probe_cache(data.root());
        Driver {
            h: None,
            pending_workflow: None,
            cfg,
            data,
            secrets,
            redactor,
            opts: TuiOptions::default(),
            core: None,
            turn: None,
            cancel: None,
            queue: VecDeque::new(),
            tx,
            approvals,
            events: Arc::new(Mutex::new(Vec::new())),
            forward: None,
            registry: harness_agent::commands::CommandRegistry::new(),
            background: Vec::new(),
            probes,
            provider: Arc::new(Mutex::new(None)),
            workflow_runs: Vec::new(),
            models: None,
            login_path: crate::login::config_path(),
            login_verifier: Arc::new(crate::login::LiveVerifier::default()),
            browser: Arc::new(harness_providers::chatgpt::SystemBrowser),
            error_action: None,
            mcp: mcp::McpUi::default(),
            sign_ins: signin::SignIns::default(),
        }
    }

    async fn send(&self, c: CoreEvent) {
        let _ = self.tx.send(AppEvent::Core(c)).await;
    }

    async fn notify(&self, level: Level, title: &str, body: Option<String>) {
        self.send(CoreEvent::Notify {
            level,
            title: title.to_owned(),
            body,
        })
        .await;
    }

    async fn panel(&self, title: &str, lines: Vec<String>) {
        self.send(CoreEvent::Panel {
            title: title.to_owned(),
            lines,
            buttons: Vec::new(),
        })
        .await;
    }

    /// Starts a session (new, resumed or with another agent) and announces
    /// it to the UI.
    async fn start(&mut self, opts: TuiOptions) -> Result<(), String> {
        self.finish_core().await;
        let core = Core::start(
            self.cfg.clone(),
            self.data.clone(),
            self.secrets.clone(),
            self.redactor.clone(),
            CoreOptions {
                cwd: opts.cwd.clone(),
                agent: opts.agent.clone(),
                model: opts.model.clone(),
                mode: opts.mode,
                fake_script: opts.fake_script.clone(),
                max_turns: None,
                budget_tokens: None,
                budget_usd: None,
                verify: true,
                headless: false,
                approvals: Arc::new(self.approvals.clone()),
                allow: Vec::new(),
                budget_approver: Arc::new(ChatBudgetApprover(self.approvals.clone())),
                resume: opts.resume.clone(),
                title: String::new(),
                checkpoints: true,
            },
        )
        .await?;
        // Earlier runs of a resumed session, so the transcript continues.
        for run in core
            .session()
            .runs
            .iter()
            .filter(|r| **r != core.recorder.run_id())
        {
            if let Ok((events, _)) = harness_tui::history::load_run(self.data.root(), *run) {
                for e in events {
                    let _ = self.tx.send(AppEvent::Run(Box::new(e))).await;
                }
            }
        }
        self.forward = Some(self.forward_events(&core));
        core.begin();
        // The user types the first prompt now (`[experimental] cache_prewarm`).
        core.spawn_prewarm("session_start");
        self.registry = harness_agent::commands::CommandRegistry::from_catalog(&core.catalog);
        self.send(CoreEvent::Session {
            model: core.model.reference.clone(),
            context_window: Some(u64::from(core.model.context_window)),
            branch: harness_config::prompts::git_branch(&core.cwd),
        })
        .await;
        self.send(effort::status_event(&core)).await;
        let palette = commands::palette_entries(&self.registry, &core);
        let slash = commands::slash_commands(&self.registry);
        self.send(CoreEvent::PaletteEntries(palette.clone())).await;
        self.send(CoreEvent::SlashCommands(slash.clone())).await;
        mcp::session_started(self, &core, palette, slash).await;
        for w in &core.warnings {
            self.notify(Level::Warning, "warning", Some(w.clone()))
                .await;
        }
        let mut opts = opts;
        let first = opts.prompt.take();
        self.opts = opts;
        self.h = Some(Handles::of(&core));
        *self.provider.lock().unwrap_or_else(|e| e.into_inner()) =
            Some(core.model.provider.clone());
        self.core = Some(core);
        if let Some(p) = first {
            self.prompt(p).await;
        }
        Ok(())
    }

    /// Forwards the recorder's live events to the UI and keeps a copy.
    fn forward_events(&self, core: &Core) -> JoinHandle<()> {
        let mut rx = core.recorder.subscribe();
        let (relay_tx, relay_rx) = tokio::sync::broadcast::channel(4096);
        let events = self.events.clone();
        events.lock().unwrap_or_else(|e| e.into_inner()).clear();
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let ui = tokio::spawn(forward_run_events(relay_rx, tx));
            loop {
                match rx.recv().await {
                    Ok(ev) => {
                        events
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .push(ev.clone());
                        let _ = relay_tx.send(ev);
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
            drop(relay_tx);
            let _ = ui.await;
        })
    }

    /// Ends the current session's run (after the running turn, if any).
    async fn finish_core(&mut self) {
        self.mcp.stop();
        if let Some(c) = &self.cancel {
            c.cancel();
        }
        if let Some(t) = self.turn.take()
            && let Ok((core, _)) = t.await
        {
            self.core = Some(core);
        }
        if let Some(core) = self.core.take() {
            let _ = core.finish().await;
        }
        // The recorder is gone, so the forwarder drains the old run's last
        // events to the UI and ends; abort only if it does not finish.
        if let Some(mut f) = self.forward.take()
            && tokio::time::timeout(std::time::Duration::from_secs(2), &mut f)
                .await
                .is_err()
        {
            f.abort();
        }
    }

    fn busy(&self) -> bool {
        self.turn.is_some()
    }

    fn start_turn(&mut self, prompt: String) {
        let Some(mut core) = self.core.take() else {
            return;
        };
        let cancel = CancellationToken::new();
        self.cancel = Some(cancel.clone());
        self.turn = Some(tokio::spawn(async move {
            let outcome = core.turn(prompt, &cancel).await;
            (core, outcome)
        }));
    }

    async fn submit(&mut self, text: String) {
        if text.trim_start().starts_with('/') {
            commands::slash(self, text.trim()).await;
        } else {
            self.prompt(text).await;
        }
    }

    /// Sends `text` to the agent now, or after the running turn.
    async fn prompt(&mut self, text: String) {
        let text = self.mcp.with_attachments(text);
        if self.busy() || self.core.is_none() {
            self.queue.push_back(text);
            self.notify(
                Level::Info,
                "queued",
                Some("sent when the agent finishes this turn".into()),
            )
            .await;
        } else {
            self.start_turn(text);
        }
    }

    async fn turn_done(&mut self, core: Core, outcome: LoopOutcome) {
        self.cancel = None;
        let shown = core.warnings.len();
        self.h = Some(Handles::of(&core));
        *self.provider.lock().unwrap_or_else(|e| e.into_inner()) =
            Some(core.model.provider.clone());
        self.core = Some(core);
        if let Some(core) = &self.core {
            for w in core.warnings.iter().skip(shown) {
                self.notify(Level::Warning, "warning", Some(w.clone()))
                    .await;
            }
        }
        if let harness_agent::agent_loop::StopReason::Error { message, action } = &outcome.stop {
            self.notify(Level::Error, "the agent stopped", Some(message.clone()))
                .await;
            if let Some(action) = action {
                commands::error_action_panel(self, message, action.clone()).await;
            }
        }
        commands::after_turn(self).await;
        // Queued commands (`/model`, `/effort` while busy) run now; the
        // first queued prompt starts the next turn.
        while let Some(next) = self.queue.pop_front() {
            if next.trim_start().starts_with('/') {
                commands::slash(self, next.trim()).await;
                if self.busy() {
                    // The command started a turn (a skill, a workflow
                    // step): the rest waits for it.
                    break;
                }
                continue;
            }
            self.start_turn(next);
            break;
        }
    }

    /// Handles one UI request; false means quit.
    async fn request(&mut self, r: UiRequest) -> bool {
        match r {
            UiRequest::Quit => return false,
            UiRequest::Submit(text) => self.submit(text).await,
            UiRequest::Interrupt => {
                if let Some(c) = &self.cancel {
                    c.cancel();
                }
                self.queue.clear();
            }
            UiRequest::Cancel | UiRequest::OpenPalette => {}
            UiRequest::ModeChanged(m) => {
                if let Some(core) = &self.core {
                    core.set_mode(m);
                }
                self.opts.mode = Some(m);
            }
            UiRequest::Run(inv) => commands::invocation(self, inv).await,
            UiRequest::UndoMemory(sha) => commands::undo_memory(self, sha).await,
            UiRequest::PanelAction(id) => commands::panel_action(self, &id).await,
            UiRequest::OpenDiff(node) => commands::diff(self, node).await,
            UiRequest::RemoveTempFiles(node) => commands::remove_temp_files(self, node).await,
            UiRequest::RestoreCheckpoint(node) => commands::restore_checkpoint(self, node).await,
            UiRequest::Artifacts(node) => commands::artifacts(self, node).await,
            UiRequest::OpenArtifact { node, name } => {
                commands::open_artifact(self, node, &name).await
            }
            UiRequest::Memory(req) => commands::memory_request(self, req).await,
            UiRequest::SetModel(name) => commands::model(self, Some(&name)).await,
            UiRequest::Resume(id) => commands::resume(self, &id).await,
            UiRequest::Login(sub) => commands::login(self, &sub).await,
            UiRequest::SignInPaste { id, text } => signin::paste(self, &id, &text).await,
            UiRequest::SignInCancel(id) => signin::cancel(self, &id),
        }
        true
    }

    async fn run(mut self, mut requests: mpsc::UnboundedReceiver<UiRequest>) {
        // MCP prompts run in the background; their text arrives here.
        let mut mcp_turns = self.mcp.take_turns();
        let mut sign_ins = self.sign_ins.take_done();
        loop {
            tokio::select! {
                Some(f) = sign_ins.recv() => signin::finished(&mut self, f).await,
                r = requests.recv() => match r {
                    Some(r) => if !self.request(r).await { break },
                    None => break,
                },
                Some(text) = mcp_turns.recv() => self.prompt(text).await,
                done = async { self.turn.as_mut().expect("turn").await }, if self.turn.is_some() => {
                    self.turn = None;
                    match done {
                        Ok((core, outcome)) => self.turn_done(core, outcome).await,
                        Err(e) => {
                            self.notify(Level::Error, "the agent task failed", Some(e.to_string())).await;
                        }
                    }
                }
            }
        }
        for b in self.background.drain(..) {
            b.abort();
        }
        self.sign_ins.abort_all();
        self.finish_core().await;
    }
}

/// Runs the TUI until the user quits. Returns the exit code.
pub async fn run(
    opts: TuiOptions,
    cfg: Config,
    data: DataDir,
    secrets: Vec<String>,
    redactor: Arc<Redactor>,
) -> u8 {
    let cwd = match crate::wiring::project_dir(opts.cwd.as_ref()) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("harness: {e}");
            return exit::ERROR;
        }
    };
    let app_cfg = match AppConfig::from_config(&cfg) {
        Ok(c) => c.with_dirs(&cwd, harness_config::paths::home_dir()),
        Err(errors) => {
            for e in errors {
                eprintln!("harness: {e}");
            }
            return exit::ERROR;
        }
    };
    let (tx, rx) = event_channel();
    let (ui_tx, ui_rx) = mpsc::unbounded_channel();
    let (approvals, mut prompts) = ChannelUi::new(16);
    // Copy actions mask secrets like every other output.
    let clipboard_redactor = redactor.clone();
    let mut driver = Driver::new(cfg, data, secrets, redactor, tx.clone(), approvals);
    let mut start_opts = opts;
    start_opts.cwd = Some(cwd);
    if let Err(e) = driver.start(start_opts).await {
        eprintln!("harness: {e}");
        return exit::ERROR;
    }
    // About daily, when there was activity: consolidate memory.
    commands::dream(&mut driver, false).await;
    let blobs = driver.h.as_ref().map(|h| h.blobs.clone()).expect("started");
    // Approval prompts (tools, gates, budgets) go to the chat.
    let ptx = tx.clone();
    let prompts_task = tokio::spawn(async move {
        while let Some(env) = prompts.recv().await {
            let env: PromptEnvelope = env;
            if ptx
                .send(AppEvent::Core(CoreEvent::Approval(env)))
                .await
                .is_err()
            {
                break;
            }
        }
    });
    let provider = driver.provider.clone();
    let core_task = tokio::spawn(driver.run(ui_rx));
    let app = App::new(app_cfg, ui_tx)
        .with_redactor(clipboard_redactor)
        .with_blobs(blobs)
        .with_request_body(Arc::new(move |name, req| {
            let p = provider.lock().unwrap_or_else(|e| e.into_inner()).clone()?;
            (p.name() == name).then(|| p.request_body(req)).flatten()
        }));
    let result = app::run(app, rx, tx).await;
    // The app is gone: its request channel closes and the driver finishes
    // the session.
    let _ = core_task.await;
    prompts_task.abort();
    match result {
        Ok(()) => exit::SUCCESS,
        Err(e) => {
            eprintln!("harness: terminal: {e}");
            exit::ERROR
        }
    }
}

#[cfg(test)]
mod tests;
