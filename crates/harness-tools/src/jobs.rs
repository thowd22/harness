//! Background bash jobs (`bash` with `run_in_background`) and the
//! `bash_jobs` tool that lists, reads and stops them.
//!
//! A [`JobManager`] belongs to one session (one per [`crate::bash::BashTool`]).
//! [`JobManager::start`] spawns the command through the session's [`Exec`]
//! like any bash call, so it runs in its own process group (locally, in the
//! sandbox, or on a remote host through the helper or the shell fallback),
//! and returns at once. A driver task then:
//!
//! - writes the combined output to a log file in a per-session temporary
//!   directory (removed with the manager), up to [`JobLimits::output_bytes`];
//!   later output is counted and dropped;
//! - keeps the last [`TAIL_BYTES`] in memory for the exit notice;
//! - records the exit status and reports it through the starting call's
//!   event sink as [`ToolEvent::Job`].
//!
//! Finished jobs are handed out once by [`JobManager::take_finished`] (the
//! agent's notice hook turns them into a message on its next request).
//! [`JobManager::stop`] kills a job's whole group through the
//! [`ExecHandle`] contract (the local executor uses `killpg`, never a
//! negative pid). [`JobManager::shutdown`] stops every job and waits for
//! them; it runs when the session ends. Dropping the manager also asks every
//! job to stop.
//!
//! At most [`JobLimits::max_jobs`] jobs run at once.
//!
//! One manager serves the whole session: the top-level agent's bash, every
//! subagent's and every workflow agent step's bash (including on remote
//! hosts) share it through [`crate::bash::BashTool::with_jobs`], so job ids
//! are unique in the session and the UI lists and stops all of them. Each
//! bash tool holds a [`JobOwner`]; [`JobManager::take_finished_for`] hands
//! an agent the jobs it started, and the session agent also the jobs whose
//! owner is gone (a subagent that ended while its job ran). A
//! [`JobManager::scoped`] handle shares the jobs but shuts down only the
//! ones started through it (a workflow run inside a session).

use std::collections::{HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

use serde::Deserialize;
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt as _, AsyncSeekExt as _, AsyncWriteExt as _};
use tokio::sync::{Notify, oneshot};

use crate::config::ToolsConfig;
use crate::exec::{Exec, ExecError, ExecEvent, ExecHandle, ExecRequest, ExitOutcome};
use crate::spill::Spiller;
use crate::tool::{
    ToolContext, ToolError, ToolErrorKind, ToolEvent, ToolEventSink, ToolOutput, parse_args,
};

/// Name of the job tool.
pub const NAME: &str = "bash_jobs";

/// One-line description shown to the model.
pub const DESCRIPTION: &str = "Background bash jobs (bash run_in_background). list: all jobs. output: new output of job `id` since the last read. wait: block until job `id` ends (or `timeout` seconds, default 60) and return its exit status and new output; use it instead of sleep when you need the result. stop: kill job `id` and every process it started.";

/// Bytes of the newest output kept in memory per job, for the exit notice.
pub const TAIL_BYTES: usize = 4096;

/// Most bytes one `output` call reads from the log; older unread output is
/// skipped with a note (the log file keeps it).
pub const READ_MAX_BYTES: u64 = 1024 * 1024;

/// Finished jobs kept for `list` and `output`; older ones are forgotten
/// (and their logs removed) once they have been reported.
pub const KEEP_FINISHED: usize = 32;

/// Default and largest `timeout` of `bash_jobs wait`, in seconds.
pub const WAIT_DEFAULT_SECS: u64 = 60;
/// See [`WAIT_DEFAULT_SECS`].
pub const WAIT_MAX_SECS: u64 = 600;

/// How long [`JobManager::stop`] and [`JobManager::shutdown`] wait for a
/// killed job to report its exit (the executor's TERM→KILL grace is 2 s).
pub const STOP_WAIT: Duration = Duration::from_secs(10);

/// Limits of a session's background jobs (`[tools]` keys).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JobLimits {
    /// Jobs that may run at once (`bash_max_jobs`).
    pub max_jobs: usize,
    /// Bytes of output written to a job's log (`bash_job_output_bytes`).
    pub output_bytes: u64,
}

impl JobLimits {
    /// The limits in `config`.
    pub fn from_config(config: &ToolsConfig) -> Self {
        Self {
            max_jobs: config.bash_max_jobs,
            output_bytes: config.bash_job_output_bytes,
        }
    }
}

impl Default for JobLimits {
    fn default() -> Self {
        Self::from_config(&ToolsConfig::default())
    }
}

/// Where a job is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobStatus {
    /// Still running.
    Running,
    /// Exited on its own with this code.
    Exited(i32),
    /// Ended by this signal (not sent by harness).
    Signaled(i32),
    /// Its own `timeout` fired and the group was killed.
    TimedOut,
    /// Stopped by `bash_jobs stop`, the user or the end of the session.
    Stopped,
    /// Supervision was lost (for example a remote connection dropped).
    Lost,
}

impl JobStatus {
    /// True while the job runs.
    pub fn is_running(self) -> bool {
        self == JobStatus::Running
    }

    /// Short state name: `running`, `exited`, `failed` (non-zero exit),
    /// `killed` (signal), `timed_out`, `stopped` or `lost`.
    pub fn state(self) -> &'static str {
        match self {
            JobStatus::Running => "running",
            JobStatus::Exited(0) => "exited",
            JobStatus::Exited(_) => "failed",
            JobStatus::Signaled(_) => "killed",
            JobStatus::TimedOut => "timed_out",
            JobStatus::Stopped => "stopped",
            JobStatus::Lost => "lost",
        }
    }

    /// Exit code, when it exited.
    pub fn exit_code(self) -> Option<i32> {
        match self {
            JobStatus::Exited(c) => Some(c),
            _ => None,
        }
    }

    /// Human wording: `running`, `exit code 0`, `killed by signal 9`, ...
    pub fn describe(self) -> String {
        match self {
            JobStatus::Running => "running".into(),
            JobStatus::Exited(c) => format!("exit code {c}"),
            JobStatus::Signaled(s) => format!("killed by signal {s}"),
            JobStatus::TimedOut => "timed out (killed)".into(),
            JobStatus::Stopped => "stopped".into(),
            JobStatus::Lost => "lost (supervision ended without an exit status)".into(),
        }
    }

    fn from_outcome(outcome: Result<ExitOutcome, ExecError>) -> Self {
        match outcome {
            Err(_) => JobStatus::Lost,
            Ok(o) if o.timed_out => JobStatus::TimedOut,
            Ok(o) if o.killed => JobStatus::Stopped,
            Ok(ExitOutcome {
                signal: Some(s), ..
            }) => JobStatus::Signaled(s),
            Ok(ExitOutcome { code: Some(c), .. }) => JobStatus::Exited(c),
            Ok(_) => JobStatus::Lost,
        }
    }
}

/// A snapshot of one job.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobInfo {
    /// Job id (`j1`, `j2`, ...), unique in the session.
    pub id: String,
    /// The command line.
    pub command: String,
    /// The bash call that started it.
    pub call_id: String,
    /// Where it is.
    pub status: JobStatus,
    /// Run time so far, or until it ended.
    pub elapsed: Duration,
    /// The log file, when one could be created.
    pub output: Option<PathBuf>,
    /// Output bytes the job produced.
    pub bytes: u64,
    /// Bytes not written to the log because of the output limit.
    pub dropped: u64,
}

impl JobInfo {
    /// One line for `list` and the UI: `j1 running 3m12s · cargo test`.
    pub fn line(&self) -> String {
        format!(
            "{} {} {} · {}",
            self.id,
            self.status.describe(),
            fmt_duration(self.elapsed),
            one_line(&self.command, 80)
        )
    }
}

/// `3m12s`, `45s`, `1h02m`, `0.4s`.
pub fn fmt_duration(d: Duration) -> String {
    let s = d.as_secs();
    if s >= 3600 {
        format!("{}h{:02}m", s / 3600, (s % 3600) / 60)
    } else if s >= 60 {
        format!("{}m{:02}s", s / 60, s % 60)
    } else if s >= 10 {
        format!("{s}s")
    } else {
        format!("{:.1}s", d.as_secs_f64())
    }
}

fn one_line(s: &str, max: usize) -> String {
    let flat: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= max {
        flat
    } else {
        let cut: String = flat.chars().take(max.saturating_sub(1)).collect();
        format!("{cut}…")
    }
}

/// What [`JobManager::start`] returns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobStarted {
    /// Job id.
    pub id: String,
    /// The log file, when one could be created.
    pub output: Option<PathBuf>,
}

struct Job {
    id: String,
    command: String,
    call_id: String,
    started: Instant,
    ended: Option<Instant>,
    status: JobStatus,
    output: Option<PathBuf>,
    bytes: u64,
    dropped: u64,
    tail: VecDeque<u8>,
    read_offset: u64,
    reported: bool,
    kill: Option<oneshot::Sender<()>>,
    owner: Option<u64>,
    scope: u64,
}

impl Job {
    fn info(&self) -> JobInfo {
        let end = self.ended.unwrap_or_else(Instant::now);
        JobInfo {
            id: self.id.clone(),
            command: self.command.clone(),
            call_id: self.call_id.clone(),
            status: self.status,
            elapsed: end.saturating_duration_since(self.started),
            output: self.output.clone(),
            bytes: self.bytes,
            dropped: self.dropped,
        }
    }
}

struct Inner {
    limits: JobLimits,
    jobs: Mutex<Vec<Job>>,
    next: Mutex<u64>,
    dir: Mutex<Option<tempfile::TempDir>>,
    changed: Notify,
    /// Live [`JobOwner`]s.
    owners: Mutex<HashSet<u64>>,
    /// Source of owner and scope ids (scope 0 is the root handle).
    ids: AtomicU64,
}

impl Inner {
    fn lock(&self) -> std::sync::MutexGuard<'_, Vec<Job>> {
        self.jobs.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn owners(&self) -> std::sync::MutexGuard<'_, HashSet<u64>> {
        self.owners.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn with_job<R>(&self, id: &str, f: impl FnOnce(&mut Job) -> R) -> Option<R> {
        self.lock().iter_mut().find(|j| j.id == id).map(f)
    }

    /// The session's log directory, created on first use.
    fn log_dir(&self) -> Option<PathBuf> {
        let mut dir = self.dir.lock().unwrap_or_else(|e| e.into_inner());
        if dir.is_none() {
            match tempfile::Builder::new().prefix("harness-jobs-").tempdir() {
                Ok(d) => *dir = Some(d),
                Err(e) => {
                    tracing::warn!(target: "harness::tools", error = %e, "no log directory for background jobs");
                    return None;
                }
            }
        }
        dir.as_ref().map(|d| d.path().to_path_buf())
    }
}

/// The background jobs of one session (see the module docs).
#[derive(Clone)]
pub struct JobManager {
    inner: Arc<Inner>,
    /// 0 for the root handle; see [`JobManager::scoped`].
    scope: u64,
}

/// Who started a job: one per bash tool (see the module docs). Dropping it
/// marks the owner gone, so its ended jobs go to the session agent.
pub struct JobOwner {
    id: u64,
    inner: Weak<Inner>,
}

impl JobOwner {
    /// The owner id, unique in its manager.
    pub fn id(&self) -> u64 {
        self.id
    }
}

impl std::fmt::Debug for JobOwner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JobOwner").field("id", &self.id).finish()
    }
}

impl Drop for JobOwner {
    fn drop(&mut self) {
        if let Some(inner) = self.inner.upgrade() {
            inner.owners().remove(&self.id);
        }
    }
}

impl std::fmt::Debug for JobManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JobManager")
            .field("limits", &self.inner.limits)
            .field("jobs", &self.inner.lock().len())
            .finish()
    }
}

impl Default for JobManager {
    fn default() -> Self {
        Self::new(JobLimits::default())
    }
}

impl JobManager {
    /// A manager with `limits` and no jobs.
    pub fn new(limits: JobLimits) -> Self {
        Self {
            inner: Arc::new(Inner {
                limits,
                jobs: Mutex::new(Vec::new()),
                next: Mutex::new(0),
                dir: Mutex::new(None),
                changed: Notify::new(),
                owners: Mutex::new(HashSet::new()),
                ids: AtomicU64::new(1),
            }),
            scope: 0,
        }
    }

    /// A handle to the same jobs whose [`JobManager::shutdown`] stops only
    /// the jobs started through it (or its bash tools). Listing, reading
    /// and stopping by id still see every job of the session.
    pub fn scoped(&self) -> JobManager {
        JobManager {
            inner: self.inner.clone(),
            scope: self.inner.ids.fetch_add(1, Ordering::Relaxed),
        }
    }

    /// A new live owner (one per bash tool).
    pub fn owner(&self) -> JobOwner {
        let id = self.inner.ids.fetch_add(1, Ordering::Relaxed);
        self.inner.owners().insert(id);
        JobOwner {
            id,
            inner: Arc::downgrade(&self.inner),
        }
    }

    /// True when both handles manage the same jobs.
    pub fn same_jobs(&self, other: &JobManager) -> bool {
        Arc::ptr_eq(&self.inner, &other.inner)
    }

    /// The limits.
    pub fn limits(&self) -> JobLimits {
        self.inner.limits
    }

    /// Starts `req` (the shell running `command`) as a background job of
    /// the call in `ctx` and returns at once. The job has no owner (see
    /// [`JobManager::start_owned`]).
    pub async fn start(
        &self,
        ctx: &ToolContext,
        exec: &dyn Exec,
        req: ExecRequest,
        command: &str,
    ) -> Result<JobStarted, ToolError> {
        self.start_owned(None, ctx, exec, req, command).await
    }

    /// [`JobManager::start`] for `owner`: its ended job is handed out by
    /// [`JobManager::take_finished_for`] with that owner.
    pub async fn start_owned(
        &self,
        owner: Option<&JobOwner>,
        ctx: &ToolContext,
        exec: &dyn Exec,
        req: ExecRequest,
        command: &str,
    ) -> Result<JobStarted, ToolError> {
        let running = self
            .inner
            .lock()
            .iter()
            .filter(|j| j.status.is_running())
            .count();
        let max = self.inner.limits.max_jobs;
        if running >= max {
            return Err(ToolError::new(
                ToolErrorKind::Unsupported,
                format!(
                    "{running} background jobs are already running (limit {max}, [tools] bash_max_jobs). Stop one with bash_jobs (action: stop) or wait for one to finish, then start this again."
                ),
            ));
        }
        let handle = exec.spawn(req).await.map_err(|e| {
            ToolError::new(
                ToolErrorKind::Exec,
                format!("{e}. Check that bash is installed and the working directory exists."),
            )
        })?;
        let id = {
            let mut n = self.inner.next.lock().unwrap_or_else(|e| e.into_inner());
            *n += 1;
            format!("j{n}")
        };
        let path = self.inner.log_dir().map(|d| d.join(format!("{id}.log")));
        let log = match &path {
            Some(p) => match tokio::fs::File::create(p).await {
                Ok(f) => Some(f),
                Err(e) => {
                    tracing::warn!(target: "harness::tools", error = %e, path = %p.display(), "cannot create a job log");
                    None
                }
            },
            None => None,
        };
        let path = path.filter(|_| log.is_some());
        let (kill_tx, kill_rx) = oneshot::channel();
        let job = Job {
            id: id.clone(),
            command: command.to_owned(),
            call_id: ctx.call_id.clone(),
            started: Instant::now(),
            ended: None,
            status: JobStatus::Running,
            output: path.clone(),
            bytes: 0,
            dropped: 0,
            tail: VecDeque::new(),
            read_offset: 0,
            reported: false,
            kill: Some(kill_tx),
            owner: owner.map(JobOwner::id),
            scope: self.scope,
        };
        let info = job.info();
        {
            let mut jobs = self.inner.lock();
            jobs.push(job);
            prune(&mut jobs);
        }
        ctx.emit(ToolEvent::Job(info.clone()));
        tokio::spawn(drive(
            Arc::downgrade(&self.inner),
            info,
            handle,
            kill_rx,
            log,
            ctx.events.clone(),
        ));
        Ok(JobStarted { id, output: path })
    }

    /// Every known job, oldest first.
    pub fn list(&self) -> Vec<JobInfo> {
        self.inner.lock().iter().map(Job::info).collect()
    }

    /// One job.
    pub fn get(&self, id: &str) -> Option<JobInfo> {
        self.inner.with_job(id, |j| j.info())
    }

    /// Number of running jobs.
    pub fn running(&self) -> usize {
        self.inner
            .lock()
            .iter()
            .filter(|j| j.status.is_running())
            .count()
    }

    /// Jobs that ended since the last call, each returned once, with the
    /// last [`TAIL_BYTES`] of their output.
    pub fn take_finished(&self) -> Vec<(JobInfo, String)> {
        self.take_where(|_| true)
    }

    /// Like [`JobManager::take_finished`], for one agent: the jobs started
    /// by `owner`, and with `orphans` also those without a live owner (the
    /// session agent takes the jobs of subagents that have ended).
    pub fn take_finished_for(&self, owner: u64, orphans: bool) -> Vec<(JobInfo, String)> {
        let live = self.inner.owners().clone();
        self.take_where(|j| {
            j.owner == Some(owner) || (orphans && j.owner.is_none_or(|o| !live.contains(&o)))
        })
    }

    fn take_where(&self, mine: impl Fn(&Job) -> bool) -> Vec<(JobInfo, String)> {
        let mut jobs = self.inner.lock();
        let mut out = Vec::new();
        for j in jobs.iter_mut() {
            if !j.status.is_running() && !j.reported && mine(j) {
                j.reported = true;
                let tail: Vec<u8> = j.tail.iter().copied().collect();
                out.push((j.info(), String::from_utf8_lossy(&tail).into_owned()));
            }
        }
        prune(&mut jobs);
        out
    }

    /// The last [`TAIL_BYTES`] of a job's output (lossy UTF-8).
    pub fn tail(&self, id: &str) -> Option<String> {
        self.inner.with_job(id, |j| {
            let tail: Vec<u8> = j.tail.iter().copied().collect();
            String::from_utf8_lossy(&tail).into_owned()
        })
    }

    /// Output of `id` written since the last `read_new` (at most
    /// [`READ_MAX_BYTES`]; older unread output is skipped with a note), and
    /// the job. Without a log file, the in-memory tail.
    pub async fn read_new(&self, id: &str) -> Result<(JobInfo, String), ToolError> {
        let Some((info, offset)) = self.inner.with_job(id, |j| (j.info(), j.read_offset)) else {
            return Err(self.unknown(id));
        };
        let Some(path) = info.output.clone() else {
            let tail = self.tail(id).unwrap_or_default();
            return Ok((info, tail));
        };
        let (text, end) = read_from(&path, offset).await.map_err(|e| {
            ToolError::new(
                ToolErrorKind::Io,
                format!("cannot read the log of job {id}: {e}. Try bash_jobs output again."),
            )
        })?;
        self.inner.with_job(id, |j| j.read_offset = end);
        Ok((info, text))
    }

    /// The last `max_bytes` of a job's log for a viewer (the model's read
    /// position does not move). Without a log file, the in-memory tail.
    pub async fn log_tail(&self, id: &str, max_bytes: u64) -> Option<String> {
        let info = self.get(id)?;
        if let Some(path) = &info.output
            && let Ok(mut f) = tokio::fs::File::open(path).await
            && let Ok(meta) = f.metadata().await
        {
            let start = meta.len().saturating_sub(max_bytes);
            let mut buf = Vec::new();
            if f.seek(std::io::SeekFrom::Start(start)).await.is_ok()
                && f.read_to_end(&mut buf).await.is_ok()
            {
                return Some(String::from_utf8_lossy(&buf).into_owned());
            }
        }
        self.tail(id)
    }

    /// Kills job `id` (its whole process group) and waits for it to end.
    pub async fn stop(&self, id: &str) -> Result<JobInfo, ToolError> {
        let Some(kill) = self.inner.with_job(id, |j| j.kill.take()) else {
            return Err(self.unknown(id));
        };
        if let Some(tx) = kill {
            let _ = tx.send(());
        }
        self.wait_until(STOP_WAIT, |jobs| {
            jobs.iter()
                .find(|j| j.id == id)
                .is_none_or(|j| !j.status.is_running())
        })
        .await;
        self.get(id).ok_or_else(|| self.unknown(id))
    }

    /// Waits up to `limit` for job `id` to end. When it has ended, its exit
    /// is marked reported (the caller shows it), so no exit notice repeats it.
    pub async fn wait(&self, id: &str, limit: Duration) -> Result<JobInfo, ToolError> {
        if self.get(id).is_none() {
            return Err(self.unknown(id));
        }
        self.wait_until(limit, |jobs| {
            jobs.iter()
                .find(|j| j.id == id)
                .is_none_or(|j| !j.status.is_running())
        })
        .await;
        self.inner
            .with_job(id, |j| {
                if !j.status.is_running() {
                    j.reported = true;
                }
                j.info()
            })
            .ok_or_else(|| self.unknown(id))
    }

    /// Stops every running job and waits (up to [`STOP_WAIT`]) until all
    /// have ended. Called when the session ends. On a
    /// [`JobManager::scoped`] handle, only the jobs started through it.
    pub async fn shutdown(&self) {
        let scope = self.scope;
        let mine = move |j: &Job| scope == 0 || j.scope == scope;
        let kills: Vec<_> = self
            .inner
            .lock()
            .iter_mut()
            .filter(|j| mine(j))
            .filter_map(|j| j.kill.take())
            .collect();
        for tx in kills {
            let _ = tx.send(());
        }
        self.wait_until(STOP_WAIT, |jobs| {
            jobs.iter()
                .filter(|j| mine(j))
                .all(|j| !j.status.is_running())
        })
        .await;
    }

    async fn wait_until(&self, limit: Duration, done: impl Fn(&[Job]) -> bool) {
        let deadline = tokio::time::Instant::now() + limit;
        loop {
            let notified = self.inner.changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if done(&self.inner.lock()) {
                return;
            }
            if tokio::time::timeout_at(deadline, notified).await.is_err() {
                return;
            }
        }
    }

    fn unknown(&self, id: &str) -> ToolError {
        let ids: Vec<String> = self.inner.lock().iter().map(|j| j.id.clone()).collect();
        let known = if ids.is_empty() {
            "there are no background jobs".to_owned()
        } else {
            format!("known jobs: {}", ids.join(", "))
        };
        ToolError::new(
            ToolErrorKind::NotFound,
            format!(
                "no background job `{id}` ({known}). Use bash_jobs (action: list) to see them."
            ),
        )
    }
}

/// Forgets the oldest reported finished jobs beyond [`KEEP_FINISHED`].
fn prune(jobs: &mut Vec<Job>) {
    let mut finished = jobs
        .iter()
        .filter(|j| !j.status.is_running() && j.reported)
        .count();
    jobs.retain(|j| {
        if finished > KEEP_FINISHED && !j.status.is_running() && j.reported {
            finished -= 1;
            if let Some(p) = &j.output {
                let _ = std::fs::remove_file(p);
            }
            false
        } else {
            true
        }
    });
}

async fn read_from(path: &Path, offset: u64) -> std::io::Result<(String, u64)> {
    let mut f = tokio::fs::File::open(path).await?;
    let len = f.metadata().await?.len();
    let start = offset.max(len.saturating_sub(READ_MAX_BYTES)).min(len);
    f.seek(std::io::SeekFrom::Start(start)).await?;
    let mut buf = Vec::with_capacity((len - start) as usize);
    (&mut f).take(len - start).read_to_end(&mut buf).await?;
    let mut text = String::new();
    if start > offset {
        text.push_str(&format!(
            "[... {} earlier bytes skipped; the full log is {} ...]\n",
            start - offset,
            path.display()
        ));
    }
    text.push_str(&String::from_utf8_lossy(&buf));
    Ok((text, len))
}

/// Feeds one job's output into its log and tail until it exits, then
/// records the outcome. Holds only a weak reference: when the manager is
/// dropped the kill sender goes with it, which kills the job. The final
/// [`ToolEvent::Job`] is emitted either way (built from `start` when the
/// manager is gone), so every job's end is recorded.
async fn drive(
    inner: Weak<Inner>,
    start: JobInfo,
    mut handle: ExecHandle,
    mut kill_rx: oneshot::Receiver<()>,
    mut log: Option<tokio::fs::File>,
    events: Arc<dyn ToolEventSink>,
) {
    let id = start.id.clone();
    let started = Instant::now()
        .checked_sub(start.elapsed)
        .unwrap_or_else(Instant::now);
    let cap = inner.upgrade().map_or(0, |i| i.limits.output_bytes);
    let mut written: u64 = 0;
    let mut total: u64 = 0;
    let mut killed = false;
    loop {
        tokio::select! {
            event = handle.next_event() => match event {
                Some(ExecEvent::Output(chunk)) => {
                    let data = chunk.data;
                    let room = cap.saturating_sub(written) as usize;
                    let keep = data.len().min(room);
                    if let Some(f) = log.as_mut() && keep > 0 {
                        let ok = f.write_all(&data[..keep]).await.is_ok() && f.flush().await.is_ok();
                        if !ok {
                            log = None;
                        }
                    }
                    if keep < data.len() && written + (keep as u64) == cap
                        && let Some(f) = log.as_mut()
                    {
                        let note = format!(
                            "\n[harness: output limit of {cap} bytes reached; later output is dropped]\n"
                        );
                        let _ = f.write_all(note.as_bytes()).await;
                        let _ = f.flush().await;
                        log = None;
                    }
                    written += keep as u64;
                    total += data.len() as u64;
                    let Some(inner) = inner.upgrade() else {
                        if !killed {
                            killed = true;
                            handle.kill();
                        }
                        continue;
                    };
                    inner.with_job(&id, |j| {
                        j.bytes += data.len() as u64;
                        j.dropped += (data.len() - keep) as u64;
                        j.tail.extend(data.iter().copied());
                        let excess = j.tail.len().saturating_sub(TAIL_BYTES);
                        j.tail.drain(..excess);
                    });
                }
                Some(ExecEvent::Liveness(_)) => {}
                None => break,
            },
            // A stop request, or the manager (and the sender) is gone.
            _ = &mut kill_rx, if !killed => {
                killed = true;
                handle.kill();
            }
        }
    }
    let outcome = if killed {
        handle.wait().await
    } else {
        handle
            .wait_or_kill(async move {
                let _ = kill_rx.await;
            })
            .await
    };
    drop(log);
    let status = JobStatus::from_outcome(outcome);
    let info = match inner.upgrade() {
        Some(inner) => {
            let info = inner.with_job(&id, |j| {
                j.status = status;
                j.ended = Some(Instant::now());
                j.kill = None;
                j.info()
            });
            inner.changed.notify_waiters();
            info
        }
        None => None,
    };
    // The manager is gone (or forgot the job): report the end from what
    // the driver knows; the log directory went with the manager.
    let info = info.unwrap_or_else(|| JobInfo {
        status,
        elapsed: started.elapsed(),
        output: None,
        bytes: total,
        dropped: total - written,
        ..start
    });
    let call_id = info.call_id.clone();
    events.emit(&call_id, ToolEvent::Job(info));
}

/// Arguments of the `bash_jobs` tool.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JobsArgs {
    /// `list`, `output`, `wait` or `stop`.
    pub action: String,
    /// The job (`output`, `wait`, `stop`).
    #[serde(default)]
    pub id: Option<String>,
    /// `wait` only: seconds to wait at most (default
    /// [`WAIT_DEFAULT_SECS`], capped at [`WAIT_MAX_SECS`]).
    #[serde(default)]
    pub timeout: Option<u64>,
}

/// JSON schema of the `bash_jobs` arguments.
pub fn schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "action": {"type": "string", "enum": ["list", "output", "wait", "stop"]},
            "id": {"type": "string", "description": "Job id, e.g. j1"},
            "timeout": {"type": "integer", "minimum": 1, "description": "wait: seconds"}
        },
        "required": ["action"],
        "additionalProperties": false
    })
}

/// The `bash_jobs` tool over a session's [`JobManager`].
pub struct JobsTool {
    jobs: JobManager,
    spiller: Spiller,
}

impl std::fmt::Debug for JobsTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JobsTool")
            .field("jobs", &self.jobs)
            .finish_non_exhaustive()
    }
}

impl JobsTool {
    /// The tool for `jobs`; large output goes through `spiller`.
    pub fn new(jobs: JobManager, spiller: Spiller) -> Self {
        Self { jobs, spiller }
    }

    /// Run with raw JSON arguments.
    pub async fn call(&self, ctx: &ToolContext, args: Value) -> Result<ToolOutput, ToolError> {
        self.run(ctx, parse_args(NAME, args)?).await
    }

    /// Run with typed arguments.
    pub async fn run(&self, ctx: &ToolContext, args: JobsArgs) -> Result<ToolOutput, ToolError> {
        let need_id = || {
            args.id.clone().ok_or_else(|| {
                ToolError::invalid_args(format!(
                    "bash_jobs action `{}` needs an `id` (for example \"j1\"); use action list to see the ids.",
                    args.action
                ))
            })
        };
        let text = match args.action.as_str() {
            "list" => {
                let jobs = self.jobs.list();
                if jobs.is_empty() {
                    "[no background jobs]".to_owned()
                } else {
                    jobs.iter().map(list_line).collect::<Vec<_>>().join("\n")
                }
            }
            "output" => {
                let id = need_id()?;
                let (info, out) = self.jobs.read_new(&id).await?;
                let mut text = self.spiller.apply(ctx, NAME, out);
                if text.is_empty() {
                    text.push_str("[no new output]");
                }
                if !text.ends_with('\n') {
                    text.push('\n');
                }
                text.push_str(&format!(
                    "[job {} {} after {}]",
                    info.id,
                    info.status.describe(),
                    fmt_duration(info.elapsed)
                ));
                text
            }
            "wait" => {
                let id = need_id()?;
                let secs = args
                    .timeout
                    .unwrap_or(WAIT_DEFAULT_SECS)
                    .clamp(1, WAIT_MAX_SECS);
                let info = self.jobs.wait(&id, Duration::from_secs(secs)).await?;
                let (_, out) = self.jobs.read_new(&id).await?;
                let mut text = self.spiller.apply(ctx, NAME, out);
                if !text.is_empty() && !text.ends_with('\n') {
                    text.push('\n');
                }
                if info.status.is_running() {
                    text.push_str(&format!(
                        "[job {} still running after {}; wait again, read its output or stop it]",
                        info.id,
                        fmt_duration(info.elapsed)
                    ));
                } else {
                    text.push_str(&format!(
                        "[job {} {} after {}]",
                        info.id,
                        info.status.describe(),
                        fmt_duration(info.elapsed)
                    ));
                }
                text
            }
            "stop" => {
                let id = need_id()?;
                let before = self.jobs.get(&id).map(|j| j.status);
                let info = self.jobs.stop(&id).await?;
                if before.is_some_and(|s| !s.is_running()) {
                    format!(
                        "[job {} had already ended: {}]",
                        info.id,
                        info.status.describe()
                    )
                } else {
                    format!(
                        "[job {} {}: the command and every process it started were killed after {}]",
                        info.id,
                        info.status.describe(),
                        fmt_duration(info.elapsed)
                    )
                }
            }
            other => {
                return Err(ToolError::invalid_args(format!(
                    "unknown bash_jobs action `{other}`; use list, output, wait or stop."
                )));
            }
        };
        Ok(ToolOutput::Text(text))
    }
}

fn list_line(j: &JobInfo) -> String {
    let mut s = j.line();
    if let Some(p) = &j.output {
        s.push_str(&format!(" · log {}", p.display()));
    }
    s
}

#[cfg(test)]
mod tests;
