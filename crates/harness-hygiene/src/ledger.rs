//! Change ledger of every file mutation.
//!
//! The ledger is the complete record of what an agent changed, per node:
//!
//! - **Tool changes.** `write` and `edit` report a
//!   [`harness_tools::tool::FileChange`]; pass it to
//!   [`Ledger::record_tool_change`]. Other tools report through
//!   [`Ledger::record`].
//! - **Bash changes.** Call [`Ledger::begin_command`] before a bash command
//!   and [`Ledger::end_command`] after it. The worktree is snapshotted on
//!   both sides and every created, modified or deleted file is recorded.
//!   Well-known build and test output (`__pycache__/`, `target/`, ...)
//!   that the user's git does not track is recorded as *generated* ([`LedgerEntry::generated`], see
//!   [`crate::artifacts`]); the entries [`Ledger::end_command`] returns say
//!   which.
//!
//! Every entry is also emitted as an [`EventKind::FileChanged`] event on its
//! node, so the ledger is part of the run's event log. A ledger can be
//! rebuilt from a stored run with [`Ledger::from_events`]. The ledger
//! should be the only producer of `file_changed` events, so agent code
//! forwards `ToolEvent::FileChanged` here instead of emitting it itself.

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use harness_events::schema::{EventKind, FileChangeKind, NodeId, Payload, RunEvent};
use harness_tools::tool::{ChangeKind, FileChange};
use serde::Serialize;

use crate::artifacts::Artifacts;
use crate::events::{EventSink, NoEvents, draft};
use crate::snapshot::{Snapshot, SnapshotOptions, relative};

/// What made a change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ChangeSource {
    /// A file tool (`write`, `edit`, ...).
    Tool {
        /// Tool name.
        tool: String,
    },
    /// A bash command, detected by comparing snapshots.
    Bash {
        /// The command line.
        command: String,
    },
    /// Rebuilt from the event log, which does not record the source.
    Recorded,
}

impl ChangeSource {
    /// Short label (`write`, `bash`, `recorded`).
    pub fn label(&self) -> &str {
        match self {
            Self::Tool { tool } => tool,
            Self::Bash { .. } => "bash",
            Self::Recorded => "recorded",
        }
    }
}

/// One recorded mutation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LedgerEntry {
    /// Order within the ledger, from 0.
    pub seq: u64,
    /// Node that made the change (`None` for run-level changes).
    pub node_id: Option<NodeId>,
    /// Workspace-relative path with `/` separators.
    pub path: String,
    /// What happened.
    pub kind: FileChangeKind,
    /// What made it.
    pub source: ChangeSource,
    /// sha256 before (`None` when created or unknown).
    pub before_hash: Option<String>,
    /// sha256 after (`None` when deleted or unknown).
    pub after_hash: Option<String>,
    /// Unified diff, when known.
    pub diff: Option<String>,
    /// A generated build or test artifact written by a bash command
    /// ([`crate::artifacts`]): listed, but not a change to verify.
    pub generated: bool,
}

/// The overall effect on one path over several entries.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct NetChange {
    /// Workspace-relative path.
    pub path: String,
    /// Net kind: created then modified is `Created`; deleted then
    /// re-created is `Modified`. Paths created and then deleted are left
    /// out of [`Ledger::net_changes`].
    pub kind: FileChangeKind,
    /// Every distinct source, in first-seen order.
    pub sources: Vec<ChangeSource>,
    /// Hash before the first entry.
    pub before_hash: Option<String>,
    /// Hash after the last entry.
    pub after_hash: Option<String>,
    /// The entries' diffs, joined in order.
    pub diffs: Vec<String>,
    /// Every entry for the path is a generated artifact.
    pub generated: bool,
}

/// A worktree snapshot taken by [`Ledger::begin_command`].
#[derive(Debug)]
pub struct CommandWatch {
    before: Snapshot,
}

#[derive(Default)]
struct Inner {
    entries: Vec<LedgerEntry>,
    last_snapshot: Option<Snapshot>,
}

/// The change ledger for one workspace (normally one per run).
pub struct Ledger {
    root: PathBuf,
    opts: SnapshotOptions,
    artifacts: Artifacts,
    sink: Arc<dyn EventSink>,
    inner: Mutex<Inner>,
}

impl std::fmt::Debug for Ledger {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Ledger")
            .field("root", &self.root)
            .field("entries", &self.lock().entries.len())
            .finish_non_exhaustive()
    }
}

impl Ledger {
    /// A ledger for the worktree at `root` that emits no events.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: harness_tools::workspace::normalize(&root.into()),
            opts: SnapshotOptions::default(),
            artifacts: Artifacts::builtin(),
            sink: Arc::new(NoEvents),
            inner: Mutex::new(Inner::default()),
        }
    }

    /// Emit every entry as a `file_changed` event to `sink`.
    pub fn with_sink(mut self, sink: Arc<dyn EventSink>) -> Self {
        self.sink = sink;
        self
    }

    /// Use different snapshot limits.
    pub fn with_options(mut self, opts: SnapshotOptions) -> Self {
        self.opts = opts;
        self
    }

    /// The worktree root.
    pub fn root(&self) -> &Path {
        &self.root
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// `path` as a workspace-relative string (paths outside the root are
    /// kept as given).
    pub fn relative(&self, path: &Path) -> String {
        let abs = harness_tools::workspace::normalize(&self.root.join(path));
        relative(&self.root, &abs).unwrap_or_else(|| path.to_string_lossy().into_owned())
    }

    /// Record one change and emit it.
    #[allow(clippy::too_many_arguments)]
    pub fn record(
        &self,
        node: Option<&NodeId>,
        path: &Path,
        kind: FileChangeKind,
        source: ChangeSource,
        before_hash: Option<String>,
        after_hash: Option<String>,
        diff: Option<String>,
    ) -> LedgerEntry {
        self.record_entry(
            node,
            path,
            kind,
            source,
            before_hash,
            after_hash,
            diff,
            false,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn record_entry(
        &self,
        node: Option<&NodeId>,
        path: &Path,
        kind: FileChangeKind,
        source: ChangeSource,
        before_hash: Option<String>,
        after_hash: Option<String>,
        diff: Option<String>,
        generated: bool,
    ) -> LedgerEntry {
        let path = self.relative(path);
        let entry = {
            let mut inner = self.lock();
            let entry = LedgerEntry {
                seq: inner.entries.len() as u64,
                node_id: node.cloned(),
                path,
                kind,
                source,
                before_hash,
                after_hash,
                diff,
                generated,
            };
            inner.entries.push(entry.clone());
            entry
        };
        self.sink.emit(draft(
            node,
            EventKind::FileChanged {
                path: entry.path.clone(),
                change: entry.kind,
                diff: entry.diff.clone().map(Payload::Text),
                generated: entry.generated,
            },
        ));
        entry
    }

    /// Record a change reported by a file tool.
    pub fn record_tool_change(&self, node: Option<&NodeId>, change: &FileChange) -> LedgerEntry {
        let kind = match change.kind {
            ChangeKind::Created => FileChangeKind::Created,
            ChangeKind::Modified => FileChangeKind::Modified,
        };
        self.record(
            node,
            &change.path,
            kind,
            ChangeSource::Tool {
                tool: change.tool.to_string(),
            },
            change.before_hash.clone(),
            Some(change.after_hash.clone()),
            Some(change.diff.clone()),
        )
    }

    /// Snapshot the worktree before a bash command.
    pub fn begin_command(&self) -> io::Result<CommandWatch> {
        let previous = self.lock().last_snapshot.clone();
        let before = Snapshot::take_with_cache(&self.root, &self.opts, previous.as_ref())?;
        Ok(CommandWatch { before })
    }

    /// Snapshot the worktree after a bash command and record every file
    /// it created, modified or deleted. Changes to well-known artifacts
    /// ([`crate::artifacts`]) that the user's git does not track are
    /// recorded with `generated` set.
    pub fn end_command(
        &self,
        watch: CommandWatch,
        node: Option<&NodeId>,
        command: &str,
    ) -> io::Result<Vec<LedgerEntry>> {
        let after = Snapshot::take_with_cache(&self.root, &self.opts, Some(&watch.before))?;
        let changes = watch.before.diff(&after);
        self.lock().last_snapshot = Some(after);
        // A path matching the built-in list is generated only if the
        // user's git does not track it (a source directory named
        // `target/`, a committed `coverage.xml`); one git call per command,
        // and only when there are candidates.
        let candidates: Vec<&str> = changes
            .iter()
            .filter(|c| self.artifacts.is_generated(&c.path))
            .map(|c| c.path.as_str())
            .collect();
        let tracked = crate::artifacts::tracked_among(&self.root, &candidates);
        let generated: Vec<bool> = changes
            .iter()
            .map(|c| self.artifacts.is_generated(&c.path) && !tracked.contains(&c.path))
            .collect();
        Ok(changes
            .into_iter()
            .zip(generated)
            .map(|(c, generated)| {
                // Artifacts are rarely text worth reading; keep the event
                // small.
                let diff = if generated { None } else { c.diff() };
                self.record_entry(
                    node,
                    Path::new(&c.path),
                    c.kind,
                    ChangeSource::Bash {
                        command: command.to_string(),
                    },
                    c.before.map(|f| f.hash),
                    c.after.map(|f| f.hash),
                    diff,
                    generated,
                )
            })
            .collect())
    }

    /// Every entry, in order.
    pub fn entries(&self) -> Vec<LedgerEntry> {
        self.lock().entries.clone()
    }

    /// The entries made by `node`.
    pub fn for_node(&self, node: &NodeId) -> Vec<LedgerEntry> {
        self.lock()
            .entries
            .iter()
            .filter(|e| e.node_id.as_ref() == Some(node))
            .cloned()
            .collect()
    }

    /// The net effect per path, for `node` (or the whole ledger when
    /// `None`), sorted by path.
    pub fn net_changes(&self, node: Option<&NodeId>) -> Vec<NetChange> {
        let entries = match node {
            Some(n) => self.for_node(n),
            None => self.entries(),
        };
        net_changes(&entries)
    }

    /// Rebuild a ledger from a run's events (`file_changed` events only;
    /// sources become [`ChangeSource::Recorded`]). Diffs spilled to blobs
    /// are dropped; resolve them first with `BlobStore::resolve` to keep
    /// them.
    pub fn from_events<'a>(
        root: impl Into<PathBuf>,
        events: impl IntoIterator<Item = &'a RunEvent>,
    ) -> Self {
        let ledger = Self::new(root);
        {
            let mut inner = ledger.lock();
            for ev in events {
                if let EventKind::FileChanged {
                    path,
                    change,
                    diff,
                    generated,
                } = &ev.kind
                {
                    let seq = inner.entries.len() as u64;
                    inner.entries.push(LedgerEntry {
                        seq,
                        node_id: ev.node_id.clone(),
                        path: path.clone(),
                        kind: *change,
                        source: ChangeSource::Recorded,
                        before_hash: None,
                        after_hash: None,
                        diff: diff.as_ref().and_then(|d| match d {
                            Payload::Text(t) => Some(t.clone()),
                            Payload::Json(v) => Some(v.to_string()),
                            Payload::Blob(_) => None,
                        }),
                        generated: *generated,
                    });
                }
            }
        }
        ledger
    }
}

/// Fold entries into one [`NetChange`] per path.
pub fn net_changes(entries: &[LedgerEntry]) -> Vec<NetChange> {
    // (existed before the first entry, net change)
    let mut map: BTreeMap<String, (bool, NetChange)> = BTreeMap::new();
    for e in entries {
        let (existed, n) = map.entry(e.path.clone()).or_insert_with(|| {
            (
                e.kind != FileChangeKind::Created,
                NetChange {
                    path: e.path.clone(),
                    kind: e.kind,
                    sources: Vec::new(),
                    before_hash: e.before_hash.clone(),
                    after_hash: None,
                    diffs: Vec::new(),
                    generated: true,
                },
            )
        });
        n.generated &= e.generated;
        let exists = e.kind != FileChangeKind::Deleted;
        n.kind = match (*existed, exists) {
            (false, _) => FileChangeKind::Created,
            (true, false) => FileChangeKind::Deleted,
            (true, true) => FileChangeKind::Modified,
        };
        if !n.sources.contains(&e.source) {
            n.sources.push(e.source.clone());
        }
        n.after_hash = e.after_hash.clone();
        n.diffs.extend(e.diff.iter().cloned());
    }
    map.into_values()
        .filter(|(existed, n)| {
            // Created and then deleted again: no net effect.
            let last_deleted = entries
                .iter()
                .rev()
                .find(|e| e.path == n.path)
                .is_some_and(|e| e.kind == FileChangeKind::Deleted);
            *existed || !last_deleted
        })
        .map(|(_, n)| n)
        .collect()
}

/// One line per net change for node detail: `+ path (write)`,
/// `~ path (edit, bash)`, `- path (bash)`, `+ a/__pycache__/m.pyc (bash,
/// generated)`.
pub fn summary_lines(changes: &[NetChange]) -> Vec<String> {
    changes
        .iter()
        .map(|c| {
            let sign = match c.kind {
                FileChangeKind::Created => '+',
                FileChangeKind::Modified => '~',
                FileChangeKind::Deleted => '-',
            };
            let sources: Vec<&str> = c.sources.iter().map(ChangeSource::label).collect();
            let generated = if c.generated { ", generated" } else { "" };
            format!("{sign} {} ({}{generated})", c.path, sources.join(", "))
        })
        .collect()
}

#[cfg(test)]
mod tests;
