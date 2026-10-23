//! The remote helper (`harness remote-agent --stdio`).
//!
//! The helper runs on the remote host and serves the [`crate::protocol`] over
//! stdin/stdout (the SSH channel). File operations and processes use the
//! same local implementations as the harness itself
//! ([`harness_tools::local`]), so remote tools behave exactly like local ones.
//!
//! **Reattach.** Jobs outlive connections. Each helper owns a session id
//! (`--session`) and listens on a private Unix socket
//! `<socket-dir>/helper-<session>.sock`. When the SSH connection drops, the
//! helper keeps its jobs running and buffers their output. A later
//! `remote-agent --stdio --session <same id>` finds the socket and simply
//! proxies to the running helper, and the client resubscribes with
//! `exec.attach`. If no client returns within the grace period
//! (`--grace`, default [`DEFAULT_GRACE`]), the helper kills every job's
//! process group and exits, so a lost connection never leaves orphans.
//!
//! **Content cache.** The helper keeps a bounded per-session cache of file
//! contents keyed by content hash ([`crate::cache`]), filled by reads and
//! writes and kept across reattaches. A read whose file still has the
//! client's `have` hash is answered with the hash only; a write marked
//! `cached` is served from the cache or refused with `CACHE_MISS` so the
//! client resends the bytes.

use std::collections::{HashMap, VecDeque};
use std::future::Future;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use harness_tools::exec::{Exec, ExecError, ExecEvent, ExecRequest, ExitOutcome, Liveness};
use harness_tools::liveness::LIVENESS_INTERVAL;
use harness_tools::local::{LocalExec, LocalWorkspace};
use harness_tools::workspace::Workspace;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use tokio::io::{AsyncRead, AsyncWrite, BufReader};
use tokio::sync::{mpsc, oneshot};

use crate::cache::{ContentCache, DEFAULT_CONTENT_CACHE, content_hash};
use crate::protocol::{
    AttachParams, AttachResult, ChangesParams, ExecExitNote, ExecLivenessNote, ExecOutputNote,
    ExecStartParams, GlobParams, GlobResult, HELPER_VERSION, HelloParams, HelloResult, JobInfo,
    JobParams, JobsResult, ListResult, MAX_FRAME, MIN_PROTOCOL_VERSION, Message, PROTOCOL_VERSION,
    PathParams, ProbeParams, PruneParams, PruneResult, ReadParams, RemoveParams, RootParams,
    RpcError, SetModeParams, WireEntry, WireStat, WireStream, WriteParams, ZSTD, codes, methods,
};
use crate::wire::{Encoder, Frame, Framing, Outbound, Zstd, read_wire_frame, write_loop};

/// How long jobs keep running without any connected client.
pub const DEFAULT_GRACE: Duration = Duration::from_secs(120);
/// Output buffered per job for replay after a reattach.
pub const DEFAULT_JOB_BUFFER: usize = 16 * 1024 * 1024;

/// Helper settings.
#[derive(Debug, Clone)]
pub struct HelperOptions {
    /// Session id (stable for one client across reconnects).
    pub session: String,
    /// Version reported in `hello` (normally [`HELPER_VERSION`]).
    pub version: String,
    /// See [`DEFAULT_GRACE`].
    pub grace: Duration,
    /// See [`DEFAULT_JOB_BUFFER`].
    pub max_job_buffer: usize,
    /// Directory for the reattach socket (default [`default_socket_dir`]).
    pub socket_dir: Option<PathBuf>,
    /// Newest protocol version to negotiate (default [`PROTOCOL_VERSION`];
    /// 1 behaves like a protocol 1 helper, for compatibility tests).
    pub max_protocol: u32,
    /// Accept the client's zstd compression offer (default true).
    pub compression: bool,
    /// Bytes of the per-session content-hash cache (default
    /// [`DEFAULT_CONTENT_CACHE`]; 0 disables it, so reads and writes always
    /// carry the bytes).
    pub content_cache: usize,
    /// How often running jobs' process groups are sampled for liveness
    /// (default [`LIVENESS_INTERVAL`]; `None` sends no `exec.liveness`).
    pub liveness_interval: Option<Duration>,
}

impl Default for HelperOptions {
    fn default() -> Self {
        Self {
            session: uuid::Uuid::now_v7().simple().to_string(),
            version: HELPER_VERSION.to_owned(),
            grace: DEFAULT_GRACE,
            max_job_buffer: DEFAULT_JOB_BUFFER,
            socket_dir: None,
            max_protocol: PROTOCOL_VERSION,
            compression: true,
            content_cache: DEFAULT_CONTENT_CACHE,
            liveness_interval: Some(LIVENESS_INTERVAL),
        }
    }
}

/// `$XDG_RUNTIME_DIR/harness`, else `~/.cache/harness/run`.
pub fn default_socket_dir() -> PathBuf {
    if let Some(d) = std::env::var_os("XDG_RUNTIME_DIR").filter(|d| !d.is_empty()) {
        return PathBuf::from(d).join("harness");
    }
    home().join(".cache/harness/run")
}

fn home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"))
}

/// The reattach socket path for `session`.
pub fn socket_path(dir: &Path, session: &str) -> PathBuf {
    let clean: String = session
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .take(64)
        .collect();
    dir.join(format!("helper-{clean}.sock"))
}

/// Expand a leading `~` with `$HOME` and require an absolute path.
pub fn helper_path(p: &str) -> Result<PathBuf, RpcError> {
    let path = if p == "~" {
        home()
    } else if let Some(rest) = p.strip_prefix("~/") {
        home().join(rest)
    } else {
        PathBuf::from(p)
    };
    if path.is_absolute() {
        Ok(path)
    } else {
        Err(RpcError::invalid_params(format!(
            "path {p:?} must be absolute"
        )))
    }
}

type Tx = mpsc::UnboundedSender<Outbound>;

#[derive(Debug)]
struct Sub {
    conn: u64,
    tx: Tx,
}

#[derive(Debug)]
struct JobState {
    log: VecDeque<(u64, Frame)>,
    bytes: usize,
    next_seq: u64,
    dropped_bytes: u64,
    running: bool,
    sub: Option<Sub>,
    kill: Option<oneshot::Sender<()>>,
}

#[derive(Debug)]
struct Job {
    program: String,
    state: Mutex<JobState>,
}

impl Job {
    fn lock(&self) -> std::sync::MutexGuard<'_, JobState> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }
}

#[derive(Debug)]
struct Inner {
    opts: HelperOptions,
    jobs: Mutex<HashMap<String, Arc<Job>>>,
    conns: AtomicUsize,
    next_conn: AtomicU64,
    exec: LocalExec,
    cache: Mutex<ContentCache>,
}

/// A helper instance: the job table plus connection handling. Cheap to clone.
#[derive(Debug, Clone)]
pub struct Helper {
    inner: Arc<Inner>,
}

/// Decrements the connection count and detaches the connection's jobs when
/// a connection ends, even if its task is aborted.
struct ConnGuard {
    helper: Helper,
    id: u64,
}

impl Drop for ConnGuard {
    fn drop(&mut self) {
        for job in self.helper.jobs_snapshot() {
            let mut st = job.lock();
            if st.sub.as_ref().is_some_and(|s| s.conn == self.id) {
                st.sub = None;
            }
        }
        self.helper.inner.conns.fetch_sub(1, Ordering::SeqCst);
    }
}

impl Helper {
    /// A helper with no jobs.
    pub fn new(opts: HelperOptions) -> Self {
        let cache = Mutex::new(ContentCache::new(opts.content_cache));
        let exec = LocalExec::new().with_liveness_interval(opts.liveness_interval);
        Self {
            inner: Arc::new(Inner {
                cache,
                opts,
                jobs: Mutex::new(HashMap::new()),
                conns: AtomicUsize::new(0),
                next_conn: AtomicU64::new(1),
                exec,
            }),
        }
    }

    /// The options.
    pub fn options(&self) -> &HelperOptions {
        &self.inner.opts
    }

    /// Connected clients.
    pub fn connections(&self) -> usize {
        self.inner.conns.load(Ordering::SeqCst)
    }

    /// Jobs still running.
    pub fn running_jobs(&self) -> usize {
        self.jobs_snapshot()
            .iter()
            .filter(|j| j.lock().running)
            .count()
    }

    fn jobs_snapshot(&self) -> Vec<Arc<Job>> {
        self.inner
            .jobs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .values()
            .cloned()
            .collect()
    }

    fn job(&self, id: &str) -> Result<Arc<Job>, RpcError> {
        self.inner
            .jobs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(id)
            .cloned()
            .ok_or_else(|| RpcError::new(codes::UNKNOWN_JOB, format!("unknown job {id}")))
    }

    /// Serve one connection until it closes. The connection is counted as
    /// soon as this is called (before the future is first polled).
    pub fn serve<R, W>(&self, reader: R, writer: W) -> impl Future<Output = ()> + Send + 'static
    where
        R: AsyncRead + Send + Unpin + 'static,
        W: AsyncWrite + Send + Unpin + 'static,
    {
        self.inner.conns.fetch_add(1, Ordering::SeqCst);
        let guard = ConnGuard {
            helper: self.clone(),
            id: self.inner.next_conn.fetch_add(1, Ordering::SeqCst),
        };
        let this = self.clone();
        async move { this.serve_inner(guard, reader, writer).await }
    }

    async fn serve_inner<R, W>(&self, guard: ConnGuard, reader: R, mut writer: W)
    where
        R: AsyncRead + Send + Unpin + 'static,
        W: AsyncWrite + Send + Unpin + 'static,
    {
        let (tx, rx) = mpsc::unbounded_channel::<Outbound>();
        let writer_task = tokio::spawn(async move {
            write_loop(&mut writer, rx).await;
        });
        let mut reader = BufReader::new(reader);
        loop {
            let frame = match read_wire_frame(&mut reader, MAX_FRAME).await {
                Ok(Some(f)) => f,
                Ok(None) => break,
                Err(e) => {
                    tracing::warn!(target: "harness::remote", "closing connection: {e}");
                    break;
                }
            };
            match Message::parse(&frame.json) {
                Ok(Message::Request { id, method, params }) if method == methods::HELLO => {
                    // Answered inline: the framing switch must follow the
                    // answer and precede anything else on this connection.
                    let (result, encoder) = match self.hello(params) {
                        Ok((r, e)) => (to_value(r), Some(e)),
                        Err(e) => (Err(e), None),
                    };
                    let _ = tx.send(Frame::new(Message::Response { id, result }).into());
                    if let Some(e) = encoder {
                        let _ = tx.send(Outbound::Switch(e));
                    }
                }
                Ok(Message::Request { id, method, params }) => {
                    let this = self.clone();
                    let tx = tx.clone();
                    let conn = guard.id;
                    let data = frame.data;
                    tokio::spawn(async move {
                        let (result, data) =
                            match this.handle(conn, &tx, &method, params, data).await {
                                Ok((v, d)) => (Ok(v), d),
                                Err(e) => (Err(e), None),
                            };
                        let message = Message::Response { id, result };
                        let _ = tx.send(Outbound::Frame(Frame { message, data }));
                    });
                }
                Ok(Message::Notification { method, params }) => {
                    if method == methods::EXEC_RELEASE
                        && let Ok(p) = serde_json::from_value::<JobParams>(params)
                    {
                        self.release(&p.job);
                    }
                }
                Ok(Message::Response { .. }) => {}
                Err(e) => tracing::warn!(target: "harness::remote", "bad frame: {e}"),
            }
        }
        drop(guard);
        drop(tx);
        // Responses still being computed hold senders; the writer ends with them.
        let _ = writer_task.await;
    }

    /// Wait until no client is connected and no job runs. Jobs left without
    /// a client for the grace period are killed first.
    pub async fn wait_idle(&self) {
        let tick = Duration::from_millis(50);
        loop {
            if self.connections() == 0 {
                if self.running_jobs() == 0 {
                    return;
                }
                let deadline = tokio::time::Instant::now() + self.inner.opts.grace;
                while self.connections() == 0
                    && self.running_jobs() > 0
                    && tokio::time::Instant::now() < deadline
                {
                    tokio::time::sleep(tick).await;
                }
                if self.connections() == 0 && self.running_jobs() > 0 {
                    tracing::warn!(target: "harness::remote", "no client reattached; killing jobs");
                    self.kill_all();
                    let stop = tokio::time::Instant::now() + Duration::from_secs(10);
                    while self.running_jobs() > 0 && tokio::time::Instant::now() < stop {
                        tokio::time::sleep(tick).await;
                    }
                    return;
                }
                continue;
            }
            tokio::time::sleep(tick).await;
        }
    }

    /// Kill every running job's process group.
    pub fn kill_all(&self) {
        for job in self.jobs_snapshot() {
            if let Some(k) = job.lock().kill.take() {
                let _ = k.send(());
            }
        }
    }

    fn release(&self, job: &str) {
        let mut jobs = self.inner.jobs.lock().unwrap_or_else(|e| e.into_inner());
        if jobs.get(job).is_some_and(|j| !j.lock().running) {
            jobs.remove(job);
        }
    }

    /// Negotiate protocol and compression for a `hello`: the answer and
    /// the encoder for the rest of the connection.
    fn hello(&self, params: Value) -> Result<(HelloResult, Encoder), RpcError> {
        let p: HelloParams = parse(params)?;
        let ours = self
            .inner
            .opts
            .max_protocol
            .clamp(MIN_PROTOCOL_VERSION, PROTOCOL_VERSION);
        let client_max = p.max_protocol.unwrap_or(p.protocol).max(p.protocol);
        let protocol = client_max.min(ours);
        if protocol < p.protocol || protocol < MIN_PROTOCOL_VERSION {
            return Err(RpcError {
                code: codes::VERSION_MISMATCH,
                message: format!(
                    "helper speaks protocol {MIN_PROTOCOL_VERSION} to {ours}, client {} to {client_max}",
                    p.protocol
                ),
                data: Some(json!({"protocol": ours, "version": self.inner.opts.version})),
            });
        }
        let offer = p
            .compression
            .filter(|_| protocol >= 2 && self.inner.opts.compression)
            .filter(|o| o.algorithms.iter().any(|a| a == ZSTD));
        let framing = if protocol >= 2 {
            Framing::Binary {
                zstd: offer.as_ref().map(|o| Zstd {
                    level: o.level.clamp(1, 19),
                    min_size: o.min_size,
                }),
            }
        } else {
            Framing::Lines
        };
        let result = HelloResult {
            protocol,
            version: self.inner.opts.version.clone(),
            arch: std::env::consts::ARCH.into(),
            os: std::env::consts::OS.into(),
            session: self.inner.opts.session.clone(),
            pid: std::process::id(),
            jobs: self.running_jobs(),
            home: std::env::var_os("HOME").map(|h| h.to_string_lossy().into_owned()),
            compression: offer.map(|_| ZSTD.to_owned()),
            content_cache: (self.inner.opts.content_cache > 0)
                .then_some(self.inner.opts.content_cache as u64),
        };
        Ok((result, Encoder::new(framing)))
    }

    /// Handle one request: the result and its binary payload. `data` is
    /// the request's protocol 2 payload.
    async fn handle(
        &self,
        conn: u64,
        tx: &Tx,
        method: &str,
        params: Value,
        data: Option<Vec<u8>>,
    ) -> Result<(Value, Option<Vec<u8>>), RpcError> {
        match method {
            methods::FS_READ => {
                let p: ReadParams = parse(params)?;
                let path = helper_path(&p.path)?;
                let data = ws().read(&path).await.map_err(|e| RpcError::from_ws(&e))?;
                if self.cache_disabled() {
                    return Ok((json!({}), Some(data)));
                }
                // The file is always read: the hash says whether the
                // client's copy is still current.
                let hash = content_hash(&data);
                let cached = p.have.as_deref() == Some(hash.as_str());
                self.remember(&hash, data.clone());
                if cached {
                    Ok((json!({"hash": hash, "cached": true}), None))
                } else {
                    Ok((json!({ "hash": hash }), Some(data)))
                }
            }
            methods::FS_WRITE | methods::FS_REPLACE => {
                let p: WriteParams = parse(params)?;
                let path = helper_path(&p.path)?;
                let bytes = self.write_bytes(&p, data)?;
                let r = if method == methods::FS_WRITE {
                    ws().write(&path, &bytes).await
                } else {
                    ws().replace_atomic(&path, &bytes).await
                };
                r.map_err(|e| RpcError::from_ws(&e))?;
                if !self.cache_disabled() {
                    let hash = p.hash.unwrap_or_else(|| content_hash(&bytes));
                    self.remember(&hash, bytes);
                }
                Ok((json!({}), None))
            }
            _ => self
                .handle_json(conn, tx, method, params)
                .await
                .map(|v| (v, None)),
        }
    }

    fn lock_cache(&self) -> std::sync::MutexGuard<'_, ContentCache> {
        self.inner.cache.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn cache_disabled(&self) -> bool {
        self.inner.opts.content_cache == 0
    }

    fn remember(&self, hash: &str, data: Vec<u8>) {
        self.lock_cache().insert(hash, Arc::new(data));
    }

    /// Statistics of the content cache.
    pub fn cache_stats(&self) -> crate::cache::CacheStats {
        self.lock_cache().stats()
    }

    /// Drop every cached file content (tests: simulate eviction).
    pub fn clear_cache(&self) {
        self.lock_cache().clear();
    }

    /// The bytes a write stores: its cached copy when `cached`, else the
    /// sent bytes, checked against `hash` when one came with them.
    fn write_bytes(&self, p: &WriteParams, data: Option<Vec<u8>>) -> Result<Vec<u8>, RpcError> {
        if p.cached {
            let hash = p.hash.as_deref().unwrap_or_default();
            return self
                .lock_cache()
                .get(hash)
                .map(|d| d.as_ref().clone())
                .ok_or_else(|| {
                    RpcError::new(
                        codes::CACHE_MISS,
                        "content not in the helper cache; resend the bytes",
                    )
                });
        }
        let bytes = data.unwrap_or_else(|| p.data.clone());
        if let Some(h) = &p.hash
            && !self.cache_disabled()
            && content_hash(&bytes) != *h
        {
            return Err(RpcError::new(
                codes::HASH_MISMATCH,
                "the bytes do not match the hash; resend without it",
            ));
        }
        Ok(bytes)
    }

    async fn handle_json(
        &self,
        conn: u64,
        tx: &Tx,
        method: &str,
        params: Value,
    ) -> Result<Value, RpcError> {
        match method {
            methods::PING => Ok(json!({})),
            methods::FS_SET_MODE => {
                let p: SetModeParams = parse(params)?;
                let path = helper_path(&p.path)?;
                ws().set_mode(&path, p.mode)
                    .await
                    .map_err(|e| RpcError::from_ws(&e))?;
                Ok(json!({}))
            }
            methods::FS_STAT => {
                let p: PathParams = parse(params)?;
                let st = ws()
                    .stat(&helper_path(&p.path)?)
                    .await
                    .map_err(|e| RpcError::from_ws(&e))?;
                to_value(WireStat::from(&st))
            }
            methods::FS_LIST => {
                let p: PathParams = parse(params)?;
                let entries = ws()
                    .list(&helper_path(&p.path)?)
                    .await
                    .map_err(|e| RpcError::from_ws(&e))?;
                to_value(ListResult {
                    entries: entries.iter().map(WireEntry::from).collect(),
                })
            }
            methods::FS_GLOB => {
                let p: GlobParams = parse(params)?;
                let root = helper_path(&p.root)?;
                let paths = LocalWorkspace::new(root)
                    .glob(&p.pattern)
                    .await
                    .map_err(|e| RpcError::from_ws(&e))?;
                to_value(GlobResult {
                    paths: paths
                        .iter()
                        .map(|p| p.to_string_lossy().into_owned())
                        .collect(),
                })
            }
            methods::FS_FIND => {
                let mut q: harness_tools::search::FindQuery = parse(params)?;
                q.base = helper_path(&q.base.to_string_lossy())?;
                let found = LocalWorkspace::new(q.base.clone())
                    .find(&q)
                    .await
                    .map_err(|e| RpcError::from_ws(&e))?;
                to_value(found)
            }
            methods::EXEC_START => self.exec_start(conn, tx, parse(params)?).await,
            methods::EXEC_KILL => {
                let p: JobParams = parse(params)?;
                let job = self.job(&p.job)?;
                if let Some(k) = job.lock().kill.take() {
                    let _ = k.send(());
                }
                Ok(json!({}))
            }
            methods::EXEC_ATTACH => {
                let p: AttachParams = parse(params)?;
                let job = self.job(&p.job)?;
                let running = attach(&job, &p, conn, tx);
                to_value(AttachResult { running })
            }
            methods::EXEC_RELEASE => {
                let p: JobParams = parse(params)?;
                self.release(&p.job);
                Ok(json!({}))
            }
            methods::EXEC_LIST => {
                let mut jobs: Vec<JobInfo> = self
                    .inner
                    .jobs
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .iter()
                    .map(|(id, j)| JobInfo {
                        job: id.clone(),
                        program: j.program.clone(),
                        running: j.lock().running,
                    })
                    .collect();
                jobs.sort_by(|a, b| a.job.cmp(&b.job));
                to_value(JobsResult { jobs })
            }
            methods::PROBE => {
                let p: ProbeParams = parse(params)?;
                let path = p.path.as_deref().map(helper_path).transpose()?;
                to_value(crate::probe::collect(path.as_deref(), self.running_jobs()).await)
            }
            methods::SYNC_MANIFEST => {
                let p: RootParams = parse(params)?;
                let root = helper_path(&p.root)?;
                let m = blocking(move || crate::sync::manifest(&root)).await?;
                to_value(m)
            }
            methods::SYNC_CHANGES => {
                let p: ChangesParams = parse(params)?;
                let root = helper_path(&p.root)?;
                let c = blocking(move || crate::sync::changes(&root, &p.baseline)).await?;
                to_value(c)
            }
            methods::SYNC_REMOVE => {
                let p: RemoveParams = parse(params)?;
                let (path, base) = (helper_path(&p.path)?, helper_path(&p.base)?);
                blocking(move || crate::sync::remove_run_dir(&path, &base)).await?;
                Ok(json!({}))
            }
            methods::SYNC_PRUNE => {
                let p: PruneParams = parse(params)?;
                let base = helper_path(&p.base)?;
                let age = p.max_age_secs.map(Duration::from_secs);
                let removed = blocking(move || crate::sync::prune_runs(&base, p.keep, age)).await?;
                to_value(PruneResult { removed })
            }
            other => Err(RpcError::new(
                codes::METHOD_NOT_FOUND,
                format!("unknown method {other}"),
            )),
        }
    }

    async fn exec_start(&self, conn: u64, tx: &Tx, p: ExecStartParams) -> Result<Value, RpcError> {
        if self.job(&p.job).is_ok() {
            return Err(RpcError::invalid_params(format!("job {} exists", p.job)));
        }
        let cwd = helper_path(&p.cwd)?;
        let mut env = Vec::new();
        if p.clear_env {
            for name in &p.host_env {
                if let Some(v) = std::env::var_os(name) {
                    env.push((name.clone(), v.to_string_lossy().into_owned()));
                }
            }
        }
        env.extend(p.env.iter().cloned());
        let req = ExecRequest {
            program: p.program.clone(),
            args: p.args.clone(),
            cwd,
            env,
            clear_env: p.clear_env,
            timeout: p.timeout_ms.map(Duration::from_millis),
        };
        let handle = self.inner.exec.spawn(req).await.map_err(|e| match e {
            ExecError::Spawn { .. } => RpcError::new(codes::SPAWN, e.to_string()),
            ExecError::Lost => RpcError::new(codes::IO, e.to_string()),
        })?;
        let (kill_tx, kill_rx) = oneshot::channel();
        let job = Arc::new(Job {
            program: p.program.clone(),
            state: Mutex::new(JobState {
                log: VecDeque::new(),
                bytes: 0,
                next_seq: 0,
                dropped_bytes: 0,
                running: true,
                sub: Some(Sub {
                    conn,
                    tx: tx.clone(),
                }),
                kill: Some(kill_tx),
            }),
        });
        self.inner
            .jobs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(p.job.clone(), job.clone());
        let max = self.inner.opts.max_job_buffer;
        tokio::spawn(drive_job(p.job, job, handle, kill_rx, max));
        Ok(json!({}))
    }
}

fn ws() -> LocalWorkspace {
    LocalWorkspace::new("/")
}

fn parse<T: DeserializeOwned>(v: Value) -> Result<T, RpcError> {
    serde_json::from_value(v).map_err(|e| RpcError::invalid_params(e.to_string()))
}

fn to_value<T: Serialize>(v: T) -> Result<Value, RpcError> {
    serde_json::to_value(v).map_err(|e| RpcError::new(codes::IO, e.to_string()))
}

async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> io::Result<T> + Send + 'static,
) -> Result<T, RpcError> {
    match tokio::task::spawn_blocking(f).await {
        Ok(Ok(v)) => Ok(v),
        Ok(Err(e)) if e.kind() == io::ErrorKind::PermissionDenied => {
            Err(RpcError::new(codes::REFUSED, e.to_string()))
        }
        Ok(Err(e)) => Err(RpcError::new(codes::IO, e.to_string())),
        Err(e) => Err(RpcError::new(codes::IO, e.to_string())),
    }
}

/// Append a frame to a job's log and send it to the subscriber.
fn push(job: &Job, make: impl FnOnce(u64) -> Frame, max: usize, is_exit: bool) {
    let mut st = job.lock();
    let seq = st.next_seq;
    st.next_seq += 1;
    let frame = make(seq);
    if let Some(sub) = &st.sub
        && sub.tx.send(Outbound::Frame(frame.clone())).is_err()
    {
        st.sub = None;
    }
    st.bytes += frame.approx_len();
    st.log.push_back((seq, frame));
    if is_exit {
        st.running = false;
        st.kill = None;
    }
    while st.bytes > max && st.log.len() > 1 {
        if let Some((_, old)) = st.log.pop_front() {
            st.bytes -= old.approx_len();
            st.dropped_bytes += old.data.as_ref().map_or(0, Vec::len) as u64;
        }
    }
}

/// Resubscribe `conn` to `job`, replaying everything from `from_seq`.
fn attach(job: &Job, p: &AttachParams, conn: u64, tx: &Tx) -> bool {
    let mut st = job.lock();
    let first = st.log.front().map(|(s, _)| *s).unwrap_or(st.next_seq);
    if p.from_seq < first && st.dropped_bytes > 0 {
        let text = format!(
            "[harness: about {} bytes of output were lost while disconnected]\n",
            st.dropped_bytes
        );
        let _ = tx.send(Outbound::Frame(output_frame(
            &p.job,
            first - 1,
            WireStream::Stderr,
            text.into_bytes(),
        )));
    }
    for (seq, frame) in &st.log {
        if *seq >= p.from_seq {
            let _ = tx.send(Outbound::Frame(frame.clone()));
        }
    }
    st.sub = Some(Sub {
        conn,
        tx: tx.clone(),
    });
    st.running
}

/// An `exec.output` notification carrying `data` as its payload.
fn output_frame(job: &str, seq: u64, stream: WireStream, data: Vec<u8>) -> Frame {
    let note = ExecOutputNote {
        job: job.to_owned(),
        seq,
        stream,
        data: Vec::new(),
    };
    let mut params = serde_json::to_value(note).unwrap_or(Value::Null);
    if let Some(o) = params.as_object_mut() {
        o.remove("data");
    }
    Frame::with_data(
        Message::Notification {
            method: methods::EXEC_OUTPUT.to_owned(),
            params,
        },
        data,
    )
}

/// Send a liveness sample to the attached client, if any. Samples are
/// ephemeral: not logged, so a reattaching client just waits for the next.
fn send_liveness(job: &Job, id: &str, l: Liveness) {
    let st = job.lock();
    if let Some(sub) = &st.sub {
        let _ = sub
            .tx
            .send(Outbound::Frame(Frame::new(Message::notification(
                methods::EXEC_LIVENESS,
                ExecLivenessNote {
                    job: id.to_owned(),
                    cpu_ms: l.cpu.as_millis() as u64,
                    procs: l.procs,
                    active: l.active,
                },
            ))));
    }
}

async fn drive_job(
    id: String,
    job: Arc<Job>,
    mut handle: harness_tools::exec::ExecHandle,
    mut kill_rx: oneshot::Receiver<()>,
    max: usize,
) {
    let mut killed = false;
    loop {
        tokio::select! {
            event = handle.next_event() => match event {
                Some(ExecEvent::Output(c)) => push(&job, |seq| output_frame(&id, seq, c.stream.into(), c.data), max, false),
                Some(ExecEvent::Liveness(l)) => send_liveness(&job, &id, l),
                None => break,
            },
            _ = &mut kill_rx, if !killed => {
                killed = true;
                handle.kill();
            }
        }
    }
    let outcome = handle.wait().await.unwrap_or(ExitOutcome {
        killed: true,
        ..Default::default()
    });
    push(
        &job,
        |seq| {
            Frame::new(Message::notification(
                methods::EXEC_EXIT,
                ExecExitNote {
                    job: id.clone(),
                    seq,
                    outcome: outcome.into(),
                },
            ))
        },
        max,
        true,
    );
}

/// Run the helper on stdin/stdout, as `harness remote-agent --stdio`.
///
/// If a helper for the same session is already running (its socket
/// answers), this process only proxies stdio to it; that is how a client
/// reattaches after a disconnect.
pub async fn run_stdio(opts: HelperOptions) -> io::Result<()> {
    let dir = opts.socket_dir.clone().unwrap_or_else(default_socket_dir);
    crate::ssh::create_private_dir(&dir)?;
    let sock = socket_path(&dir, &opts.session);
    if let Ok(stream) = tokio::net::UnixStream::connect(&sock).await {
        return proxy(tokio::io::stdin(), tokio::io::stdout(), stream).await;
    }
    let _ = std::fs::remove_file(&sock);
    let listener = tokio::net::UnixListener::bind(&sock)?;
    // Survive the SSH session's hangup so jobs can be reattached.
    let _hup = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::hangup()).ok();
    let helper = Helper::new(opts);
    tokio::spawn(helper.serve(tokio::io::stdin(), tokio::io::stdout()));
    let h2 = helper.clone();
    let accept = tokio::spawn(async move {
        while let Ok((s, _)) = listener.accept().await {
            let (r, w) = s.into_split();
            tokio::spawn(h2.serve(r, w));
        }
    });
    helper.wait_idle().await;
    accept.abort();
    let _ = std::fs::remove_file(&sock);
    Ok(())
}

/// Copy `input` to `stream` and `stream` to `output` until the stream closes.
pub async fn proxy<I, O>(input: I, mut output: O, stream: tokio::net::UnixStream) -> io::Result<()>
where
    I: AsyncRead + Unpin + Send + 'static,
    O: AsyncWrite + Unpin,
{
    use tokio::io::AsyncWriteExt;
    let (mut sr, mut sw) = stream.into_split();
    let up = tokio::spawn(async move {
        let mut input = input;
        let _ = tokio::io::copy(&mut input, &mut sw).await;
        let _ = sw.shutdown().await;
    });
    let r = tokio::io::copy(&mut sr, &mut output).await;
    let _ = output.flush().await;
    up.abort();
    r.map(|_| ())
}

/// Usage text for `harness remote-agent`.
pub const USAGE: &str = "usage: harness remote-agent --stdio [--session ID] [--grace SECS] [--socket-dir DIR] [--no-compression] [--content-cache BYTES]\n";

/// Parse `remote-agent` arguments (without the subcommand name).
pub fn parse_args(args: &[String]) -> Result<Option<HelperOptions>, String> {
    let mut opts = HelperOptions::default();
    let mut stdio = false;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let mut val = |name: &str| {
            it.next()
                .cloned()
                .ok_or_else(|| format!("{name} needs a value"))
        };
        match a.as_str() {
            "--stdio" => stdio = true,
            "--session" => opts.session = val("--session")?,
            "--grace" => {
                let v = val("--grace")?;
                opts.grace = Duration::from_secs_f64(
                    v.parse::<f64>()
                        .map_err(|_| format!("--grace: bad number {v:?}"))?,
                );
            }
            "--socket-dir" => opts.socket_dir = Some(PathBuf::from(val("--socket-dir")?)),
            "--no-compression" => opts.compression = false,
            "--content-cache" => {
                let v = val("--content-cache")?;
                opts.content_cache = v
                    .parse()
                    .map_err(|_| format!("--content-cache: bad byte count {v:?}"))?;
            }
            // Testing only: behave like an older helper.
            "--max-protocol" => {
                let v = val("--max-protocol")?;
                opts.max_protocol = v
                    .parse()
                    .map_err(|_| format!("--max-protocol: bad number {v:?}"))?;
            }
            // Testing only: advertise a different version in `hello`.
            "--advertise-version" => opts.version = val("--advertise-version")?,
            "--version" | "-V" => return Ok(None),
            other => return Err(format!("unknown argument {other:?}")),
        }
    }
    if !stdio {
        return Err("--stdio is required".into());
    }
    Ok(Some(opts))
}

/// Entry point for `harness remote-agent ...` (`args` exclude the
/// subcommand). Returns the process exit code.
pub fn main(args: Vec<String>) -> i32 {
    let opts = match parse_args(&args) {
        Ok(Some(o)) => o,
        Ok(None) => {
            println!("harness-remote-agent {HELPER_VERSION} (protocol {PROTOCOL_VERSION})");
            return 0;
        }
        Err(e) => {
            eprintln!("harness remote-agent: {e}\n{USAGE}");
            return 2;
        }
    };
    let rt = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("harness remote-agent: {e}");
            return 1;
        }
    };
    let res = rt.block_on(run_stdio(opts));
    // stdin is read on a blocking thread that may never return.
    rt.shutdown_background();
    match res {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("harness remote-agent: {e}");
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn args() {
        let a = |s: &str| s.split_whitespace().map(String::from).collect::<Vec<_>>();
        let o = parse_args(&a("--stdio --session abc --grace 1.5 --socket-dir /tmp/x"))
            .unwrap()
            .unwrap();
        assert_eq!(o.session, "abc");
        assert_eq!(o.grace, Duration::from_millis(1500));
        assert_eq!(o.socket_dir, Some(PathBuf::from("/tmp/x")));
        assert!(parse_args(&a("--session x")).is_err());
        assert!(parse_args(&a("--stdio --bogus")).is_err());
        assert!(parse_args(&a("--version")).unwrap().is_none());
        let o = parse_args(&a("--stdio --no-compression --max-protocol 1"))
            .unwrap()
            .unwrap();
        assert!(!o.compression);
        assert_eq!(o.max_protocol, 1);
        assert!(parse_args(&a("--stdio --max-protocol x")).is_err());
        let o = parse_args(&a("--stdio --content-cache 0"))
            .unwrap()
            .unwrap();
        assert_eq!(o.content_cache, 0);
        assert_eq!(
            parse_args(&a("--stdio")).unwrap().unwrap().content_cache,
            DEFAULT_CONTENT_CACHE
        );
        assert!(parse_args(&a("--stdio --content-cache lots")).is_err());
    }

    #[test]
    fn paths() {
        assert!(helper_path("relative").is_err());
        assert_eq!(helper_path("/a/b").unwrap(), PathBuf::from("/a/b"));
        assert!(helper_path("~/x").unwrap().is_absolute());
        let p = socket_path(Path::new("/d"), "a/../b c");
        assert_eq!(p, PathBuf::from("/d/helper-abc.sock"));
    }
}
