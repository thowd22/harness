//! The workflow DAG executor.
//!
//! [`Executor::run`] runs a [`Workflow`] as nodes in the run's event log:
//!
//! - a workflow node (`kind: workflow`), then one node per step (child of
//!   the workflow node, `needs` edges between them), declared queued up
//!   front so the map shows the whole DAG immediately; `matrix` /
//!   `for_each` instances are child nodes of their step node;
//! - ready steps (all `needs` finished) run in parallel, each holding a
//!   [`Scheduler`] slot (approval and nested-workflow steps do not);
//!   steps with `runs_on` also hold a host job slot ([`HostPool`]);
//! - `if` (false or failed upstream: skipped), `retries` (a `node_retry`
//!   event per attempt), `timeout` (per attempt), `on_failure` (`abort`
//!   cancels running steps and skips the rest; `continue` lets dependents
//!   run), `loop.until` with `max_iterations` and `retry_step`;
//! - workflow and step budgets: a soft warning at `warn_at`, then a hard
//!   stop or a warning per `on_exceed` ([`crate::schema::Budget`]);
//! - cancellation stops running steps and marks every other step
//!   cancelled;
//! - [`WorkflowState`] is saved to `<run dir>/workflow.json` after every
//!   change, so a crashed workflow can resume ([`crate::rerun`]);
//! - `goal` and `tasks` seed the run's [`TaskBoard`]; tasks linked to a
//!   step follow its status and owner.
//!
//! The work of each step type is done by a [`StepRunner`] from
//! [`Runners`] ([`crate::runners`] has the standard ones), so the engine
//! is independent of providers, processes and checkpoints.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use futures::future::join_all;
use harness_agent::scheduler::{AgentSlot, Scheduler};
use harness_agent::todo::{Task, TaskBoard, TaskStatus};
use harness_events::schema::{
    ApprovalDecision, EventKind, NodeId, NodeKind, NodeStatus, Payload, RunStatus,
};
use harness_events::store::{EventDraft, RunRecorder};
use serde_json::{Value, json};
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

use crate::artifacts::{self, ArtifactInfo, ArtifactStore};
use crate::gates::{ApprovalGate, GateRequest};
use crate::placement::{HostLease, HostPool, local_lease};
use crate::rerun::{
    BudgetState, ReusePlan, STATE_VERSION, StepRecord, StepStatus, WorkflowState, WorkflowStatus,
};
use crate::schema::{
    Budget, Expand, Expect, OnExceed, OnFailure, RunsOn, ScopeSpec, Step, StepKind, Workflow,
};
use crate::template::{self, Context, RunView, StepView};

/// Deepest nesting of `uses:` workflows.
pub const MAX_NESTING: u32 = 4;
/// Grace period for a step to stop after its timeout fired.
pub const TIMEOUT_GRACE: Duration = Duration::from_secs(10);

/// Spend limits passed to a step runner (hard limits only).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct StepLimits {
    /// USD cap.
    pub usd: Option<f64>,
    /// Token cap.
    pub tokens: Option<u64>,
}

/// What a runner must do (templates already rendered).
#[derive(Debug, Clone, PartialEq)]
pub enum StepAction {
    /// Run `agent` with `brief`.
    Agent {
        /// Agent name.
        agent: String,
        /// Model override.
        model: Option<String>,
        /// The brief (rendered prompt plus inlined artifacts).
        brief: String,
    },
    /// Run a shell command.
    Shell {
        /// Command line.
        command: String,
    },
    /// Run a verify command.
    Verify {
        /// Command line.
        command: String,
        /// Pass or fail expected.
        expect: Expect,
    },
    /// Bug-fix proof.
    Proof {
        /// Test command.
        test: String,
        /// Paths kept when the fix is reverted.
        keep: Vec<String>,
    },
    /// Memory consolidation with these settings.
    Dream {
        /// The step's `with:` settings.
        settings: serde_json::Map<String, Value>,
    },
}

/// One unit of work for a [`StepRunner`].
#[derive(Debug, Clone)]
pub struct StepRequest {
    /// Workflow name.
    pub workflow: String,
    /// Step id.
    pub step: String,
    /// Instance key (`fix`, `lint[2]`); names the artifact directory.
    pub instance: String,
    /// The instance's node (record events here or on children).
    pub node: NodeId,
    /// The work.
    pub action: StepAction,
    /// Declared output names.
    pub outputs: Vec<String>,
    /// Declared artifact names to produce.
    pub artifacts: Vec<String>,
    /// Where shell steps write artifacts (`$ARTIFACTS_DIR`).
    pub artifacts_dir: PathBuf,
    /// Edit scope globs.
    pub scope: Vec<String>,
    /// Remote host, when placed on one.
    pub host: Option<String>,
    /// Hard spend limits.
    pub limits: StepLimits,
    /// For bug-fix proofs: the checkpoint taken before the fix.
    pub checkpoint: Option<u64>,
    /// The step's scheduler slot (agent runners reuse it).
    pub slot: Option<Arc<AgentSlot>>,
    /// Cancellation for this attempt.
    pub cancel: CancellationToken,
    /// Attempt number, from 1.
    pub attempt: u32,
}

/// What a step produced.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct StepOutput {
    /// Outputs (`steps.<id>.outputs`).
    pub outputs: serde_json::Map<String, Value>,
    /// Artifact contents to store.
    pub artifacts: BTreeMap<String, Vec<u8>>,
    /// Known spend.
    pub cost_usd: f64,
    /// Tokens used.
    pub tokens: u64,
}

/// A failed attempt.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct StepFailure {
    /// Why.
    pub message: String,
    /// True when it stopped because of cancellation.
    pub cancelled: bool,
    /// Spend of the failed attempt.
    pub cost_usd: f64,
    /// Tokens of the failed attempt.
    pub tokens: u64,
}

impl StepFailure {
    /// A plain failure.
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            ..Self::default()
        }
    }

    /// A cancellation.
    pub fn cancelled() -> Self {
        Self {
            message: "cancelled".into(),
            cancelled: true,
            ..Self::default()
        }
    }
}

/// Does the work of one step type.
#[async_trait]
pub trait StepRunner: Send + Sync {
    /// Runs one attempt.
    async fn run(&self, req: StepRequest) -> Result<StepOutput, StepFailure>;
}

/// Takes a checkpoint of the worktree (before agent steps, so bug-fix
/// proofs can revert a fix).
pub trait Checkpointer: Send + Sync {
    /// Snapshots the worktree; returns the checkpoint id.
    fn checkpoint(&self, node: &NodeId, label: &str) -> Result<u64, String>;
}

/// Looks up nested workflows by name.
pub trait WorkflowLibrary: Send + Sync {
    /// The workflow called `name`.
    fn get(&self, name: &str) -> Option<Workflow>;
}

impl WorkflowLibrary for BTreeMap<String, Workflow> {
    fn get(&self, name: &str) -> Option<Workflow> {
        BTreeMap::get(self, name).cloned()
    }
}

/// The runners for each step type. A missing runner fails steps of that
/// type with a clear message.
#[derive(Clone)]
pub struct Runners {
    /// `agent` steps.
    pub agent: Option<Arc<dyn StepRunner>>,
    /// `shell` steps.
    pub shell: Option<Arc<dyn StepRunner>>,
    /// `verify` steps.
    pub verify: Option<Arc<dyn StepRunner>>,
    /// `bugfix-proof` steps.
    pub proof: Option<Arc<dyn StepRunner>>,
    /// `dream` steps.
    pub dream: Option<Arc<dyn StepRunner>>,
    /// `approval` steps.
    pub gates: Arc<dyn ApprovalGate>,
    /// Checkpoints before agent steps.
    pub checkpoints: Option<Arc<dyn Checkpointer>>,
}

impl std::fmt::Debug for Runners {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Runners")
            .field("agent", &self.agent.is_some())
            .field("shell", &self.shell.is_some())
            .field("verify", &self.verify.is_some())
            .field("proof", &self.proof.is_some())
            .field("dream", &self.dream.is_some())
            .field("checkpoints", &self.checkpoints.is_some())
            .finish_non_exhaustive()
    }
}

impl Runners {
    /// Only gates; add runners with the builder methods.
    pub fn new(gates: Arc<dyn ApprovalGate>) -> Self {
        Self {
            agent: None,
            shell: None,
            verify: None,
            proof: None,
            dream: None,
            gates,
            checkpoints: None,
        }
    }

    /// Sets the agent runner.
    pub fn with_agent(mut self, r: Arc<dyn StepRunner>) -> Self {
        self.agent = Some(r);
        self
    }
    /// Sets the shell runner.
    pub fn with_shell(mut self, r: Arc<dyn StepRunner>) -> Self {
        self.shell = Some(r);
        self
    }
    /// Sets the verify runner.
    pub fn with_verify(mut self, r: Arc<dyn StepRunner>) -> Self {
        self.verify = Some(r);
        self
    }
    /// Sets the bug-fix proof runner.
    pub fn with_proof(mut self, r: Arc<dyn StepRunner>) -> Self {
        self.proof = Some(r);
        self
    }
    /// Sets the memory consolidation runner.
    pub fn with_dream(mut self, r: Arc<dyn StepRunner>) -> Self {
        self.dream = Some(r);
        self
    }
    /// Sets the checkpointer.
    pub fn with_checkpoints(mut self, c: Arc<dyn Checkpointer>) -> Self {
        self.checkpoints = Some(c);
        self
    }
}

/// Result of a workflow run.
#[derive(Debug, Clone, PartialEq)]
pub struct RunOutcome {
    /// Final status.
    pub status: WorkflowStatus,
    /// Why it failed, if it did.
    pub error: Option<String>,
    /// Final state (also in `workflow.json`).
    pub state: WorkflowState,
}

struct Shared {
    recorder: RunRecorder,
    runners: Runners,
    scheduler: Scheduler,
    library: Option<Arc<dyn WorkflowLibrary>>,
    board: Option<Arc<TaskBoard>>,
    hosts: Option<Arc<HostPool>>,
    store: ArtifactStore,
}

impl Shared {
    fn emit(&self, draft: EventDraft) {
        if let Err(e) = self.recorder.emit(draft) {
            tracing::warn!(target: "harness::workflow", error = %e, "could not record event");
        }
    }

    fn node(&self, node: &NodeId, kind: EventKind) {
        self.emit(EventDraft::node(node.clone(), kind));
    }
}

/// Runs workflows.
#[derive(Clone)]
pub struct Executor {
    shared: Arc<Shared>,
}

impl std::fmt::Debug for Executor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Executor")
            .field("runners", &self.shared.runners)
            .finish_non_exhaustive()
    }
}

/// Fills in defaults and checks types and required inputs.
pub fn resolve_inputs(
    wf: &Workflow,
    given: &serde_json::Map<String, Value>,
) -> Result<serde_json::Map<String, Value>, String> {
    let mut out = serde_json::Map::new();
    for k in given.keys() {
        if wf.input(k).is_none() {
            return Err(format!("unknown input `{k}` for workflow `{}`", wf.name));
        }
    }
    for i in &wf.inputs {
        match given.get(&i.name).or(i.default.as_ref()) {
            Some(v) => {
                if !i.kind.accepts(v) {
                    return Err(format!(
                        "input `{}` must be a {}, got {v}",
                        i.name,
                        i.kind.as_str()
                    ));
                }
                out.insert(i.name.clone(), v.clone());
            }
            None if i.required => return Err(format!("missing required input `{}`", i.name)),
            None => {}
        }
    }
    Ok(out)
}

impl Executor {
    /// An executor recording to `recorder`, running steps with `runners`
    /// within `scheduler`'s limits.
    pub fn new(recorder: RunRecorder, runners: Runners, scheduler: Scheduler) -> Self {
        let store = ArtifactStore::new(recorder.dir());
        Self {
            shared: Arc::new(Shared {
                recorder,
                runners,
                scheduler,
                library: None,
                board: None,
                hosts: None,
                store,
            }),
        }
    }

    fn update(mut self, f: impl FnOnce(&mut Shared)) -> Self {
        if let Some(s) = Arc::get_mut(&mut self.shared) {
            f(s);
        }
        self
    }

    /// Resolves `uses:` through `library`.
    pub fn with_library(self, library: Arc<dyn WorkflowLibrary>) -> Self {
        self.update(|s| s.library = Some(library))
    }

    /// Seeds and updates `board` (goal bar).
    pub fn with_board(self, board: Arc<TaskBoard>) -> Self {
        self.update(|s| s.board = Some(board))
    }

    /// Places `runs_on` steps with `hosts`.
    pub fn with_hosts(self, hosts: Arc<HostPool>) -> Self {
        self.update(|s| s.hosts = Some(hosts))
    }

    /// The artifact store of the run.
    pub fn artifacts(&self) -> &ArtifactStore {
        &self.shared.store
    }

    /// Runs `wf` with `inputs` as the whole run: records `run_started` /
    /// `run_finished`, saves state to the run directory. With `reuse`
    /// (resume / re-run) the reused steps are taken over unchanged.
    pub async fn run(
        &self,
        wf: &Workflow,
        inputs: &serde_json::Map<String, Value>,
        reuse: Option<ReusePlan>,
        cancel: &CancellationToken,
    ) -> RunOutcome {
        let sh = &self.shared;
        let name = match &reuse {
            Some(p) => format!(
                "{} ({} of {})",
                wf.name,
                match p.origin.kind {
                    crate::rerun::OriginKind::Resume => "resume",
                    crate::rerun::OriginKind::Rerun => "re-run",
                },
                p.origin.run.0
            ),
            None => wf.name.clone(),
        };
        sh.emit(EventDraft::run(EventKind::RunStarted {
            name,
            workflow: Some(wf.name.clone()),
            host: None,
        }));
        let out = match resolve_inputs(wf, inputs) {
            Ok(inputs) => {
                let run = WfRun::new(
                    sh.clone(),
                    wf.clone(),
                    inputs,
                    NodeId::from(wf.name.clone()),
                    None,
                    0,
                    true,
                    reuse,
                );
                run.execute(cancel).await
            }
            Err(e) => RunOutcome {
                status: WorkflowStatus::Failed,
                error: Some(e),
                state: WorkflowState::default(),
            },
        };
        let status = match out.status {
            WorkflowStatus::Succeeded => RunStatus::Succeeded,
            WorkflowStatus::Cancelled => RunStatus::Cancelled,
            WorkflowStatus::Failed | WorkflowStatus::Running => RunStatus::Failed,
        };
        sh.emit(EventDraft::run(EventKind::RunFinished {
            status,
            error: out.error.clone(),
        }));
        if let Err(e) = sh.recorder.flush().await {
            tracing::warn!(target: "harness::workflow", error = %e, "could not flush the run log");
        }
        out
    }
}

/// One finished step (all instances).
#[derive(Debug, Clone, Default)]
struct StepResult {
    status: StepStatus,
    outputs: serde_json::Map<String, Value>,
    artifacts: BTreeMap<String, PathBuf>,
    error: Option<String>,
    cost_usd: f64,
    tokens: u64,
    attempts: u32,
    checkpoint: Option<u64>,
}

struct WfRun {
    sh: Arc<Shared>,
    wf: Workflow,
    node: NodeId,
    parent: Option<NodeId>,
    depth: u32,
    persist: bool,
    state: Mutex<WorkflowState>,
    reuse: Option<ReusePlan>,
}

/// Outputs plus artifact name to path.
type StoredOutputs = (serde_json::Map<String, Value>, BTreeMap<String, PathBuf>);

fn step_status_to_node(s: StepStatus) -> NodeStatus {
    match s {
        StepStatus::Pending => NodeStatus::Queued,
        StepStatus::Running => NodeStatus::Running,
        StepStatus::Succeeded => NodeStatus::Succeeded,
        StepStatus::Failed => NodeStatus::Failed,
        StepStatus::Skipped => NodeStatus::Skipped,
        StepStatus::Cancelled => NodeStatus::Cancelled,
    }
}

fn task_status(s: StepStatus) -> TaskStatus {
    match s {
        StepStatus::Pending => TaskStatus::Pending,
        StepStatus::Running => TaskStatus::InProgress,
        StepStatus::Succeeded => TaskStatus::Done,
        StepStatus::Failed => TaskStatus::Failed,
        StepStatus::Skipped | StepStatus::Cancelled => TaskStatus::Cancelled,
    }
}

fn node_kind(step: &Step) -> NodeKind {
    match step.kind {
        StepKind::Agent { .. } => NodeKind::Agent,
        StepKind::Shell { .. } => NodeKind::Shell,
        StepKind::Verify { .. } | StepKind::BugfixProof { .. } => NodeKind::Verify,
        StepKind::Approval { .. } => NodeKind::Approval,
        StepKind::Workflow { .. } => NodeKind::Workflow,
        StepKind::Dream { .. } => NodeKind::Agent,
    }
}

fn agent_of(step: &Step) -> (Option<String>, Option<String>) {
    match &step.kind {
        StepKind::Agent { agent, model, .. } => (Some(agent.clone()), model.clone()),
        _ => (None, None),
    }
}

fn min_opt<T: PartialOrd + Copy>(a: Option<T>, b: Option<T>) -> Option<T> {
    match (a, b) {
        (Some(a), Some(b)) => Some(if b < a { b } else { a }),
        (a, None) => a,
        (None, b) => b,
    }
}

/// Percent string for budget messages.
fn pct(spent: f64, limit: f64) -> String {
    if limit <= 0.0 {
        return "100%".into();
    }
    format!("{:.0}%", spent / limit * 100.0)
}

/// Checks spend against `budget`; returns `(warning, exceeded)` messages
/// for the first crossing of each threshold.
fn budget_check(
    what: &str,
    budget: &Budget,
    spent_usd: f64,
    tokens: u64,
    warned: &mut bool,
    exceeded: &mut bool,
) -> (Option<String>, Option<String>) {
    let over_usd = budget.usd.is_some_and(|l| spent_usd >= l);
    let over_tok = budget.tokens.is_some_and(|l| tokens >= l);
    let near_usd = budget.usd.is_some_and(|l| spent_usd >= l * budget.warn_at);
    let near_tok = budget
        .tokens
        .is_some_and(|l| tokens as f64 >= l as f64 * budget.warn_at);
    let describe = || {
        let mut parts = Vec::new();
        if let Some(l) = budget.usd {
            parts.push(format!(
                "${spent_usd:.2} of ${l:.2} ({})",
                pct(spent_usd, l)
            ));
        }
        if let Some(l) = budget.tokens {
            parts.push(format!(
                "{tokens} of {l} tokens ({})",
                pct(tokens as f64, l as f64)
            ));
        }
        parts.join(", ")
    };
    let mut warn = None;
    let mut over = None;
    if (over_usd || over_tok) && !*exceeded {
        *exceeded = true;
        *warned = true;
        over = Some(format!(
            "budget: {what} exhausted its budget: {}",
            describe()
        ));
    } else if (near_usd || near_tok) && !*warned {
        *warned = true;
        warn = Some(format!("budget warning: {what} has used {}", describe()));
    }
    (warn, over)
}

impl WfRun {
    #[allow(clippy::too_many_arguments)]
    fn new(
        sh: Arc<Shared>,
        wf: Workflow,
        inputs: serde_json::Map<String, Value>,
        node: NodeId,
        parent: Option<NodeId>,
        depth: u32,
        persist: bool,
        reuse: Option<ReusePlan>,
    ) -> Arc<Self> {
        let mut steps = BTreeMap::new();
        for s in &wf.steps {
            steps.insert(
                s.id.clone(),
                StepRecord {
                    node: Some(Self::step_node_of(&node, &s.id)),
                    ..StepRecord::default()
                },
            );
        }
        let state = WorkflowState {
            version: STATE_VERSION,
            workflow: wf.name.clone(),
            run_id: Some(sh.recorder.run_id()),
            node: Some(node.clone()),
            inputs,
            origin: reuse.as_ref().map(|p| p.origin.clone()),
            status: WorkflowStatus::Running,
            error: None,
            steps,
            budget: BudgetState {
                limit_usd: wf.budget.and_then(|b| b.usd),
                limit_tokens: wf.budget.and_then(|b| b.tokens),
                ..BudgetState::default()
            },
        };
        Arc::new(Self {
            sh,
            wf,
            node,
            parent,
            depth,
            persist,
            state: Mutex::new(state),
            reuse,
        })
    }

    fn step_node_of(wf_node: &NodeId, id: &str) -> NodeId {
        NodeId::from(format!("{}/{id}", wf_node.as_str()))
    }

    fn step_node(&self, id: &str) -> NodeId {
        Self::step_node_of(&self.node, id)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, WorkflowState> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn save(&self) {
        if !self.persist {
            return;
        }
        let st = self.lock().clone();
        if let Err(e) = st.save(self.sh.recorder.dir()) {
            tracing::warn!(target: "harness::workflow", error = %e, "could not save workflow state");
        }
    }

    fn context(&self) -> Context {
        let st = self.lock();
        let mut ctx = Context {
            inputs: st.inputs.clone(),
            run: RunView {
                id: self.sh.recorder.run_id().0.to_string(),
                dir: self.sh.recorder.dir().display().to_string(),
            },
            ..Context::default()
        };
        for (id, r) in &st.steps {
            // A step reset by a loop keeps its last result visible.
            if r.status.is_terminal() || r.iterations > 0 {
                ctx.steps.insert(
                    id.clone(),
                    StepView {
                        outputs: r.outputs.clone(),
                        status: r.status.as_str().into(),
                        artifacts: r
                            .artifacts
                            .iter()
                            .map(|(k, p)| (k.clone(), p.display().to_string()))
                            .collect(),
                    },
                );
            }
        }
        ctx
    }

    fn sync_task(&self, step: &Step, status: StepStatus, node: &NodeId) {
        if let Some(board) = &self.sh.board
            && self.depth == 0
        {
            let (agent, _) = agent_of(step);
            let owner = (status != StepStatus::Pending).then_some(node);
            if let Err(e) = board.sync_step(&step.id, task_status(status), owner, agent.as_deref())
            {
                tracing::warn!(target: "harness::workflow", error = %e, "could not update the task list");
            }
        }
    }

    fn set_status(&self, step: &Step, status: StepStatus) {
        let node = self.step_node(&step.id);
        self.lock().steps.entry(step.id.clone()).or_default().status = status;
        self.sync_task(step, status, &node);
        self.save();
    }

    fn finish_node(&self, node: &NodeId, status: StepStatus, error: Option<String>) {
        self.sh.node(
            node,
            EventKind::NodeFinished {
                status: step_status_to_node(status),
                error,
            },
        );
    }

    fn warn(&self, message: String) {
        tracing::warn!(target: "harness::workflow", "{message}");
        self.sh.node(
            &self.node,
            EventKind::Error {
                message,
                recoverable: true,
                action: None,
            },
        );
    }

    fn declare_step(&self, step: &Step, queued: bool) {
        let (agent, model) = agent_of(step);
        let mut draft = EventDraft::node(
            self.step_node(&step.id),
            EventKind::NodeStarted {
                kind: node_kind(step),
                name: step.id.clone(),
                agent,
                model,
                host: None,
                needs: step.needs.iter().map(|n| self.step_node(n)).collect(),
                queued,
            },
        );
        draft = draft.with_parent(self.node.clone());
        self.sh.emit(draft);
    }

    fn take_over(&self, step: &Step, plan: &ReusePlan) {
        let prev_store = ArtifactStore::new(&plan.previous_dir);
        let mut rec = plan
            .previous
            .steps
            .get(&step.id)
            .cloned()
            .unwrap_or_default();
        let mut paths = BTreeMap::new();
        if let Ok(all) = prev_store.list_all() {
            for key in all.keys() {
                if key == &step.id || key.starts_with(&format!("{}[", step.id)) {
                    match self.sh.store.copy_from(&prev_store, key) {
                        Ok(copied) => {
                            if key == &step.id {
                                paths.extend(copied);
                            }
                        }
                        Err(e) => self.warn(format!("could not copy artifacts of `{key}`: {e}")),
                    }
                }
            }
        }
        for (name, p) in &mut rec.artifacts {
            if let Some(newp) = paths.get(name) {
                *p = newp.clone();
            }
        }
        rec.node = Some(self.step_node(&step.id));
        rec.reused_from = Some(plan.origin.run);
        let status = rec.status;
        self.lock().steps.insert(step.id.clone(), rec);
        self.declare_step(step, false);
        self.finish_node(&self.step_node(&step.id), status, None);
        self.sync_task(step, status, &self.step_node(&step.id));
    }

    async fn execute(self: Arc<Self>, cancel: &CancellationToken) -> RunOutcome {
        let token = cancel.child_token();
        let wf_draft = EventDraft::node(
            self.node.clone(),
            EventKind::NodeStarted {
                kind: NodeKind::Workflow,
                name: self.wf.name.clone(),
                agent: None,
                model: None,
                host: None,
                needs: Vec::new(),
                queued: false,
            },
        );
        self.sh.emit(match &self.parent {
            Some(p) => wf_draft.with_parent(p.clone()),
            None => wf_draft,
        });

        // Goal bar.
        if self.depth == 0
            && let Some(board) = &self.sh.board
        {
            if let Some(goal) = &self.wf.goal {
                match template::render(goal, &self.context().to_value()) {
                    Ok(g) => {
                        let _ = board.set_goal(Some(g));
                    }
                    Err(e) => self.warn(format!("goal: {e}")),
                }
            }
            let tasks = self.wf.tasks.iter().map(|t| {
                let task = Task::new(t.id.clone(), t.title.clone());
                match &t.step {
                    Some(s) => task.with_step(s.clone()),
                    None => task,
                }
            });
            if let Err(e) = board.seed(tasks) {
                self.warn(format!("could not seed the task list: {e}"));
            }
        }

        let reuse_ids = self
            .reuse
            .as_ref()
            .map(|p| p.reuse.clone())
            .unwrap_or_default();
        let mut pending: BTreeSet<String> = BTreeSet::new();
        for s in &self.wf.steps {
            if let Some(plan) = self.reuse.as_ref().filter(|_| reuse_ids.contains(&s.id)) {
                self.take_over(s, plan);
            } else {
                self.declare_step(s, true);
                pending.insert(s.id.clone());
            }
        }
        self.save();

        let mut join: JoinSet<(String, StepResult)> = JoinSet::new();
        let mut running: BTreeSet<String> = BTreeSet::new();
        let mut abort: Option<String> = None;
        loop {
            if abort.is_none() && !token.is_cancelled() {
                self.start_ready(&mut pending, &mut running, &mut join, &token);
            }
            if running.is_empty() {
                break;
            }
            let Some(joined) = join.join_next().await else {
                break;
            };
            let (id, mut res) = match joined {
                Ok(v) => v,
                Err(e) => {
                    abort.get_or_insert(format!("a step task crashed: {e}"));
                    token.cancel();
                    continue;
                }
            };
            running.remove(&id);
            let Some(step) = self.wf.step(&id).cloned() else {
                continue;
            };

            // Budget accounting.
            let mut over = None;
            {
                let mut st = self.lock();
                st.budget.spent_usd += res.cost_usd;
                st.budget.tokens += res.tokens;
                if let Some(b) = &self.wf.budget {
                    let (spent, tokens) = (st.budget.spent_usd, st.budget.tokens);
                    let BudgetState {
                        warned, exceeded, ..
                    } = &mut st.budget;
                    let (w, o) = budget_check(
                        &format!("workflow `{}`", self.wf.name),
                        b,
                        spent,
                        tokens,
                        warned,
                        exceeded,
                    );
                    drop(st);
                    if let Some(w) = w {
                        self.warn(w);
                    }
                    if let Some(o) = o {
                        if b.on_exceed == OnExceed::Stop {
                            over = Some(o);
                        } else {
                            self.warn(o);
                        }
                    }
                }
            }

            // Loops.
            let mut reset = Vec::new();
            if res.status == StepStatus::Succeeded
                && let Some(l) = &step.loop_spec
            {
                let iterations = self.lock().steps.get(&id).map_or(0, |r| r.iterations) + 1;
                self.record(&step, &res, iterations);
                let done = template::eval_bool(&l.until, &self.context().to_value());
                match done {
                    Ok(true) => {}
                    Ok(false)
                        if iterations < l.max_iterations && abort.is_none() && over.is_none() =>
                    {
                        let from = l.retry_step.clone().unwrap_or_else(|| id.clone());
                        reset = self.wf.loop_chain(&from, &id).into_iter().collect();
                    }
                    Ok(false) => {
                        res.status = StepStatus::Failed;
                        res.error = Some(format!(
                            "loop.until was not met after {iterations} iteration(s)"
                        ));
                    }
                    Err(e) => {
                        res.status = StepStatus::Failed;
                        res.error = Some(format!("loop.until: {e}"));
                    }
                }
                if !reset.is_empty() {
                    for r in &reset {
                        if let Some(s) = self.wf.step(r) {
                            if *r != id {
                                self.sh.node(
                                    &self.step_node(r),
                                    EventKind::NodeRetry {
                                        attempt: iterations + 1,
                                        reason: format!(
                                            "loop `{id}`: `until` not met, iteration {}",
                                            iterations + 1
                                        ),
                                    },
                                );
                            }
                            self.set_status(s, StepStatus::Pending);
                            pending.insert(r.clone());
                        }
                    }
                    self.save();
                    if let Some(o) = over {
                        abort.get_or_insert(o);
                        token.cancel();
                    }
                    continue;
                }
                let node = self.step_node(&id);
                self.finish_node(&node, res.status, res.error.clone());
                {
                    let mut st = self.lock();
                    let r = st.steps.entry(id.clone()).or_default();
                    r.status = res.status;
                    r.error.clone_from(&res.error);
                }
                self.sync_task(&step, res.status, &node);
                self.save();
            } else {
                let iterations = self.lock().steps.get(&id).map_or(0, |r| r.iterations);
                self.record(&step, &res, iterations);
            }
            if res.status == StepStatus::Failed && step.on_failure == OnFailure::Abort {
                abort.get_or_insert(format!(
                    "step `{id}` failed: {}",
                    res.error.clone().unwrap_or_default()
                ));
                token.cancel();
            }
            if let Some(o) = over {
                abort.get_or_insert(o);
                token.cancel();
            }
        }

        // Whatever never started.
        let cancelled = cancel.is_cancelled();
        for id in &pending {
            let Some(step) = self.wf.step(id) else {
                continue;
            };
            let (status, why) = if cancelled {
                (StepStatus::Cancelled, "cancelled".to_owned())
            } else {
                (
                    StepStatus::Skipped,
                    format!(
                        "not run: {}",
                        abort
                            .clone()
                            .unwrap_or_else(|| "upstream did not finish".into())
                    ),
                )
            };
            {
                let mut st = self.lock();
                let r = st.steps.entry(id.clone()).or_default();
                r.status = status;
                r.error = Some(why.clone());
            }
            self.finish_node(&self.step_node(id), status, Some(why));
            self.sync_task(step, status, &self.step_node(id));
        }

        let (status, error) = if cancelled {
            (WorkflowStatus::Cancelled, Some("cancelled".to_owned()))
        } else if let Some(a) = abort {
            (WorkflowStatus::Failed, Some(a))
        } else {
            (WorkflowStatus::Succeeded, None)
        };
        {
            let mut st = self.lock();
            st.status = status;
            st.error.clone_from(&error);
        }
        self.save();
        self.sh.node(
            &self.node,
            EventKind::NodeFinished {
                status: match status {
                    WorkflowStatus::Succeeded => NodeStatus::Succeeded,
                    WorkflowStatus::Cancelled => NodeStatus::Cancelled,
                    _ => NodeStatus::Failed,
                },
                error: error.clone(),
            },
        );
        let state = self.lock().clone();
        RunOutcome {
            status,
            error,
            state,
        }
    }

    fn record(&self, step: &Step, res: &StepResult, iterations: u32) {
        {
            let mut st = self.lock();
            let r = st.steps.entry(step.id.clone()).or_default();
            r.status = res.status;
            r.outputs.clone_from(&res.outputs);
            r.artifacts.clone_from(&res.artifacts);
            r.error.clone_from(&res.error);
            r.cost_usd += res.cost_usd;
            r.tokens += res.tokens;
            r.attempts += res.attempts;
            r.iterations = iterations;
            if res.checkpoint.is_some() {
                r.checkpoint = res.checkpoint;
            }
        }
        self.sync_task(step, res.status, &self.step_node(&step.id));
        self.save();
    }

    fn need_ok(&self, need: &str) -> Option<bool> {
        let st = self.lock();
        let r = st.steps.get(need)?;
        if !r.status.is_terminal() {
            return None;
        }
        let cont = self
            .wf
            .step(need)
            .is_some_and(|s| s.on_failure == OnFailure::Continue);
        Some(r.status == StepStatus::Succeeded || (r.status == StepStatus::Failed && cont))
    }

    fn start_ready(
        self: &Arc<Self>,
        pending: &mut BTreeSet<String>,
        running: &mut BTreeSet<String>,
        join: &mut JoinSet<(String, StepResult)>,
        token: &CancellationToken,
    ) {
        let order: Vec<Step> = self.wf.topo_order().cloned().collect();
        for step in order {
            if !pending.contains(&step.id) {
                continue;
            }
            let needs: Vec<Option<bool>> = step.needs.iter().map(|n| self.need_ok(n)).collect();
            if needs.iter().any(Option::is_none) {
                continue;
            }
            pending.remove(&step.id);
            let all_ok = needs.iter().all(|n| *n == Some(true));
            let ctx = self.context();
            let skip = match (&step.condition, all_ok) {
                (None, false) => Some(Ok("an upstream step did not succeed".to_owned())),
                (None, true) => None,
                (Some(c), _) => match template::eval_bool(c, &ctx.to_value()) {
                    Ok(true) => None,
                    Ok(false) => Some(Ok(format!("`if` is false: {c}"))),
                    Err(e) => Some(Err(format!("if: {e}"))),
                },
            };
            if let Some(why) = skip {
                let (status, msg) = match why {
                    Ok(m) => (StepStatus::Skipped, m),
                    Err(m) => (StepStatus::Failed, m),
                };
                let res = StepResult {
                    status,
                    error: Some(msg),
                    ..StepResult::default()
                };
                self.finish_node(&self.step_node(&step.id), status, res.error.clone());
                running.insert(step.id.clone());
                let id = step.id.clone();
                join.spawn(async move { (id, res) });
                continue;
            }
            self.set_status(&step, StepStatus::Running);
            running.insert(step.id.clone());
            let me = self.clone();
            let token = token.clone();
            let id = step.id.clone();
            join.spawn(async move {
                let res = me.run_step(&step, ctx, &token).await;
                (id, res)
            });
        }
    }

    fn expand(&self, step: &Step, ctx: &Context) -> Result<Vec<(String, Context)>, String> {
        let list = |v: &Value, what: &str| -> Result<Vec<Value>, String> {
            let v =
                template::render_value(v, &ctx.to_value()).map_err(|e| format!("{what}: {e}"))?;
            match v {
                Value::Array(a) => Ok(a),
                other => Err(format!("{what} must evaluate to a list, got {other}")),
            }
        };
        Ok(match &step.expand {
            None => vec![(step.id.clone(), ctx.clone())],
            Some(Expand::ForEach(v)) => list(v, "for_each")?
                .into_iter()
                .enumerate()
                .map(|(i, item)| {
                    let mut c = ctx.clone();
                    c.item = Some(item);
                    (format!("{}[{i}]", step.id), c)
                })
                .collect(),
            Some(Expand::Matrix(m)) => {
                let mut combos: Vec<serde_json::Map<String, Value>> = vec![serde_json::Map::new()];
                for (k, v) in m {
                    let values = list(v, &format!("matrix.{k}"))?;
                    let mut next = Vec::new();
                    for c in &combos {
                        for val in &values {
                            let mut c = c.clone();
                            c.insert(k.clone(), val.clone());
                            next.push(c);
                        }
                    }
                    combos = next;
                }
                combos
                    .into_iter()
                    .enumerate()
                    .map(|(i, combo)| {
                        let mut c = ctx.clone();
                        c.matrix = Some(combo);
                        (format!("{}[{i}]", step.id), c)
                    })
                    .collect()
            }
        })
    }

    async fn run_step(
        self: &Arc<Self>,
        step: &Step,
        ctx: Context,
        token: &CancellationToken,
    ) -> StepResult {
        let step_node = self.step_node(&step.id);
        let iteration = self.lock().steps.get(&step.id).map_or(0, |r| r.iterations);
        if iteration > 0 {
            self.sh.node(
                &step_node,
                EventKind::NodeRetry {
                    attempt: iteration + 1,
                    reason: format!(
                        "loop iteration {} of {}",
                        iteration + 1,
                        step.loop_spec.as_ref().map_or(1, |l| l.max_iterations)
                    ),
                },
            );
        }
        let instances = match self.expand(step, &ctx) {
            Ok(v) => v,
            Err(e) => {
                let res = StepResult {
                    status: StepStatus::Failed,
                    error: Some(e),
                    ..StepResult::default()
                };
                if step.loop_spec.is_none() {
                    self.finish_node(&step_node, res.status, res.error.clone());
                }
                return res;
            }
        };
        let res = if step.expand.is_none() {
            let (key, c) = instances.into_iter().next().unwrap_or_default();
            self.run_instance(step, key, step_node.clone(), step.id.clone(), c, token)
                .await
        } else {
            self.sh.node(&step_node, EventKind::NodeRunning);
            let (agent, model) = agent_of(step);
            let futures = instances.into_iter().map(|(key, c)| {
                let node = NodeId::from(format!("{}/{key}", self.node.as_str()));
                let label = match (&c.matrix, &c.item) {
                    (Some(m), _) => format!(
                        "{} ({})",
                        step.id,
                        m.iter()
                            .map(|(k, v)| format!(
                                "{k}={}",
                                v.as_str().map_or_else(|| v.to_string(), str::to_owned)
                            ))
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                    (None, Some(v)) => format!(
                        "{} ({})",
                        step.id,
                        v.as_str().map_or_else(|| v.to_string(), str::to_owned)
                    ),
                    _ => key.clone(),
                };
                self.sh.emit(
                    EventDraft::node(
                        node.clone(),
                        EventKind::NodeStarted {
                            kind: node_kind(step),
                            name: label.clone(),
                            agent: agent.clone(),
                            model: model.clone(),
                            host: None,
                            needs: Vec::new(),
                            queued: true,
                        },
                    )
                    .with_parent(step_node.clone()),
                );
                let me = self.clone();
                let token = token.clone();
                async move { me.run_instance(step, key, node, label, c, &token).await }
            });
            let results = join_all(futures).await;
            let mut agg = StepResult {
                status: StepStatus::Succeeded,
                ..StepResult::default()
            };
            let mut errors = Vec::new();
            for (i, r) in results.iter().enumerate() {
                agg.cost_usd += r.cost_usd;
                agg.tokens += r.tokens;
                agg.attempts += r.attempts;
                for (k, v) in &r.outputs {
                    let e = agg
                        .outputs
                        .entry(k.clone())
                        .or_insert_with(|| Value::Array(Vec::new()));
                    if let Value::Array(a) = e {
                        a.push(v.clone());
                    }
                }
                for (k, p) in &r.artifacts {
                    agg.artifacts.insert(format!("{k}_{i}"), p.clone());
                }
                match r.status {
                    StepStatus::Succeeded | StepStatus::Skipped => {}
                    StepStatus::Cancelled => {
                        if agg.status != StepStatus::Failed {
                            agg.status = StepStatus::Cancelled;
                        }
                    }
                    _ => agg.status = StepStatus::Failed,
                }
                if let Some(e) = &r.error
                    && r.status != StepStatus::Succeeded
                {
                    errors.push(format!("[{i}] {e}"));
                }
            }
            if !errors.is_empty() {
                agg.error = Some(errors.join("; "));
            }
            if step.loop_spec.is_none() || agg.status != StepStatus::Succeeded {
                self.finish_node(&step_node, agg.status, agg.error.clone());
            }
            return agg;
        };
        if step.loop_spec.is_none() || res.status != StepStatus::Succeeded {
            self.finish_node(&step_node, res.status, res.error.clone());
        }
        res
    }

    async fn run_instance(
        self: &Arc<Self>,
        step: &Step,
        key: String,
        node: NodeId,
        label: String,
        ctx: Context,
        token: &CancellationToken,
    ) -> StepResult {
        let vctx = ctx.to_value();
        match &step.kind {
            StepKind::Approval { message } => {
                return self.run_gate(step, &node, message, &vctx, token).await;
            }
            StepKind::Workflow { uses, with } => {
                return self.run_nested(step, &node, uses, with, &vctx, token).await;
            }
            _ => {}
        }
        let fail = |m: String| StepResult {
            status: StepStatus::Failed,
            error: Some(m),
            ..StepResult::default()
        };
        // Render first so template errors fail fast.
        let action = match self.action(step, &vctx) {
            Ok(a) => a,
            Err(e) => return fail(e),
        };
        let scope = match &step.scope {
            None => Vec::new(),
            Some(ScopeSpec::Globs(g)) => g.clone(),
            Some(ScopeSpec::Template(t)) => {
                match template::render_value(&Value::String(t.clone()), &vctx) {
                    Ok(Value::Array(a)) => a
                        .into_iter()
                        .filter_map(|v| v.as_str().map(str::to_owned))
                        .collect(),
                    Ok(Value::String(s)) => s
                        .split(',')
                        .map(|x| x.trim().to_owned())
                        .filter(|x| !x.is_empty())
                        .collect(),
                    Ok(other) => return fail(format!("scope must render to a list, got {other}")),
                    Err(e) => return fail(format!("scope: {e}")),
                }
            }
        };
        let runner = match &step.kind {
            StepKind::Agent { .. } => self.sh.runners.agent.clone(),
            StepKind::Shell { .. } => self.sh.runners.shell.clone(),
            StepKind::Verify { .. } => self.sh.runners.verify.clone(),
            StepKind::BugfixProof { .. } => self.sh.runners.proof.clone(),
            StepKind::Dream { .. } => self.sh.runners.dream.clone(),
            _ => None,
        };
        let Some(runner) = runner else {
            return fail(format!(
                "no runner is configured for `{}` steps",
                step.kind.type_name()
            ));
        };

        // Scheduler slot, then host.
        let slot = match self.sh.scheduler.acquire_agent(token, || {}).await {
            Ok(s) => Arc::new(s),
            Err(_) => {
                return StepResult {
                    status: StepStatus::Cancelled,
                    error: Some("cancelled while queued".into()),
                    ..StepResult::default()
                };
            }
        };
        let lease: HostLease = match (&step.runs_on, &self.sh.hosts) {
            (None | Some(RunsOn::Local), _) => local_lease(),
            (Some(r), Some(pool)) => match pool.acquire(r, token, || {}).await {
                Ok(l) => l,
                Err(crate::placement::PlacementError::Cancelled) => {
                    return StepResult {
                        status: StepStatus::Cancelled,
                        error: Some("cancelled while waiting for a host".into()),
                        ..StepResult::default()
                    };
                }
                Err(e) => return fail(e.to_string()),
            },
            (Some(_), None) => {
                return fail("runs_on needs remote hosts, but none are configured".into());
            }
        };
        let host = lease.placement().host().map(str::to_owned);
        let (agent, model) = agent_of(step);
        // Running (re-declared to carry the host).
        let mut started = EventDraft::node(
            node.clone(),
            EventKind::NodeStarted {
                kind: node_kind(step),
                name: label,
                agent,
                model,
                host: host.clone(),
                needs: Vec::new(),
                queued: false,
            },
        );
        if node != self.step_node(&step.id) {
            started = started.with_parent(self.step_node(&step.id));
        } else {
            started = started.with_parent(self.node.clone());
        }
        self.sh.emit(started);
        self.sh.node(&node, EventKind::NodeRunning);

        // Checkpoint before an agent changes files; proofs revert to the
        // checkpoint of the nearest upstream agent step.
        let mut checkpoint = None;
        if matches!(step.kind, StepKind::Agent { .. })
            && let Some(cp) = &self.sh.runners.checkpoints
        {
            match cp.checkpoint(&node, &format!("before workflow step {key}")) {
                Ok(id) => checkpoint = Some(id),
                Err(e) => self.warn(format!("step `{key}`: could not take a checkpoint: {e}")),
            }
        }
        let proof_checkpoint = if matches!(step.kind, StepKind::BugfixProof { .. }) {
            let st = self.lock();
            let up = self.wf.upstream(&step.id);
            self.wf
                .topo_order()
                .filter(|s| up.contains(&s.id) && matches!(s.kind, StepKind::Agent { .. }))
                .filter_map(|s| st.steps.get(&s.id).and_then(|r| r.checkpoint))
                .last()
        } else {
            None
        };

        let limits = self.limits(step);
        let artifacts_dir = match self.sh.store.dir_for(&key) {
            Ok(d) => d,
            Err(e) => return fail(format!("cannot create the artifact directory: {e}")),
        };
        let mut res = StepResult {
            checkpoint,
            ..StepResult::default()
        };
        let mut step_warned = false;
        let mut step_exceeded = false;
        let max_attempts = step.retries + 1;
        for attempt in 1..=max_attempts {
            if attempt > 1 {
                self.sh.node(
                    &node,
                    EventKind::NodeRetry {
                        attempt,
                        reason: res.error.clone().unwrap_or_default(),
                    },
                );
            }
            let attempt_token = token.child_token();
            let req = StepRequest {
                workflow: self.wf.name.clone(),
                step: step.id.clone(),
                instance: key.clone(),
                node: node.clone(),
                action: action.clone(),
                outputs: step.outputs.clone(),
                artifacts: step.produced_artifacts().to_vec(),
                artifacts_dir: artifacts_dir.clone(),
                scope: scope.clone(),
                host: host.clone(),
                limits,
                checkpoint: proof_checkpoint,
                slot: Some(slot.clone()),
                cancel: attempt_token.clone(),
                attempt,
            };
            res.attempts = attempt;
            let outcome =
                run_with_timeout(runner.as_ref(), req, step.timeout, &attempt_token).await;
            match outcome {
                Ok(out) => {
                    res.cost_usd += out.cost_usd;
                    res.tokens += out.tokens;
                    match self.store_outputs(step, &key, out) {
                        Ok((outputs, artifacts)) => {
                            res.outputs = outputs;
                            res.artifacts = artifacts;
                            res.status = StepStatus::Succeeded;
                            res.error = None;
                        }
                        Err(e) => {
                            res.status = StepStatus::Failed;
                            res.error = Some(e);
                        }
                    }
                }
                Err(f) => {
                    res.cost_usd += f.cost_usd;
                    res.tokens += f.tokens;
                    res.status = if f.cancelled && token.is_cancelled() {
                        StepStatus::Cancelled
                    } else {
                        StepStatus::Failed
                    };
                    res.error = Some(f.message);
                }
            }
            if let Some(b) = &step.budget {
                let (w, o) = budget_check(
                    &format!("step `{key}`"),
                    b,
                    res.cost_usd,
                    res.tokens,
                    &mut step_warned,
                    &mut step_exceeded,
                );
                if let Some(w) = w {
                    self.warn(w);
                }
                if let Some(o) = o {
                    if b.on_exceed == OnExceed::Stop {
                        res.status = StepStatus::Failed;
                        res.error = Some(o);
                        break;
                    }
                    self.warn(o);
                }
            }
            if res.status != StepStatus::Failed || token.is_cancelled() {
                break;
            }
        }
        drop(lease);
        // Release explicitly: a runner may keep its request (and the Arc).
        slot.suspend();
        drop(slot);
        if node != self.step_node(&step.id) {
            self.finish_node(&node, res.status, res.error.clone());
        }
        res
    }

    fn limits(&self, step: &Step) -> StepLimits {
        let mut l = StepLimits::default();
        if let Some(b) = step.budget.filter(|b| b.on_exceed == OnExceed::Stop) {
            l.usd = b.usd;
            l.tokens = b.tokens;
        }
        if let Some(b) = self.wf.budget.filter(|b| b.on_exceed == OnExceed::Stop) {
            let st = self.lock();
            l.usd = min_opt(l.usd, b.usd.map(|u| (u - st.budget.spent_usd).max(0.0)));
            l.tokens = min_opt(
                l.tokens,
                b.tokens.map(|t| t.saturating_sub(st.budget.tokens)),
            );
        }
        l
    }

    fn action(&self, step: &Step, ctx: &Value) -> Result<StepAction, String> {
        let r = |what: &str, t: &str| template::render(t, ctx).map_err(|e| format!("{what}: {e}"));
        Ok(match &step.kind {
            StepKind::Agent {
                agent,
                model,
                prompt,
            } => {
                let mut brief = match prompt {
                    Some(p) => r("prompt", p)?,
                    None => format!(
                        "Carry out the `{}` step of the `{}` workflow.",
                        step.id, self.wf.name
                    ),
                };
                // Inline artifacts the prompt refers to.
                if let Some(p) = prompt {
                    let refs = template::references(p).unwrap_or_default();
                    let st = self.lock();
                    let mut inline = Vec::new();
                    for rf in refs {
                        let parts: Vec<&str> = rf.split('.').collect();
                        if parts.len() == 4
                            && parts[0] == "steps"
                            && parts[2] == "artifacts"
                            && let Some(path) = st
                                .steps
                                .get(parts[1])
                                .and_then(|s| s.artifacts.get(parts[3]))
                        {
                            inline.push((parts[3].to_owned(), path.clone()));
                        }
                    }
                    drop(st);
                    brief.push_str(&artifacts::inline_section(&inline));
                }
                StepAction::Agent {
                    agent: agent.clone(),
                    model: match model {
                        Some(m) => Some(r("model", m)?),
                        None => None,
                    },
                    brief,
                }
            }
            StepKind::Shell { run } => StepAction::Shell {
                command: r("run", run)?,
            },
            StepKind::Verify { command, expect } => StepAction::Verify {
                command: r("verify", command)?,
                expect: *expect,
            },
            StepKind::BugfixProof { test, keep } => StepAction::Proof {
                test: r("test", test)?,
                keep: keep.clone(),
            },
            StepKind::Dream { with } => StepAction::Dream {
                settings: with.clone(),
            },
            StepKind::Approval { .. } | StepKind::Workflow { .. } => {
                return Err("internal: not a runner step".into());
            }
        })
    }

    fn store_outputs(
        &self,
        step: &Step,
        key: &str,
        out: StepOutput,
    ) -> Result<StoredOutputs, String> {
        let mut paths = BTreeMap::new();
        for name in step.produced_artifacts() {
            let path = match out.artifacts.get(name) {
                Some(data) => self
                    .sh
                    .store
                    .write(key, name, data)
                    .map_err(|e| format!("cannot store artifact `{name}`: {e}"))?,
                None => {
                    let p = self.sh.store.path(key, name).map_err(|e| e.to_string())?;
                    if !p.is_file() {
                        return Err(format!(
                            "step did not produce its declared artifact `{name}`"
                        ));
                    }
                    p
                }
            };
            paths.insert(name.clone(), path);
        }
        if let StepKind::Agent { .. } = step.kind {
            let missing: Vec<&String> = step
                .outputs
                .iter()
                .filter(|o| !out.outputs.contains_key(*o))
                .collect();
            if !missing.is_empty() {
                return Err(format!(
                    "agent did not return outputs: {}",
                    missing
                        .iter()
                        .map(|s| s.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            }
        }
        Ok((out.outputs, paths))
    }

    async fn run_gate(
        &self,
        step: &Step,
        node: &NodeId,
        message: &str,
        ctx: &Value,
        token: &CancellationToken,
    ) -> StepResult {
        let message = match template::render(message, ctx) {
            Ok(m) => m,
            Err(e) => {
                return StepResult {
                    status: StepStatus::Failed,
                    error: Some(format!("message: {e}")),
                    ..StepResult::default()
                };
            }
        };
        let mut review: Vec<ArtifactInfo> = Vec::new();
        {
            let st = self.lock();
            for name in &step.artifacts {
                for up in self.wf.upstream(&step.id) {
                    if let Some(p) = st.steps.get(&up).and_then(|r| r.artifacts.get(name)) {
                        review.push(ArtifactInfo {
                            step: up.clone(),
                            name: name.clone(),
                            size: std::fs::metadata(p).map(|m| m.len()).unwrap_or(0),
                            path: p.clone(),
                        });
                    }
                }
            }
        }
        self.sh.node(node, EventKind::NodeRunning);
        let approval_id = format!("gate-{}", node.as_str());
        let mut preview = message.clone();
        for a in &review {
            preview.push_str(&format!(
                "\n- {} ({}): {}",
                a.name,
                a.step,
                a.path.display()
            ));
        }
        self.sh.node(
            node,
            EventKind::ApprovalRequested {
                approval_id: approval_id.clone(),
                action: message.clone(),
                preview: Some(Payload::Text(preview)),
            },
        );
        self.sync_task(step, StepStatus::Running, node);
        if let Some(board) = &self.sh.board
            && self.depth == 0
        {
            let _ = board.sync_step(&step.id, TaskStatus::Blocked, Some(node), None);
        }
        let decision = self
            .sh
            .runners
            .gates
            .decide(
                GateRequest {
                    approval_id: approval_id.clone(),
                    node: node.clone(),
                    step: step.id.clone(),
                    workflow: self.wf.name.clone(),
                    message,
                    artifacts: review,
                },
                token,
            )
            .await;
        self.sh.node(
            node,
            EventKind::ApprovalResolved {
                approval_id,
                decision: if decision.approved {
                    ApprovalDecision::Approved
                } else {
                    ApprovalDecision::Denied
                },
                by: decision.by.clone(),
            },
        );
        let mut outputs = serde_json::Map::new();
        outputs.insert("approved".into(), json!(decision.approved));
        outputs.insert("by".into(), json!(decision.by));
        outputs.insert("reason".into(), json!(decision.reason));
        StepResult {
            status: if decision.approved {
                StepStatus::Succeeded
            } else if token.is_cancelled() {
                StepStatus::Cancelled
            } else {
                StepStatus::Failed
            },
            error: (!decision.approved).then(|| {
                format!(
                    "approval denied by {}{}",
                    decision.by,
                    decision
                        .reason
                        .as_deref()
                        .map(|r| format!(": {r}"))
                        .unwrap_or_default()
                )
            }),
            outputs,
            attempts: 1,
            ..StepResult::default()
        }
    }

    async fn run_nested(
        &self,
        step: &Step,
        node: &NodeId,
        uses: &str,
        with: &serde_json::Map<String, Value>,
        ctx: &Value,
        token: &CancellationToken,
    ) -> StepResult {
        let fail = |m: String| StepResult {
            status: StepStatus::Failed,
            error: Some(m),
            ..StepResult::default()
        };
        if self.depth + 1 > MAX_NESTING {
            return fail(format!(
                "workflows are nested deeper than {MAX_NESTING} levels"
            ));
        }
        let Some(child) = self.sh.library.as_ref().and_then(|l| l.get(uses)) else {
            return fail(format!("unknown workflow `{uses}`"));
        };
        let inputs = match template::render_value(&Value::Object(with.clone()), ctx) {
            Ok(Value::Object(m)) => m,
            Ok(_) => serde_json::Map::new(),
            Err(e) => return fail(format!("with: {e}")),
        };
        let inputs = match resolve_inputs(&child, &inputs) {
            Ok(i) => i,
            Err(e) => return fail(e),
        };
        self.sh.node(node, EventKind::NodeRunning);
        let run = WfRun::new(
            self.sh.clone(),
            child,
            inputs,
            NodeId::from(format!("{}/{uses}", node.as_str())),
            Some(node.clone()),
            self.depth + 1,
            false,
            None,
        );
        let out = Box::pin(run.execute(token)).await;
        let mut res = StepResult {
            status: match out.status {
                WorkflowStatus::Succeeded => StepStatus::Succeeded,
                WorkflowStatus::Cancelled => StepStatus::Cancelled,
                _ => StepStatus::Failed,
            },
            error: out.error.clone(),
            cost_usd: out.state.budget.spent_usd,
            tokens: out.state.budget.tokens,
            attempts: 1,
            ..StepResult::default()
        };
        for (id, r) in &out.state.steps {
            res.outputs
                .insert(id.clone(), Value::Object(r.outputs.clone()));
        }
        for name in step.produced_artifacts() {
            match out.state.steps.values().find_map(|r| r.artifacts.get(name)) {
                Some(p) => {
                    res.artifacts.insert(name.clone(), p.clone());
                }
                None if res.status == StepStatus::Succeeded => {
                    res.status = StepStatus::Failed;
                    res.error = Some(format!(
                        "nested workflow `{uses}` produced no artifact `{name}`"
                    ));
                }
                None => {}
            }
        }
        res
    }
}

async fn run_with_timeout(
    runner: &dyn StepRunner,
    req: StepRequest,
    timeout: Option<Duration>,
    attempt_token: &CancellationToken,
) -> Result<StepOutput, StepFailure> {
    let fut = runner.run(req);
    let Some(t) = timeout else {
        return fut.await;
    };
    tokio::pin!(fut);
    tokio::select! {
        r = &mut fut => r,
        _ = tokio::time::sleep(t) => {
            attempt_token.cancel();
            let partial = tokio::time::timeout(TIMEOUT_GRACE, &mut fut).await;
            let (cost_usd, tokens) = match &partial {
                Ok(Ok(o)) => (o.cost_usd, o.tokens),
                Ok(Err(f)) => (f.cost_usd, f.tokens),
                Err(_) => (0.0, 0),
            };
            Err(StepFailure {
                message: format!("timed out after {}", fmt_duration(t)),
                cancelled: false,
                cost_usd,
                tokens,
            })
        }
    }
}

fn fmt_duration(d: Duration) -> String {
    let s = d.as_secs();
    if s == 0 {
        format!("{}ms", d.as_millis())
    } else if s.is_multiple_of(60) && s >= 60 {
        format!("{}m", s / 60)
    } else {
        format!("{s}s")
    }
}

#[cfg(test)]
mod tests;
