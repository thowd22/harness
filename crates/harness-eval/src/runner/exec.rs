//! Executors run one task attempt headless in a prepared workspace.
//!
//! [`ProcessExecutor`] is what `harness eval run` uses: it spawns the
//! `harness` binary (`harness run --json ...`), so evals measure exactly what
//! users run, with any provider. [`InProcessExecutor`] runs the agent loop in
//! this process with the scripted fake provider and the built-in defaults; it
//! needs no binary and no credentials, which makes it the executor for CI
//! and tests.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use harness_agent::agent_loop::{AgentLoop, Conversation, LoopConfig, LoopLimits};
use harness_agent::dispatch::{
    Dispatcher, PolicyGate, RecorderToolEvents, RunBlobSink, StandardTools, ToolSet,
};
use harness_config::discovery::{BuiltinFile, Catalog, Roots};
use harness_config::ini::Config;
use harness_config::prompts::{PromptVars, SessionPrompt};
use harness_events::redact::Redactor;
use harness_events::schema::{EventKind, NodeId, RunEvent, RunId, RunStatus};
use harness_events::store::{EventDraft, RecorderOptions, RunRecorder, read_run};
use harness_policy::approvals::{
    ApprovalAnswer, ApprovalBroker, ApprovalPrompt, ApprovalUi, RecorderApprovalEvents,
};
use harness_policy::rules::{Mode, PolicyConfig, PolicyEngine, PolicyOverlay};
use harness_providers::fake::FakeProvider;
use harness_tools::bash::EnvScrubber;
use harness_tools::config::ToolsConfig;
use harness_tools::local::{LocalExec, LocalWorkspace};
use tokio::io::{AsyncBufReadExt, AsyncReadExt};
use tokio_util::sync::CancellationToken;

use crate::suite::ResolvedTask;

/// What an executor observed for one attempt.
#[derive(Debug, Clone, Default)]
pub struct Execution {
    /// Process exit code (in-process: the `harness run` code it maps to).
    pub exit_code: Option<i32>,
    /// The redacted run events.
    pub events: Vec<RunEvent>,
    /// Diagnostics (stderr), truncated.
    pub stderr: String,
    /// Whether the attempt hit its timeout.
    pub timed_out: bool,
    /// Wall time of the agent run.
    pub wall: Duration,
}

/// Runs one attempt of a task in `workspace`.
#[async_trait]
pub trait Executor: Send + Sync {
    /// Short name stamped into results (`process`, `in-process`).
    fn name(&self) -> &str;

    /// Runs the task. `Err` means the attempt could not be run at all.
    async fn execute(
        &self,
        task: &ResolvedTask,
        workspace: &Path,
        cancel: &CancellationToken,
    ) -> Result<Execution, String>;
}

/// Most stderr kept per attempt.
const MAX_STDERR: usize = 8 * 1024;

/// Spawns the `harness` binary for each attempt.
#[derive(Debug, Clone)]
pub struct ProcessExecutor {
    program: PathBuf,
    env: Vec<(String, String)>,
}

impl ProcessExecutor {
    /// Uses `program` (usually `std::env::current_exe()`).
    pub fn new(program: impl Into<PathBuf>) -> Self {
        Self {
            program: program.into(),
            env: Vec::new(),
        }
    }

    /// Adds environment variables for every attempt (e.g. `HARNESS_CONFIG`).
    pub fn with_env(mut self, env: Vec<(String, String)>) -> Self {
        self.env = env;
        self
    }

    /// The command-line arguments for `task` (without the program).
    ///
    /// Prompt tasks run `harness run --json --approvals deny --cwd WS ...`;
    /// workflow tasks run `harness wf run NAME --json --cwd WS --input k=v`.
    pub fn args(task: &ResolvedTask, workspace: &Path) -> Vec<String> {
        let mut a: Vec<String> = Vec::new();
        match (&task.prompt, &task.workflow) {
            (_, Some(wf)) => {
                a.extend(["wf".into(), "run".into(), wf.name.clone()]);
                for (k, v) in &wf.inputs {
                    let v = match v {
                        serde_json::Value::String(s) => s.clone(),
                        other => other.to_string(),
                    };
                    a.extend(["--input".into(), format!("{k}={v}")]);
                }
                // Nobody can answer a gate during an eval: gates are
                // approved up front (tool calls still follow --approvals).
                a.push("--approve-all".into());
            }
            _ => a.push("run".into()),
        }
        a.push("--json".into());
        if task.allow.is_empty() {
            a.extend(["--approvals".into(), "deny".into()]);
        } else {
            a.extend(["--approvals".into(), "allowlist".into()]);
            for p in &task.allow {
                a.extend(["--allow".into(), p.clone()]);
            }
        }
        a.extend(["--cwd".into(), workspace.display().to_string()]);
        if let Some(m) = &task.mode {
            a.extend(["--mode".into(), m.clone()]);
        }
        if let Some(agent) = &task.agent {
            a.extend(["--agent".into(), agent.clone()]);
        }
        if let Some(m) = &task.model {
            a.extend(["--model".into(), m.clone()]);
        }
        if let Some(s) = &task.fake_script {
            a.extend(["--fake-script".into(), s.display().to_string()]);
        }
        if let Some(n) = task.max_turns {
            a.extend(["--max-turns".into(), n.to_string()]);
        }
        if let Some(b) = task.budget_usd {
            a.extend(["--budget-usd".into(), b.to_string()]);
        }
        if let (Some(p), None) = (&task.prompt, &task.workflow) {
            a.push("--".into());
            a.push(p.clone());
        }
        a
    }
}

/// Parses `harness run --json` output: one event per line; other lines are
/// ignored.
pub fn parse_events(stdout: &str) -> Vec<RunEvent> {
    stdout
        .lines()
        .filter_map(|l| serde_json::from_str::<RunEvent>(l.trim()).ok())
        .collect()
}

#[async_trait]
impl Executor for ProcessExecutor {
    fn name(&self) -> &str {
        "process"
    }

    async fn execute(
        &self,
        task: &ResolvedTask,
        workspace: &Path,
        cancel: &CancellationToken,
    ) -> Result<Execution, String> {
        let mut cmd = tokio::process::Command::new(&self.program);
        cmd.args(Self::args(task, workspace))
            .current_dir(workspace)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);
        #[cfg(unix)]
        cmd.process_group(0);
        for (k, v) in &self.env {
            cmd.env(k, v);
        }
        let started = Instant::now();
        let mut child = cmd
            .spawn()
            .map_err(|e| format!("starting {}: {e}", self.program.display()))?;
        let stdout = child.stdout.take().ok_or("no stdout")?;
        let mut stderr = child.stderr.take().ok_or("no stderr")?;
        let read_out = tokio::spawn(async move {
            let mut lines = tokio::io::BufReader::new(stdout).lines();
            let mut events = Vec::new();
            while let Ok(Some(line)) = lines.next_line().await {
                if let Ok(ev) = serde_json::from_str::<RunEvent>(line.trim()) {
                    events.push(ev);
                }
            }
            events
        });
        let read_err = tokio::spawn(async move {
            let mut buf = Vec::new();
            let _ = stderr.read_to_end(&mut buf).await;
            buf.truncate(MAX_STDERR);
            String::from_utf8_lossy(&buf).into_owned()
        });
        let timeout = Duration::from_secs(task.timeout_secs);
        let pid = child.id();
        let (code, timed_out) = tokio::select! {
            r = child.wait() => (r.map_err(|e| e.to_string())?.code(), false),
            _ = tokio::time::sleep(timeout) => { kill_group(pid, &mut child).await; (None, true) }
            _ = cancel.cancelled() => { kill_group(pid, &mut child).await; (None, false) }
        };
        let wall = started.elapsed();
        // Leftover grandchildren could hold the pipes open; do not wait long.
        let grace = Duration::from_secs(2);
        Ok(Execution {
            exit_code: code,
            events: tokio::time::timeout(grace, read_out)
                .await
                .ok()
                .and_then(Result::ok)
                .unwrap_or_default(),
            stderr: tokio::time::timeout(grace, read_err)
                .await
                .ok()
                .and_then(Result::ok)
                .unwrap_or_default(),
            timed_out,
            wall,
        })
    }
}

/// Kills the child's whole process group (it was started as a group leader).
pub(super) async fn kill_group(pid: Option<u32>, child: &mut tokio::process::Child) {
    #[cfg(unix)]
    if let Some(pgid) = group_id(pid) {
        // A direct killpg(2), not `kill -KILL -<pgid>`: procps-ng `kill`
        // parses a short negative id such as `-169` as `-1` and SIGKILLs
        // every process the user owns.
        let _ = nix::sys::signal::killpg(
            nix::unistd::Pid::from_raw(pgid),
            nix::sys::signal::Signal::SIGKILL,
        );
    }
    #[cfg(not(unix))]
    let _ = pid;
    let _ = child.kill().await;
}

/// The process group to signal for a child started as a group leader.
/// `None` for ids that would address more than one group: 0 is the caller's
/// own group and 1 (as `-1`) is every process the user may signal.
fn group_id(pid: Option<u32>) -> Option<i32> {
    pid.and_then(|p| i32::try_from(p).ok()).filter(|&p| p > 1)
}

/// Denies every approval (headless evals never ask a human). The request
/// is still recorded, so approvals requested are counted.
struct DenyAll;

#[async_trait]
impl ApprovalUi for DenyAll {
    async fn ask(&self, prompt: ApprovalPrompt) -> Vec<(String, ApprovalAnswer)> {
        prompt
            .requests
            .into_iter()
            .map(|r| {
                (
                    r.approval_id,
                    ApprovalAnswer::Deny {
                        reason: Some("eval run: calls that need approval are denied".into()),
                    },
                )
            })
            .collect()
    }
}

/// Runs the agent loop in-process with the fake provider and built-in
/// defaults only (no global or project config), so results are hermetic.
#[derive(Debug, Clone)]
pub struct InProcessExecutor {
    builtins: &'static [BuiltinFile],
    mode: Mode,
}

impl Default for InProcessExecutor {
    fn default() -> Self {
        Self::new()
    }
}

impl InProcessExecutor {
    /// An executor using [`harness_config::builtins::BUILTINS`] in
    /// `auto-edit` mode.
    pub fn new() -> Self {
        Self {
            builtins: harness_config::builtins::BUILTINS,
            mode: Mode::AutoEdit,
        }
    }

    /// Uses other built-in files (tests).
    pub fn with_builtins(mut self, builtins: &'static [BuiltinFile]) -> Self {
        self.builtins = builtins;
        self
    }

    /// Sets the autonomy mode.
    pub fn with_mode(mut self, mode: Mode) -> Self {
        self.mode = mode;
        self
    }
}

#[async_trait]
impl Executor for InProcessExecutor {
    fn name(&self) -> &str {
        "in-process"
    }

    async fn execute(
        &self,
        task: &ResolvedTask,
        workspace: &Path,
        cancel: &CancellationToken,
    ) -> Result<Execution, String> {
        let Some(script) = &task.fake_script else {
            return Err(
                "the in-process executor runs only fake-provider tasks; use `harness eval run` for real models"
                    .into(),
            );
        };
        let Some(prompt) = &task.prompt else {
            return Err("the in-process executor runs only prompt tasks".into());
        };
        let provider = Arc::new(FakeProvider::new(
            harness_providers::fake::FakeScript::from_yaml_file(script)
                .map_err(|e| format!("{}: {e}", script.display()))?,
        ));
        let data = tempfile::tempdir().map_err(|e| e.to_string())?;
        let catalog = Catalog::load(&Roots::new(self.builtins));
        let name = task
            .agent
            .clone()
            .unwrap_or_else(|| harness_config::builtins::DEFAULT_AGENT.into());
        let agent = catalog
            .agents
            .get(&name)
            .cloned()
            .ok_or_else(|| format!("unknown agent `{name}`"))?;
        let system = match &agent.prompt {
            harness_config::agents::PromptRef::Inline(t) => t.clone(),
            harness_config::agents::PromptRef::Named(p) => {
                let p = catalog
                    .prompts
                    .get(p)
                    .ok_or_else(|| format!("unknown prompt `{p}`"))?;
                SessionPrompt::new(p, PromptVars::detect(workspace))
                    .map_err(|e| e.to_string())?
                    .text()
                    .to_owned()
            }
        };

        let recorder = RunRecorder::create(
            data.path(),
            RunId::new(),
            Arc::new(Redactor::default()),
            RecorderOptions::default(),
        )
        .await
        .map_err(|e| format!("run log: {e}"))?;
        let _ = recorder.emit(EventDraft::run(EventKind::RunStarted {
            name: agent.name.clone(),
            workflow: None,
            host: None,
            title: None,
            agent_names: Default::default(),
        }));
        let all = ToolSet::standard(StandardTools {
            workspace: Arc::new(LocalWorkspace::new(workspace)),
            exec: Arc::new(LocalExec::new()),
            cwd: workspace.to_owned(),
            config: ToolsConfig::default(),
            scrubber: EnvScrubber::new(Vec::<String>::new(), Vec::<String>::new()),
            spill: Arc::new(RunBlobSink(recorder.blobs().clone())),
        });
        let (tools, _missing) = all.select(&agent.request_tools());
        let cfg = Config::defaults();
        let engine = PolicyEngine::new(&PolicyConfig::from_ini(&cfg, Vec::new(), None), workspace)
            .map_err(|e| e.to_string())?
            .with_overlay(
                format!("agent {}", agent.name),
                &PolicyOverlay::from(&agent.policy),
            )
            .map_err(|e| e.to_string())?;
        for p in &task.allow {
            engine.grants().allow_command(p);
        }
        let mode = match &task.mode {
            Some(m) => Mode::parse(m).ok_or_else(|| format!("unknown mode `{m}`"))?,
            None => self.mode,
        };
        let broker = ApprovalBroker::new(engine.grants().clone(), Arc::new(DenyAll), workspace)
            .with_events(Arc::new(RecorderApprovalEvents(recorder.clone())));
        let gate = PolicyGate::new(engine, Arc::new(broker), mode);
        let node = NodeId::random();
        let dispatcher = Dispatcher::new(tools, Arc::new(gate)).with_events(Arc::new(
            RecorderToolEvents::new(recorder.clone(), node.clone(), workspace),
        ));
        let mut config = LoopConfig::new(&agent.name, "fake");
        config.limits = LoopLimits {
            max_turns: task.max_turns.or(agent.max_turns),
            max_tokens: agent.budget.tokens,
            max_usd: task.budget_usd.or(agent.budget.usd),
        };
        // The same context management as the binary: elision and
        // compaction (with the task's own provider) before every request.
        let context = harness_agent::compaction::ContextManager::new(
            harness_agent::compaction::ContextSettings::from_config(
                &cfg.context,
                harness_providers::provider::Provider::capabilities(provider.as_ref(), "fake")
                    .context_window,
            ),
        )
        .with_elision(Arc::new(RunBlobSink(recorder.blobs().clone())))
        .with_compactor(harness_agent::compaction::Compactor::new(
            provider.clone(),
            "fake",
            cfg.context.keep_recent_turns,
        ));
        let mut agent_loop = AgentLoop::new(provider, dispatcher, recorder.clone(), config)
            .with_node(node)
            .with_hook(Arc::new(context));
        let mut conversation = Conversation::new(Some(system));
        conversation.push_user(prompt.clone());

        let started = Instant::now();
        let child = cancel.child_token();
        let timeout = Duration::from_secs(task.timeout_secs);
        let (outcome, timed_out) = tokio::select! {
            o = agent_loop.run(&mut conversation, &child) => (Some(o), false),
            _ = tokio::time::sleep(timeout) => { child.cancel(); (None, true) }
        };
        let wall = started.elapsed();
        drop(agent_loop);
        let (status, code) = match &outcome {
            Some(o) => match o.stop {
                harness_agent::agent_loop::StopReason::EndTurn if o.verified != Some(false) => {
                    (RunStatus::Succeeded, 0)
                }
                harness_agent::agent_loop::StopReason::EndTurn => (RunStatus::Failed, 3),
                harness_agent::agent_loop::StopReason::Budget { .. } => (RunStatus::Failed, 4),
                harness_agent::agent_loop::StopReason::MaxTurns { .. } => (RunStatus::Failed, 5),
                harness_agent::agent_loop::StopReason::Cancelled => (RunStatus::Cancelled, 130),
                harness_agent::agent_loop::StopReason::Error { .. } => (RunStatus::Failed, 1),
            },
            None => (RunStatus::Cancelled, 130),
        };
        let _ = recorder.emit(EventDraft::run(EventKind::RunFinished {
            status,
            error: outcome.as_ref().and_then(|o| o.stop.error_text()),
        }));
        let dir = recorder.dir().to_owned();
        recorder.finish().await.map_err(|e| e.to_string())?;
        drop(recorder);
        let events = read_run(&dir).map_err(|e| e.to_string())?.events;
        Ok(Execution {
            exit_code: (!timed_out).then_some(code),
            events,
            stderr: String::new(),
            timed_out,
            wall,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::suite::WorkflowSpec;

    fn task() -> ResolvedTask {
        ResolvedTask {
            id: "t".into(),
            prompt: Some("fix --it".into()),
            workflow: None,
            agent: Some("coder".into()),
            fake_script: Some("/s/f.yaml".into()),
            model: None,
            repeats: 1,
            timeout_secs: 5,
            max_turns: Some(7),
            budget_usd: Some(0.5),
            mode: None,
            allow: vec![],
        }
    }

    #[test]
    fn process_args_for_prompt_and_workflow_tasks() {
        let a = ProcessExecutor::args(&task(), Path::new("/ws"));
        assert_eq!(
            a.join(" "),
            "run --json --approvals deny --cwd /ws --agent coder --fake-script /s/f.yaml --max-turns 7 --budget-usd 0.5 -- fix --it"
        );
        let mut t = task();
        t.prompt = None;
        t.fake_script = None;
        t.model = Some("sonnet".into());
        t.mode = Some("auto".into());
        t.allow = vec!["cargo test*".into()];
        t.workflow = Some(WorkflowSpec {
            name: "fix-bug".into(),
            inputs: [("issue".to_owned(), serde_json::json!("crash"))].into(),
        });
        let a = ProcessExecutor::args(&t, Path::new("/ws"));
        assert_eq!(
            a.join(" "),
            "wf run fix-bug --input issue=crash --approve-all --json --approvals allowlist --allow cargo test* --cwd /ws --mode auto --agent coder --model sonnet --max-turns 7 --budget-usd 0.5"
        );
    }

    /// Writes an executable stub script named `harness` into `dir`.
    ///
    /// The executable is created by a child process (`install`), never by this
    /// multi-threaded test process. If this process held a write descriptor on
    /// the file, a concurrent test's fork could inherit it and the later exec
    /// would fail with ETXTBSY ("Text file busy").
    fn write_stub(dir: &Path, script: &str) -> std::path::PathBuf {
        let src = dir.join("harness.src");
        std::fs::write(&src, script).unwrap();
        let stub = dir.join("harness");
        let status = std::process::Command::new("install")
            .arg("-m")
            .arg("755")
            .arg(&src)
            .arg(&stub)
            .status()
            .unwrap();
        assert!(status.success(), "install failed: {status}");
        stub
    }

    #[tokio::test]
    async fn process_executor_collects_json_events_from_a_stub_binary() {
        let dir = tempfile::tempdir().unwrap();
        let ev = serde_json::json!({
            "seq": 0, "ts": "2026-10-22T00:00:00Z",
            "run_id": "0192f0c0-0000-7000-8000-000000000000",
            "node_id": null, "parent_node_id": null,
            "type": "run_finished", "status": "succeeded", "error": null
        });
        let stub = write_stub(
            dir.path(),
            &format!("#!/bin/sh\necho 'not json'\necho '{ev}'\necho oops >&2\nexit 0\n"),
        );
        let ex = ProcessExecutor::new(&stub);
        let out = ex
            .execute(&task(), dir.path(), &CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(out.exit_code, Some(0));
        assert_eq!(out.events.len(), 1);
        assert_eq!(out.stderr.trim(), "oops");
        assert!(!out.timed_out);
    }

    #[tokio::test]
    async fn process_executor_times_out() {
        let dir = tempfile::tempdir().unwrap();
        let stub = write_stub(dir.path(), "#!/bin/sh\nsleep 10\n");
        let mut t = task();
        t.timeout_secs = 0;
        let out = ProcessExecutor::new(&stub)
            .execute(&t, dir.path(), &CancellationToken::new())
            .await
            .unwrap();
        assert!(out.timed_out);
        assert_eq!(out.exit_code, None);
    }

    #[test]
    fn group_ids_that_address_several_groups_are_never_signalled() {
        assert_eq!(group_id(None), None);
        assert_eq!(group_id(Some(0)), None, "0 is the caller's own group");
        assert_eq!(group_id(Some(1)), None, "-1 is every process of the user");
        assert_eq!(group_id(Some(u32::MAX)), None);
        assert_eq!(group_id(Some(169)), Some(169));
    }

    /// Regression: `kill -KILL -<pgid>` with procps-ng could become
    /// kill(-1, SIGKILL). A timeout must only kill the task's own group.
    #[cfg(unix)]
    #[tokio::test]
    async fn timeout_kills_only_the_task_group() {
        use std::os::unix::process::CommandExt;
        let dir = tempfile::tempdir().unwrap();
        let mut bystander = std::process::Command::new("sleep")
            .arg("30")
            .process_group(0)
            .spawn()
            .unwrap();
        let stub = write_stub(dir.path(), "#!/bin/sh\nsleep 10 &\nsleep 10\n");
        let mut t = task();
        t.timeout_secs = 0;
        let out = ProcessExecutor::new(&stub)
            .execute(&t, dir.path(), &CancellationToken::new())
            .await
            .unwrap();
        assert!(out.timed_out);
        let alive = bystander.try_wait().unwrap().is_none();
        let _ = bystander.kill();
        let _ = bystander.wait();
        assert!(alive, "a process outside the task's group was killed");
    }

    #[test]
    fn parse_events_skips_non_json() {
        assert!(parse_events("hello\n{}\n").is_empty());
    }
}
