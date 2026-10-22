//! Shared helpers for workflow integration tests.
#![allow(dead_code)]

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use harness_agent::dispatch::{AllowAll, ToolSet};
use harness_agent::scheduler::{Scheduler, SchedulerConfig};
use harness_agent::subagents::{ChildBuilder, ChildParts, SubagentLimits, SubagentRuntime};
use harness_agent::todo::TaskBoard;
use harness_config::agents::{Agent, PromptRef};
use harness_config::discovery::{Artifact, Kind, Scope, SourceFile};
use harness_events::redact::Redactor;
use harness_events::schema::{NodeId, RunEvent, RunId};
use harness_events::store::{RecorderOptions, RunRecorder, read_run};
use harness_hygiene::proof::{ProofError, ProofWorktree};
use harness_providers::fake::FakeProvider;
use harness_tools::local::LocalExec;
use harness_workflow::executor::{
    Checkpointer, Executor, Runners, StepFailure, StepOutput, StepRequest, StepRunner,
};
use harness_workflow::gates::{ApprovalGate, HeadlessGates};
use harness_workflow::runners::{
    AgentStepRunner, ProofStepRunner, ShellStepRunner, VerifyStepRunner,
};
use harness_workflow::schema::{Workflow, parse};

pub fn wf(text: &str) -> Workflow {
    parse(text, None).unwrap_or_else(|e| {
        panic!(
            "{}",
            e.iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("\n")
        )
    })
}

pub fn agent(yaml: &str) -> Agent {
    Agent::load(&SourceFile::inline(
        Scope::Global,
        Kind::Agent,
        "agents/x.yaml",
        yaml,
    ))
    .unwrap()
}

/// Agents backed by fake providers.
#[derive(Default, Clone)]
pub struct Agents {
    pub agents: HashMap<String, (Agent, FakeProvider)>,
}

impl Agents {
    pub fn with(mut self, yaml: &str, p: FakeProvider) -> Self {
        let a = agent(yaml);
        self.agents.insert(a.name.clone(), (a, p));
        self
    }
}

#[async_trait]
impl ChildBuilder for Agents {
    async fn build(&self, name: &str, model: Option<&str>) -> Result<ChildParts, String> {
        let (agent, p) = self
            .agents
            .get(name)
            .ok_or_else(|| format!("unknown agent `{name}`"))?;
        Ok(ChildParts {
            agent: agent.clone(),
            system: match &agent.prompt {
                PromptRef::Inline(t) | PromptRef::Named(t) => t.clone(),
            },
            provider: Arc::new(p.clone()),
            model: model.unwrap_or("fake-model").into(),
            tools: ToolSet::new(),
            gate: Arc::new(AllowAll),
            cost: Some(Arc::new(|u: &harness_providers::model::Usage| {
                Some(u.total() as f64 * 0.001)
            })),
            root: None,
        })
    }
    fn describe(&self, _: &str) -> Option<String> {
        None
    }
}

pub async fn recorder(dir: &Path) -> RunRecorder {
    RunRecorder::create(
        dir,
        RunId::new(),
        Arc::new(Redactor::default()),
        RecorderOptions::default(),
    )
    .await
    .unwrap()
}

pub async fn events(rec: &RunRecorder) -> Vec<RunEvent> {
    rec.flush().await.unwrap();
    read_run(rec.dir()).unwrap().events
}

/// Swaps a `flag` file between `broken` and `fixed`.
pub struct FlagWorktree(pub PathBuf);

impl ProofWorktree for FlagWorktree {
    fn apply_original(&self) -> Result<(), ProofError> {
        std::fs::write(&self.0, "broken").map_err(|e| ProofError::Checkpoint(e.into()))
    }
    fn apply_fix(&self) -> Result<(), ProofError> {
        std::fs::write(&self.0, "fixed").map_err(|e| ProofError::Checkpoint(e.into()))
    }
}

/// Counts checkpoints.
#[derive(Default)]
pub struct CountingCheckpoints(pub AtomicUsize);

impl Checkpointer for CountingCheckpoints {
    fn checkpoint(&self, _node: &NodeId, _label: &str) -> Result<u64, String> {
        Ok(self.0.fetch_add(1, Ordering::SeqCst) as u64 + 1)
    }
}

pub struct Env {
    pub dir: tempfile::TempDir,
    pub work: PathBuf,
    pub recorder: RunRecorder,
    pub scheduler: Scheduler,
    pub board: Arc<TaskBoard>,
}

impl Env {
    pub async fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let work = dir.path().join("work");
        std::fs::create_dir_all(&work).unwrap();
        let recorder = recorder(&dir.path().join("data")).await;
        Self {
            dir,
            work,
            recorder,
            scheduler: Scheduler::new(SchedulerConfig::default()),
            board: Arc::new(TaskBoard::new()),
        }
    }

    pub fn data(&self) -> PathBuf {
        self.dir.path().join("data")
    }

    pub fn runtime(&self, agents: Agents) -> SubagentRuntime {
        SubagentRuntime::new(
            self.recorder.clone(),
            Arc::new(agents),
            SubagentLimits::default(),
        )
        .with_scheduler(self.scheduler.clone())
        .with_board(self.board.clone())
    }

    /// The standard runners over local processes in `work`.
    pub fn runners(&self, agents: Agents, gates: Arc<dyn ApprovalGate>) -> Runners {
        let exec = Arc::new(LocalExec::new());
        let verify = VerifyStepRunner::new(exec.clone(), &self.work, self.recorder.clone());
        let flag = self.work.join("flag");
        Runners::new(gates)
            .with_agent(Arc::new(AgentStepRunner::new(self.runtime(agents))))
            .with_shell(Arc::new(
                ShellStepRunner::new(exec, &self.work).with_recorder(self.recorder.clone()),
            ))
            .with_verify(Arc::new(verify.clone()))
            .with_proof(Arc::new(ProofStepRunner::with_worktree(
                verify,
                Arc::new(move |_, _| {
                    Ok(Arc::new(FlagWorktree(flag.clone())) as Arc<dyn ProofWorktree>)
                }),
            )))
            .with_checkpoints(Arc::new(CountingCheckpoints::default()))
    }

    pub fn executor(&self, runners: Runners) -> Executor {
        Executor::new(self.recorder.clone(), runners, self.scheduler.clone())
            .with_board(self.board.clone())
    }
}

pub fn headless_approve_all() -> Arc<dyn ApprovalGate> {
    Arc::new(HeadlessGates::from_flags(true, [], []))
}

/// A runner driven by a closure; records requests and concurrency.
pub struct Scripted {
    pub calls: Mutex<Vec<StepRequest>>,
    pub running: AtomicUsize,
    pub max_running: AtomicUsize,
    pub delay: Duration,
    #[allow(clippy::type_complexity)]
    pub behave: Box<dyn Fn(&StepRequest, usize) -> Result<StepOutput, StepFailure> + Send + Sync>,
}

impl Scripted {
    pub fn new(
        delay_ms: u64,
        behave: impl Fn(&StepRequest, usize) -> Result<StepOutput, StepFailure> + Send + Sync + 'static,
    ) -> Arc<Self> {
        Arc::new(Self {
            calls: Mutex::new(Vec::new()),
            running: AtomicUsize::new(0),
            max_running: AtomicUsize::new(0),
            delay: Duration::from_millis(delay_ms),
            behave: Box::new(behave),
        })
    }

    pub fn ok() -> Arc<Self> {
        Self::new(0, |_, _| Ok(StepOutput::default()))
    }

    pub fn calls(&self) -> Vec<StepRequest> {
        self.calls.lock().unwrap().clone()
    }

    pub fn steps(&self) -> Vec<String> {
        self.calls().into_iter().map(|r| r.instance).collect()
    }
}

#[async_trait]
impl StepRunner for Scripted {
    async fn run(&self, req: StepRequest) -> Result<StepOutput, StepFailure> {
        let n = {
            let mut c = self.calls.lock().unwrap();
            c.push(req.clone());
            c.len() - 1
        };
        let now = self.running.fetch_add(1, Ordering::SeqCst) + 1;
        self.max_running.fetch_max(now, Ordering::SeqCst);
        let r = tokio::select! {
            _ = tokio::time::sleep(self.delay) => (self.behave)(&req, n),
            _ = req.cancel.cancelled() => Err(StepFailure::cancelled()),
        };
        self.running.fetch_sub(1, Ordering::SeqCst);
        r
    }
}
