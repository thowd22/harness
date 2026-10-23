//! JSON-RPC protocol between harness and the remote helper.
//!
//! The helper (`harness remote-agent --stdio`) and the client speak
//! [JSON-RPC 2.0](https://www.jsonrpc.org/specification) over the SSH
//! channel. Protocol 1 sends each message as one JSON object on one line
//! (newline-delimited JSON), with binary payloads (file contents, process
//! output) as base64 strings. Protocol 2 keeps plain messages as JSON lines
//! but sends binary payloads as length-prefixed raw bytes and compresses
//! large frames with zstd; see [`crate::wire`] for the framing.
//!
//! The first request on every connection is [`methods::HELLO`], always a
//! protocol 1 JSON line. It carries the protocol range the client speaks
//! ([`HelloParams::protocol`] up to [`HelloParams::max_protocol`]), its
//! compression offer and the client's harness version, so either side can
//! detect a mismatch (the client then upgrades the helper, see
//! [`crate::install`]). The helper answers with the protocol and
//! compression it chose; both sides switch framing right after the answer.
//! A protocol 1 helper ignores the new fields and answers protocol 1, so
//! old helpers keep working, uncompressed.
//!
//! Server-to-client notifications ([`methods::EXEC_OUTPUT`],
//! [`methods::EXEC_EXIT`]) carry a per-job sequence number so a client that
//! reattaches after a disconnect can resume without duplicates.
//! [`methods::EXEC_LIVENESS`] samples are ephemeral: sent only to the
//! attached client, never logged or replayed, and ignored by clients that
//! predate them.

use std::io;
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use harness_tools::exec::{ExitOutcome, StreamKind};
use harness_tools::workspace::{DirEntry, FileKind, FileStat, WsError};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt};

use crate::probe::ProbeReport;
use crate::sync::{ChangeSet, Manifest};

/// Newest wire protocol version this build speaks. Bump on any
/// incompatible change. Version 2 added binary payloads and negotiated
/// zstd compression.
pub const PROTOCOL_VERSION: u32 = 2;

/// Oldest wire protocol version this build still speaks (newline-delimited
/// JSON with base64 payloads, uncompressed).
pub const MIN_PROTOCOL_VERSION: u32 = 1;

/// Name of the zstd frame compression in the `hello` negotiation.
pub const ZSTD: &str = "zstd";

/// Default zstd level for helper frames: low, so compression stays faster
/// than the link up to roughly gigabit speeds.
pub const DEFAULT_ZSTD_LEVEL: i32 = 1;

/// Frames whose body (JSON plus binary payload) is at least this many bytes
/// are compressed when compression was negotiated; smaller ones are sent
/// as-is.
pub const DEFAULT_COMPRESS_MIN: usize = 4 * 1024;

/// Version of this harness build, which is also the helper version.
pub const HELPER_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Largest accepted frame, in bytes: one JSON line, or for protocol 2 the
/// *decompressed* body of a binary frame (so a small compressed frame
/// cannot expand past it).
pub const MAX_FRAME: usize = 64 * 1024 * 1024;

/// Method and notification names.
pub mod methods {
    /// Handshake: `HelloParams` -> `HelloResult`.
    pub const HELLO: &str = "hello";
    /// Liveness check; returns `{}`.
    pub const PING: &str = "ping";
    /// Read a file: `PathParams` -> `DataResult`.
    pub const FS_READ: &str = "fs.read";
    /// Write (truncate) a file: `WriteParams`.
    pub const FS_WRITE: &str = "fs.write";
    /// Atomically replace a file: `WriteParams`.
    pub const FS_REPLACE: &str = "fs.replace_atomic";
    /// chmod: `SetModeParams`.
    pub const FS_SET_MODE: &str = "fs.set_mode";
    /// stat (following links): `PathParams` -> `WireStat`.
    pub const FS_STAT: &str = "fs.stat";
    /// List a directory: `PathParams` -> `ListResult`.
    pub const FS_LIST: &str = "fs.list";
    /// Glob under a root: `GlobParams` -> `GlobResult`.
    pub const FS_GLOB: &str = "fs.glob";
    /// File search for the grep and glob tools:
    /// `harness_tools::search::FindQuery` -> `FindResult`. Helpers that
    /// predate it answer `METHOD_NOT_FOUND`.
    pub const FS_FIND: &str = "fs.find";
    /// Start a process: `ExecStartParams`.
    pub const EXEC_START: &str = "exec.start";
    /// Kill a process group: `JobParams`.
    pub const EXEC_KILL: &str = "exec.kill";
    /// Resubscribe to a job after reconnecting: `AttachParams` -> `AttachResult`.
    pub const EXEC_ATTACH: &str = "exec.attach";
    /// Forget a finished job: `JobParams`.
    pub const EXEC_RELEASE: &str = "exec.release";
    /// List jobs: -> `JobsResult`.
    pub const EXEC_LIST: &str = "exec.list";
    /// Notification: process output (`ExecOutputNote`).
    pub const EXEC_OUTPUT: &str = "exec.output";
    /// Notification: process exit (`ExecExitNote`).
    pub const EXEC_EXIT: &str = "exec.exit";
    /// Notification: liveness sample of a running job (`ExecLivenessNote`).
    pub const EXEC_LIVENESS: &str = "exec.liveness";
    /// Host resources: `ProbeParams` -> `ProbeReport`.
    pub const PROBE: &str = "probe";
    /// Manifest of a directory tree: `RootParams` -> `Manifest`.
    pub const SYNC_MANIFEST: &str = "sync.manifest";
    /// Changes since a baseline manifest: `ChangesParams` -> `ChangeSet`.
    pub const SYNC_CHANGES: &str = "sync.changes";
    /// Remove a run directory: `RemoveParams`.
    pub const SYNC_REMOVE: &str = "sync.remove";
    /// Remove old run directories: `PruneParams` -> `PruneResult`.
    pub const SYNC_PRUNE: &str = "sync.prune";
}

/// JSON-RPC error codes used by the helper.
pub mod codes {
    /// Invalid JSON.
    pub const PARSE_ERROR: i64 = -32700;
    /// Not a valid request object.
    pub const INVALID_REQUEST: i64 = -32600;
    /// Unknown method.
    pub const METHOD_NOT_FOUND: i64 = -32601;
    /// Bad parameters.
    pub const INVALID_PARAMS: i64 = -32602;
    /// Path does not exist.
    pub const NOT_FOUND: i64 = -32001;
    /// Permission denied.
    pub const PERMISSION_DENIED: i64 = -32002;
    /// Path is a directory.
    pub const IS_DIRECTORY: i64 = -32003;
    /// Path is not a directory.
    pub const NOT_A_DIRECTORY: i64 = -32004;
    /// Invalid glob pattern.
    pub const INVALID_PATTERN: i64 = -32005;
    /// Other I/O error.
    pub const IO: i64 = -32006;
    /// The process could not be started.
    pub const SPAWN: i64 = -32010;
    /// Unknown job id (for example the helper restarted).
    pub const UNKNOWN_JOB: i64 = -32011;
    /// The protocol version is not supported.
    pub const VERSION_MISMATCH: i64 = -32020;
    /// The operation is refused (for example a path outside the allowed base).
    pub const REFUSED: i64 = -32030;
    /// A write named cached content (`cached: true`) the helper does not
    /// hold; resend it with the bytes.
    pub const CACHE_MISS: i64 = -32040;
    /// The bytes of a write do not match the `hash` sent with them; resend
    /// without the hash.
    pub const HASH_MISMATCH: i64 = -32041;
}

/// A JSON-RPC error object.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, thiserror::Error)]
#[error("{message} (code {code})")]
pub struct RpcError {
    /// Error code (see [`codes`]).
    pub code: i64,
    /// Human-readable message.
    pub message: String,
    /// Optional structured data.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

impl RpcError {
    /// An error with `code` and `message`.
    pub fn new(code: i64, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            data: None,
        }
    }

    /// Invalid parameters.
    pub fn invalid_params(message: impl Into<String>) -> Self {
        Self::new(codes::INVALID_PARAMS, message)
    }

    /// Map a workspace error onto a wire error, keeping the path in `data`.
    pub fn from_ws(err: &WsError) -> Self {
        if let WsError::InvalidPattern { pattern, reason } = err {
            let mut e = Self::new(codes::INVALID_PATTERN, err.to_string());
            e.data = Some(json!({ "pattern": pattern, "reason": reason }));
            return e;
        }
        let (code, path) = match err {
            WsError::NotFound(p) => (codes::NOT_FOUND, Some(p)),
            WsError::PermissionDenied(p) => (codes::PERMISSION_DENIED, Some(p)),
            WsError::IsDirectory(p) => (codes::IS_DIRECTORY, Some(p)),
            WsError::NotADirectory(p) => (codes::NOT_A_DIRECTORY, Some(p)),
            WsError::InvalidPattern { .. } => (codes::INVALID_PATTERN, None),
            WsError::Io { path, .. } => (codes::IO, Some(path)),
        };
        let mut e = Self::new(code, err.to_string());
        if let Some(p) = path {
            e.data = Some(json!({ "path": p.to_string_lossy() }));
        }
        e
    }

    /// Map a wire error back to a workspace error for `path`.
    pub fn into_ws(self, path: impl Into<PathBuf>) -> WsError {
        let path = path.into();
        match self.code {
            codes::NOT_FOUND => WsError::NotFound(path),
            codes::PERMISSION_DENIED => WsError::PermissionDenied(path),
            codes::IS_DIRECTORY => WsError::IsDirectory(path),
            codes::NOT_A_DIRECTORY => WsError::NotADirectory(path),
            codes::INVALID_PATTERN => {
                let field = |k: &str| {
                    self.data
                        .as_ref()
                        .and_then(|d| d.get(k))
                        .and_then(|v| v.as_str())
                        .map(str::to_owned)
                };
                match (field("pattern"), field("reason")) {
                    (Some(pattern), Some(reason)) => WsError::InvalidPattern { pattern, reason },
                    _ => WsError::InvalidPattern {
                        pattern: path.to_string_lossy().into_owned(),
                        reason: self.message,
                    },
                }
            }
            _ => WsError::Io {
                path,
                source: io::Error::other(self.message),
            },
        }
    }
}

/// One decoded protocol message.
#[derive(Debug, Clone, PartialEq)]
pub enum Message {
    /// A call expecting a response.
    Request {
        /// Request id, echoed by the response.
        id: u64,
        /// Method name.
        method: String,
        /// Parameters (`null` if absent).
        params: Value,
    },
    /// A one-way message.
    Notification {
        /// Method name.
        method: String,
        /// Parameters.
        params: Value,
    },
    /// The answer to a request.
    Response {
        /// Id of the request.
        id: u64,
        /// Result or error.
        result: Result<Value, RpcError>,
    },
}

impl Message {
    /// A request.
    pub fn request(id: u64, method: &str, params: impl Serialize) -> Self {
        Message::Request {
            id,
            method: method.to_owned(),
            params: serde_json::to_value(params).unwrap_or(Value::Null),
        }
    }

    /// A notification.
    pub fn notification(method: &str, params: impl Serialize) -> Self {
        Message::Notification {
            method: method.to_owned(),
            params: serde_json::to_value(params).unwrap_or(Value::Null),
        }
    }

    /// Encode as one JSON line without the trailing newline.
    pub fn to_line(&self) -> String {
        self.to_value().to_string()
    }

    /// The JSON-RPC object for this message.
    pub fn to_value(&self) -> Value {
        match self {
            Message::Request { id, method, params } => {
                json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params})
            }
            Message::Notification { method, params } => {
                json!({"jsonrpc": "2.0", "method": method, "params": params})
            }
            Message::Response { id, result: Ok(r) } => {
                json!({"jsonrpc": "2.0", "id": id, "result": r})
            }
            Message::Response { id, result: Err(e) } => {
                json!({"jsonrpc": "2.0", "id": id, "error": e})
            }
        }
    }

    /// Decode one line.
    pub fn parse(line: &str) -> Result<Message, RpcError> {
        let v: Value = serde_json::from_str(line)
            .map_err(|e| RpcError::new(codes::PARSE_ERROR, format!("invalid JSON: {e}")))?;
        let obj = v
            .as_object()
            .ok_or_else(|| RpcError::new(codes::INVALID_REQUEST, "message is not an object"))?;
        let id = obj.get("id").and_then(Value::as_u64);
        let params = obj.get("params").cloned().unwrap_or(Value::Null);
        if let Some(method) = obj.get("method").and_then(Value::as_str) {
            let method = method.to_owned();
            return Ok(match id {
                Some(id) => Message::Request { id, method, params },
                None => Message::Notification { method, params },
            });
        }
        let id = id.ok_or_else(|| RpcError::new(codes::INVALID_REQUEST, "response without id"))?;
        if let Some(err) = obj.get("error") {
            let err: RpcError = serde_json::from_value(err.clone())
                .map_err(|e| RpcError::new(codes::INVALID_REQUEST, format!("bad error: {e}")))?;
            return Ok(Message::Response {
                id,
                result: Err(err),
            });
        }
        Ok(Message::Response {
            id,
            result: Ok(obj.get("result").cloned().unwrap_or(Value::Null)),
        })
    }
}

/// Read one newline-terminated protocol 1 frame. `Ok(None)` at end of
/// stream.
///
/// Frames longer than [`MAX_FRAME`] are an error, so a misbehaving peer
/// cannot exhaust memory. [`crate::wire::read_wire_frame`] reads both
/// protocol 1 and protocol 2 frames.
pub async fn read_frame<R: AsyncBufRead + Unpin>(reader: &mut R) -> io::Result<Option<String>> {
    read_line_limited(reader, MAX_FRAME).await
}

/// Read one newline-terminated line of at most `max` bytes (without the
/// newline). `Ok(None)` at end of stream.
pub(crate) async fn read_line_limited<R: AsyncBufRead + Unpin>(
    reader: &mut R,
    max: usize,
) -> io::Result<Option<String>> {
    let mut buf = Vec::new();
    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            if buf.is_empty() {
                return Ok(None);
            }
            break;
        }
        if let Some(pos) = available.iter().position(|&b| b == b'\n') {
            buf.extend_from_slice(&available[..pos]);
            reader.consume(pos + 1);
            break;
        }
        let n = available.len();
        buf.extend_from_slice(available);
        reader.consume(n);
        if buf.len() > max {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "protocol frame too large",
            ));
        }
    }
    String::from_utf8(buf)
        .map(Some)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "frame is not UTF-8"))
}

/// Write one frame (adds the newline) and flush.
pub async fn write_frame<W: AsyncWrite + Unpin>(writer: &mut W, line: &str) -> io::Result<()> {
    writer.write_all(line.as_bytes()).await?;
    writer.write_all(b"\n").await?;
    writer.flush().await
}

/// Serde helper: `Vec<u8>` as a base64 string.
///
/// Protocol 1 carries every binary payload this way. Protocol 2 sends the
/// payload as raw bytes after the JSON instead (see [`crate::wire`]), so
/// the `data` fields using this helper are absent from protocol 2 JSON and
/// deserialize as empty; the bytes come from [`crate::wire::Frame::data`].
pub mod b64 {
    use base64::Engine;
    use base64::engine::general_purpose::STANDARD;
    use serde::{Deserialize, Deserializer, Serializer};

    /// Serialize bytes as base64.
    pub fn serialize<S: Serializer>(data: &[u8], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&STANDARD.encode(data))
    }

    /// Deserialize base64 into bytes.
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
        let s = String::deserialize(d)?;
        STANDARD.decode(s).map_err(serde::de::Error::custom)
    }

    /// Encode bytes (for tests and diagnostics).
    pub fn encode(data: &[u8]) -> String {
        STANDARD.encode(data)
    }
}

/// `hello` parameters.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HelloParams {
    /// Oldest protocol version the client speaks. Protocol 1 clients send
    /// their only version; newer clients send [`MIN_PROTOCOL_VERSION`] so a
    /// protocol 1 helper (which requires exactly 1) accepts them.
    pub protocol: u32,
    /// Client harness version.
    pub version: String,
    /// Newest protocol version the client speaks (absent: `protocol`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_protocol: Option<u32>,
    /// Frame compression the client accepts (absent: none). Only offered
    /// when the transport does not compress already.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compression: Option<CompressionOffer>,
}

/// The client's frame compression offer in `hello`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CompressionOffer {
    /// Algorithms in order of preference (currently only [`ZSTD`]).
    pub algorithms: Vec<String>,
    /// zstd level both sides use for the frames they send.
    pub level: i32,
    /// Frames with a smaller body are sent as-is.
    pub min_size: usize,
}

/// `hello` result.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HelloResult {
    /// Helper protocol version.
    pub protocol: u32,
    /// Helper harness version.
    pub version: String,
    /// Machine architecture (`std::env::consts::ARCH`).
    pub arch: String,
    /// Operating system (`std::env::consts::OS`).
    pub os: String,
    /// Helper session id (stable across reattaches).
    pub session: String,
    /// Helper process id.
    pub pid: u32,
    /// Number of jobs the helper is supervising.
    pub jobs: usize,
    /// The helper user's home directory, used to make `~/` paths absolute.
    #[serde(default)]
    pub home: Option<String>,
    /// Frame compression the helper chose from the client's offer
    /// (`None`: frames are not compressed). Absent from protocol 1 helpers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compression: Option<String>,
    /// Size in bytes of the helper's per-session content-hash cache
    /// (`None`: the helper has none, so the client never sends a write by
    /// hash). See [`crate::cache`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_cache: Option<u64>,
}

/// A single path parameter (absolute on the helper host; a leading `~/`
/// is expanded by the helper).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PathParams {
    /// Path.
    pub path: String,
}

/// `fs.read` parameters (a superset of [`PathParams`]).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReadParams {
    /// Path.
    pub path: String,
    /// Content hash of the copy the client holds for this path: when the
    /// file still has it, the helper answers `cached` without the bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub have: Option<String>,
}

/// File bytes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DataResult {
    /// Contents (empty when `cached`).
    #[serde(with = "b64", default)]
    pub data: Vec<u8>,
    /// Content hash of the file (absent from helpers without a content
    /// cache).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hash: Option<String>,
    /// The file's content is the client's `have` copy; no bytes were sent.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub cached: bool,
}

/// `fs.write` / `fs.replace_atomic` parameters.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WriteParams {
    /// Path.
    pub path: String,
    /// New contents (empty when `cached`).
    #[serde(with = "b64", default)]
    pub data: Vec<u8>,
    /// Content hash of the new contents. With bytes, the helper checks
    /// them against it ([`codes::HASH_MISMATCH`]) and caches them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hash: Option<String>,
    /// Write the helper's cached copy of `hash` instead of sent bytes
    /// ([`codes::CACHE_MISS`] when it holds none).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub cached: bool,
}

/// `fs.set_mode` parameters.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SetModeParams {
    /// Path.
    pub path: String,
    /// Permission bits.
    pub mode: u32,
}

/// File kind on the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WireKind {
    /// Regular file.
    File,
    /// Directory.
    Dir,
    /// Symbolic link.
    Symlink,
    /// Anything else.
    Other,
}

impl From<FileKind> for WireKind {
    fn from(k: FileKind) -> Self {
        match k {
            FileKind::File => WireKind::File,
            FileKind::Dir => WireKind::Dir,
            FileKind::Symlink => WireKind::Symlink,
            FileKind::Other => WireKind::Other,
        }
    }
}

impl From<WireKind> for FileKind {
    fn from(k: WireKind) -> Self {
        match k {
            WireKind::File => FileKind::File,
            WireKind::Dir => FileKind::Dir,
            WireKind::Symlink => FileKind::Symlink,
            WireKind::Other => FileKind::Other,
        }
    }
}

/// `fs.stat` result.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WireStat {
    /// Kind.
    pub kind: WireKind,
    /// Size in bytes.
    pub len: u64,
    /// Permission bits.
    pub mode: Option<u32>,
    /// Modification time in milliseconds since the Unix epoch.
    pub modified_ms: Option<u64>,
}

impl From<&FileStat> for WireStat {
    fn from(s: &FileStat) -> Self {
        Self {
            kind: s.kind.into(),
            len: s.len,
            mode: s.mode,
            modified_ms: s
                .modified
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map(|d| d.as_millis() as u64),
        }
    }
}

impl From<WireStat> for FileStat {
    fn from(s: WireStat) -> Self {
        Self {
            kind: s.kind.into(),
            len: s.len,
            mode: s.mode,
            modified: s
                .modified_ms
                .map(|ms| SystemTime::UNIX_EPOCH + Duration::from_millis(ms)),
        }
    }
}

/// One directory entry on the wire.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WireEntry {
    /// File name.
    pub name: String,
    /// Kind (links not followed).
    pub kind: WireKind,
    /// Size in bytes.
    pub len: u64,
}

impl From<&DirEntry> for WireEntry {
    fn from(e: &DirEntry) -> Self {
        Self {
            name: e.name.clone(),
            kind: e.kind.into(),
            len: e.len,
        }
    }
}

impl From<WireEntry> for DirEntry {
    fn from(e: WireEntry) -> Self {
        Self {
            name: e.name,
            kind: e.kind.into(),
            len: e.len,
        }
    }
}

/// `fs.list` result.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ListResult {
    /// Entries sorted by name.
    pub entries: Vec<WireEntry>,
}

/// `fs.glob` parameters.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GlobParams {
    /// Root directory.
    pub root: String,
    /// globset pattern relative to the root.
    pub pattern: String,
}

/// `fs.glob` result.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GlobResult {
    /// Root-relative paths, sorted.
    pub paths: Vec<String>,
}

/// `exec.start` parameters.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExecStartParams {
    /// Client-chosen job id (unique per helper session).
    pub job: String,
    /// Program.
    pub program: String,
    /// Arguments.
    pub args: Vec<String>,
    /// Working directory.
    pub cwd: String,
    /// Variables to set.
    pub env: Vec<(String, String)>,
    /// Start from an empty environment.
    pub clear_env: bool,
    /// Names whose values the helper copies from its own (remote login)
    /// environment when `clear_env` is set, e.g. `PATH` and `HOME`.
    #[serde(default)]
    pub host_env: Vec<String>,
    /// Timeout in milliseconds.
    pub timeout_ms: Option<u64>,
}

/// A job id parameter.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JobParams {
    /// Job id.
    pub job: String,
}

/// `exec.attach` parameters.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AttachParams {
    /// Job id.
    pub job: String,
    /// First sequence number the client has not seen.
    pub from_seq: u64,
}

/// `exec.attach` result.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AttachResult {
    /// True if the process is still running.
    pub running: bool,
}

/// Output stream on the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WireStream {
    /// stdout.
    Stdout,
    /// stderr.
    Stderr,
}

impl From<StreamKind> for WireStream {
    fn from(k: StreamKind) -> Self {
        match k {
            StreamKind::Stdout => WireStream::Stdout,
            StreamKind::Stderr => WireStream::Stderr,
        }
    }
}

impl From<WireStream> for StreamKind {
    fn from(k: WireStream) -> Self {
        match k {
            WireStream::Stdout => StreamKind::Stdout,
            WireStream::Stderr => StreamKind::Stderr,
        }
    }
}

/// `exec.output` notification.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExecOutputNote {
    /// Job id.
    pub job: String,
    /// Per-job sequence number.
    pub seq: u64,
    /// Stream.
    pub stream: WireStream,
    /// Bytes.
    #[serde(with = "b64", default)]
    pub data: Vec<u8>,
}

/// Exit outcome on the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireOutcome {
    /// Exit code.
    pub code: Option<i32>,
    /// Terminating signal.
    pub signal: Option<i32>,
    /// Timed out.
    pub timed_out: bool,
    /// Killed on request.
    pub killed: bool,
}

impl From<ExitOutcome> for WireOutcome {
    fn from(o: ExitOutcome) -> Self {
        Self {
            code: o.code,
            signal: o.signal,
            timed_out: o.timed_out,
            killed: o.killed,
        }
    }
}

impl From<WireOutcome> for ExitOutcome {
    fn from(o: WireOutcome) -> Self {
        Self {
            code: o.code,
            signal: o.signal,
            timed_out: o.timed_out,
            killed: o.killed,
        }
    }
}

/// `exec.exit` notification (always the last message of a job).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExecExitNote {
    /// Job id.
    pub job: String,
    /// Per-job sequence number.
    pub seq: u64,
    /// How the process ended.
    pub outcome: WireOutcome,
}

/// `exec.liveness` notification: a periodic sample of a job's process
/// group (CPU time and process counts).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecLivenessNote {
    /// Job id.
    pub job: String,
    /// CPU time used by the group's live processes, in ms.
    pub cpu_ms: u64,
    /// Live processes in the group.
    pub procs: u32,
    /// Processes running, runnable or in uninterruptible wait.
    pub active: u32,
}

/// One job in `exec.list`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JobInfo {
    /// Job id.
    pub job: String,
    /// Program.
    pub program: String,
    /// Still running.
    pub running: bool,
}

/// `exec.list` result.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JobsResult {
    /// Jobs known to the helper.
    pub jobs: Vec<JobInfo>,
}

/// `probe` parameters.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ProbeParams {
    /// Directory whose filesystem is reported as `disk` (default: home).
    pub path: Option<String>,
}

/// `sync.manifest` parameters.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RootParams {
    /// Tree root.
    pub root: String,
}

/// `sync.changes` parameters.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChangesParams {
    /// Tree root.
    pub root: String,
    /// Manifest taken right after the sync.
    pub baseline: Manifest,
}

/// `sync.remove` parameters.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RemoveParams {
    /// Directory to remove.
    pub path: String,
    /// It must be strictly inside this directory.
    pub base: String,
}

/// `sync.prune` parameters.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PruneParams {
    /// Directory holding run directories.
    pub base: String,
    /// Keep at least this many most recent run directories.
    pub keep: usize,
    /// Remove run directories older than this (seconds), beyond `keep`.
    pub max_age_secs: Option<u64>,
}

/// `sync.prune` result.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PruneResult {
    /// Removed directory names.
    pub removed: Vec<String>,
}

/// Type aliases for results defined elsewhere, listed here so the protocol
/// surface is visible in one place.
pub type ProbeResult = ProbeReport;
/// `sync.changes` result.
pub type ChangesResult = ChangeSet;

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::BufReader;

    #[test]
    fn messages_roundtrip() {
        let msgs = [
            Message::request(1, methods::FS_READ, PathParams { path: "/a".into() }),
            Message::notification(methods::EXEC_RELEASE, JobParams { job: "j".into() }),
            Message::Response {
                id: 7,
                result: Ok(json!({"x": 1})),
            },
            Message::Response {
                id: 8,
                result: Err(RpcError::new(codes::NOT_FOUND, "gone")),
            },
        ];
        for m in msgs {
            let line = m.to_line();
            assert!(!line.contains('\n'));
            assert!(line.contains("\"jsonrpc\":\"2.0\""));
            assert_eq!(Message::parse(&line).unwrap(), m);
        }
    }

    #[test]
    fn parse_errors() {
        assert_eq!(Message::parse("{").unwrap_err().code, codes::PARSE_ERROR);
        assert_eq!(
            Message::parse("[]").unwrap_err().code,
            codes::INVALID_REQUEST
        );
        assert_eq!(
            Message::parse("{\"result\":1}").unwrap_err().code,
            codes::INVALID_REQUEST
        );
    }

    #[test]
    fn binary_payloads_are_base64() {
        let p = WriteParams {
            path: "/f".into(),
            data: vec![0, 159, 146, 150, b'\n'],
            hash: None,
            cached: false,
        };
        let s = serde_json::to_string(&p).unwrap();
        assert!(s.contains(&b64::encode(&p.data)));
        let back: WriteParams = serde_json::from_str(&s).unwrap();
        assert_eq!(back, p);
        assert!(!s.contains("hash") && !s.contains("cached"), "{s}");
    }

    #[test]
    fn cache_fields_are_optional_both_ways() {
        // An old client's fs.read params decode as a read without `have`,
        // and an old helper's answer decodes as uncached.
        let r: ReadParams = serde_json::from_str(r#"{"path":"/a"}"#).unwrap();
        assert_eq!(r.have, None);
        let d: DataResult = serde_json::from_str(r#"{"data":""}"#).unwrap();
        assert_eq!((d.hash, d.cached), (None, false));
        let w = WriteParams {
            path: "/f".into(),
            data: Vec::new(),
            hash: Some("ab".into()),
            cached: true,
        };
        let s = serde_json::to_string(&w).unwrap();
        assert_eq!(serde_json::from_str::<WriteParams>(&s).unwrap(), w);
        let h: HelloResult = serde_json::from_value(serde_json::json!({
            "protocol": 2, "version": "x", "arch": "a", "os": "o",
            "session": "s", "pid": 1, "jobs": 0
        }))
        .unwrap();
        assert_eq!(h.content_cache, None);
    }

    #[test]
    fn ws_errors_roundtrip() {
        let cases = [
            WsError::NotFound("/x".into()),
            WsError::PermissionDenied("/x".into()),
            WsError::IsDirectory("/x".into()),
            WsError::NotADirectory("/x".into()),
        ];
        for e in cases {
            let wire = RpcError::from_ws(&e);
            let back = wire.into_ws("/x");
            assert_eq!(
                std::mem::discriminant(&back),
                std::mem::discriminant(&e),
                "{e:?}"
            );
        }
        let e = WsError::InvalidPattern {
            pattern: "[".into(),
            reason: "unclosed".into(),
        };
        assert!(matches!(
            RpcError::from_ws(&e).into_ws("["),
            WsError::InvalidPattern { .. }
        ));
    }

    #[tokio::test]
    async fn frames_split_on_newlines_and_limit_size() {
        let data = b"{\"a\":1}\n{\"b\":2}\npartial".to_vec();
        let mut r = BufReader::new(&data[..]);
        assert_eq!(read_frame(&mut r).await.unwrap().unwrap(), "{\"a\":1}");
        assert_eq!(read_frame(&mut r).await.unwrap().unwrap(), "{\"b\":2}");
        assert_eq!(read_frame(&mut r).await.unwrap().unwrap(), "partial");
        assert!(read_frame(&mut r).await.unwrap().is_none());

        let big = vec![b'x'; MAX_FRAME + 10];
        let mut r = BufReader::new(&big[..]);
        assert!(read_frame(&mut r).await.is_err());
    }

    #[test]
    fn stat_converts_mtime() {
        let st = FileStat {
            kind: FileKind::File,
            len: 3,
            mode: Some(0o644),
            modified: Some(UNIX_EPOCH + Duration::from_millis(1234)),
        };
        let w = WireStat::from(&st);
        assert_eq!(w.modified_ms, Some(1234));
        assert_eq!(FileStat::from(w), st);
    }
}
