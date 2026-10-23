//! Client side of the helper protocol: connection lifecycle, calls,
//! keepalive, reconnect and job reattach.
//!
//! A [`RemoteClient`] talks to one helper through a [`Connector`]
//! ([`SshConnector`] in production, [`CommandConnector`] for a local helper
//! process, [`InProcessConnector`] in tests). Connection state changes are
//! published as [`ConnectionEvent`]s.
//!
//! When the connection drops while jobs are running, the client reconnects
//! with the same session id, which lands on the same helper if it is still
//! alive (see [`crate::helper`]), and resubscribes every job with
//! `exec.attach` from the last sequence number it saw: output resumes
//! without gaps or duplicates. If the helper is gone, each job's
//! [`ExecHandle::wait`](harness_tools::exec::ExecHandle::wait) returns
//! [`ExecError::Lost`](harness_tools::exec::ExecError::Lost), which fails
//! the step so the workflow can retry it.
//!
//! **Wire format.** The client offers protocol 1 to [`PROTOCOL_VERSION`] and,
//! unless [`ClientOptions::compression`] is off or the transport already
//! compresses ([`Connector::transport_compressed`], SSH `Compression=yes`),
//! zstd frame compression. A protocol 1 helper answers protocol 1 and the
//! connection stays uncompressed JSON lines; see [`crate::wire`].
//!
//! **Content cache.** [`RemoteClient::read_file`] and
//! [`RemoteClient::write_file`] go through a per-session content-hash cache
//! ([`crate::cache`], size [`ClientOptions::content_cache`]): content the
//! helper already holds travels as its hash, and the bytes saved are
//! recorded in the operation's audit event.

use std::collections::HashMap;
use std::io;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant, SystemTime};

use async_trait::async_trait;
use harness_tools::exec::{ExitOutcome, Liveness, OutputChunk};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, BufReader};
use tokio::sync::{broadcast, mpsc, oneshot};
use tokio::task::JoinHandle;

use crate::audit::{AuditEvent, AuditSink, SecretGuard, TracingAudit};
use crate::cache::{ContentCache, DEFAULT_CONTENT_CACHE, PeerCache, content_hash};
use crate::helper::Helper;
use crate::probe::ProbeReport;
use crate::protocol::DataResult;
use crate::protocol::{
    AttachParams, AttachResult, ChangesParams, CompressionOffer, DEFAULT_COMPRESS_MIN,
    DEFAULT_ZSTD_LEVEL, ExecExitNote, ExecLivenessNote, ExecOutputNote, HELPER_VERSION,
    HelloParams, HelloResult, JobParams, JobsResult, MAX_FRAME, MIN_PROTOCOL_VERSION, Message,
    PROTOCOL_VERSION, ProbeParams, PruneParams, PruneResult, ReadParams, RemoveParams, RootParams,
    RpcError, WriteParams, ZSTD, b64, codes, methods,
};
use crate::ssh::{SshError, SshTarget};
use crate::sync::{ChangeSet, Manifest};
use crate::wire::{Encoder, Frame, Framing, Outbound, Zstd, read_wire_frame, write_loop};

/// An open byte stream to a helper.
pub struct Connection {
    /// Helper's stdout.
    pub reader: Box<dyn AsyncRead + Send + Unpin>,
    /// Helper's stdin.
    pub writer: Box<dyn AsyncWrite + Send + Unpin>,
    /// The local process carrying the stream (ssh), if any.
    pub child: Option<tokio::process::Child>,
    /// Captured stderr of `child`, used to explain failures.
    pub stderr: Option<Arc<Mutex<String>>>,
}

impl std::fmt::Debug for Connection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Connection")
            .field("child", &self.child.as_ref().and_then(|c| c.id()))
            .finish_non_exhaustive()
    }
}

/// Opens connections to a helper.
#[async_trait]
pub trait Connector: Send + Sync {
    /// Open a connection for helper session `session`.
    async fn connect(&self, session: &str) -> io::Result<Connection>;
    /// The host name used in errors (ssh classification) and events.
    fn host(&self) -> String;
    /// True when the transport already compresses (SSH `Compression=yes`),
    /// so the client does not offer helper frame compression and data is
    /// never compressed twice.
    fn transport_compressed(&self) -> bool {
        false
    }
}

/// Spawn `cmd` and wrap its stdio as a [`Connection`], capturing stderr.
pub fn spawn_connection(mut cmd: tokio::process::Command) -> io::Result<Connection> {
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let mut child = cmd.spawn()?;
    let reader = child
        .stdout
        .take()
        .ok_or_else(|| io::Error::other("no stdout"))?;
    let writer = child
        .stdin
        .take()
        .ok_or_else(|| io::Error::other("no stdin"))?;
    let stderr = Arc::new(Mutex::new(String::new()));
    if let Some(mut err) = child.stderr.take() {
        let buf = stderr.clone();
        tokio::spawn(async move {
            let mut chunk = [0u8; 4096];
            while let Ok(n) = err.read(&mut chunk).await {
                if n == 0 {
                    break;
                }
                let mut b = buf.lock().unwrap_or_else(|e| e.into_inner());
                if b.len() < 64 * 1024 {
                    b.push_str(&String::from_utf8_lossy(&chunk[..n]));
                }
            }
        });
    }
    Ok(Connection {
        reader: Box::new(reader),
        writer: Box::new(writer),
        child: Some(child),
        stderr: Some(stderr),
    })
}

/// Runs a local helper program (`program args... --session <id>`).
#[derive(Debug, Clone)]
pub struct CommandConnector {
    /// Program.
    pub program: String,
    /// Arguments before `--session`.
    pub args: Vec<String>,
    /// Name reported as the host.
    pub host: String,
}

impl CommandConnector {
    /// Connector for `program args`.
    pub fn new(program: impl Into<String>, args: Vec<String>) -> Self {
        Self {
            program: program.into(),
            args,
            host: "local".into(),
        }
    }
}

#[async_trait]
impl Connector for CommandConnector {
    async fn connect(&self, session: &str) -> io::Result<Connection> {
        let mut cmd = tokio::process::Command::new(&self.program);
        cmd.args(&self.args).args(["--session", session]);
        spawn_connection(cmd)
    }

    fn host(&self) -> String {
        self.host.clone()
    }
}

/// Runs the helper on a remote host over ssh.
#[derive(Debug, Clone)]
pub struct SshConnector {
    /// Destination.
    pub target: SshTarget,
    /// Remote helper path (`~/` is expanded by the remote shell).
    pub helper_path: String,
    /// Extra helper arguments after `remote-agent --stdio`.
    pub extra_args: Vec<String>,
}

impl SshConnector {
    /// Connector running `helper_path remote-agent --stdio` on `target`.
    pub fn new(target: SshTarget, helper_path: impl Into<String>) -> Self {
        Self {
            target,
            helper_path: helper_path.into(),
            extra_args: Vec::new(),
        }
    }

    /// The remote command line for `session`.
    pub fn remote_command(&self, session: &str) -> String {
        let mut line = crate::ssh::quote_remote_path(&self.helper_path);
        let mut rest: Vec<String> = vec!["remote-agent".into(), "--stdio".into()];
        rest.extend(self.extra_args.iter().cloned());
        rest.extend(["--session".into(), session.into()]);
        line.push(' ');
        line.push_str(&crate::ssh::remote_command_line(&rest));
        line
    }
}

#[async_trait]
impl Connector for SshConnector {
    async fn connect(&self, session: &str) -> io::Result<Connection> {
        self.target.prepare()?;
        let cmd = self.target.command_line(&self.remote_command(session));
        spawn_connection(cmd)
    }

    fn host(&self) -> String {
        self.target.host.clone()
    }

    fn transport_compressed(&self) -> bool {
        self.target.compression
    }
}

/// Serves an in-process [`Helper`] over in-memory pipes. For tests and
/// simulations: [`InProcessConnector::disconnect_all`] cuts every
/// connection, and [`InProcessConnector::set_helper`] swaps the helper (as
/// if the remote helper had died and a fresh one started).
#[derive(Debug, Clone)]
pub struct InProcessConnector {
    helper: Arc<Mutex<Helper>>,
    tasks: Arc<Mutex<Vec<JoinHandle<()>>>>,
    refuse: Arc<AtomicBool>,
}

impl InProcessConnector {
    /// Connector serving `helper`.
    pub fn new(helper: Helper) -> Self {
        Self {
            helper: Arc::new(Mutex::new(helper)),
            tasks: Arc::default(),
            refuse: Arc::default(),
        }
    }

    /// The current helper.
    pub fn helper(&self) -> Helper {
        self.helper
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Replace the helper used for new connections.
    pub fn set_helper(&self, helper: Helper) {
        *self.helper.lock().unwrap_or_else(|e| e.into_inner()) = helper;
    }

    /// Abort every open connection (the client sees end of stream).
    pub fn disconnect_all(&self) {
        for t in self
            .tasks
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .drain(..)
        {
            t.abort();
        }
    }

    /// Make new connection attempts fail (network down) or succeed again.
    pub fn set_refuse(&self, refuse: bool) {
        self.refuse.store(refuse, Ordering::SeqCst);
    }
}

#[async_trait]
impl Connector for InProcessConnector {
    async fn connect(&self, _session: &str) -> io::Result<Connection> {
        if self.refuse.load(Ordering::SeqCst) {
            return Err(io::Error::new(
                io::ErrorKind::ConnectionRefused,
                "simulated network failure",
            ));
        }
        let (client_side, helper_side) = tokio::io::duplex(1 << 20);
        let (hr, hw) = tokio::io::split(helper_side);
        let task = tokio::spawn(self.helper().serve(hr, hw));
        self.tasks
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(task);
        let (cr, cw) = tokio::io::split(client_side);
        Ok(Connection {
            reader: Box::new(cr),
            writer: Box::new(cw),
            child: None,
            stderr: None,
        })
    }

    fn host(&self) -> String {
        "in-process".into()
    }
}

/// A connection state change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnectionState {
    /// First connection attempt.
    Connecting,
    /// Connected (after the first attempt or a reconnect).
    Connected {
        /// True after a reconnect.
        reconnected: bool,
        /// Jobs resubscribed.
        reattached: usize,
    },
    /// The connection dropped.
    Disconnected {
        /// Why.
        reason: String,
    },
    /// Reconnect attempt `attempt` of `max`.
    Reconnecting {
        /// Attempt number (1-based).
        attempt: u32,
        /// Maximum attempts.
        max: u32,
    },
    /// Connecting failed for good.
    Failed {
        /// Why.
        reason: String,
    },
    /// A job could not be reattached (its step fails and may be retried).
    JobLost {
        /// Job id.
        job: String,
    },
}

/// A host-tagged connection event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectionEvent {
    /// `[remote.<name>]` name.
    pub host: String,
    /// What happened.
    pub state: ConnectionState,
}

/// Errors from [`RemoteClient`].
#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    /// The connection could not be opened.
    #[error("cannot connect to {host}: {message}")]
    Connect {
        /// Host.
        host: String,
        /// Why.
        message: String,
    },
    /// ssh failed (unknown host key, auth, ...).
    #[error(transparent)]
    Ssh(#[from] SshError),
    /// The connection dropped before the answer arrived.
    #[error("connection to the remote helper was lost")]
    Disconnected,
    /// No answer in time.
    #[error("remote call {0} timed out")]
    Timeout(String),
    /// The helper returned an error.
    #[error("remote helper: {0}")]
    Rpc(#[from] RpcError),
    /// The helper's version differs from ours.
    #[error("remote helper version {remote} (protocol {protocol}) does not match harness {local}")]
    VersionMismatch {
        /// Our version.
        local: String,
        /// Helper version.
        remote: String,
        /// Helper protocol.
        protocol: u32,
    },
    /// The request contained a harness credential and was not sent.
    #[error("refusing to send {op} to the remote host: it contains a harness credential")]
    SecretBlocked {
        /// Operation.
        op: String,
    },
    /// The answer could not be decoded.
    #[error("bad answer from remote helper: {0}")]
    Decode(String),
    /// The client was closed.
    #[error("remote client is closed")]
    Closed,
}

impl ClientError {
    /// The RPC error code, if the helper answered with an error.
    pub fn rpc_code(&self) -> Option<i64> {
        match self {
            ClientError::Rpc(e) => Some(e.code),
            _ => None,
        }
    }
}

/// Helper frame compression (protocol 2) as offered by the client.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WireCompression {
    /// Offer zstd to the helper. Even when true it is not offered over a
    /// transport that compresses already.
    pub enabled: bool,
    /// zstd level both sides use (default [`DEFAULT_ZSTD_LEVEL`]).
    pub level: i32,
    /// Frames with a smaller body are sent as-is (default
    /// [`DEFAULT_COMPRESS_MIN`]).
    pub min_size: usize,
}

impl Default for WireCompression {
    fn default() -> Self {
        Self {
            enabled: true,
            level: DEFAULT_ZSTD_LEVEL,
            min_size: DEFAULT_COMPRESS_MIN,
        }
    }
}

impl WireCompression {
    /// No helper frame compression.
    pub fn off() -> Self {
        Self {
            enabled: false,
            ..Self::default()
        }
    }
}

/// [`RemoteClient`] settings.
#[derive(Clone)]
pub struct ClientOptions {
    /// `[remote.<name>]` name, used in events and audit.
    pub host: String,
    /// Required helper version (`None` accepts any with the same protocol).
    pub expected_version: Option<String>,
    /// Reconnect attempts after a drop.
    pub reconnect_attempts: u32,
    /// Delay before reconnect attempt n is `n * reconnect_delay`.
    pub reconnect_delay: Duration,
    /// Per-call timeout.
    pub call_timeout: Duration,
    /// Application-level ping interval (in addition to ssh keepalives).
    pub keepalive: Option<Duration>,
    /// Refuses outgoing credentials.
    pub guard: SecretGuard,
    /// Receives one event per operation.
    pub audit: Arc<dyn AuditSink>,
    /// Helper frame compression (default: zstd level 1 for frames of
    /// 4 KiB or more, unless the transport compresses).
    pub compression: WireCompression,
    /// Bytes of the per-session content-hash cache (default
    /// [`DEFAULT_CONTENT_CACHE`]; 0 disables it).
    pub content_cache: usize,
}

impl std::fmt::Debug for ClientOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClientOptions")
            .field("host", &self.host)
            .field("expected_version", &self.expected_version)
            .field("reconnect_attempts", &self.reconnect_attempts)
            .field("compression", &self.compression)
            .field("content_cache", &self.content_cache)
            .finish_non_exhaustive()
    }
}

impl ClientOptions {
    /// Defaults for `host`: require our own version, 3 reconnect attempts,
    /// 5 minute calls, 15 s pings, an empty secret guard (set it with
    /// [`ClientOptions::with_secrets`]) and audit events to the log
    /// ([`TracingAudit`]).
    pub fn new(host: impl Into<String>) -> Self {
        Self {
            host: host.into(),
            expected_version: Some(HELPER_VERSION.to_owned()),
            reconnect_attempts: 3,
            reconnect_delay: Duration::from_millis(500),
            call_timeout: Duration::from_secs(300),
            keepalive: Some(Duration::from_secs(15)),
            guard: SecretGuard::default(),
            audit: Arc::new(TracingAudit),
            compression: WireCompression::default(),
            content_cache: DEFAULT_CONTENT_CACHE,
        }
    }

    /// Use a content-hash cache of `bytes` (0 disables it).
    pub fn with_content_cache(mut self, bytes: usize) -> Self {
        self.content_cache = bytes;
        self
    }

    /// Use `compression` for helper frames.
    pub fn with_compression(mut self, compression: WireCompression) -> Self {
        self.compression = compression;
        self
    }

    /// Refuse to send any of `values` (the resolved harness credentials).
    pub fn with_secrets(mut self, values: impl IntoIterator<Item = String>) -> Self {
        self.guard = SecretGuard::new(values);
        self
    }

    /// Send audit events to `audit`.
    pub fn with_audit(mut self, audit: Arc<dyn AuditSink>) -> Self {
        self.audit = audit;
        self
    }
}

/// A decoded result and its binary payload.
type Reply = (Value, Option<Vec<u8>>);
type Pending = Mutex<HashMap<u64, oneshot::Sender<Result<Reply, ClientError>>>>;

struct Live {
    tx: mpsc::UnboundedSender<Outbound>,
    pending: Pending,
    alive: AtomicBool,
    tasks: Mutex<Vec<JoinHandle<()>>>,
    child: Mutex<Option<tokio::process::Child>>,
}

impl Drop for Live {
    fn drop(&mut self) {
        for t in self
            .tasks
            .get_mut()
            .unwrap_or_else(|e| e.into_inner())
            .drain(..)
        {
            t.abort();
        }
    }
}

impl Live {
    async fn request(
        &self,
        id: u64,
        method: &str,
        params: Value,
        data: Option<Vec<u8>>,
        timeout: Duration,
    ) -> Result<Reply, ClientError> {
        if !self.alive.load(Ordering::SeqCst) {
            return Err(ClientError::Disconnected);
        }
        let (tx, rx) = oneshot::channel();
        self.pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(id, tx);
        let message = Message::Request {
            id,
            method: method.to_owned(),
            params,
        };
        if self
            .tx
            .send(Outbound::Frame(Frame { message, data }))
            .is_err()
        {
            self.pending
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&id);
            return Err(ClientError::Disconnected);
        }
        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(r)) => r,
            Ok(Err(_)) => Err(ClientError::Disconnected),
            Err(_) => {
                self.pending
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .remove(&id);
                Err(ClientError::Timeout(method.to_owned()))
            }
        }
    }

    fn fail_pending(&self) {
        let drained: Vec<_> = self
            .pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .drain()
            .collect();
        for (_, tx) in drained {
            let _ = tx.send(Err(ClientError::Disconnected));
        }
    }
}

struct JobSink {
    output: mpsc::UnboundedSender<OutputChunk>,
    status: Option<oneshot::Sender<ExitOutcome>>,
    liveness: mpsc::UnboundedSender<Liveness>,
    next_seq: u64,
}

/// Most path entries kept beside the content cache.
const MAX_PATHS: usize = 16_384;

/// The client half of the content-hash cache.
struct ClientCache {
    /// Contents this client holds.
    content: ContentCache,
    /// Remote path -> hash of the content last read or written there.
    paths: HashMap<String, String>,
    /// What the helper probably holds.
    peer: PeerCache,
    /// Helper pid the peer model belongs to.
    peer_pid: Option<u32>,
}

impl ClientCache {
    fn new(capacity: usize) -> Self {
        Self {
            content: ContentCache::new(capacity),
            paths: HashMap::new(),
            peer: PeerCache::new(0),
            peer_pid: None,
        }
    }

    /// The hash to offer for a read of `path`, when its content is held.
    fn have(&self, path: &str) -> Option<String> {
        self.paths
            .get(path)
            .filter(|h| self.content.contains(h))
            .cloned()
    }

    /// `path` now holds `data` (whose hash is `hash`), and so does the helper.
    fn learn(&mut self, path: &str, hash: String, data: &[u8]) {
        if self.content.is_disabled() {
            return;
        }
        self.peer.note(&hash, data.len());
        if !self.content.contains(&hash) && !self.content.insert(&hash, Arc::new(data.to_vec())) {
            self.paths.remove(path);
            return;
        }
        if self.paths.len() >= MAX_PATHS {
            let content = &self.content;
            self.paths.retain(|_, h| content.contains(h));
            if self.paths.len() >= MAX_PATHS {
                self.paths.clear();
            }
        }
        self.paths.insert(path.to_owned(), hash);
    }
}

struct Inner {
    opts: ClientOptions,
    cache: Mutex<ClientCache>,
    connector: Arc<dyn Connector>,
    session: String,
    live: Mutex<Option<Arc<Live>>>,
    connect_lock: tokio::sync::Mutex<()>,
    next_id: AtomicU64,
    jobs: Mutex<HashMap<String, JobSink>>,
    events: broadcast::Sender<ConnectionEvent>,
    hello: Mutex<Option<HelloResult>>,
    closed: AtomicBool,
    ever_connected: AtomicBool,
}

impl Inner {
    fn emit(&self, state: ConnectionState) {
        tracing::debug!(target: "harness::remote", host = %self.opts.host, ?state, "connection");
        let _ = self.events.send(ConnectionEvent {
            host: self.opts.host.clone(),
            state,
        });
    }

    fn current(&self) -> Option<Arc<Live>> {
        self.live
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
            .filter(|l| l.alive.load(Ordering::SeqCst))
    }

    fn lock_jobs(&self) -> std::sync::MutexGuard<'_, HashMap<String, JobSink>> {
        self.jobs.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn fail_all_jobs(&self) {
        let lost: Vec<String> = self.lock_jobs().drain().map(|(k, _)| k).collect();
        for job in lost {
            self.emit(ConnectionState::JobLost { job });
        }
    }
}

/// A connection to one remote helper. Cheap to clone; all clones share the
/// connection and job table.
#[derive(Clone)]
pub struct RemoteClient {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for RemoteClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RemoteClient")
            .field("host", &self.inner.opts.host)
            .field("session", &self.inner.session)
            .finish_non_exhaustive()
    }
}

/// Methods that are safe to resend after a disconnect.
const IDEMPOTENT: &[&str] = &[
    methods::PING,
    methods::FS_READ,
    methods::FS_STAT,
    methods::FS_LIST,
    methods::FS_GLOB,
    methods::FS_FIND,
    methods::FS_SET_MODE,
    // Whole-file writes: sending one twice leaves the same file. Retrying
    // matters when the connection drops right after the request was
    // queued (seen on macOS CI after a helper restart).
    methods::FS_WRITE,
    methods::FS_REPLACE,
    methods::EXEC_LIST,
    methods::PROBE,
    methods::SYNC_MANIFEST,
    methods::SYNC_CHANGES,
];

impl RemoteClient {
    /// Connect through `connector`, handshake and check the helper version.
    pub async fn connect(
        connector: Arc<dyn Connector>,
        opts: ClientOptions,
    ) -> Result<RemoteClient, ClientError> {
        let (events, _) = broadcast::channel(256);
        let client = RemoteClient {
            inner: Arc::new(Inner {
                cache: Mutex::new(ClientCache::new(opts.content_cache)),
                opts,
                connector,
                session: uuid::Uuid::now_v7().simple().to_string(),
                live: Mutex::new(None),
                connect_lock: tokio::sync::Mutex::new(()),
                next_id: AtomicU64::new(1),
                jobs: Mutex::new(HashMap::new()),
                events,
                hello: Mutex::new(None),
                closed: AtomicBool::new(false),
                ever_connected: AtomicBool::new(false),
            }),
        };
        client.ensure_live().await?;
        Ok(client)
    }

    /// Host name.
    pub fn host(&self) -> &str {
        &self.inner.opts.host
    }

    /// Helper session id.
    pub fn session(&self) -> &str {
        &self.inner.session
    }

    /// The last handshake answer.
    pub fn hello(&self) -> Option<HelloResult> {
        self.inner
            .hello
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Make a remote path absolute: a leading `~/` becomes the helper's
    /// home directory (from the handshake).
    pub fn expand_home(&self, path: &str) -> String {
        let home = self.hello().and_then(|h| h.home);
        match (path.strip_prefix("~/"), home) {
            (Some(rest), Some(h)) => format!("{}/{rest}", h.trim_end_matches('/')),
            _ => path.to_owned(),
        }
    }

    /// True while a connection is up.
    pub fn is_connected(&self) -> bool {
        self.inner.current().is_some()
    }

    /// Subscribe to connection events.
    pub fn subscribe(&self) -> broadcast::Receiver<ConnectionEvent> {
        self.inner.events.subscribe()
    }

    /// Record an audit event for a remote operation that does not go
    /// through the helper protocol (rsync push, uploads).
    pub fn record_audit(
        &self,
        op: &str,
        target: Option<&str>,
        started: Instant,
        error: Option<String>,
    ) {
        self.inner.opts.audit.record(&AuditEvent {
            host: self.inner.opts.host.clone(),
            op: op.to_owned(),
            target: target.map(str::to_owned),
            ok: error.is_none(),
            error,
            duration: started.elapsed(),
            at: SystemTime::now() - started.elapsed(),
            bytes_saved: 0,
        });
    }

    /// The secret guard.
    pub fn guard(&self) -> &SecretGuard {
        &self.inner.opts.guard
    }

    /// Close the connection. Running jobs are killed by the helper once its
    /// grace period passes without a client.
    pub fn close(&self) {
        self.inner.closed.store(true, Ordering::SeqCst);
        if let Some(l) = self
            .inner
            .live
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
        {
            l.alive.store(false, Ordering::SeqCst);
            l.fail_pending();
        }
    }

    /// Call `method` and decode the result. `target` (a path or program)
    /// is recorded in the audit event. A binary payload in the answer is
    /// handed to the decoder as the base64 `data` field, so the protocol
    /// types decode the same on every protocol version; use
    /// [`RemoteClient::call_data`] to avoid that copy.
    pub async fn call<P: Serialize, R: DeserializeOwned>(
        &self,
        method: &str,
        target: Option<&str>,
        params: P,
    ) -> Result<R, ClientError> {
        let (mut value, data) = self.call_raw(method, target, params, None).await?;
        if let (Some(d), Some(obj)) = (data, value.as_object_mut()) {
            obj.insert("data".into(), Value::String(b64::encode(&d)));
        }
        serde_json::from_value(value).map_err(|e| ClientError::Decode(e.to_string()))
    }

    /// Call `method` with an optional binary payload `data` (the request's
    /// `data` field, sent raw on protocol 2 and as base64 on protocol 1)
    /// and decode the result; the answer's payload is returned beside it
    /// (`None` from a protocol 1 helper, whose payload stays in the JSON).
    ///
    /// The payload is checked by the secret guard like the parameters.
    pub async fn call_data<P: Serialize, R: DeserializeOwned>(
        &self,
        method: &str,
        target: Option<&str>,
        params: P,
        data: Option<Vec<u8>>,
    ) -> Result<(R, Option<Vec<u8>>), ClientError> {
        if data
            .as_deref()
            .is_some_and(|d| self.inner.opts.guard.contains_secret(d))
        {
            return Err(ClientError::SecretBlocked {
                op: method.to_owned(),
            });
        }
        let (value, data) = self.call_raw(method, target, params, data).await?;
        let r = serde_json::from_value(value).map_err(|e| ClientError::Decode(e.to_string()))?;
        Ok((r, data))
    }

    async fn call_raw<P: Serialize>(
        &self,
        method: &str,
        target: Option<&str>,
        params: P,
        data: Option<Vec<u8>>,
    ) -> Result<Reply, ClientError> {
        let started = Instant::now();
        let at = SystemTime::now();
        let res = self.call_value(method, params, data).await;
        self.audit_op(method, target, started, at, res.as_ref().err(), 0);
        res
    }

    fn audit_op(
        &self,
        method: &str,
        target: Option<&str>,
        started: Instant,
        at: SystemTime,
        error: Option<&ClientError>,
        bytes_saved: u64,
    ) {
        self.inner.opts.audit.record(&AuditEvent {
            host: self.inner.opts.host.clone(),
            op: method.to_owned(),
            target: target.map(str::to_owned),
            ok: error.is_none(),
            error: error.map(ToString::to_string),
            duration: started.elapsed(),
            at,
            bytes_saved,
        });
    }

    fn lock_cache(&self) -> std::sync::MutexGuard<'_, ClientCache> {
        self.inner.cache.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Statistics of this client's content cache.
    pub fn cache_stats(&self) -> crate::cache::CacheStats {
        self.lock_cache().content.stats()
    }

    /// Drop every cached content on this side (tests: simulate eviction).
    pub fn clear_cache(&self) {
        let mut c = self.lock_cache();
        c.content.clear();
        c.paths.clear();
    }

    /// Run `f` on the client's content cache (tests: simulate corruption).
    #[doc(hidden)]
    pub fn with_cache_for_test<T>(&self, f: impl FnOnce(&mut ContentCache) -> T) -> T {
        f(&mut self.lock_cache().content)
    }

    /// Read the file at `path` (absolute on the host) through the content
    /// cache: when the helper finds the file still has the content this
    /// client holds, only the hash crosses the wire. A copy that was
    /// evicted or no longer matches its hash falls back to a full read, so
    /// the bytes returned are always the file's current contents. One
    /// `fs.read` audit event records the bytes saved.
    pub async fn read_file(&self, path: &str) -> Result<Vec<u8>, ClientError> {
        let started = Instant::now();
        let at = SystemTime::now();
        let res = self.read_cached(path).await;
        let saved = res.as_ref().map_or(0, |(_, s)| *s);
        self.audit_op(
            methods::FS_READ,
            Some(path),
            started,
            at,
            res.as_ref().err(),
            saved,
        );
        res.map(|(d, _)| d)
    }

    async fn fetch(&self, path: &str, have: Option<String>) -> Result<DataResult, ClientError> {
        let params = ReadParams {
            path: path.to_owned(),
            have,
        };
        let (value, payload) = self.call_value(methods::FS_READ, params, None).await?;
        let mut r: DataResult =
            serde_json::from_value(value).map_err(|e| ClientError::Decode(e.to_string()))?;
        // Protocol 2 sends the bytes as the payload, protocol 1 in JSON.
        if let Some(p) = payload {
            r.data = p;
        }
        Ok(r)
    }

    async fn read_cached(&self, path: &str) -> Result<(Vec<u8>, u64), ClientError> {
        let have = self.lock_cache().have(path);
        let mut r = self.fetch(path, have.clone()).await?;
        if r.cached {
            let hit = match (&have, &r.hash) {
                (Some(h), Some(rh)) if h == rh => self.lock_cache().content.get(h),
                _ => None,
            };
            if let Some(d) = hit {
                let mut c = self.lock_cache();
                c.peer.note(have.as_deref().unwrap_or_default(), d.len());
                return Ok((d.as_ref().clone(), d.len() as u64));
            }
            // Evicted or corrupted since the request was sent.
            r = self.fetch(path, None).await?;
            if r.cached {
                return Err(ClientError::Decode(
                    "helper answered fs.read from a cache the client did not offer".into(),
                ));
            }
        }
        let mut c = self.lock_cache();
        match r.hash {
            Some(h) if content_hash(&r.data) == h => c.learn(path, h, &r.data),
            _ => {
                c.paths.remove(path);
            }
        }
        Ok((r.data, 0))
    }

    /// Write `data` to `path` with `method` (`fs.write` or
    /// `fs.replace_atomic`) through the content cache: when the helper
    /// holds this content already (this session sent or received it), only
    /// the hash is sent. A miss on the helper or a hash mismatch falls back
    /// to sending the bytes, so the file always ends up with `data`. The
    /// secret guard checks `data` whichever way it travels. One audit event
    /// records the bytes saved.
    pub async fn write_file(
        &self,
        method: &str,
        path: &str,
        data: &[u8],
    ) -> Result<(), ClientError> {
        if self.inner.opts.guard.contains_secret(data) {
            return Err(ClientError::SecretBlocked {
                op: method.to_owned(),
            });
        }
        let started = Instant::now();
        let at = SystemTime::now();
        let res = self.write_cached(method, path, data).await;
        let saved = res.as_ref().map_or(0, |s| *s);
        self.audit_op(method, Some(path), started, at, res.as_ref().err(), saved);
        res.map(|_| ())
    }

    async fn send_write(
        &self,
        method: &str,
        path: &str,
        hash: Option<String>,
        data: Option<&[u8]>,
    ) -> Result<(), ClientError> {
        let params = WriteParams {
            path: path.to_owned(),
            data: Vec::new(),
            cached: data.is_none(),
            hash,
        };
        self.call_value(method, params, data.map(<[u8]>::to_vec))
            .await
            .map(|_| ())
    }

    async fn write_cached(
        &self,
        method: &str,
        path: &str,
        data: &[u8],
    ) -> Result<u64, ClientError> {
        let enabled = {
            let c = self.lock_cache();
            !c.content.is_disabled() && c.peer_pid.is_some()
        };
        if !enabled {
            self.send_write(method, path, None, Some(data)).await?;
            return Ok(0);
        }
        let hash = content_hash(data);
        if self.lock_cache().peer.holds(&hash) {
            match self
                .send_write(method, path, Some(hash.clone()), None)
                .await
            {
                Ok(()) => {
                    self.lock_cache().learn(path, hash, data);
                    return Ok(data.len() as u64);
                }
                Err(ClientError::Rpc(e)) if e.code == codes::CACHE_MISS => {
                    self.lock_cache().peer.forget(&hash);
                }
                Err(e) => return Err(e),
            }
        }
        match self
            .send_write(method, path, Some(hash.clone()), Some(data))
            .await
        {
            Ok(()) => self.lock_cache().learn(path, hash, data),
            Err(ClientError::Rpc(e)) if e.code == codes::HASH_MISMATCH => {
                // The bytes did not arrive as sent; send them plainly and
                // keep them out of the cache.
                self.send_write(method, path, None, Some(data)).await?;
                let mut c = self.lock_cache();
                c.paths.remove(path);
                c.peer.forget(&hash);
            }
            Err(e) => return Err(e),
        }
        Ok(0)
    }

    async fn call_value<P: Serialize>(
        &self,
        method: &str,
        params: P,
        mut data: Option<Vec<u8>>,
    ) -> Result<Reply, ClientError> {
        let params =
            serde_json::to_value(params).map_err(|e| ClientError::Decode(e.to_string()))?;
        if self
            .inner
            .opts
            .guard
            .contains_secret_str(&params.to_string())
        {
            return Err(ClientError::SecretBlocked {
                op: method.to_owned(),
            });
        }
        let mut tries = if IDEMPOTENT.contains(&method) { 2 } else { 1 };
        loop {
            let live = self.ensure_live().await?;
            let id = self.inner.next_id.fetch_add(1, Ordering::SeqCst);
            let send = if tries > 1 { data.clone() } else { data.take() };
            match live
                .request(
                    id,
                    method,
                    params.clone(),
                    send,
                    self.inner.opts.call_timeout,
                )
                .await
            {
                Err(ClientError::Disconnected) if tries > 1 => tries -= 1,
                r => return r,
            }
        }
    }

    /// Liveness check.
    pub async fn ping(&self) -> Result<(), ClientError> {
        self.call::<_, Value>(methods::PING, None, serde_json::json!({}))
            .await
            .map(|_| ())
    }

    /// Probe host resources (`disk_path` defaults to the remote home).
    pub async fn probe(&self, disk_path: Option<&str>) -> Result<ProbeReport, ClientError> {
        self.call(
            methods::PROBE,
            disk_path,
            ProbeParams {
                path: disk_path.map(str::to_owned),
            },
        )
        .await
    }

    /// Jobs known to the helper.
    pub async fn jobs(&self) -> Result<JobsResult, ClientError> {
        self.call(methods::EXEC_LIST, None, serde_json::json!({}))
            .await
    }

    /// Manifest of a remote tree.
    pub async fn manifest(&self, root: &str) -> Result<Manifest, ClientError> {
        self.call(
            methods::SYNC_MANIFEST,
            Some(root),
            RootParams { root: root.into() },
        )
        .await
    }

    /// Changes of a remote tree since `baseline`.
    pub async fn changes(&self, root: &str, baseline: &Manifest) -> Result<ChangeSet, ClientError> {
        self.call(
            methods::SYNC_CHANGES,
            Some(root),
            ChangesParams {
                root: root.into(),
                baseline: baseline.clone(),
            },
        )
        .await
    }

    /// Remove a run dir strictly inside `base`.
    pub async fn remove_run_dir(&self, path: &str, base: &str) -> Result<(), ClientError> {
        self.call::<_, Value>(
            methods::SYNC_REMOVE,
            Some(path),
            RemoveParams {
                path: path.into(),
                base: base.into(),
            },
        )
        .await
        .map(|_| ())
    }

    /// Prune old run dirs under `base`.
    pub async fn prune(
        &self,
        base: &str,
        keep: usize,
        max_age: Option<Duration>,
    ) -> Result<Vec<String>, ClientError> {
        let r: PruneResult = self
            .call(
                methods::SYNC_PRUNE,
                Some(base),
                PruneParams {
                    base: base.into(),
                    keep,
                    max_age_secs: max_age.map(|d| d.as_secs()),
                },
            )
            .await?;
        Ok(r.removed)
    }

    /// Register a job before starting it, so no output is missed. Returns
    /// its output, exit status and liveness channels.
    pub(crate) fn register_job(
        &self,
        job: &str,
    ) -> (
        mpsc::UnboundedReceiver<OutputChunk>,
        oneshot::Receiver<ExitOutcome>,
        mpsc::UnboundedReceiver<Liveness>,
    ) {
        let (otx, orx) = mpsc::unbounded_channel();
        let (stx, srx) = oneshot::channel();
        let (ltx, lrx) = mpsc::unbounded_channel();
        self.inner.lock_jobs().insert(
            job.to_owned(),
            JobSink {
                output: otx,
                status: Some(stx),
                liveness: ltx,
                next_seq: 0,
            },
        );
        (orx, srx, lrx)
    }

    /// Forget a job (start failed).
    pub(crate) fn forget_job(&self, job: &str) {
        self.inner.lock_jobs().remove(job);
    }

    /// True while the job has not reported its exit.
    pub(crate) fn job_active(&self, job: &str) -> bool {
        self.inner.lock_jobs().contains_key(job)
    }

    async fn ensure_live(&self) -> Result<Arc<Live>, ClientError> {
        if let Some(l) = self.inner.current() {
            return Ok(l);
        }
        let _guard = self.inner.connect_lock.lock().await;
        if let Some(l) = self.inner.current() {
            return Ok(l);
        }
        if self.inner.closed.load(Ordering::SeqCst) {
            return Err(ClientError::Closed);
        }
        let reconnect = self.inner.ever_connected.load(Ordering::SeqCst);
        let max = if reconnect {
            self.inner.opts.reconnect_attempts.max(1)
        } else {
            1
        };
        if !reconnect {
            self.inner.emit(ConnectionState::Connecting);
        }
        let mut last = ClientError::Disconnected;
        for attempt in 1..=max {
            if reconnect {
                self.inner
                    .emit(ConnectionState::Reconnecting { attempt, max });
                tokio::time::sleep(self.inner.opts.reconnect_delay * attempt).await;
            }
            match self.open().await {
                Ok(live) => {
                    *self.inner.live.lock().unwrap_or_else(|e| e.into_inner()) = Some(live.clone());
                    self.inner.ever_connected.store(true, Ordering::SeqCst);
                    let reattached = if reconnect {
                        self.reattach(&live).await
                    } else {
                        0
                    };
                    self.inner.emit(ConnectionState::Connected {
                        reconnected: reconnect,
                        reattached,
                    });
                    return Ok(live);
                }
                Err(e @ ClientError::VersionMismatch { .. }) => {
                    last = e;
                    break;
                }
                Err(e) => last = e,
            }
        }
        self.inner.emit(ConnectionState::Failed {
            reason: last.to_string(),
        });
        if reconnect {
            self.inner.fail_all_jobs();
        }
        Err(last)
    }

    /// Resubscribe every job on a fresh connection; jobs the helper no
    /// longer knows are failed.
    async fn reattach(&self, live: &Arc<Live>) -> usize {
        let jobs: Vec<(String, u64)> = self
            .inner
            .lock_jobs()
            .iter()
            .map(|(k, s)| (k.clone(), s.next_seq))
            .collect();
        let mut ok = 0;
        for (job, from_seq) in jobs {
            let id = self.inner.next_id.fetch_add(1, Ordering::SeqCst);
            let params = serde_json::to_value(AttachParams {
                job: job.clone(),
                from_seq,
            })
            .unwrap_or(Value::Null);
            let r = live
                .request(
                    id,
                    methods::EXEC_ATTACH,
                    params,
                    None,
                    self.inner.opts.call_timeout,
                )
                .await
                .and_then(|(v, _)| {
                    serde_json::from_value::<AttachResult>(v)
                        .map_err(|e| ClientError::Decode(e.to_string()))
                });
            match r {
                Ok(_) => ok += 1,
                Err(e) => {
                    tracing::warn!(target: "harness::remote", %job, "reattach failed: {e}");
                    self.inner.lock_jobs().remove(&job);
                    self.inner.emit(ConnectionState::JobLost { job });
                }
            }
        }
        ok
    }

    async fn open(&self) -> Result<Arc<Live>, ClientError> {
        let host = self.inner.connector.host();
        let conn = self
            .inner
            .connector
            .connect(&self.inner.session)
            .await
            .map_err(|e| ClientError::Connect {
                host: host.clone(),
                message: e.to_string(),
            })?;
        let Connection {
            reader,
            writer,
            child,
            stderr,
        } = conn;
        let (tx, rx) = mpsc::unbounded_channel::<Outbound>();
        let live = Arc::new(Live {
            tx,
            pending: Mutex::new(HashMap::new()),
            alive: AtomicBool::new(true),
            tasks: Mutex::new(Vec::new()),
            child: Mutex::new(child),
        });
        let writer_task = tokio::spawn(write_loop(writer, rx));
        let reader_task = tokio::spawn(read_loop(
            Arc::downgrade(&self.inner),
            Arc::downgrade(&live),
            BufReader::new(reader),
        ));
        {
            let mut t = live.tasks.lock().unwrap_or_else(|e| e.into_inner());
            t.push(writer_task);
            t.push(reader_task);
        }
        let id = self.inner.next_id.fetch_add(1, Ordering::SeqCst);
        let wire = self.inner.opts.compression;
        let offer = (wire.enabled && !self.inner.connector.transport_compressed()).then(|| {
            CompressionOffer {
                algorithms: vec![ZSTD.to_owned()],
                level: wire.level,
                min_size: wire.min_size,
            }
        });
        let params = serde_json::to_value(HelloParams {
            protocol: MIN_PROTOCOL_VERSION,
            version: HELPER_VERSION.to_owned(),
            max_protocol: Some(PROTOCOL_VERSION),
            compression: offer,
        })
        .unwrap_or(Value::Null);
        let hello = live
            .request(id, methods::HELLO, params, None, Duration::from_secs(60))
            .await;
        let hello: HelloResult = match hello {
            Ok((v, _)) => {
                serde_json::from_value(v).map_err(|e| ClientError::Decode(e.to_string()))?
            }
            Err(ClientError::Rpc(e)) if e.code == codes::VERSION_MISMATCH => {
                let data = e.data.unwrap_or_default();
                return Err(ClientError::VersionMismatch {
                    local: HELPER_VERSION.to_owned(),
                    remote: data["version"].as_str().unwrap_or("?").to_owned(),
                    protocol: data["protocol"].as_u64().unwrap_or(0) as u32,
                });
            }
            Err(e) => return Err(self.explain_failure(&live, stderr, e).await),
        };
        if !(MIN_PROTOCOL_VERSION..=PROTOCOL_VERSION).contains(&hello.protocol)
            || self
                .inner
                .opts
                .expected_version
                .as_ref()
                .is_some_and(|v| *v != hello.version)
        {
            return Err(ClientError::VersionMismatch {
                local: self
                    .inner
                    .opts
                    .expected_version
                    .clone()
                    .unwrap_or_else(|| HELPER_VERSION.to_owned()),
                remote: hello.version,
                protocol: hello.protocol,
            });
        }
        let framing = if hello.protocol >= 2 {
            Framing::Binary {
                zstd: (hello.compression.as_deref() == Some(ZSTD)).then_some(Zstd {
                    level: wire.level,
                    min_size: wire.min_size,
                }),
            }
        } else {
            Framing::Lines
        };
        tracing::debug!(target: "harness::remote", host = %host, protocol = hello.protocol, ?framing, "helper framing");
        if live
            .tx
            .send(Outbound::Switch(Encoder::new(framing)))
            .is_err()
        {
            return Err(ClientError::Disconnected);
        }
        {
            // A different helper process holds a different (empty) cache.
            let mut c = self.lock_cache();
            if c.peer_pid != Some(hello.pid) || hello.content_cache.is_none() {
                let cap = hello.content_cache.map_or(0, |b| b as usize);
                c.peer.reset(cap);
                c.peer_pid = hello.content_cache.map(|_| hello.pid);
            }
        }
        *self.inner.hello.lock().unwrap_or_else(|e| e.into_inner()) = Some(hello);
        if let Some(every) = self.inner.opts.keepalive {
            let t = tokio::spawn(keepalive(
                Arc::downgrade(&self.inner),
                Arc::downgrade(&live),
                every,
            ));
            live.tasks.lock().unwrap_or_else(|e| e.into_inner()).push(t);
        }
        Ok(live)
    }

    /// Turn a failed handshake into an actionable error using the exit code
    /// and stderr of the transport process (ssh).
    async fn explain_failure(
        &self,
        live: &Live,
        stderr: Option<Arc<Mutex<String>>>,
        err: ClientError,
    ) -> ClientError {
        let child = live.child.lock().unwrap_or_else(|e| e.into_inner()).take();
        let Some(mut child) = child else { return err };
        let code = match tokio::time::timeout(Duration::from_secs(5), child.wait()).await {
            Ok(Ok(st)) => st.code(),
            _ => {
                let _ = child.start_kill();
                None
            }
        };
        // Let the stderr reader drain.
        tokio::time::sleep(Duration::from_millis(50)).await;
        let text = stderr
            .map(|s| s.lock().unwrap_or_else(|e| e.into_inner()).clone())
            .unwrap_or_default();
        if code == Some(255) || !text.trim().is_empty() {
            let host = self.inner.connector.host();
            return match crate::ssh::classify(&host, code, &text) {
                SshError::Failed { stderr, .. } if code != Some(255) => ClientError::Connect {
                    host,
                    message: format!("helper exited ({code:?}): {stderr}"),
                },
                e => ClientError::Ssh(e),
            };
        }
        err
    }
}

fn on_disconnect(inner: &Arc<Inner>, live: &Arc<Live>, reason: &str) {
    if !live.alive.swap(false, Ordering::SeqCst) {
        return;
    }
    live.fail_pending();
    if let Some(c) = live
        .child
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_mut()
    {
        let _ = c.start_kill();
    }
    inner.emit(ConnectionState::Disconnected {
        reason: reason.to_owned(),
    });
    let has_jobs = !inner.lock_jobs().is_empty();
    if has_jobs && !inner.closed.load(Ordering::SeqCst) {
        let client = RemoteClient {
            inner: inner.clone(),
        };
        tokio::spawn(async move {
            let _ = client.ensure_live().await;
        });
    }
}

async fn read_loop<R: AsyncRead + Unpin>(
    inner: Weak<Inner>,
    live: Weak<Live>,
    mut reader: BufReader<R>,
) {
    let reason = loop {
        let frame = match read_wire_frame(&mut reader, MAX_FRAME).await {
            Ok(Some(f)) => f,
            Ok(None) => break "connection closed".to_owned(),
            Err(e) => break e.to_string(),
        };
        let (Some(inner), Some(live)) = (inner.upgrade(), live.upgrade()) else {
            return;
        };
        match Message::parse(&frame.json) {
            Ok(Message::Response { id, result }) => {
                let tx = live
                    .pending
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .remove(&id);
                if let Some(tx) = tx {
                    let data = frame.data;
                    let _ = tx.send(result.map(|v| (v, data)).map_err(ClientError::Rpc));
                }
            }
            Ok(Message::Notification { method, params }) => {
                dispatch_note(&inner, &live, &method, params, frame.data)
            }
            Ok(Message::Request { .. }) => {}
            Err(e) => tracing::warn!(target: "harness::remote", "bad frame from helper: {e}"),
        }
    };
    if let (Some(inner), Some(live)) = (inner.upgrade(), live.upgrade()) {
        on_disconnect(&inner, &live, &reason);
    }
}

fn dispatch_note(inner: &Inner, live: &Live, method: &str, params: Value, data: Option<Vec<u8>>) {
    match method {
        methods::EXEC_OUTPUT => {
            let Ok(mut n) = serde_json::from_value::<ExecOutputNote>(params) else {
                return;
            };
            if let Some(d) = data {
                n.data = d;
            }
            let mut jobs = inner.lock_jobs();
            if let Some(sink) = jobs.get_mut(&n.job)
                && n.seq >= sink.next_seq
            {
                sink.next_seq = n.seq + 1;
                let _ = sink.output.send(OutputChunk {
                    stream: n.stream.into(),
                    data: n.data,
                });
            }
        }
        methods::EXEC_LIVENESS => {
            let Ok(n) = serde_json::from_value::<ExecLivenessNote>(params) else {
                return;
            };
            if let Some(sink) = inner.lock_jobs().get(&n.job) {
                let _ = sink.liveness.send(Liveness {
                    cpu: Duration::from_millis(n.cpu_ms),
                    procs: n.procs,
                    active: n.active,
                });
            }
        }
        methods::EXEC_EXIT => {
            let Ok(n) = serde_json::from_value::<ExecExitNote>(params) else {
                return;
            };
            let sink = {
                let mut jobs = inner.lock_jobs();
                match jobs.get(&n.job) {
                    Some(s) if n.seq >= s.next_seq => jobs.remove(&n.job),
                    _ => None,
                }
            };
            if let Some(mut sink) = sink {
                drop(sink.output);
                if let Some(st) = sink.status.take() {
                    let _ = st.send(n.outcome.into());
                }
                let _ = live
                    .tx
                    .send(Outbound::Frame(Frame::new(Message::notification(
                        methods::EXEC_RELEASE,
                        JobParams { job: n.job },
                    ))));
            }
        }
        _ => {}
    }
}

async fn keepalive(inner: Weak<Inner>, live: Weak<Live>, every: Duration) {
    loop {
        tokio::time::sleep(every).await;
        let (Some(i), Some(l)) = (inner.upgrade(), live.upgrade()) else {
            return;
        };
        if !l.alive.load(Ordering::SeqCst) {
            return;
        }
        let id = i.next_id.fetch_add(1, Ordering::SeqCst);
        let r = l
            .request(
                id,
                methods::PING,
                serde_json::json!({}),
                None,
                every.max(Duration::from_secs(1)),
            )
            .await;
        if matches!(r, Err(ClientError::Timeout(_))) {
            on_disconnect(&i, &l, "keepalive timed out");
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{read_frame, write_frame};

    #[tokio::test]
    async fn connects_and_handshakes_in_process() {
        let c = InProcessConnector::new(Helper::new(Default::default()));
        let client = RemoteClient::connect(Arc::new(c), ClientOptions::new("box"))
            .await
            .unwrap();
        let h = client.hello().unwrap();
        assert_eq!(h.protocol, PROTOCOL_VERSION);
        assert_eq!(h.version, HELPER_VERSION);
        client.ping().await.unwrap();
        assert!(client.is_connected());
    }

    #[tokio::test]
    async fn generic_calls_decode_binary_payloads_on_protocol_2() {
        use crate::protocol::{DataResult, PathParams, WriteParams};
        let c = InProcessConnector::new(Helper::new(Default::default()));
        let client = RemoteClient::connect(Arc::new(c), ClientOptions::new("box"))
            .await
            .unwrap();
        assert_eq!(client.hello().unwrap().protocol, 2);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("f").to_string_lossy().into_owned();
        let data = vec![7u8; 10_000];
        // Typed params with the bytes inside still work (base64 in JSON).
        client
            .call::<_, Value>(
                methods::FS_WRITE,
                None,
                WriteParams {
                    path: path.clone(),
                    data: data.clone(),
                    hash: None,
                    cached: false,
                },
            )
            .await
            .unwrap();
        let r: DataResult = client
            .call(methods::FS_READ, None, PathParams { path: path.clone() })
            .await
            .unwrap();
        assert_eq!(r.data, data);
        let (r, raw): (DataResult, _) = client
            .call_data(methods::FS_READ, None, PathParams { path }, None)
            .await
            .unwrap();
        assert!(r.data.is_empty(), "payload is not in the JSON");
        assert_eq!(raw.unwrap(), data);
    }

    #[tokio::test]
    async fn version_mismatch_is_reported() {
        let helper = Helper::new(crate::helper::HelperOptions {
            version: "0.0.1".into(),
            ..Default::default()
        });
        let c = InProcessConnector::new(helper);
        let err = RemoteClient::connect(Arc::new(c), ClientOptions::new("box"))
            .await
            .unwrap_err();
        assert!(
            matches!(err, ClientError::VersionMismatch { ref remote, .. } if remote == "0.0.1"),
            "{err}"
        );
    }

    #[tokio::test]
    async fn connect_failure_is_reported() {
        let c = InProcessConnector::new(Helper::new(Default::default()));
        c.set_refuse(true);
        let err = RemoteClient::connect(Arc::new(c), ClientOptions::new("box"))
            .await
            .unwrap_err();
        assert!(matches!(err, ClientError::Connect { .. }), "{err}");
    }

    #[tokio::test]
    async fn unknown_method_is_an_rpc_error() {
        let c = InProcessConnector::new(Helper::new(Default::default()));
        let client = RemoteClient::connect(Arc::new(c), ClientOptions::new("box"))
            .await
            .unwrap();
        let err = client
            .call::<_, Value>("nope", None, serde_json::json!({}))
            .await
            .unwrap_err();
        assert_eq!(err.rpc_code(), Some(codes::METHOD_NOT_FOUND));
    }

    #[tokio::test]
    async fn lifecycle_events_on_disconnect_and_reconnect() {
        let c = InProcessConnector::new(Helper::new(Default::default()));
        let mut opts = ClientOptions::new("box");
        opts.reconnect_delay = Duration::from_millis(10);
        let client = RemoteClient::connect(Arc::new(c.clone()), opts)
            .await
            .unwrap();
        let mut ev = client.subscribe();
        c.disconnect_all();
        let e = tokio::time::timeout(Duration::from_secs(5), ev.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(e.host, "box");
        assert!(
            matches!(e.state, ConnectionState::Disconnected { .. }),
            "{e:?}"
        );
        // The next call reconnects.
        client.ping().await.unwrap();
        let mut states = Vec::new();
        while let Ok(e) = ev.try_recv() {
            states.push(e.state);
        }
        assert!(
            states.contains(&ConnectionState::Reconnecting { attempt: 1, max: 3 }),
            "{states:?}"
        );
        assert!(states.iter().any(|s| matches!(
            s,
            ConnectionState::Connected {
                reconnected: true,
                ..
            }
        )));
    }

    #[tokio::test]
    async fn keepalive_detects_a_hung_helper() {
        // A "helper" that answers hello and then never again.
        struct Hung;
        #[async_trait]
        impl Connector for Hung {
            async fn connect(&self, _s: &str) -> io::Result<Connection> {
                let (client_side, helper_side) = tokio::io::duplex(1 << 16);
                let (hr, mut hw) = tokio::io::split(helper_side);
                tokio::spawn(async move {
                    let mut r = BufReader::new(hr);
                    let line = read_frame(&mut r).await.unwrap().unwrap();
                    let Message::Request { id, .. } = Message::parse(&line).unwrap() else {
                        panic!()
                    };
                    let hello = HelloResult {
                        protocol: PROTOCOL_VERSION,
                        version: HELPER_VERSION.into(),
                        arch: "x".into(),
                        os: "y".into(),
                        session: "s".into(),
                        pid: 1,
                        jobs: 0,
                        home: None,
                        compression: None,
                        content_cache: None,
                    };
                    let resp = Message::Response {
                        id,
                        result: Ok(serde_json::to_value(hello).unwrap()),
                    };
                    write_frame(&mut hw, &resp.to_line()).await.unwrap();
                    // Swallow everything, answer nothing.
                    while let Ok(Some(_)) = read_frame(&mut r).await {}
                    drop(hw);
                });
                let (cr, cw) = tokio::io::split(client_side);
                Ok(Connection {
                    reader: Box::new(cr),
                    writer: Box::new(cw),
                    child: None,
                    stderr: None,
                })
            }
            fn host(&self) -> String {
                "hung".into()
            }
        }
        let mut opts = ClientOptions::new("hung");
        opts.keepalive = Some(Duration::from_millis(100));
        let client = RemoteClient::connect(Arc::new(Hung), opts).await.unwrap();
        let mut ev = client.subscribe();
        let e = tokio::time::timeout(Duration::from_secs(5), ev.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            e.state,
            ConnectionState::Disconnected {
                reason: "keepalive timed out".into()
            }
        );
        assert!(!client.is_connected());
    }

    #[tokio::test]
    async fn secrets_are_blocked_before_sending() {
        let c = InProcessConnector::new(Helper::new(Default::default()));
        let mut opts = ClientOptions::new("box");
        opts.guard = SecretGuard::new(["sk-test-secret-value".to_string()]);
        let client = RemoteClient::connect(Arc::new(c), opts).await.unwrap();
        let err = client
            .call::<_, Value>(
                methods::FS_STAT,
                None,
                serde_json::json!({"path": "/sk-test-secret-value"}),
            )
            .await
            .unwrap_err();
        assert!(matches!(err, ClientError::SecretBlocked { .. }));
    }
}
