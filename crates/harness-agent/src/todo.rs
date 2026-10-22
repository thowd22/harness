//! The `todo` tool: one task list shared by every agent in a run.
//!
//! The [`TaskBoard`] is the run's goal and task list (the goal bar). Agents
//! whose `tools:` include `todo` get a [`TodoTool`] bound to their own node
//! ([`Actor`]); workflows seed the board from their `goal`/`tasks` and keep
//! step-linked tasks in sync ([`TaskBoard::seed`], [`TaskBoard::sync_step`]).
//!
//! Every change:
//!
//! - is applied under one lock and numbered with a strictly increasing
//!   `version`, so parallel agents never lose writes;
//! - is broadcast as a [`TaskEvent`] to [`TaskBoard::subscribe`]rs (live
//!   views);
//! - is appended to the run's task journal ([`TASKS_FILE`] in the run
//!   directory) while the lock is held, so the journal order is the apply
//!   order. [`TaskBoard::open`] replays a journal and
//!   [`TaskBoard::continue_from`] carries a previous run's tasks into a
//!   resumed run.

use std::fs::{File, OpenOptions};
use std::io::{self, BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use harness_events::schema::NodeId;
use harness_tools::tool::{ToolContext, ToolError, ToolOutput};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::broadcast;

use crate::dispatch::AgentTool;

/// Tool name.
pub const NAME: &str = "todo";
/// Tool description sent to the model.
pub const DESCRIPTION: &str = "Shared task list for this run, shown to the user. \
add: new task (you own it). update: change title/status, claim=true to take it over. list: all tasks.";
/// Task journal file name inside the run directory.
pub const TASKS_FILE: &str = "tasks.jsonl";
/// Capacity of the live event channel.
pub const EVENT_CAPACITY: usize = 256;

/// Status of one task.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    /// Not started.
    #[default]
    Pending,
    /// Someone is working on it.
    InProgress,
    /// Waiting on something.
    Blocked,
    /// Finished.
    Done,
    /// Gave up or broke.
    Failed,
    /// No longer needed.
    Cancelled,
}

impl TaskStatus {
    /// Every status, in display order.
    pub const ALL: [TaskStatus; 6] = [
        Self::Pending,
        Self::InProgress,
        Self::Blocked,
        Self::Done,
        Self::Failed,
        Self::Cancelled,
    ];

    /// The snake_case name.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::InProgress => "in_progress",
            Self::Blocked => "blocked",
            Self::Done => "done",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }

    /// Parses a snake_case name.
    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|t| t.as_str() == s)
    }

    /// True for done, failed and cancelled.
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Done | Self::Failed | Self::Cancelled)
    }
}

/// One task on the board.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Task {
    /// Stable id (`t1`, `t2`, ... or a workflow-declared id).
    pub id: String,
    /// What to do.
    pub title: String,
    /// Status.
    pub status: TaskStatus,
    /// Owning agent node.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<NodeId>,
    /// Owning agent name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    /// Workflow step this task follows (status and owner come from it).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step: Option<String>,
    /// Board version of the last change to this task.
    pub version: u64,
}

impl Task {
    /// A pending, unowned task.
    pub fn new(id: impl Into<String>, title: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            title: title.into(),
            status: TaskStatus::Pending,
            owner: None,
            agent: None,
            step: None,
            version: 0,
        }
    }

    /// Links the task to a workflow step.
    pub fn with_step(mut self, step: impl Into<String>) -> Self {
        self.step = Some(step.into());
        self
    }

    /// One line for the model: `t1 [in_progress] Title (agent)`.
    pub fn line(&self) -> String {
        let mut s = format!("{} [{}] {}", self.id, self.status.as_str(), self.title);
        if let Some(a) = &self.agent {
            s.push_str(&format!(" ({a})"));
        }
        s
    }
}

/// The agent calling the tool.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Actor {
    /// Its node.
    pub node: NodeId,
    /// Its agent name.
    pub agent: String,
}

impl Actor {
    /// `agent` running as `node`.
    pub fn new(node: NodeId, agent: impl Into<String>) -> Self {
        Self {
            node,
            agent: agent.into(),
        }
    }
}

/// A requested change.
#[derive(Debug, Clone, PartialEq)]
pub enum TaskOp {
    /// Adds a task owned by the caller.
    Add {
        /// Id; generated when `None`.
        id: Option<String>,
        /// Title.
        title: String,
        /// Initial status (default pending).
        status: Option<TaskStatus>,
    },
    /// Changes a task.
    Update {
        /// Which task.
        id: String,
        /// New title.
        title: Option<String>,
        /// New status.
        status: Option<TaskStatus>,
        /// Make the caller the owner.
        claim: bool,
    },
}

/// What a [`TaskEvent`] changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskChange {
    /// A task was added.
    Added,
    /// A task changed.
    Updated,
    /// The goal changed.
    Goal,
    /// Full state carried over from a previous run.
    Snapshot,
}

/// One applied change (live event and journal line).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskEvent {
    /// Board version after the change (strictly increasing).
    pub version: u64,
    /// Kind of change.
    pub change: TaskChange,
    /// The task after the change (`added` / `updated`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task: Option<Task>,
    /// The goal (`goal` / `snapshot`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub goal: Option<String>,
    /// All tasks (`snapshot`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tasks: Vec<Task>,
    /// The node that made the change, if an agent did.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub by: Option<NodeId>,
}

/// The board's state at one version.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct TaskSnapshot {
    /// Goal line.
    pub goal: Option<String>,
    /// Tasks in creation order.
    pub tasks: Vec<Task>,
    /// Version of the last change.
    pub version: u64,
}

impl TaskSnapshot {
    /// `(done, total)` for the goal-bar header.
    pub fn progress(&self) -> (usize, usize) {
        let done = self
            .tasks
            .iter()
            .filter(|t| t.status == TaskStatus::Done)
            .count();
        (done, self.tasks.len())
    }

    /// True when the goal bar should be shown.
    pub fn has_tasks(&self) -> bool {
        !self.tasks.is_empty()
    }
}

/// Why a change was refused (the message is written for the model).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TodoError {
    /// `add` with an id that exists.
    #[error("task `{0}` already exists. Fix: use action=update with id `{0}`.")]
    Exists(String),
    /// `update` of an unknown id.
    #[error("no task `{id}`. Fix: use one of: {known}.")]
    Unknown {
        /// Requested id.
        id: String,
        /// Known ids.
        known: String,
    },
    /// Empty title.
    #[error("title must not be empty. Fix: give a short task title.")]
    EmptyTitle,
    /// Journal write failed.
    #[error("could not save the task list: {0}. Fix: retry.")]
    Io(String),
}

struct BoardState {
    snapshot: TaskSnapshot,
    next_id: u64,
    journal: Option<File>,
}

/// The run's shared task list.
pub struct TaskBoard {
    state: Mutex<BoardState>,
    tx: broadcast::Sender<TaskEvent>,
    path: Option<PathBuf>,
}

impl std::fmt::Debug for TaskBoard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TaskBoard")
            .field("path", &self.path)
            .field("snapshot", &self.snapshot())
            .finish()
    }
}

impl Default for TaskBoard {
    fn default() -> Self {
        Self::new()
    }
}

fn apply_event(snap: &mut TaskSnapshot, next_id: &mut u64, ev: &TaskEvent) {
    snap.version = snap.version.max(ev.version);
    match ev.change {
        TaskChange::Goal => snap.goal.clone_from(&ev.goal),
        TaskChange::Snapshot => {
            snap.goal.clone_from(&ev.goal);
            snap.tasks.clone_from(&ev.tasks);
        }
        TaskChange::Added | TaskChange::Updated => {
            if let Some(t) = &ev.task {
                match snap.tasks.iter_mut().find(|x| x.id == t.id) {
                    Some(x) => *x = t.clone(),
                    None => snap.tasks.push(t.clone()),
                }
            }
        }
    }
    for t in &snap.tasks {
        if let Some(n) = t.id.strip_prefix('t').and_then(|n| n.parse::<u64>().ok()) {
            *next_id = (*next_id).max(n + 1);
        }
    }
}

fn read_journal(path: &Path) -> io::Result<(TaskSnapshot, u64)> {
    let mut snap = TaskSnapshot::default();
    let mut next = 1;
    let file = match File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok((snap, next)),
        Err(e) => return Err(e),
    };
    for line in BufReader::new(file).lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        // A torn last line (crash mid-write) is ignored.
        match serde_json::from_str::<TaskEvent>(&line) {
            Ok(ev) => apply_event(&mut snap, &mut next, &ev),
            Err(e) => {
                tracing::warn!(target: "harness::todo", error = %e, "skipping bad task journal line")
            }
        }
    }
    Ok((snap, next))
}

fn open_append(path: &Path) -> io::Result<File> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    OpenOptions::new().create(true).append(true).open(path)
}

impl TaskBoard {
    /// An empty in-memory board.
    pub fn new() -> Self {
        Self {
            state: Mutex::new(BoardState {
                snapshot: TaskSnapshot::default(),
                next_id: 1,
                journal: None,
            }),
            tx: broadcast::channel(EVENT_CAPACITY).0,
            path: None,
        }
    }

    /// Opens the journal at `path` (usually `<run dir>/tasks.jsonl`),
    /// replaying any existing lines, and appends future changes to it.
    pub fn open(path: impl Into<PathBuf>) -> io::Result<Self> {
        let path = path.into();
        let (snapshot, next_id) = read_journal(&path)?;
        let journal = open_append(&path)?;
        Ok(Self {
            state: Mutex::new(BoardState {
                snapshot,
                next_id,
                journal: Some(journal),
            }),
            tx: broadcast::channel(EVENT_CAPACITY).0,
            path: Some(path),
        })
    }

    /// Opens a fresh journal at `path` that starts with the state of the
    /// previous run's journal `previous` (session resume).
    pub fn continue_from(previous: &Path, path: impl Into<PathBuf>) -> io::Result<Self> {
        let (prev, _) = read_journal(previous)?;
        let board = Self::open(path)?;
        if prev.has_tasks() || prev.goal.is_some() {
            let mut st = board.lock();
            let version = st.snapshot.version.max(prev.version) + 1;
            let ev = TaskEvent {
                version,
                change: TaskChange::Snapshot,
                task: None,
                goal: prev.goal.clone(),
                tasks: prev.tasks.clone(),
                by: None,
            };
            board
                .commit(&mut st, ev)
                .map_err(|e| io::Error::other(e.to_string()))?;
        }
        Ok(board)
    }

    /// The journal path, if any.
    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, BoardState> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Applies, journals and broadcasts `ev` while the lock is held.
    fn commit(&self, st: &mut BoardState, ev: TaskEvent) -> Result<TaskEvent, TodoError> {
        if let Some(j) = st.journal.as_mut() {
            let mut line = serde_json::to_string(&ev).map_err(|e| TodoError::Io(e.to_string()))?;
            line.push('\n');
            j.write_all(line.as_bytes())
                .and_then(|()| j.flush())
                .map_err(|e| TodoError::Io(e.to_string()))?;
        }
        let BoardState {
            snapshot, next_id, ..
        } = st;
        apply_event(snapshot, next_id, &ev);
        let _ = self.tx.send(ev.clone());
        Ok(ev)
    }

    /// Live change events.
    pub fn subscribe(&self) -> broadcast::Receiver<TaskEvent> {
        self.tx.subscribe()
    }

    /// The current state.
    pub fn snapshot(&self) -> TaskSnapshot {
        self.lock().snapshot.clone()
    }

    /// One task by id.
    pub fn get(&self, id: &str) -> Option<Task> {
        self.lock()
            .snapshot
            .tasks
            .iter()
            .find(|t| t.id == id)
            .cloned()
    }

    /// Sets the goal line.
    pub fn set_goal(&self, goal: Option<String>) -> Result<TaskEvent, TodoError> {
        let mut st = self.lock();
        let ev = TaskEvent {
            version: st.snapshot.version + 1,
            change: TaskChange::Goal,
            task: None,
            goal,
            tasks: Vec::new(),
            by: None,
        };
        self.commit(&mut st, ev)
    }

    /// Adds `tasks` that do not exist yet (workflow `tasks:`); existing ids
    /// are left alone so a resumed workflow keeps its progress.
    pub fn seed(&self, tasks: impl IntoIterator<Item = Task>) -> Result<Vec<TaskEvent>, TodoError> {
        let mut st = self.lock();
        let mut out = Vec::new();
        for mut t in tasks {
            if st.snapshot.tasks.iter().any(|x| x.id == t.id) {
                continue;
            }
            t.version = st.snapshot.version + 1;
            let ev = TaskEvent {
                version: t.version,
                change: TaskChange::Added,
                task: Some(t),
                goal: None,
                tasks: Vec::new(),
                by: None,
            };
            out.push(self.commit(&mut st, ev)?);
        }
        Ok(out)
    }

    /// Copies a workflow step's status and owner onto every task linked to
    /// `step`. Returns the events of the tasks that changed.
    pub fn sync_step(
        &self,
        step: &str,
        status: TaskStatus,
        owner: Option<&NodeId>,
        agent: Option<&str>,
    ) -> Result<Vec<TaskEvent>, TodoError> {
        let mut st = self.lock();
        let linked: Vec<Task> = st
            .snapshot
            .tasks
            .iter()
            .filter(|t| t.step.as_deref() == Some(step))
            .cloned()
            .collect();
        let mut out = Vec::new();
        for mut t in linked {
            let new_owner = owner.cloned().or(t.owner.clone());
            let new_agent = agent.map(str::to_owned).or(t.agent.clone());
            if t.status == status && t.owner == new_owner && t.agent == new_agent {
                continue;
            }
            t.status = status;
            t.owner = new_owner;
            t.agent = new_agent;
            t.version = st.snapshot.version + 1;
            let ev = TaskEvent {
                version: t.version,
                change: TaskChange::Updated,
                task: Some(t),
                goal: None,
                tasks: Vec::new(),
                by: owner.cloned(),
            };
            out.push(self.commit(&mut st, ev)?);
        }
        Ok(out)
    }

    /// Applies an agent's change. `add` makes the caller the owner;
    /// `update` with `claim` transfers ownership to the caller.
    pub fn apply(&self, op: TaskOp, actor: &Actor) -> Result<TaskEvent, TodoError> {
        let mut st = self.lock();
        let version = st.snapshot.version + 1;
        let (change, task) = match op {
            TaskOp::Add { id, title, status } => {
                let title = title.trim().to_owned();
                if title.is_empty() {
                    return Err(TodoError::EmptyTitle);
                }
                let id = match id.filter(|s| !s.trim().is_empty()) {
                    Some(id) => {
                        if st.snapshot.tasks.iter().any(|t| t.id == id) {
                            return Err(TodoError::Exists(id));
                        }
                        id
                    }
                    None => loop {
                        let id = format!("t{}", st.next_id);
                        st.next_id += 1;
                        if !st.snapshot.tasks.iter().any(|t| t.id == id) {
                            break id;
                        }
                    },
                };
                let task = Task {
                    id,
                    title,
                    status: status.unwrap_or_default(),
                    owner: Some(actor.node.clone()),
                    agent: Some(actor.agent.clone()),
                    step: None,
                    version,
                };
                (TaskChange::Added, task)
            }
            TaskOp::Update {
                id,
                title,
                status,
                claim,
            } => {
                let Some(mut task) = st.snapshot.tasks.iter().find(|t| t.id == id).cloned() else {
                    let known: Vec<&str> =
                        st.snapshot.tasks.iter().map(|t| t.id.as_str()).collect();
                    return Err(TodoError::Unknown {
                        id,
                        known: if known.is_empty() {
                            "(none yet; use action=add)".into()
                        } else {
                            known.join(", ")
                        },
                    });
                };
                if let Some(title) = title {
                    let title = title.trim().to_owned();
                    if title.is_empty() {
                        return Err(TodoError::EmptyTitle);
                    }
                    task.title = title;
                }
                if let Some(s) = status {
                    task.status = s;
                }
                if claim || task.owner.is_none() {
                    task.owner = Some(actor.node.clone());
                    task.agent = Some(actor.agent.clone());
                }
                task.version = version;
                (TaskChange::Updated, task)
            }
        };
        let ev = TaskEvent {
            version,
            change,
            task: Some(task),
            goal: None,
            tasks: Vec::new(),
            by: Some(actor.node.clone()),
        };
        self.commit(&mut st, ev)
    }
}

/// The compact argument schema.
pub fn schema() -> Value {
    let statuses: Vec<&str> = TaskStatus::ALL.iter().map(|s| s.as_str()).collect();
    json!({
        "type": "object",
        "properties": {
            "action": {"enum": ["add", "update", "list"]},
            "id": {"type": "string"},
            "title": {"type": "string"},
            "status": {"enum": statuses},
            "claim": {"type": "boolean"}
        },
        "required": ["action"],
        "additionalProperties": false
    })
}

#[derive(Deserialize)]
struct Args {
    action: String,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    claim: bool,
}

/// The board as a `tasks` event (goal bar, replays).
pub fn tasks_event(snapshot: &TaskSnapshot) -> harness_events::schema::EventKind {
    harness_events::schema::EventKind::Tasks {
        goal: snapshot.goal.clone(),
        tasks: snapshot
            .tasks
            .iter()
            .map(|t| harness_events::schema::TaskItem {
                id: t.id.clone(),
                title: t.title.clone(),
                status: t.status.as_str().to_owned(),
                owner: t.owner.clone(),
                agent: t.agent.clone(),
            })
            .collect(),
        version: snapshot.version,
    }
}

/// Records every change of `board` to `recorder` as a `tasks` snapshot
/// event, starting with the current state when it has tasks or a goal.
/// The task ends when the board or the recorder closes.
pub fn record_tasks(
    board: &Arc<TaskBoard>,
    recorder: harness_events::store::RunRecorder,
) -> tokio::task::JoinHandle<()> {
    let mut rx = board.subscribe();
    let board = Arc::downgrade(board);
    let initial = board.upgrade().map(|b| b.snapshot());
    tokio::spawn(async move {
        let mut last: Option<u64> = None;
        // Snapshots are read when the change arrives, so several changes may
        // show up in one; never record the same version twice.
        let mut emit = |s: &TaskSnapshot| {
            if last.is_some_and(|v| s.version <= v) {
                return true;
            }
            last = Some(s.version);
            recorder
                .emit(harness_events::store::EventDraft::run(tasks_event(s)))
                .is_ok()
        };
        if let Some(s) = initial
            && (s.has_tasks() || s.goal.is_some())
            && !emit(&s)
        {
            return;
        }
        loop {
            match rx.recv().await {
                Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => {
                    let Some(b) = board.upgrade() else { return };
                    let snap = b.snapshot();
                    drop(b);
                    if !emit(&snap) {
                        return;
                    }
                }
                Err(broadcast::error::RecvError::Closed) => return,
            }
        }
    })
}

/// The `todo` tool bound to one agent node.
#[derive(Debug, Clone)]
pub struct TodoTool {
    board: Arc<TaskBoard>,
    actor: Actor,
}

impl TodoTool {
    /// The tool for `actor` on `board`.
    pub fn new(board: Arc<TaskBoard>, actor: Actor) -> Self {
        Self { board, actor }
    }

    /// The board.
    pub fn board(&self) -> &Arc<TaskBoard> {
        &self.board
    }

    fn list_text(&self) -> String {
        let snap = self.board.snapshot();
        if snap.tasks.is_empty() {
            return "No tasks yet.".into();
        }
        let (done, total) = snap.progress();
        let mut s = format!("{done}/{total} done\n");
        for t in &snap.tasks {
            s.push_str(&t.line());
            s.push('\n');
        }
        s.trim_end().to_owned()
    }
}

#[async_trait]
impl AgentTool for TodoTool {
    fn name(&self) -> &str {
        NAME
    }
    fn description(&self) -> &str {
        DESCRIPTION
    }
    fn schema(&self) -> Value {
        schema()
    }
    fn read_only(&self) -> bool {
        // Changes are serialized by the board itself.
        true
    }
    async fn call(&self, _ctx: &ToolContext, args: Value) -> Result<ToolOutput, ToolError> {
        let a: Args = serde_json::from_value(args).map_err(|e| {
            ToolError::invalid_args(format!(
                "bad todo arguments: {e}. Fix: pass action plus id/title/status."
            ))
        })?;
        let status = match a.status.as_deref() {
            None => None,
            Some(s) => Some(TaskStatus::parse(s).ok_or_else(|| {
                ToolError::invalid_args(format!(
                    "unknown status `{s}`. Fix: use pending, in_progress, blocked, done, failed or cancelled."
                ))
            })?),
        };
        let op = match a.action.as_str() {
            "list" => return Ok(ToolOutput::Text(self.list_text())),
            "add" => TaskOp::Add {
                id: a.id,
                title: a.title.unwrap_or_default(),
                status,
            },
            "update" => TaskOp::Update {
                id: a.id.ok_or_else(|| {
                    ToolError::invalid_args("update needs `id`. Fix: pass the task id from list.")
                })?,
                title: a.title,
                status,
                claim: a.claim,
            },
            other => {
                return Err(ToolError::invalid_args(format!(
                    "unknown action `{other}`. Fix: use add, update or list."
                )));
            }
        };
        let ev = self
            .board
            .apply(op, &self.actor)
            .map_err(|e| ToolError::invalid_args(e.to_string()))?;
        let task = ev.task.map(|t| t.line()).unwrap_or_default();
        Ok(ToolOutput::Text(format!("ok: {task}")))
    }
}

#[cfg(test)]
mod tests;
