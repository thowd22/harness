//! Types shared by every tool: call context, output, errors and events.
//!
//! Tools take a [`ToolContext`] and typed arguments and return a
//! [`ToolOutput`] or a [`ToolError`]. Side information the agent loop or UI
//! cares about (file changes, live output, spills) goes out through the
//! context's [`ToolEventSink`] as [`ToolEvent`]s, which the agent maps onto
//! the run's event log.

use std::fmt;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use crate::exec::StreamKind;
use crate::tracker::{ReadTracker, StagedReads};

/// Per-call context handed to every tool invocation.
#[derive(Clone)]
pub struct ToolContext {
    /// Provider tool-call id, copied into every event this call emits.
    pub call_id: String,
    /// True when the active model accepts image input (read returns images).
    pub supports_images: bool,
    /// Where events go.
    pub events: Arc<dyn ToolEventSink>,
    /// Decides which found paths search tools may show (protected paths
    /// are dropped); `None` shows everything.
    pub read_filter: Option<ReadFilter>,
    /// When set, read records go here instead of the session's
    /// [`ReadTracker`] until the caller commits them (speculative calls,
    /// see [`StagedReads`]). `None` records directly.
    pub staged_reads: Option<Arc<StagedReads>>,
}

/// Returns false for an absolute path the agent may not read (see
/// [`ToolContext::may_read`]).
pub type ReadFilter = Arc<dyn Fn(&std::path::Path) -> bool + Send + Sync>;

impl ToolContext {
    /// A context with the given call id, no image support and no event sink.
    pub fn new(call_id: impl Into<String>) -> Self {
        Self {
            call_id: call_id.into(),
            supports_images: false,
            events: Arc::new(NoEvents),
            read_filter: None,
            staged_reads: None,
        }
    }

    /// Hold read records in `stage` instead of applying them directly.
    pub fn with_staged_reads(mut self, stage: Option<Arc<StagedReads>>) -> Self {
        self.staged_reads = stage;
        self
    }

    /// Record that the model has seen `path` with `hash`: in `tracker`, or
    /// in the staged reads when this context has them.
    pub fn record_read(&self, tracker: &Arc<ReadTracker>, path: &std::path::Path, hash: String) {
        match &self.staged_reads {
            Some(stage) => stage.record(tracker, path, hash),
            None => tracker.record(path, hash),
        }
    }

    /// Set the read filter for search results.
    pub fn with_read_filter(mut self, filter: Option<ReadFilter>) -> Self {
        self.read_filter = filter;
        self
    }

    /// True unless the read filter rejects `path` (absolute).
    pub fn may_read(&self, path: &std::path::Path) -> bool {
        self.read_filter.as_ref().is_none_or(|f| f(path))
    }

    /// Set image support.
    pub fn with_images(mut self, supports_images: bool) -> Self {
        self.supports_images = supports_images;
        self
    }

    /// Set the event sink.
    pub fn with_events(mut self, events: Arc<dyn ToolEventSink>) -> Self {
        self.events = events;
        self
    }

    /// Emit `event` to the sink.
    pub fn emit(&self, event: ToolEvent) {
        self.events.emit(&self.call_id, event);
    }
}

impl fmt::Debug for ToolContext {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ToolContext")
            .field("call_id", &self.call_id)
            .field("supports_images", &self.supports_images)
            .finish_non_exhaustive()
    }
}

/// What a tool returns to the model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolOutput {
    /// Plain text.
    Text(String),
    /// An image for providers that accept image input.
    Image {
        /// MIME type, e.g. `image/png`.
        media_type: String,
        /// Raw image bytes (the provider encodes them).
        data: Vec<u8>,
        /// Short text description sent alongside the image.
        caption: String,
    },
}

impl ToolOutput {
    /// The text, or the caption for an image.
    pub fn text(&self) -> &str {
        match self {
            ToolOutput::Text(t) => t,
            ToolOutput::Image { caption, .. } => caption,
        }
    }
}

/// Category of a [`ToolError`], for callers that branch on it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolErrorKind {
    /// Arguments were missing, malformed or out of range.
    InvalidArgs,
    /// The path does not exist.
    NotFound,
    /// The file must be read before it can be modified.
    NotRead,
    /// The file changed after it was last read.
    StaleRead,
    /// `old_string` was not found.
    NoMatch,
    /// `old_string` matched more than once and `replace_all` was not set.
    AmbiguousMatch,
    /// The file is binary or too large for this operation.
    Unsupported,
    /// The process could not be started or supervised.
    Exec,
    /// Any other I/O failure.
    Io,
}

/// A tool failure. The message is sent to the model and always says how to
/// recover.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct ToolError {
    /// Category.
    pub kind: ToolErrorKind,
    /// Model-facing explanation and recovery hint.
    pub message: String,
}

impl ToolError {
    /// An error of `kind` with `message`.
    pub fn new(kind: ToolErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    /// Shorthand for [`ToolErrorKind::InvalidArgs`].
    pub fn invalid_args(message: impl Into<String>) -> Self {
        Self::new(ToolErrorKind::InvalidArgs, message)
    }
}

impl From<crate::workspace::WsError> for ToolError {
    fn from(e: crate::workspace::WsError) -> Self {
        use crate::workspace::WsError;
        let kind = match &e {
            WsError::NotFound(_) => ToolErrorKind::NotFound,
            WsError::InvalidPattern { .. } => ToolErrorKind::InvalidArgs,
            _ => ToolErrorKind::Io,
        };
        let hint = match &e {
            WsError::NotFound(_) => {
                " Check the path (relative paths are resolved from the project root); bash `ls` or `find` can help locate it."
            }
            WsError::PermissionDenied(_) => " This path cannot be accessed; choose another path.",
            _ => "",
        };
        ToolError::new(kind, format!("{e}.{hint}"))
    }
}

/// Parse JSON tool arguments into `T`, turning serde errors into
/// [`ToolErrorKind::InvalidArgs`] with the tool name.
pub fn parse_args<T: serde::de::DeserializeOwned>(
    tool: &str,
    args: serde_json::Value,
) -> Result<T, ToolError> {
    serde_json::from_value(args).map_err(|e| {
        ToolError::invalid_args(format!(
            "invalid arguments for {tool}: {e}. Fix the arguments to match the {tool} schema and call it again."
        ))
    })
}

/// Whether a mutation created a file or changed an existing one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangeKind {
    /// The file did not exist before.
    Created,
    /// An existing file was changed.
    Modified,
}

/// One file mutation, for the change ledger.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileChange {
    /// Tool that made the change (`write` or `edit`).
    pub tool: &'static str,
    /// Resolved path of the file.
    pub path: PathBuf,
    /// Created or modified.
    pub kind: ChangeKind,
    /// sha256 (hex) of the content before, `None` for a new file.
    pub before_hash: Option<String>,
    /// sha256 (hex) of the content after.
    pub after_hash: String,
    /// Unified diff (`a/<path>` → `b/<path>`, 3 lines of context).
    pub diff: String,
}

/// A tool output that was moved to a blob because it was too large.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpillRecord {
    /// Tool whose output was spilled.
    pub tool: String,
    /// Blob holding the full output (`spill:<id>` pointer for read).
    pub blob_id: String,
    /// Size of the full output in bytes.
    pub original_bytes: usize,
    /// Size of what was sent to the model instead.
    pub kept_bytes: usize,
    /// Estimated tokens of the full output.
    pub original_tokens: usize,
    /// Estimated tokens of what was sent.
    pub kept_tokens: usize,
}

impl SpillRecord {
    /// Estimated tokens saved by spilling.
    pub fn tokens_saved(&self) -> usize {
        self.original_tokens.saturating_sub(self.kept_tokens)
    }
}

/// Rough token estimate used for budgets: one token per four bytes, rounded up.
pub fn estimate_tokens(bytes: usize) -> usize {
    bytes.div_ceil(4)
}

/// Something a tool reports while running.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolEvent {
    /// A file was created or modified.
    FileChanged(FileChange),
    /// Live process output (bash), for the UI.
    Output {
        /// Stream the bytes came from.
        stream: StreamKind,
        /// Raw bytes.
        data: Vec<u8>,
    },
    /// Output exceeded the budget and was spilled to a blob.
    OutputSpilled(SpillRecord),
    /// Progress of a running process (bash): its timeout once at the
    /// start, then each liveness sample, so the UI can show a quiet but
    /// busy command as working.
    Progress(ToolProgress),
    /// A background job (bash `run_in_background`) started or ended. It is
    /// reported with the id of the bash call that started it, also after
    /// that call has returned.
    Job(crate::jobs::JobInfo),
    /// What a change-tracking wrapper (the change ledger around `bash`)
    /// found the call changed, sent once before the call returns. The agent
    /// loop uses it instead of assuming that every successful mutating
    /// call changed files.
    WorktreeChanges {
        /// Changed files that count (not generated artifacts).
        files: usize,
        /// Generated build or test artifacts (`__pycache__/`, ...).
        generated: usize,
    },
}

/// What a running tool reports about itself besides output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ToolProgress {
    /// The call's effective timeout, when it has one.
    pub timeout: Option<std::time::Duration>,
    /// The latest liveness sample; `None` when not (yet) known.
    pub liveness: Option<crate::liveness::Liveness>,
}

/// Receives [`ToolEvent`]s. Implementations must not block for long: live
/// output events arrive while the process is running.
pub trait ToolEventSink: Send + Sync {
    /// Handle one event from the call `call_id`.
    fn emit(&self, call_id: &str, event: ToolEvent);
}

/// Sink that drops every event.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoEvents;

impl ToolEventSink for NoEvents {
    fn emit(&self, _call_id: &str, _event: ToolEvent) {}
}

/// Sink that keeps every event in memory; handy in tests.
#[derive(Debug, Default)]
pub struct RecordingEvents {
    events: Mutex<Vec<(String, ToolEvent)>>,
}

impl RecordingEvents {
    /// An empty recorder.
    pub fn new() -> Self {
        Self::default()
    }

    /// All events so far, with their call ids.
    pub fn events(&self) -> Vec<(String, ToolEvent)> {
        self.events
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
}

impl ToolEventSink for RecordingEvents {
    fn emit(&self, call_id: &str, event: ToolEvent) {
        self.events
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push((call_id.to_string(), event));
    }
}
