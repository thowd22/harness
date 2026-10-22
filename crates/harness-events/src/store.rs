//! Append-only run store (`events.jsonl`).
//!
//! Layout, one directory per run:
//!
//! ```text
//! <data_dir>/runs/<run-id>/events.jsonl   header line, then one RunEvent per line
//! <data_dir>/runs/<run-id>/blobs/<sha256> large payloads (see crate::blobs)
//! ```
//!
//! [`RunRecorder`] is the single entry point for producing events. For each
//! [`EventDraft`] it assigns the next sequence number and a timestamp, runs
//! the [`Redactor`] over it (streaming deltas go through a per-call
//! [`StreamRedactor`]), spills large payloads to the blob store, then hands
//! the event to the async file writer and to live subscribers (the UI). So
//! nothing unredacted is ever persisted or delivered.
//!
//! [`read_run`] replays a run from disk. A final line cut short by a crash is
//! ignored and reported via [`Replay::truncated`].

use std::collections::HashMap;
use std::io::{self, BufRead};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use time::OffsetDateTime;
use tokio::io::{AsyncWriteExt, BufWriter};
use tokio::sync::{broadcast, mpsc, oneshot};

use crate::blobs::{BLOBS_DIR, BlobStore};
use crate::redact::{Redactor, StreamRedactor};
use crate::schema::{
    DeltaChannel, EventKind, NodeId, OutputStream, RunEvent, RunHeader, RunId, SchemaError,
    migrate_event,
};

/// Directory holding all runs inside the data directory.
pub const RUNS_DIR: &str = "runs";
/// Event log file name inside a run directory.
pub const EVENTS_FILE: &str = "events.jsonl";
/// Default size in bytes above which payloads go to the blob store.
pub const DEFAULT_BLOB_THRESHOLD: usize = 16 * 1024;
/// Capacity of the live event broadcast channel.
pub const BROADCAST_CAPACITY: usize = 4096;

/// `<data_dir>/runs`.
pub fn runs_dir(data_dir: &Path) -> PathBuf {
    data_dir.join(RUNS_DIR)
}

/// `<data_dir>/runs/<run_id>`.
pub fn run_dir(data_dir: &Path, run_id: RunId) -> PathBuf {
    runs_dir(data_dir).join(run_id.to_string())
}

/// Errors from the run store.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    /// Filesystem error.
    #[error("run store I/O error: {0}")]
    Io(#[from] io::Error),
    /// JSON (de)serialization failed.
    #[error("invalid event JSON on line {line}: {source}")]
    Json {
        /// 1-based line number (1 is the header).
        line: usize,
        /// Parser error.
        #[source]
        source: serde_json::Error,
    },
    /// The run file has no header line.
    #[error("run file has no header")]
    MissingHeader,
    /// The header's schema version cannot be read by this build.
    #[error(transparent)]
    Schema(#[from] SchemaError),
    /// Sequence numbers are not strictly increasing.
    #[error("event sequence out of order on line {line}: {seq} after {prev}")]
    OutOfOrder {
        /// 1-based line number.
        line: usize,
        /// Sequence found.
        seq: u64,
        /// Previous sequence.
        prev: u64,
    },
    /// The recorder was already finished or its writer stopped.
    #[error("run recorder is closed")]
    Closed,
}

/// An event before the recorder stamps it with sequence and time.
#[derive(Debug, Clone, PartialEq)]
pub struct EventDraft {
    /// Node the event belongs to (`None` for run-level events).
    pub node_id: Option<NodeId>,
    /// Parent node. When `None`, the recorder fills in the parent given in
    /// that node's `node_started` event.
    pub parent_node_id: Option<NodeId>,
    /// The event.
    pub kind: EventKind,
}

impl EventDraft {
    /// A run-level event.
    pub fn run(kind: EventKind) -> Self {
        Self {
            node_id: None,
            parent_node_id: None,
            kind,
        }
    }

    /// An event for `node`.
    pub fn node(node: impl Into<NodeId>, kind: EventKind) -> Self {
        Self {
            node_id: Some(node.into()),
            parent_node_id: None,
            kind,
        }
    }

    /// Sets the parent node.
    pub fn with_parent(mut self, parent: impl Into<NodeId>) -> Self {
        self.parent_node_id = Some(parent.into());
        self
    }
}

/// Options for [`RunRecorder::create`].
#[derive(Debug, Clone)]
pub struct RecorderOptions {
    /// Payloads larger than this many bytes are stored as blobs.
    pub blob_threshold: usize,
    /// Config hash written to the run header ([`RunHeader::config_hash`]).
    pub config_hash: Option<String>,
}

impl Default for RecorderOptions {
    fn default() -> Self {
        Self {
            blob_threshold: DEFAULT_BLOB_THRESHOLD,
            config_hash: None,
        }
    }
}

impl RecorderOptions {
    /// Stamps the run header with `hash`.
    pub fn with_config_hash(mut self, hash: impl Into<String>) -> Self {
        self.config_hash = Some(hash.into());
        self
    }
}

enum Cmd {
    Line(String),
    Flush(oneshot::Sender<io::Result<()>>),
}

struct EmitState {
    next_seq: u64,
    parents: HashMap<NodeId, NodeId>,
    streams: HashMap<(String, DeltaChannel), (StreamRedactor, Option<NodeId>)>,
    outputs: HashMap<(String, OutputStream), (StreamRedactor, Option<NodeId>)>,
    tx: Option<mpsc::UnboundedSender<Cmd>>,
}

struct Inner {
    run_id: RunId,
    dir: PathBuf,
    blobs: BlobStore,
    redactor: Arc<Redactor>,
    opts: RecorderOptions,
    bus: broadcast::Sender<RunEvent>,
    state: Mutex<EmitState>,
    task: Mutex<Option<tokio::task::JoinHandle<io::Result<()>>>>,
}

/// Records the events of one run. Cheap to clone; all clones share the same
/// sequence counter and file.
///
/// Must be created inside a Tokio runtime (the file writer is a task).
#[derive(Clone)]
pub struct RunRecorder {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for RunRecorder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RunRecorder")
            .field("run_id", &self.inner.run_id)
            .field("dir", &self.inner.dir)
            .finish_non_exhaustive()
    }
}

impl RunRecorder {
    /// Creates `<data_dir>/runs/<run_id>/`, writes the header and starts the
    /// writer task.
    pub async fn create(
        data_dir: &Path,
        run_id: RunId,
        redactor: Arc<Redactor>,
        opts: RecorderOptions,
    ) -> Result<Self, StoreError> {
        let dir = run_dir(data_dir, run_id);
        tokio::fs::create_dir_all(&dir).await?;
        let mut header = RunHeader::new(run_id);
        header.config_hash = opts.config_hash.clone();
        let mut file = tokio::fs::OpenOptions::new()
            .create_new(true)
            .append(true)
            .open(dir.join(EVENTS_FILE))
            .await?;
        let mut line = serde_json::to_string(&header).map_err(io::Error::from)?;
        line.push('\n');
        file.write_all(line.as_bytes()).await?;
        file.flush().await?;

        let (tx, rx) = mpsc::unbounded_channel();
        let task = tokio::spawn(writer_task(BufWriter::new(file), rx));
        let (bus, _) = broadcast::channel(BROADCAST_CAPACITY);
        Ok(Self {
            inner: Arc::new(Inner {
                run_id,
                blobs: BlobStore::new(dir.join(BLOBS_DIR)),
                dir,
                redactor,
                opts,
                bus,
                state: Mutex::new(EmitState {
                    next_seq: 0,
                    parents: HashMap::new(),
                    streams: HashMap::new(),
                    outputs: HashMap::new(),
                    tx: Some(tx),
                }),
                task: Mutex::new(Some(task)),
            }),
        })
    }

    /// The run id.
    pub fn run_id(&self) -> RunId {
        self.inner.run_id
    }

    /// The run directory.
    pub fn dir(&self) -> &Path {
        &self.inner.dir
    }

    /// The run's blob store.
    pub fn blobs(&self) -> &BlobStore {
        &self.inner.blobs
    }

    /// The redactor applied to every event. The agent loop uses it (or the
    /// `tool_result` event returned by [`emit`](Self::emit)) so tool output
    /// is redacted before it is sent to the model.
    pub fn redactor(&self) -> &Arc<Redactor> {
        &self.inner.redactor
    }

    /// Subscribes to redacted events as they are recorded (for the UI).
    pub fn subscribe(&self) -> broadcast::Receiver<RunEvent> {
        self.inner.bus.subscribe()
    }

    /// Records one event and returns the event(s) actually recorded, already
    /// redacted.
    ///
    /// - A `delta` may return nothing while its text is held back by the
    ///   stream redactor.
    /// - `model_call_finished` first returns one `delta` per channel with the
    ///   text that was still held for that call.
    ///
    /// Large payloads are moved to the blob store in the returned events too.
    pub fn emit(&self, draft: EventDraft) -> Result<Vec<RunEvent>, StoreError> {
        let mut state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.tx.is_none() {
            return Err(StoreError::Closed);
        }
        let EventDraft {
            node_id,
            mut parent_node_id,
            mut kind,
        } = draft;

        if let (EventKind::NodeStarted { .. }, Some(node), Some(parent)) =
            (&kind, &node_id, &parent_node_id)
        {
            state.parents.insert(node.clone(), parent.clone());
        }
        if parent_node_id.is_none() {
            parent_node_id = node_id.as_ref().and_then(|n| state.parents.get(n).cloned());
        }

        let mut kinds = Vec::with_capacity(1);
        match &mut kind {
            EventKind::Delta {
                call_id,
                channel,
                text,
            } => {
                let key = (call_id.clone(), *channel);
                let entry = state
                    .streams
                    .entry(key)
                    .or_insert_with(|| (self.inner.redactor.stream(), node_id.clone()));
                *text = entry.0.push(text);
                if !text.is_empty() {
                    kinds.push((node_id.clone(), parent_node_id.clone(), kind));
                }
            }
            EventKind::ToolOutput {
                call_id,
                stream,
                text,
            } => {
                let entry = state
                    .outputs
                    .entry((call_id.clone(), *stream))
                    .or_insert_with(|| (self.inner.redactor.stream(), node_id.clone()));
                *text = entry.0.push(text);
                if !text.is_empty() {
                    kinds.push((node_id.clone(), parent_node_id.clone(), kind));
                }
            }
            EventKind::ToolResult { call_id, .. } => {
                for stream in OUTPUT_STREAMS {
                    if let Some(held) = state.outputs.remove(&(call_id.clone(), stream)) {
                        kinds.extend(held_output(&state, call_id, stream, held));
                    }
                }
                self.inner.redactor.redact_event(&mut kind);
                kinds.push((node_id, parent_node_id, kind));
            }
            EventKind::ModelCallFinished { call_id, .. } => {
                for channel in CHANNELS {
                    if let Some((mut s, node)) = state.streams.remove(&(call_id.clone(), channel)) {
                        let text = s.finish();
                        if !text.is_empty() {
                            let parent = node.as_ref().and_then(|n| state.parents.get(n).cloned());
                            let delta = EventKind::Delta {
                                call_id: call_id.clone(),
                                channel,
                                text,
                            };
                            kinds.push((node, parent, delta));
                        }
                    }
                }
                self.inner.redactor.redact_event(&mut kind);
                kinds.push((node_id, parent_node_id, kind));
            }
            _ => {
                self.inner.redactor.redact_event(&mut kind);
                kinds.push((node_id, parent_node_id, kind));
            }
        }

        kinds
            .into_iter()
            .map(|(node, parent, kind)| self.record(&mut state, node, parent, kind))
            .collect()
    }

    /// Stamps, spills, persists and broadcasts one already redacted event.
    fn record(
        &self,
        state: &mut EmitState,
        node_id: Option<NodeId>,
        parent_node_id: Option<NodeId>,
        mut kind: EventKind,
    ) -> Result<RunEvent, StoreError> {
        for payload in kind.payloads_mut() {
            self.inner
                .blobs
                .spill(payload, self.inner.opts.blob_threshold)?;
        }
        let event = RunEvent {
            seq: state.next_seq,
            ts: OffsetDateTime::now_utc(),
            run_id: self.inner.run_id,
            node_id,
            parent_node_id,
            kind,
        };
        let mut line = serde_json::to_string(&event).map_err(io::Error::from)?;
        line.push('\n');
        let tx = state.tx.as_ref().ok_or(StoreError::Closed)?;
        tx.send(Cmd::Line(line)).map_err(|_| StoreError::Closed)?;
        state.next_seq += 1;
        // Having no live subscribers is fine.
        let _ = self.inner.bus.send(event.clone());
        Ok(event)
    }

    /// Waits until every event emitted so far is written and flushed.
    pub async fn flush(&self) -> Result<(), StoreError> {
        let (done, wait) = oneshot::channel();
        {
            let state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            let tx = state.tx.as_ref().ok_or(StoreError::Closed)?;
            tx.send(Cmd::Flush(done)).map_err(|_| StoreError::Closed)?;
        }
        wait.await.map_err(|_| StoreError::Closed)??;
        Ok(())
    }

    /// Releases text still held for unfinished model calls, closes the file
    /// and waits for the writer. Later `emit` calls on any clone fail with
    /// [`StoreError::Closed`].
    pub async fn finish(&self) -> Result<(), StoreError> {
        let task = {
            let mut state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            if state.tx.is_some() {
                let mut held: Vec<_> = state.streams.drain().collect();
                held.sort_by(|a, b| {
                    a.0.0
                        .cmp(&b.0.0)
                        .then(channel_rank(a.0.1).cmp(&channel_rank(b.0.1)))
                });
                for ((call_id, channel), (mut stream, node)) in held {
                    let text = stream.finish();
                    if !text.is_empty() {
                        let parent = node.as_ref().and_then(|n| state.parents.get(n).cloned());
                        let kind = EventKind::Delta {
                            call_id,
                            channel,
                            text,
                        };
                        self.record(&mut state, node, parent, kind)?;
                    }
                }
                let mut outputs: Vec<_> = state.outputs.drain().collect();
                outputs.sort_by(|a, b| a.0.cmp(&b.0));
                for ((call_id, stream), held) in outputs {
                    if let Some((node, parent, kind)) = held_output(&state, &call_id, stream, held)
                    {
                        self.record(&mut state, node, parent, kind)?;
                    }
                }
            }
            state.tx = None;
            self.inner
                .task
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .take()
        };
        if let Some(task) = task {
            task.await.map_err(|e| io::Error::other(e.to_string()))??;
        }
        Ok(())
    }
}

fn channel_rank(c: DeltaChannel) -> u8 {
    match c {
        DeltaChannel::Text => 0,
        DeltaChannel::Reasoning => 1,
        DeltaChannel::ToolArgs => 2,
    }
}

const CHANNELS: [DeltaChannel; 3] = [
    DeltaChannel::Text,
    DeltaChannel::Reasoning,
    DeltaChannel::ToolArgs,
];

const OUTPUT_STREAMS: [OutputStream; 2] = [OutputStream::Stdout, OutputStream::Stderr];

type Held = (Option<NodeId>, Option<NodeId>, EventKind);

/// The `tool_output` event for text still held by a finished stream.
fn held_output(
    state: &EmitState,
    call_id: &str,
    stream: OutputStream,
    (mut redactor, node): (StreamRedactor, Option<NodeId>),
) -> Option<Held> {
    let text = redactor.finish();
    if text.is_empty() {
        return None;
    }
    let parent = node.as_ref().and_then(|n| state.parents.get(n).cloned());
    Some((
        node,
        parent,
        EventKind::ToolOutput {
            call_id: call_id.to_owned(),
            stream,
            text,
        },
    ))
}

async fn writer_task(
    mut file: BufWriter<tokio::fs::File>,
    mut rx: mpsc::UnboundedReceiver<Cmd>,
) -> io::Result<()> {
    while let Some(cmd) = rx.recv().await {
        let mut waiters = Vec::new();
        let mut next = Some(cmd);
        // Drain whatever is queued, then flush once per batch.
        while let Some(cmd) = next {
            match cmd {
                Cmd::Line(line) => file.write_all(line.as_bytes()).await?,
                Cmd::Flush(done) => waiters.push(done),
            }
            next = rx.try_recv().ok();
        }
        let res = file.flush().await;
        for w in waiters {
            let _ = w.send(match &res {
                Ok(()) => Ok(()),
                Err(e) => Err(io::Error::new(e.kind(), e.to_string())),
            });
        }
        res?;
    }
    file.flush().await?;
    file.into_inner().sync_all().await
}

/// A run loaded from disk.
#[derive(Debug, Clone)]
pub struct Replay {
    /// The header (schema version as stored on disk).
    pub header: RunHeader,
    /// Events in sequence order, migrated to the current schema.
    pub events: Vec<RunEvent>,
    /// True when an incomplete final line (crash mid-write) was skipped.
    pub truncated: bool,
}

/// Reads `<run_dir>/events.jsonl`.
///
/// The header's schema version is checked first: unsupported versions are
/// rejected, older supported ones are migrated with
/// [`migrate_event`]. An unparsable *final*
/// line is treated as a crash-truncated write and skipped; an unparsable line
/// anywhere else is an error.
pub fn read_run(run_dir: &Path) -> Result<Replay, StoreError> {
    let file = std::fs::File::open(run_dir.join(EVENTS_FILE))?;
    let mut lines = io::BufReader::new(file).lines().enumerate().peekable();

    let header_line = match lines.next() {
        Some((_, line)) => line?,
        None => return Err(StoreError::MissingHeader),
    };
    let header_value: serde_json::Value = serde_json::from_str(&header_line)
        .map_err(|source| StoreError::Json { line: 1, source })?;
    let version = header_value
        .get("schema_version")
        .and_then(serde_json::Value::as_u64)
        .ok_or(StoreError::MissingHeader)?;
    let version = u32::try_from(version).unwrap_or(u32::MAX);
    // Reject before attempting to parse the rest of the header.
    migrate_event(version, serde_json::Value::Null)?;
    let header: RunHeader = serde_json::from_value(header_value)
        .map_err(|source| StoreError::Json { line: 1, source })?;

    let mut events: Vec<RunEvent> = Vec::new();
    let mut truncated = false;
    while let Some((idx, line)) = lines.next() {
        let line_no = idx + 1;
        let line = match line {
            Ok(l) => l,
            // Invalid UTF-8 from a torn write on the last line.
            Err(e) if e.kind() == io::ErrorKind::InvalidData && lines.peek().is_none() => {
                truncated = true;
                break;
            }
            Err(e) => return Err(e.into()),
        };
        if line.trim().is_empty() {
            continue;
        }
        let is_last = lines.peek().is_none();
        let parsed = serde_json::from_str::<serde_json::Value>(&line)
            .map_err(StoreError::from_json(line_no))
            .and_then(|v| Ok(migrate_event(header.schema_version, v)?))
            .and_then(|v| {
                serde_json::from_value::<RunEvent>(v).map_err(StoreError::from_json(line_no))
            });
        match parsed {
            Ok(event) => {
                if let Some(prev) = events.last()
                    && event.seq <= prev.seq
                {
                    return Err(StoreError::OutOfOrder {
                        line: line_no,
                        seq: event.seq,
                        prev: prev.seq,
                    });
                }
                events.push(event);
            }
            Err(StoreError::Json { .. }) if is_last => truncated = true,
            Err(e) => return Err(e),
        }
    }
    Ok(Replay {
        header,
        events,
        truncated,
    })
}

impl StoreError {
    fn from_json(line: usize) -> impl Fn(serde_json::Error) -> StoreError {
        move |source| StoreError::Json { line, source }
    }
}

/// Lists run ids under `<data_dir>/runs`, oldest first (ids are UUID v7).
pub fn list_runs(data_dir: &Path) -> io::Result<Vec<RunId>> {
    let dir = runs_dir(data_dir);
    let mut ids = Vec::new();
    match std::fs::read_dir(&dir) {
        Ok(entries) => {
            for entry in entries {
                let entry = entry?;
                if let Some(id) = entry
                    .file_name()
                    .to_str()
                    .and_then(|s| uuid::Uuid::parse_str(s).ok())
                {
                    ids.push(RunId(id));
                }
            }
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    ids.sort();
    Ok(ids)
}
