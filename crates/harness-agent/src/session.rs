//! Interactive session state: persistence, listing and resume.
//!
//! A session is a conversation with one agent in one project. It may span
//! several runs (one per process lifetime): resuming starts a *new* run in
//! the same session, so an `events.jsonl` that was torn by a crash is never
//! appended to.
//!
//! - [`SessionStore`] keeps `<data_dir>/sessions/<id>/session.json`
//!   ([`SessionMeta`]: title, project, times, cost, runs and file hashes).
//!   [`SessionHandle::record`] rewrites it atomically after every loop run,
//!   so a session is never more than one turn behind its event log.
//! - [`rebuild_conversation`] folds a run's events back into the exact
//!   provider conversation, including signed reasoning and opaque blocks.
//! - [`SessionStore::resume`] rebuilds the latest run and compares the hashes
//!   of every file the conversation touched with the disk
//!   ([`FileDrift`]), so the user is warned when files changed.

use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::path::{Path, PathBuf};

use harness_events::blobs::BlobStore;
use harness_events::schema::{BlobEncoding, EventKind, Payload, RunEvent, RunId};
use harness_events::store::{self, StoreError};
use harness_providers::model::{ContentBlock, Message, Role, ToolResultPart};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use time::OffsetDateTime;

use crate::agent_loop::{Conversation, LoopOutcome};
use crate::robustness;

/// File name of the session metadata.
pub const SESSION_FILE: &str = "session.json";

/// Maximum title length in characters.
pub const TITLE_MAX_CHARS: usize = 60;

/// Errors from session persistence.
#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    /// Filesystem error.
    #[error("session i/o: {0}")]
    Io(#[from] io::Error),
    /// Metadata is not valid JSON.
    #[error("session metadata {path}: {source}")]
    Corrupt {
        /// The file.
        path: PathBuf,
        /// Parse error.
        source: serde_json::Error,
    },
    /// No session matches.
    #[error("no session matches `{0}`")]
    NotFound(String),
    /// Several sessions match a prefix.
    #[error("`{prefix}` matches {count} sessions; use more characters")]
    Ambiguous {
        /// The prefix given.
        prefix: String,
        /// How many matched.
        count: usize,
    },
    /// The session has no runs to resume from.
    #[error("session {0} has no recorded runs")]
    NoRuns(String),
    /// The run log could not be read.
    #[error("run log: {0}")]
    Store(#[from] StoreError),
    /// A recorded payload could not be decoded.
    #[error("run log payload: {0}")]
    Payload(String),
}

/// Persistent metadata of one session.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionMeta {
    /// Session id (UUID v7, sortable by creation time).
    pub id: String,
    /// Short title (from the first prompt).
    pub title: String,
    /// Project root.
    pub project: PathBuf,
    /// Creation time.
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    /// Last save.
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
    /// Agent name.
    pub agent: String,
    /// Model reference.
    pub model: String,
    /// Known cost in USD over all runs.
    pub cost_usd: f64,
    /// True when some call's cost was unknown.
    #[serde(default)]
    pub cost_unknown: bool,
    /// Total tokens over all runs.
    pub tokens: u64,
    /// Runs of this session, oldest first.
    pub runs: Vec<RunId>,
    /// Stop code of the last loop run (`end_turn`, `cancelled`, ...).
    #[serde(default)]
    pub last_stop: Option<String>,
    /// sha256 of every project file the conversation touched, as of the
    /// last save (`None` = the file did not exist).
    #[serde(default)]
    pub files: BTreeMap<String, Option<String>>,
}

impl SessionMeta {
    /// A new session for `project`.
    pub fn new(
        project: impl Into<PathBuf>,
        agent: impl Into<String>,
        model: impl Into<String>,
        prompt: &str,
    ) -> Self {
        let now = OffsetDateTime::now_utc();
        Self {
            id: uuid::Uuid::now_v7().to_string(),
            title: title_from_prompt(prompt),
            project: project.into(),
            created_at: now,
            updated_at: now,
            agent: agent.into(),
            model: model.into(),
            cost_usd: 0.0,
            cost_unknown: false,
            tokens: 0,
            runs: Vec::new(),
            last_stop: None,
            files: BTreeMap::new(),
        }
    }

    /// One line for listings: id prefix, time, cost, project and title.
    pub fn listing_line(&self) -> String {
        let when = self
            .updated_at
            .format(&time::macros::format_description!(
                "[year]-[month]-[day] [hour]:[minute]"
            ))
            .unwrap_or_default();
        let cost = if self.cost_unknown && self.cost_usd == 0.0 {
            "unknown".to_owned()
        } else {
            format!(
                "${:.4}{}",
                self.cost_usd,
                if self.cost_unknown { "+" } else { "" }
            )
        };
        format!(
            "{}  {when}  {cost:>9}  {}  {}",
            &self.id[..self.id.len().min(13)],
            self.project.display(),
            self.title
        )
    }
}

/// A title from the first prompt: first line, whitespace collapsed,
/// at most [`TITLE_MAX_CHARS`] characters.
pub fn title_from_prompt(prompt: &str) -> String {
    let line = prompt.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
    let words: Vec<&str> = line.split_whitespace().collect();
    let title = words.join(" ");
    if title.is_empty() {
        "(untitled)".into()
    } else {
        robustness::truncate_chars(&title, TITLE_MAX_CHARS)
    }
}

/// Stores session metadata under one directory.
#[derive(Debug, Clone)]
pub struct SessionStore {
    dir: PathBuf,
}

impl SessionStore {
    /// A store at `dir` (normally `DataDir::sessions()`).
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    /// The directory.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn file(&self, id: &str) -> PathBuf {
        self.dir.join(id).join(SESSION_FILE)
    }

    /// Writes `meta` atomically (temp file + rename).
    pub fn save(&self, meta: &SessionMeta) -> Result<(), SessionError> {
        let path = self.file(&meta.id);
        let dir = path.parent().unwrap_or(&self.dir);
        std::fs::create_dir_all(dir)?;
        let tmp = dir.join(format!(".{SESSION_FILE}.tmp"));
        let text = serde_json::to_vec_pretty(meta).map_err(io::Error::from)?;
        std::fs::write(&tmp, text)?;
        std::fs::rename(&tmp, &path)?;
        Ok(())
    }

    /// Loads session `id`.
    pub fn load(&self, id: &str) -> Result<SessionMeta, SessionError> {
        let path = self.file(id);
        let text = match std::fs::read(&path) {
            Ok(t) => t,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                return Err(SessionError::NotFound(id.into()));
            }
            Err(e) => return Err(e.into()),
        };
        serde_json::from_slice(&text).map_err(|source| SessionError::Corrupt { path, source })
    }

    /// Every readable session, most recently updated first. Unreadable
    /// entries are skipped with a warning.
    pub fn list(&self) -> Result<Vec<SessionMeta>, SessionError> {
        let mut out = Vec::new();
        let entries = match std::fs::read_dir(&self.dir) {
            Ok(e) => e,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(out),
            Err(e) => return Err(e.into()),
        };
        for entry in entries {
            let entry = entry?;
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            if !entry.path().join(SESSION_FILE).is_file() {
                continue;
            }
            match self.load(&name) {
                Ok(meta) => out.push(meta),
                Err(e) => {
                    tracing::warn!(target: "harness::session", session = %name, error = %e, "skipping unreadable session")
                }
            }
        }
        out.sort_by(|a, b| b.updated_at.cmp(&a.updated_at).then(b.id.cmp(&a.id)));
        Ok(out)
    }

    /// Finds a session by id or unique id prefix. `last` picks the most
    /// recently updated session.
    pub fn find(&self, id_or_prefix: &str) -> Result<SessionMeta, SessionError> {
        let all = self.list()?;
        if id_or_prefix == "last" {
            return all
                .into_iter()
                .next()
                .ok_or_else(|| SessionError::NotFound("last".into()));
        }
        let matches: Vec<SessionMeta> = all
            .into_iter()
            .filter(|m| m.id.starts_with(id_or_prefix))
            .collect();
        match matches.len() {
            0 => Err(SessionError::NotFound(id_or_prefix.into())),
            1 => Ok(matches.into_iter().next().unwrap_or_else(|| unreachable!())),
            count => Err(SessionError::Ambiguous {
                prefix: id_or_prefix.into(),
                count,
            }),
        }
    }

    /// Rebuilds the conversation of session `id` from its latest run and
    /// checks the files it touched for changes on disk.
    pub fn resume(&self, id_or_prefix: &str, data_dir: &Path) -> Result<Resumed, SessionError> {
        let meta = self.find(id_or_prefix)?;
        let run = *meta
            .runs
            .last()
            .ok_or_else(|| SessionError::NoRuns(meta.id.clone()))?;
        let run_dir = store::run_dir(data_dir, run);
        let replay = store::read_run(&run_dir)?;
        let mut warnings = Vec::new();
        if replay.truncated {
            warnings.push(format!(
                "the last event of run {} was cut off (crash); the conversation is rebuilt up to the last complete event",
                run.0
            ));
        }
        let blobs = BlobStore::new(run_dir.join(harness_events::blobs::BLOBS_DIR));
        let conversation = rebuild_conversation(&replay.events, &blobs)?;
        let drift = file_drift(&meta.project, &meta.files);
        warnings.extend(drift.iter().map(FileDrift::warning));
        Ok(Resumed {
            meta,
            conversation,
            drift,
            warnings,
        })
    }
}

/// A rebuilt session ready to continue.
#[derive(Debug, Clone)]
pub struct Resumed {
    /// The session.
    pub meta: SessionMeta,
    /// The provider conversation as of the last recorded event.
    pub conversation: Conversation,
    /// Files that changed since the last save.
    pub drift: Vec<FileDrift>,
    /// Messages for the user (drift, truncated log).
    pub warnings: Vec<String>,
}

/// How a touched file differs from the saved hash.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DriftKind {
    /// Content changed.
    Modified,
    /// The file is gone.
    Deleted,
    /// The file did not exist and now does.
    Created,
}

/// A file that changed on disk since the session was saved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileDrift {
    /// Project-relative path.
    pub path: String,
    /// What changed.
    pub kind: DriftKind,
}

impl FileDrift {
    /// User-facing warning text.
    pub fn warning(&self) -> String {
        let what = match self.kind {
            DriftKind::Modified => "was modified",
            DriftKind::Deleted => "was deleted",
            DriftKind::Created => "was created",
        };
        format!(
            "{} {what} on disk since this session was saved; the agent's view of it may be stale",
            self.path
        )
    }
}

fn hash_file(path: &Path) -> Option<String> {
    std::fs::read(path)
        .ok()
        .map(|b| hex::encode(Sha256::digest(&b)))
}

/// Hashes `paths` (project-relative) under `project`.
pub fn snapshot_files<'a>(
    project: &Path,
    paths: impl IntoIterator<Item = &'a str>,
) -> BTreeMap<String, Option<String>> {
    paths
        .into_iter()
        .map(|p| (p.to_owned(), hash_file(&project.join(p))))
        .collect()
}

/// Files whose current hash differs from `saved`.
pub fn file_drift(project: &Path, saved: &BTreeMap<String, Option<String>>) -> Vec<FileDrift> {
    saved
        .iter()
        .filter_map(|(path, old)| {
            let now = hash_file(&project.join(path));
            let kind = match (old, &now) {
                (Some(a), Some(b)) if a != b => DriftKind::Modified,
                (Some(_), None) => DriftKind::Deleted,
                (None, Some(_)) => DriftKind::Created,
                _ => return None,
            };
            Some(FileDrift {
                path: path.clone(),
                kind,
            })
        })
        .collect()
}

/// Project-relative paths the conversation's `read`, `write` and `edit`
/// calls touched. Paths outside the project are ignored.
pub fn touched_paths(conversation: &Conversation, project: &Path) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for msg in &conversation.messages {
        for (_, name, input) in msg.tool_uses() {
            if !matches!(name, "read" | "write" | "edit") {
                continue;
            }
            let Some(path) = input.get("path").and_then(Value::as_str) else {
                continue;
            };
            if path.starts_with(harness_tools::spill::SPILL_SCHEME) {
                continue;
            }
            let abs = harness_tools::workspace::normalize(&project.join(path));
            if let Ok(rel) = abs.strip_prefix(project) {
                out.insert(rel.display().to_string());
            }
        }
    }
    out
}

/// A live session: metadata kept in sync with the loop.
#[derive(Debug, Clone)]
pub struct SessionHandle {
    store: SessionStore,
    meta: SessionMeta,
}

impl SessionHandle {
    /// Saves `meta` and returns a handle.
    pub fn start(store: SessionStore, meta: SessionMeta) -> Result<Self, SessionError> {
        store.save(&meta)?;
        Ok(Self { store, meta })
    }

    /// The metadata.
    pub fn meta(&self) -> &SessionMeta {
        &self.meta
    }

    /// Registers a new run of this session and saves.
    pub fn add_run(&mut self, run: RunId) -> Result<(), SessionError> {
        if !self.meta.runs.contains(&run) {
            self.meta.runs.push(run);
        }
        self.touch_and_save()
    }

    /// Records a finished loop run: totals, stop code and file hashes.
    pub fn record(
        &mut self,
        outcome: &LoopOutcome,
        conversation: &Conversation,
    ) -> Result<(), SessionError> {
        self.meta.cost_usd += outcome.cost_usd;
        self.meta.cost_unknown |= outcome.cost_unknown;
        self.meta.tokens += outcome.usage.total();
        self.meta.last_stop = Some(outcome.stop.code().to_owned());
        let paths = touched_paths(conversation, &self.meta.project);
        self.meta.files = snapshot_files(&self.meta.project, paths.iter().map(String::as_str));
        self.touch_and_save()
    }

    fn touch_and_save(&mut self) -> Result<(), SessionError> {
        self.meta.updated_at = OffsetDateTime::now_utc();
        self.store.save(&self.meta)
    }
}

fn resolve(blobs: &BlobStore, payload: &Payload) -> Result<Value, SessionError> {
    let resolved = blobs
        .resolve(payload)
        .map_err(|e| SessionError::Payload(e.to_string()))?;
    match resolved {
        Payload::Json(v) => Ok(v),
        Payload::Text(t) => {
            serde_json::from_str(&t).map_err(|e| SessionError::Payload(e.to_string()))
        }
        Payload::Blob(b) => match b.encoding {
            BlobEncoding::Json | BlobEncoding::Text => Err(SessionError::Payload(format!(
                "unresolved blob {}",
                b.sha256
            ))),
        },
    }
}

fn resolve_text(blobs: &BlobStore, payload: &Payload) -> Result<Payload, SessionError> {
    blobs
        .resolve(payload)
        .map_err(|e| SessionError::Payload(e.to_string()))
}

/// Rebuilds the provider conversation from a run's events.
///
/// Only top-level agent nodes (no parent) count; subagents have their own
/// conversations. Each `model_call_started` request carries
/// `messages_from` and the messages added since then, plus `system` when
/// it changed; each `model_call_finished` carries the assistant message.
/// `tool_result` events after the last model call become a trailing user
/// message, and any `tool_use` still without a result gets an error result,
/// so the rebuilt conversation is always valid for the provider.
pub fn rebuild_conversation(
    events: &[RunEvent],
    blobs: &BlobStore,
) -> Result<Conversation, SessionError> {
    let mut system: Option<String> = None;
    let mut messages: Vec<Message> = Vec::new();
    let mut trailing: Vec<ContentBlock> = Vec::new();
    for event in events.iter().filter(|e| e.parent_node_id.is_none()) {
        match &event.kind {
            EventKind::ModelCallStarted { request, .. } => {
                let v = resolve(blobs, request)?;
                let from = v.get("messages_from").and_then(Value::as_u64).unwrap_or(0);
                let from = usize::try_from(from)
                    .unwrap_or(usize::MAX)
                    .min(messages.len());
                let new: Vec<Message> = serde_json::from_value(
                    v.get("messages")
                        .cloned()
                        .unwrap_or(Value::Array(Vec::new())),
                )
                .map_err(|e| SessionError::Payload(format!("request messages: {e}")))?;
                if let Some(s) = v.get("system") {
                    system = s.as_str().map(str::to_owned);
                }
                messages.truncate(from);
                messages.extend(new);
                trailing.clear();
            }
            EventKind::ModelCallFinished {
                response: Some(response),
                ..
            } => {
                let v = resolve(blobs, response)?;
                let msg: Message = serde_json::from_value(v)
                    .map_err(|e| SessionError::Payload(format!("response message: {e}")))?;
                messages.push(msg);
                trailing.clear();
            }
            EventKind::ToolResult {
                call_id,
                ok,
                output,
                ..
            } => {
                let content = match resolve_text(blobs, output)? {
                    Payload::Text(text) => vec![ToolResultPart::Text { text }],
                    Payload::Json(v) => serde_json::from_value(v.clone()).unwrap_or_else(|_| {
                        vec![ToolResultPart::Text {
                            text: v.to_string(),
                        }]
                    }),
                    Payload::Blob(_) => Vec::new(),
                };
                trailing.push(ContentBlock::ToolResult {
                    tool_use_id: call_id.clone(),
                    content,
                    is_error: !ok,
                });
            }
            _ => {}
        }
    }
    if !trailing.is_empty() && messages.last().is_some_and(|m| m.role == Role::Assistant) {
        messages.push(Message {
            role: Role::User,
            content: trailing,
        });
    }
    robustness::ensure_tool_results(&mut messages);
    Ok(Conversation::with_messages(system, messages))
}

#[cfg(test)]
mod tests;
