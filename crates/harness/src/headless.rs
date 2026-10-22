//! `harness run` (headless agent run) and `harness sessions`.

use std::io::Write;
use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;
use harness_agent::agent_loop::{LoopOutcome, StopReason};
use harness_agent::budget::DenyOverruns;
use harness_agent::session::SessionStore;
use harness_config::ini::Config;
use harness_config::paths::DataDir;
use harness_events::redact::Redactor;
use harness_events::store::RunRecorder;
use harness_policy::approvals::{ApprovalAnswer, ApprovalPrompt, ApprovalUi};
use harness_policy::rules::Mode;
use tokio_util::sync::CancellationToken;

use crate::cli::{Approvals, RunArgs, exit};
use crate::core::{Core, CoreOptions};

/// Answers every approval prompt with a denial (headless mode never asks).
/// In allowlist mode the `--allow` patterns were already granted, so only
/// calls outside the allow list reach this point.
pub struct HeadlessUi {
    approvals: Approvals,
}

impl HeadlessUi {
    /// A UI for the given `--approvals` mode.
    pub fn new(approvals: Approvals) -> Self {
        Self { approvals }
    }

    /// The reason given to the model.
    pub fn reason(&self) -> &'static str {
        match self.approvals {
            Approvals::Deny => {
                "headless run with --approvals deny: calls that need approval are denied"
            }
            Approvals::Allowlist => {
                "headless run with --approvals allowlist: this call matches no --allow pattern"
            }
        }
    }
}

#[async_trait]
impl ApprovalUi for HeadlessUi {
    async fn ask(&self, prompt: ApprovalPrompt) -> Vec<(String, ApprovalAnswer)> {
        prompt
            .requests
            .into_iter()
            .map(|r| {
                (
                    r.approval_id,
                    ApprovalAnswer::Deny {
                        reason: Some(self.reason().into()),
                    },
                )
            })
            .collect()
    }
}

/// Maps a loop outcome to the process exit code.
pub fn exit_code(outcome: &LoopOutcome) -> u8 {
    match &outcome.stop {
        StopReason::EndTurn if outcome.verified == Some(false) => exit::VERIFY_FAILED,
        StopReason::EndTurn => exit::SUCCESS,
        StopReason::Budget { .. } => exit::BUDGET,
        StopReason::MaxTurns { .. } => exit::MAX_TURNS,
        StopReason::Cancelled => exit::CANCELLED,
        StopReason::Error { .. } => exit::ERROR,
    }
}

/// Parses `--mode`.
pub fn parse_mode(s: &str) -> Result<Mode, String> {
    Mode::parse(s).ok_or_else(|| format!("--mode: expected ask, auto-edit or auto, got `{s}`"))
}

/// Prints each redacted event as one JSON line (`--json`).
pub struct JsonPrinter {
    handle: tokio::task::JoinHandle<()>,
    stop: tokio::sync::oneshot::Sender<()>,
}

impl JsonPrinter {
    /// Starts printing the events of `recorder`. Subscribe before the first
    /// event so none is missed.
    pub fn spawn(recorder: &RunRecorder) -> Self {
        use tokio::sync::broadcast::error::{RecvError, TryRecvError};
        let mut rx = recorder.subscribe();
        let (stop, mut stopped) = tokio::sync::oneshot::channel::<()>();
        let print = |event: &harness_events::schema::RunEvent| {
            let mut out = std::io::stdout().lock();
            if let Ok(line) = serde_json::to_string(event) {
                let _ = writeln!(out, "{line}");
            }
            let _ = out.flush();
        };
        let lagged = |n: u64| {
            eprintln!("harness: --json output fell behind; {n} events skipped (see the run log)");
        };
        let handle = tokio::spawn(async move {
            loop {
                tokio::select! {
                    biased;
                    ev = rx.recv() => match ev {
                        Ok(event) => print(&event),
                        Err(RecvError::Lagged(n)) => lagged(n),
                        Err(RecvError::Closed) => return,
                    },
                    _ = &mut stopped => break,
                }
            }
            // The log is finished: print what is still queued.
            loop {
                match rx.try_recv() {
                    Ok(event) => print(&event),
                    Err(TryRecvError::Lagged(n)) => lagged(n),
                    Err(_) => return,
                }
            }
        });
        Self { handle, stop }
    }

    /// Prints the remaining events and stops. Call after the recorder's
    /// `finish()`.
    pub async fn finish(self) {
        let _ = self.stop.send(());
        let _ = self.handle.await;
    }
}

/// Cancels `token` on Ctrl+C or SIGTERM.
pub fn spawn_signal_cancel(token: CancellationToken) {
    tokio::spawn(async move {
        use tokio::signal::unix::{SignalKind, signal};
        match signal(SignalKind::terminate()) {
            Ok(mut term) => {
                tokio::select! {
                    _ = tokio::signal::ctrl_c() => {}
                    _ = term.recv() => {}
                }
            }
            Err(_) => {
                let _ = tokio::signal::ctrl_c().await;
            }
        }
        token.cancel();
    });
}

/// Formats a cost for the summary line.
pub fn cost_label(outcome: &LoopOutcome) -> String {
    if outcome.cost_unknown && outcome.cost_usd == 0.0 {
        "cost unknown".to_owned()
    } else {
        format!("${:.4}", outcome.cost_usd)
    }
}

/// `harness run`. Returns the exit code.
pub async fn run(
    args: RunArgs,
    cfg: Config,
    data: DataDir,
    secrets: Vec<String>,
    redactor: Arc<Redactor>,
) -> u8 {
    match run_inner(args, cfg, data, secrets, redactor).await {
        Ok(code) => code,
        Err(e) => {
            eprintln!("harness: {e}");
            exit::ERROR
        }
    }
}

async fn run_inner(
    args: RunArgs,
    cfg: Config,
    data: DataDir,
    secrets: Vec<String>,
    redactor: Arc<Redactor>,
) -> Result<u8, String> {
    let mode = args.mode.as_deref().map(parse_mode).transpose()?;
    let opts = CoreOptions {
        cwd: args.cwd.clone(),
        agent: args.agent.clone(),
        model: args.model.clone(),
        mode,
        fake_script: args.fake_script.clone(),
        max_turns: args.max_turns,
        budget_tokens: args.budget_tokens,
        budget_usd: args.budget_usd,
        verify: !args.no_verify,
        headless: true,
        approvals: Arc::new(HeadlessUi::new(args.approvals)),
        allow: if args.approvals == Approvals::Allowlist {
            args.allow.clone()
        } else {
            Vec::new()
        },
        budget_approver: Arc::new(DenyOverruns),
        resume: args.resume.clone(),
        title: args.prompt.clone(),
        checkpoints: false,
    };
    let mut core = Core::start(cfg, data, secrets, redactor, opts).await?;
    for w in &core.warnings {
        eprintln!("harness: warning: {w}");
    }
    let json_printer = args.json.then(|| JsonPrinter::spawn(&core.recorder));
    core.begin();

    let cancel = CancellationToken::new();
    spawn_signal_cancel(cancel.clone());
    let started = Instant::now();
    let shown = core.warnings.len();
    let outcome = core.turn(args.prompt.clone(), &cancel).await;
    let session = core.session_id().to_owned();
    for w in &core.warnings[shown..] {
        eprintln!("harness: warning: {w}");
    }
    if let Err(e) = core.finish().await {
        eprintln!("harness: warning: {e}");
    }
    if let Some(printer) = json_printer {
        printer.finish().await;
    } else if !outcome.final_text.is_empty() {
        println!("{}", outcome.final_text);
    }
    eprintln!(
        "harness: {} after {} turn(s) in {:.1}s, {} tokens, {}; session {session}",
        crate::core::finished_error(&outcome).unwrap_or_else(|| "done".into()),
        outcome.turns,
        started.elapsed().as_secs_f64(),
        outcome.usage.total(),
        cost_label(&outcome),
    );
    Ok(exit_code(&outcome))
}

/// `harness sessions`: lists saved sessions, newest first.
pub fn sessions(data: &DataDir, out: &mut dyn Write) -> u8 {
    let store = SessionStore::new(data.sessions());
    match store.list() {
        Ok(list) if list.is_empty() => {
            let _ = writeln!(out, "no saved sessions");
            exit::SUCCESS
        }
        Ok(list) => {
            for m in list {
                let _ = writeln!(out, "{}", m.listing_line());
            }
            exit::SUCCESS
        }
        Err(e) => {
            eprintln!("harness: {e}");
            exit::ERROR
        }
    }
}
