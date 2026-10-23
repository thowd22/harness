//! The stdio transport: a local subprocess speaking newline-delimited
//! JSON-RPC on stdin and stdout.
//!
//! - The server runs in its own session and process group (`setsid`), so
//!   it has no controlling terminal and the whole group can be signalled
//!   with `killpg`. Process ids 0 and 1 are never signalled, and `kill` is
//!   never run with a negative id.
//! - stderr is read line by line, redacted and written to the log (target
//!   `harness_mcp::stderr`), never to the terminal. The last lines are kept
//!   for error messages.
//! - Malformed stdout lines are logged and skipped; the connection stays up.
//! - Shutdown follows the spec: close stdin, wait, then SIGTERM and SIGKILL
//!   to the group. Dropping a transport SIGKILLs the group, and
//!   [`kill_all`] kills every live server group (for exit paths that skip
//!   destructors).
//! - A group is signalled only until its leader is reaped. Once reaped its
//!   id can be reused by an unrelated process, so later shutdowns and drops
//!   (for example of a connection that crashed hours ago) send nothing.
//! - The environment is scrubbed with the same policy as the bash tool
//!   ([`EnvScrubber`]): provider key names, `HARNESS_*`, configured
//!   credential variables and values containing a known credential are
//!   removed before the server's own `env.*` values are added.

use std::collections::{BTreeSet, HashMap, VecDeque};
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use harness_config::secrets::Secret;
use harness_events::redact::Redactor;
use harness_tools::bash::EnvScrubber;
use nix::sys::signal::{Signal, killpg};
use nix::unistd::Pid;
use serde_json::{Value, json};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::{broadcast, oneshot, watch};
use tokio::task::JoinHandle;

use crate::error::{McpError, Result};
use crate::protocol::{self, Incoming, Notification};

/// Longest stdout message accepted (bytes); longer lines are dropped.
pub const MAX_MESSAGE_BYTES: usize = 16 * 1024 * 1024;
/// Longest stderr line logged (bytes); the rest is cut.
const MAX_STDERR_LINE: usize = 4096;
/// stderr lines kept for error messages.
const STDERR_TAIL_LINES: usize = 20;

/// Process groups of running stdio servers.
static LIVE_GROUPS: Mutex<BTreeSet<i32>> = Mutex::new(BTreeSet::new());

/// SIGKILLs the process group of every running stdio MCP server. For exit
/// paths that do not run destructors (`std::process::exit`, signal
/// handlers); normal shutdown and drop already clean up.
pub fn kill_all() {
    let groups: Vec<i32> = LIVE_GROUPS
        .lock()
        .map(|g| g.iter().copied().collect())
        .unwrap_or_default();
    for pgid in groups {
        signal_group(pgid, Signal::SIGKILL);
    }
}

/// Number of stdio server process groups currently tracked as live.
pub fn live_groups() -> usize {
    LIVE_GROUPS.lock().map(|g| g.len()).unwrap_or(0)
}

/// Signals process group `pgid`. Ids 0 and 1 (the caller's own group and
/// init) are never signalled.
fn signal_group(pgid: i32, sig: Signal) {
    if pgid > 1 {
        #[cfg(test)]
        tests::record_signal(pgid, sig);
        // ESRCH just means the group is already gone.
        let _ = killpg(Pid::from_raw(pgid), sig);
    }
}

fn track(pgid: i32) {
    if pgid > 1
        && let Ok(mut g) = LIVE_GROUPS.lock()
    {
        g.insert(pgid);
    }
}

fn untrack(pgid: i32) {
    if let Ok(mut g) = LIVE_GROUPS.lock() {
        g.remove(&pgid);
    }
}

/// One server's process group. It may be signalled only while its leader
/// has not been reaped: until then the leader (alive or a zombie) holds the
/// id, so it cannot belong to anything else. The waiter marks it reaped
/// under the same lock it signals under, so no signal can follow the reap.
struct Group {
    pgid: i32,
    reaped: Mutex<bool>,
}

impl Group {
    fn new(pgid: i32) -> Self {
        track(pgid);
        Self {
            pgid,
            reaped: Mutex::new(false),
        }
    }

    /// Signals the group unless its leader was reaped. Returns whether a
    /// signal was sent.
    fn signal(&self, sig: Signal) -> bool {
        let reaped = self.reaped.lock().unwrap_or_else(|e| e.into_inner());
        if *reaped {
            return false;
        }
        signal_group(self.pgid, sig);
        true
    }

    /// Called right after the leader was reaped: kills what is left of the
    /// group (stray children keep the id reserved, so this reaches only
    /// them) and stops all further signals and tracking.
    fn reaped(&self) {
        let mut reaped = self.reaped.lock().unwrap_or_else(|e| e.into_inner());
        if !*reaped {
            signal_group(self.pgid, Signal::SIGKILL);
            *reaped = true;
            untrack(self.pgid);
        }
    }

    /// The transport is going away: kill the group if it is still ours.
    fn release(&self) {
        let reaped = self.reaped.lock().unwrap_or_else(|e| e.into_inner());
        if !*reaped {
            signal_group(self.pgid, Signal::SIGKILL);
            untrack(self.pgid);
        }
    }
}

/// Starts `cmd` in a new session, which also makes it the leader of a new
/// process group whose id is its pid.
#[allow(unsafe_code)]
fn new_session(cmd: &mut Command) {
    // SAFETY: the closure runs in the forked child before exec and only
    // calls setsid(2), which is async-signal-safe; it allocates nothing and
    // touches no locks.
    unsafe {
        cmd.pre_exec(|| {
            nix::unistd::setsid()
                .map(|_| ())
                .map_err(std::io::Error::from)
        });
    }
}

/// The last lines a server wrote to stderr (already redacted).
#[derive(Debug, Clone, Default)]
pub struct StderrTail(Arc<Mutex<VecDeque<String>>>);

impl StderrTail {
    fn push(&self, line: String) {
        if let Ok(mut t) = self.0.lock() {
            if t.len() == STDERR_TAIL_LINES {
                t.pop_front();
            }
            t.push_back(line);
        }
    }

    /// The kept lines, oldest first.
    pub fn lines(&self) -> Vec<String> {
        self.0
            .lock()
            .map(|t| t.iter().cloned().collect())
            .unwrap_or_default()
    }

    /// The last few lines joined for an error message, prefixed with
    /// `prefix`, or empty when there are none.
    pub fn summary(&self, prefix: &str) -> String {
        let lines = self.lines();
        let last: Vec<&str> = lines
            .iter()
            .rev()
            .take(3)
            .rev()
            .map(String::as_str)
            .collect();
        if last.is_empty() {
            String::new()
        } else {
            format!("{prefix}{}", last.join(" | "))
        }
    }
}

/// How to start a stdio server.
pub struct SpawnSpec {
    /// Server name (for logs and errors).
    pub server: String,
    /// Program.
    pub command: String,
    /// Arguments.
    pub args: Vec<String>,
    /// Working directory.
    pub cwd: Option<PathBuf>,
    /// Extra environment (resolved `env.*` values).
    pub env: Vec<(String, Secret)>,
    /// Variables removed from the inherited environment (other
    /// credentials such as provider `api_key_env` variables), on top of
    /// the bash tool's defaults ([`EnvScrubber`]).
    pub scrub_env: Vec<String>,
    /// Known credential values: inherited variables whose value contains
    /// one are removed too.
    pub scrub_values: Vec<String>,
    /// Redactor for stderr and logged messages.
    pub redactor: Arc<Redactor>,
    /// Where recent stderr lines are kept.
    pub stderr_tail: StderrTail,
}

type SharedStdin = Arc<tokio::sync::Mutex<Option<ChildStdin>>>;
type Pending = Arc<Mutex<HashMap<u64, (String, oneshot::Sender<Result<Value>>)>>>;

/// A running stdio server.
pub struct StdioTransport {
    server: String,
    group: Arc<Group>,
    stdin: SharedStdin,
    pending: Pending,
    next_id: AtomicU64,
    /// Set once stdout closed: why the connection is gone.
    dead: Arc<Mutex<Option<McpError>>>,
    /// Exit status description once the leader exited.
    exited: watch::Receiver<Option<String>>,
    /// Answer server-to-client requests with an error (legacy servers).
    legacy: Arc<AtomicBool>,
    stderr_tail: StderrTail,
    tasks: Vec<JoinHandle<()>>,
    shut_down: AtomicBool,
    notifications: broadcast::Sender<Notification>,
}

impl StdioTransport {
    /// Starts the server process.
    pub fn spawn(spec: SpawnSpec) -> Result<Self> {
        let mut cmd = Command::new(&spec.command);
        cmd.args(&spec.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(false);
        for var in scrubbed_vars(
            &EnvScrubber::new(
                spec.scrub_env.iter().cloned(),
                spec.scrub_values.iter().cloned(),
            ),
            std::env::vars_os(),
        ) {
            cmd.env_remove(var);
        }
        for (k, v) in &spec.env {
            cmd.env(k, v.expose());
        }
        if let Some(cwd) = &spec.cwd {
            cmd.current_dir(cwd);
        }
        new_session(&mut cmd);
        let mut child = cmd.spawn().map_err(|e| McpError::Spawn {
            server: spec.server.clone(),
            command: spec.command.clone(),
            message: e.to_string(),
        })?;
        let pgid = child.id().and_then(|p| i32::try_from(p).ok()).unwrap_or(0);
        let group = Arc::new(Group::new(pgid));
        let stdin = child.stdin.take();
        let stdout = child.stdout.take().expect("stdout is piped");
        let stderr = child.stderr.take().expect("stderr is piped");
        tracing::info!(server = %spec.server, command = %spec.command, pgid, "MCP server started");

        let pending: Pending = Arc::default();
        let dead: Arc<Mutex<Option<McpError>>> = Arc::default();
        let legacy = Arc::new(AtomicBool::new(false));
        let (exit_tx, exit_rx) = watch::channel(None);
        let stdin: SharedStdin = Arc::new(tokio::sync::Mutex::new(stdin));
        let (notifications, _) = broadcast::channel(protocol::NOTIFICATION_BUFFER);

        let (stderr_done_tx, stderr_done) = watch::channel(false);
        let stderr_task = tokio::spawn(read_stderr(
            BufReader::new(stderr),
            spec.server.clone(),
            spec.redactor.clone(),
            spec.stderr_tail.clone(),
            stderr_done_tx,
        ));
        let waiter = tokio::spawn(wait_child(
            child,
            group.clone(),
            spec.server.clone(),
            exit_tx,
        ));
        let reader = tokio::spawn(read_stdout(ReaderCtx {
            server: spec.server.clone(),
            stdout: BufReader::new(stdout),
            pending: pending.clone(),
            dead: dead.clone(),
            exited: exit_rx.clone(),
            stderr_done,
            legacy: legacy.clone(),
            redactor: spec.redactor,
            tail: spec.stderr_tail.clone(),
            stdin: stdin.clone(),
            notifications: notifications.clone(),
        }));
        Ok(Self {
            server: spec.server,
            group,
            stdin,
            pending,
            next_id: AtomicU64::new(1),
            dead,
            exited: exit_rx,
            legacy,
            stderr_tail: spec.stderr_tail,
            tasks: vec![stderr_task, waiter, reader],
            shut_down: AtomicBool::new(false),
            notifications,
        })
    }

    /// Notifications the server sends from now on.
    pub fn subscribe(&self) -> broadcast::Receiver<Notification> {
        self.notifications.subscribe()
    }

    /// Sends a long-lived request (`subscriptions/listen`) without waiting
    /// for its response, which only arrives when the server ends the
    /// subscription. Notifications on it reach [`subscribe`](Self::subscribe).
    pub async fn open_stream(&self, method: &str, params: Value) -> Result<()> {
        if let Some(e) = self.death() {
            return Err(e);
        }
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let (tx, _rx) = oneshot::channel();
        if let Ok(mut p) = self.pending.lock() {
            p.insert(id, (method.to_owned(), tx));
        }
        if let Err(e) = self
            .write_line(&protocol::request(id, method, params))
            .await
        {
            self.forget(id);
            return Err(e);
        }
        Ok(())
    }

    /// The server's process group id (its pid).
    pub fn pgid(&self) -> i32 {
        self.group.pgid
    }

    /// Answer server-to-client requests (legacy servers only); modern
    /// clients must not send responses.
    pub fn set_legacy(&self, legacy: bool) {
        self.legacy.store(legacy, Ordering::SeqCst);
    }

    /// True while the process is running and stdout is open.
    pub fn is_alive(&self) -> bool {
        !self.shut_down.load(Ordering::SeqCst)
            && self.dead.lock().map(|d| d.is_none()).unwrap_or(false)
    }

    /// Why the connection ended, if it did.
    pub fn death(&self) -> Option<McpError> {
        if self.shut_down.load(Ordering::SeqCst) {
            return Some(McpError::Closed(self.server.clone()));
        }
        self.dead.lock().ok().and_then(|d| d.clone())
    }

    /// The last stderr lines.
    pub fn stderr_tail(&self) -> &StderrTail {
        &self.stderr_tail
    }

    async fn write_line(&self, msg: &Value) -> Result<()> {
        let mut line = serde_json::to_string(msg).expect("JSON values serialize");
        line.push('\n');
        let mut guard = self.stdin.lock().await;
        let Some(stdin) = guard.as_mut() else {
            return Err(McpError::Closed(self.server.clone()));
        };
        let res = async {
            stdin.write_all(line.as_bytes()).await?;
            stdin.flush().await
        }
        .await;
        drop(guard);
        match res {
            Ok(()) => Ok(()),
            Err(_) => Err(self.wait_death().await),
        }
    }

    /// After a failed write: waits briefly for the reader to record why the
    /// server went away, so the error carries its exit status and stderr.
    async fn wait_death(&self) -> McpError {
        for _ in 0..100 {
            if let Some(e) = self.death() {
                return e;
            }
            tokio::time::sleep(Duration::from_millis(30)).await;
        }
        self.death_or_exited()
    }

    fn death_or_exited(&self) -> McpError {
        self.death().unwrap_or_else(|| McpError::Exited {
            server: self.server.clone(),
            status: "stdin closed".into(),
            stderr: self.stderr_tail.summary("; stderr: "),
        })
    }

    /// Sends a request and waits up to `limit` for its response. On timeout
    /// the request is cancelled with `notifications/cancelled`.
    pub async fn request(&self, method: &str, params: Value, limit: Duration) -> Result<Value> {
        if let Some(e) = self.death() {
            return Err(e);
        }
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = oneshot::channel();
        if let Ok(mut p) = self.pending.lock() {
            p.insert(id, (method.to_owned(), tx));
        }
        if let Err(e) = self
            .write_line(&protocol::request(id, method, params))
            .await
        {
            self.forget(id);
            return Err(e);
        }
        match tokio::time::timeout(limit, rx).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err(self.wait_death().await),
            Err(_) => {
                self.forget(id);
                let _ = self
                    .notify(
                        "notifications/cancelled",
                        json!({"requestId": id, "reason": "timed out"}),
                    )
                    .await;
                Err(McpError::Timeout {
                    server: self.server.clone(),
                    method: method.to_owned(),
                    limit,
                })
            }
        }
    }

    fn forget(&self, id: u64) {
        if let Ok(mut p) = self.pending.lock() {
            p.remove(&id);
        }
    }

    /// Sends a notification.
    pub async fn notify(&self, method: &str, params: Value) -> Result<()> {
        self.write_line(&protocol::notification(method, params))
            .await
    }

    /// Graceful shutdown: close stdin, wait `grace` for the server to exit,
    /// then SIGTERM the group, wait `grace` again, then SIGKILL it.
    pub async fn shutdown(&self, grace: Duration) {
        if self.shut_down.swap(true, Ordering::SeqCst) {
            return;
        }
        // Closing stdin is the spec's graceful shutdown signal.
        drop(self.stdin.lock().await.take());
        let mut exited = self.exited.clone();
        let wait = |rx: &mut watch::Receiver<Option<String>>| {
            let mut rx = rx.clone();
            async move {
                let _ = rx.wait_for(Option::is_some).await;
            }
        };
        if tokio::time::timeout(grace, wait(&mut exited))
            .await
            .is_err()
        {
            tracing::debug!(server = %self.server, "MCP server ignored stdin EOF; sending SIGTERM");
            self.group.signal(Signal::SIGTERM);
            self.group.signal(Signal::SIGCONT);
            if tokio::time::timeout(grace, wait(&mut exited))
                .await
                .is_err()
            {
                tracing::debug!(server = %self.server, "MCP server ignored SIGTERM; sending SIGKILL");
                self.group.signal(Signal::SIGKILL);
                let _ = tokio::time::timeout(grace, wait(&mut exited)).await;
            }
        }
        // If the leader is still unreaped, kill the group (the leader and
        // its children). Once reaped, the waiter already killed the
        // children, and the id may now belong to someone else.
        self.group.release();
        tracing::info!(server = %self.server, "MCP server stopped");
    }
}

impl Drop for StdioTransport {
    fn drop(&mut self) {
        // Never leave a server (or anything it started) running, and never
        // signal a group whose leader was already reaped.
        self.group.release();
        for t in &self.tasks {
            t.abort();
        }
    }
}

/// Waits for the leader to exit, then kills what is left of its group.
async fn wait_child(
    mut child: Child,
    group: Arc<Group>,
    server: String,
    exit_tx: watch::Sender<Option<String>>,
) {
    let status = match child.wait().await {
        Ok(s) => describe_status(s),
        Err(e) => format!("wait failed: {e}"),
    };
    // Stray background processes must not outlive the server. From here
    // on nothing signals the group (only the instant between the reap
    // inside `wait` and this lock is unguarded; pids are not reused that
    // fast).
    group.reaped();
    tracing::info!(server = %server, status = %status, "MCP server exited");
    let _ = exit_tx.send(Some(status));
}

/// Names of inherited variables `scrubber` removes (non-UTF-8 values are
/// checked lossily; non-UTF-8 names cannot match a policy name).
fn scrubbed_vars(
    scrubber: &EnvScrubber,
    vars: impl IntoIterator<Item = (std::ffi::OsString, std::ffi::OsString)>,
) -> Vec<std::ffi::OsString> {
    vars.into_iter()
        .filter(|(k, v)| scrubber.is_secret(&k.to_string_lossy(), &v.to_string_lossy()))
        .map(|(k, _)| k)
        .collect()
}

fn describe_status(s: std::process::ExitStatus) -> String {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(sig) = s.signal() {
            return format!("killed by signal {sig}");
        }
    }
    match s.code() {
        Some(c) => format!("exit code {c}"),
        None => "unknown status".into(),
    }
}

/// Result of reading one bounded line.
enum Line {
    Data(Vec<u8>),
    TooLong(usize),
    Eof,
}

/// Reads one `\n`-terminated line, keeping at most `max` bytes.
async fn read_bounded<R: AsyncBufRead + Unpin>(r: &mut R, max: usize) -> std::io::Result<Line> {
    let mut buf = Vec::new();
    let mut total = 0usize;
    loop {
        let chunk = r.fill_buf().await?;
        if chunk.is_empty() {
            return Ok(if total == 0 {
                Line::Eof
            } else if total > max {
                Line::TooLong(total)
            } else {
                Line::Data(buf)
            });
        }
        let (part, done) = match chunk.iter().position(|&b| b == b'\n') {
            Some(i) => (&chunk[..i], Some(i + 1)),
            None => (chunk, None),
        };
        total += part.len();
        if total <= max {
            buf.extend_from_slice(part);
        }
        let consumed = done.unwrap_or(chunk.len());
        r.consume(consumed);
        if done.is_some() {
            if total > max {
                return Ok(Line::TooLong(total));
            }
            if buf.last() == Some(&b'\r') {
                buf.pop();
            }
            return Ok(Line::Data(buf));
        }
    }
}

async fn read_stderr<R: AsyncBufRead + Unpin>(
    mut r: R,
    server: String,
    redactor: Arc<Redactor>,
    tail: StderrTail,
    done: watch::Sender<bool>,
) {
    let _done = DoneOnDrop(done);
    loop {
        match read_bounded(&mut r, MAX_STDERR_LINE).await {
            Ok(Line::Data(bytes)) => {
                let text = String::from_utf8_lossy(&bytes);
                let line = redactor.redact(text.trim_end()).into_owned();
                if line.is_empty() {
                    continue;
                }
                tracing::info!(target: "harness_mcp::stderr", server = %server, "{line}");
                tail.push(line);
            }
            Ok(Line::TooLong(n)) => {
                tracing::info!(target: "harness_mcp::stderr", server = %server, "[{n}-byte stderr line omitted]");
            }
            Ok(Line::Eof) | Err(_) => return,
        }
    }
}

/// Signals `true` when dropped (the stderr reader finished).
struct DoneOnDrop(watch::Sender<bool>);

impl Drop for DoneOnDrop {
    fn drop(&mut self) {
        let _ = self.0.send(true);
    }
}

struct ReaderCtx<R> {
    server: String,
    stdout: R,
    pending: Pending,
    dead: Arc<Mutex<Option<McpError>>>,
    exited: watch::Receiver<Option<String>>,
    stderr_done: watch::Receiver<bool>,
    legacy: Arc<AtomicBool>,
    redactor: Arc<Redactor>,
    tail: StderrTail,
    stdin: SharedStdin,
    notifications: broadcast::Sender<Notification>,
}

/// Short, redacted excerpt of an untrusted line for the log.
fn excerpt(redactor: &Redactor, bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    let cut: String = text.chars().take(200).collect();
    redactor.redact(&cut).into_owned()
}

async fn read_stdout<R: AsyncBufRead + Unpin>(mut ctx: ReaderCtx<R>) {
    loop {
        let line = match read_bounded(&mut ctx.stdout, MAX_MESSAGE_BYTES).await {
            Ok(Line::Data(b)) => b,
            Ok(Line::TooLong(n)) => {
                tracing::warn!(server = %ctx.server, bytes = n, "MCP server sent an oversized message; dropped");
                continue;
            }
            Ok(Line::Eof) | Err(_) => break,
        };
        if line.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        let value: Value = match serde_json::from_slice(&line) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(
                    server = %ctx.server,
                    error = %e,
                    line = %excerpt(&ctx.redactor, &line),
                    "MCP server wrote a line that is not JSON; ignored"
                );
                continue;
            }
        };
        match protocol::parse_incoming(&value) {
            Ok(Incoming::Response { id, outcome }) => {
                let Some(id) = id.as_u64() else {
                    tracing::warn!(server = %ctx.server, "MCP server sent a response with an unknown id; ignored");
                    continue;
                };
                let entry = ctx.pending.lock().ok().and_then(|mut p| p.remove(&id));
                match entry {
                    Some((method, tx)) => {
                        let result = outcome.map_err(|error| McpError::Rpc {
                            server: ctx.server.clone(),
                            method,
                            error,
                        });
                        let _ = tx.send(result);
                    }
                    None => {
                        tracing::debug!(server = %ctx.server, id, "response to a cancelled or unknown request; ignored")
                    }
                }
            }
            Ok(Incoming::Notification { method, params }) => {
                let text = excerpt(&ctx.redactor, params.to_string().as_bytes());
                tracing::debug!(server = %ctx.server, method = %method, params = %text, "MCP notification");
                let _ = ctx.notifications.send(Notification { method, params });
            }
            Ok(Incoming::Request { id, method }) => {
                tracing::debug!(server = %ctx.server, method = %method, "MCP server request; not supported");
                if ctx.legacy.load(Ordering::SeqCst) {
                    let reply = json!({"jsonrpc": "2.0", "id": id, "error": {
                        "code": protocol::METHOD_NOT_FOUND,
                        "message": format!("harness does not support `{method}`"),
                    }});
                    if let Some(stdin) = ctx.stdin.lock().await.as_mut() {
                        let _ = stdin.write_all(format!("{reply}\n").as_bytes()).await;
                        let _ = stdin.flush().await;
                    }
                }
            }
            Err(why) => {
                tracing::warn!(
                    server = %ctx.server,
                    why = %why,
                    line = %excerpt(&ctx.redactor, &line),
                    "MCP server sent an invalid JSON-RPC message; ignored"
                );
            }
        }
    }
    // stdout closed: the server is gone (or going). Give the waiter a
    // moment to report the exit status.
    let mut exited = ctx.exited.clone();
    let status = match tokio::time::timeout(
        Duration::from_secs(2),
        exited.wait_for(Option::is_some),
    )
    .await
    {
        Ok(Ok(s)) => s.clone().unwrap_or_default(),
        _ => "closed its output".into(),
    };
    // Let the last stderr lines (often the reason) arrive first.
    let mut done = ctx.stderr_done.clone();
    let _ = tokio::time::timeout(Duration::from_secs(1), done.wait_for(|d| *d)).await;
    let err = McpError::Exited {
        server: ctx.server.clone(),
        status,
        stderr: ctx.tail.summary("; stderr: "),
    };
    if let Ok(mut d) = ctx.dead.lock() {
        *d = Some(err.clone());
    }
    let waiting: Vec<_> = ctx
        .pending
        .lock()
        .map(|mut p| p.drain().collect())
        .unwrap_or_default();
    for (_, (_, tx)) in waiting {
        let _ = tx.send(Err(err.clone()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every (pgid, signal) sent by this test binary.
    static SENT: Mutex<Vec<(i32, Signal)>> = Mutex::new(Vec::new());

    pub(super) fn record_signal(pgid: i32, sig: Signal) {
        if let Ok(mut s) = SENT.lock() {
            s.push((pgid, sig));
        }
    }

    fn sent_to(pgid: i32) -> usize {
        SENT.lock()
            .map(|s| s.iter().filter(|(p, _)| *p == pgid).count())
            .unwrap_or(0)
    }

    fn tracked(pgid: i32) -> bool {
        LIVE_GROUPS.lock().map(|g| g.contains(&pgid)).unwrap()
    }

    fn spec(script: &str) -> SpawnSpec {
        SpawnSpec {
            server: "t".into(),
            command: "sh".into(),
            args: vec!["-c".into(), script.into()],
            cwd: None,
            env: Vec::new(),
            scrub_env: Vec::new(),
            scrub_values: Vec::new(),
            redactor: Arc::new(Redactor::new(std::iter::empty::<&str>())),
            stderr_tail: StderrTail::default(),
        }
    }

    async fn wait_exited(t: &StdioTransport) {
        let mut rx = t.exited.clone();
        tokio::time::timeout(Duration::from_secs(15), rx.wait_for(Option::is_some))
            .await
            .expect("server exits")
            .unwrap();
    }

    #[tokio::test]
    async fn no_signal_after_the_leader_was_reaped() {
        // A server that exits on its own (a crash): the waiter reaps it and
        // kills the rest of its group once.
        let t = StdioTransport::spawn(spec("exit 3")).unwrap();
        let pgid = t.pgid();
        assert!(pgid > 1);
        wait_exited(&t).await;
        assert!(!tracked(pgid), "a reaped group is no longer tracked");
        let after_reap = sent_to(pgid);
        assert_eq!(after_reap, 1, "the waiter kills leftover children once");
        // Shutdown and drop, possibly much later when the pid may belong
        // to an unrelated process group, send nothing.
        t.shutdown(Duration::from_millis(50)).await;
        drop(t);
        assert_eq!(sent_to(pgid), after_reap);
    }

    #[tokio::test]
    async fn graceful_shutdown_then_drop_signals_nothing_extra() {
        // `exec cat` exits on stdin EOF, so shutdown needs no signal; the
        // only one is the waiter's cleanup right after the reap.
        let t = StdioTransport::spawn(spec("exec cat")).unwrap();
        let pgid = t.pgid();
        t.shutdown(Duration::from_secs(10)).await;
        wait_exited(&t).await;
        drop(t);
        assert_eq!(sent_to(pgid), 1);
        assert!(!tracked(pgid));
    }

    #[tokio::test]
    async fn dropping_a_running_server_kills_its_group() {
        let t = StdioTransport::spawn(spec("exec sleep 600")).unwrap();
        let pgid = t.pgid();
        assert!(tracked(pgid));
        drop(t);
        assert_eq!(sent_to(pgid), 1);
        assert!(!tracked(pgid));
    }

    #[test]
    fn environment_uses_the_bash_scrub_policy() {
        let scrubber =
            EnvScrubber::new(["MY_KEY_VAR".to_owned()], ["sk-resolved-123456".to_owned()]);
        let vars = [
            ("ANTHROPIC_API_KEY", "a"),
            ("OPENAI_API_KEY", "b"),
            ("HARNESS_CONFIG", "/x"),
            ("MY_KEY_VAR", "c"),
            ("COPY_OF_KEY", "Bearer sk-resolved-123456"),
            ("PATH", "/usr/bin"),
            ("HOME", "/home/u"),
        ]
        .map(|(k, v)| (k.into(), v.into()));
        let removed: Vec<String> = scrubbed_vars(&scrubber, vars)
            .into_iter()
            .map(|k| k.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            removed,
            [
                "ANTHROPIC_API_KEY",
                "OPENAI_API_KEY",
                "HARNESS_CONFIG",
                "MY_KEY_VAR",
                "COPY_OF_KEY"
            ]
        );
    }

    #[tokio::test]
    async fn bounded_lines() {
        let data: &[u8] = b"short\r\n0123456789abc\nlast";
        let mut r = BufReader::with_capacity(4, data);
        assert!(matches!(read_bounded(&mut r, 10).await.unwrap(), Line::Data(b) if b == b"short"));
        assert!(matches!(
            read_bounded(&mut r, 10).await.unwrap(),
            Line::TooLong(13)
        ));
        assert!(matches!(read_bounded(&mut r, 10).await.unwrap(), Line::Data(b) if b == b"last"));
        assert!(matches!(read_bounded(&mut r, 10).await.unwrap(), Line::Eof));
    }

    #[test]
    fn group_zero_and_one_are_never_signalled() {
        // Would kill our own group (0) or init (1) if not guarded.
        signal_group(0, Signal::SIGCONT);
        signal_group(1, Signal::SIGCONT);
        signal_group(-5, Signal::SIGCONT);
    }
}
