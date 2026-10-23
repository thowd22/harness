//! The `memory` tool exposed to agents.
//!
//! Actions: `search`, `read`, `write` (append one bullet, or several with
//! `bullets`, to a note) and
//! `forget` (remove matching bullets). The tool exists only for agents with
//! `memory: read` (search and read) or `memory: read-write` (all actions);
//! see [`spec`] and [`MemoryTool::for_agent`].
//!
//! Every write is validated ([`guard::validate`]: format, secret scan, task
//! state, session summaries, rediscoverable facts), applied in the session's
//! worktree, committed with a message that references the run, and published
//! to the drive (see [`crate::sync`]). New memories apply to future sessions;
//! the current session's [`crate::index::SessionIndex`] stays frozen.

pub mod guard;

use std::fmt::Write as _;
use std::fs;
use std::sync::{Arc, Mutex};

use harness_config::agents::MemoryAccess;
use harness_events::redact::Redactor;
use harness_events::schema::MemoryAction;
use harness_providers::model::ToolSpec;
use harness_tools::tool::{ToolContext, ToolError, ToolErrorKind, ToolOutput, parse_args};
use serde::Deserialize;
use serde_json::{Value, json};
use time::{Date, OffsetDateTime};

use crate::drive::{Bullet, DriveError, Note, NotePath, io_err, read_note_at, run_link};
use crate::index::{INDEX_FILE, SearchIndex, UsageKind, UsageLog};
use crate::sync::{Published, SessionCheckout};

pub use guard::{RejectKind, Rejection};

/// Tool name.
pub const NAME: &str = harness_config::agents::MEMORY_TOOL;
/// Default number of search hits.
pub const DEFAULT_SEARCH_LIMIT: usize = 5;
/// Largest accepted search limit.
pub const MAX_SEARCH_LIMIT: usize = 20;
/// Path prefix the model may use for the current project's notes:
/// `project/<topic>` expands to `projects/<project-key>/<topic>.md`.
pub const PROJECT_ALIAS: &str = "project/";
/// Most bullets one `write` may carry.
pub const MAX_BULLETS_PER_WRITE: usize = 10;

/// Description for read-write agents. It also teaches when to write.
pub const DESCRIPTION: &str = "Long-term memory shared across sessions (Markdown notes of one-line bullets). \
Actions: search {query}, read {path}, write {path, text | bullets, title?} appends one bullet (text) or up to 10 (bullets) to one note, forget {path, text} removes bullets containing text. \
Each bullet is one line of at most 400 characters (1000 in project/overview); put several lessons in one write as bullets. \
Paths: MEMORY.md (general preferences), project/<topic> (this project), tools/<tool>. Link notes with [[path]]. \
Write when: the user corrects you (save the rule to follow, not the mistake); the user states a preference; a decision is made (with its reason); you hit a non-obvious gotcha about the repo, environment or a tool. \
Never save: session summaries, task state (PR/issue numbers, statuses), facts easy to rediscover from the repo, or secrets. \
Asked to index or learn the codebase into memory: write one project/overview note (at most 10 bullets: purpose, architecture, how to build and test) plus the non-obvious gotchas you found, not a file or module map, and tell the user the rest is re-read from the repo each session. Writes apply to future sessions.";

/// Description for read-only agents.
pub const READ_ONLY_DESCRIPTION: &str = "Long-term memory shared across sessions (Markdown notes of one-line bullets). \
Actions: search {query} finds relevant bullets, read {path} returns a whole note. \
Paths: MEMORY.md, project/<topic> (this project), projects/<key>/<topic>, tools/<tool>.";

/// The JSON schema for `access`. `None` when memory is off.
pub fn schema(access: MemoryAccess) -> Option<Value> {
    let actions: &[&str] = match access {
        MemoryAccess::Off => return None,
        MemoryAccess::Read => &["search", "read"],
        MemoryAccess::ReadWrite => &["search", "read", "write", "forget"],
    };
    let mut props = json!({
        "action": {"type": "string", "enum": actions},
        "query": {"type": "string", "description": "search: words to look for"},
        "limit": {"type": "integer", "minimum": 1, "maximum": MAX_SEARCH_LIMIT},
        "path": {"type": "string", "description": "note path, e.g. project/testing or tools/cargo"},
    });
    if access == MemoryAccess::ReadWrite {
        props["text"] = json!({
            "type": "string",
            "maxLength": guard::MAX_OVERVIEW_BULLET_CHARS,
            "description": "write: one one-line lesson (at most 400 characters; 1000 in project/overview); forget: text of the bullets to remove",
        });
        props["bullets"] = json!({
            "type": "array",
            "items": {"type": "string", "maxLength": guard::MAX_OVERVIEW_BULLET_CHARS},
            "maxItems": MAX_BULLETS_PER_WRITE,
            "description": "write: several one-line lessons for the same note, one per item, each checked on its own",
        });
        props["title"] = json!({"type": "string", "description": "write: heading for a new note"});
    }
    Some(json!({
        "type": "object",
        "properties": props,
        "required": ["action"],
        "additionalProperties": false,
    }))
}

/// The tool spec to send to the model, or `None` when `access` is
/// [`MemoryAccess::Off`] (the tool then does not appear at all).
pub fn spec(access: MemoryAccess) -> Option<ToolSpec> {
    let description = match access {
        MemoryAccess::Off => return None,
        MemoryAccess::Read => READ_ONLY_DESCRIPTION,
        MemoryAccess::ReadWrite => DESCRIPTION,
    };
    Some(ToolSpec {
        name: NAME.to_owned(),
        description: description.to_owned(),
        input_schema: schema(access)?,
    })
}

/// The effective memory access of an agent: off when `[memory] enabled` is
/// false, or for headless runs unless `[memory] headless_access` is set.
pub fn effective_access(
    agent: MemoryAccess,
    config: &harness_config::ini::config::Memory,
    headless: bool,
) -> MemoryAccess {
    if !config.enabled || (headless && !config.headless_access) {
        MemoryAccess::Off
    } else {
        agent
    }
}

/// Tool arguments.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum MemoryArgs {
    /// BM25 search.
    Search {
        /// Query words.
        query: String,
        /// Max hits.
        #[serde(default)]
        limit: Option<usize>,
    },
    /// Read a whole note.
    Read {
        /// Note path.
        path: String,
    },
    /// Append one bullet (`text`) or several (`bullets`) to one note.
    Write {
        /// Note path.
        path: String,
        /// One lesson. May be empty when `bullets` is given.
        #[serde(default)]
        text: String,
        /// Several lessons, each validated on its own.
        #[serde(default)]
        bullets: Vec<String>,
        /// Heading for a new note.
        #[serde(default)]
        title: Option<String>,
    },
    /// Remove bullets containing `text`.
    Forget {
        /// Note path.
        path: String,
        /// Text the bullets contain.
        text: String,
    },
}

/// A change made by `write` or `forget`, for the run log (`EventKind::Memory`)
/// and the "Updated memory" card.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryChange {
    /// Written, Updated or Deleted.
    pub action: MemoryAction,
    /// The note.
    pub path: NotePath,
    /// The bullet texts added or removed.
    pub bullets: Vec<String>,
    /// The published commit (use it to undo with `git revert`).
    pub commit: Published,
}

impl MemoryChange {
    /// One-line summary.
    pub fn summary(&self) -> String {
        let verb = match self.action {
            MemoryAction::Deleted => "forgot",
            _ => "saved",
        };
        format!("{verb} in {}: {}", self.path, self.bullets.join(" | "))
    }
}

/// What a call produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryOutcome {
    /// Text for the model.
    pub output: String,
    /// The drive change, for writes and forgets that changed something.
    pub change: Option<MemoryChange>,
}

/// Settings for one session's memory tool.
#[derive(Debug, Clone)]
pub struct MemoryToolConfig {
    /// The agent's access (after [`effective_access`]).
    pub access: MemoryAccess,
    /// Run id recorded in commits and usage stats.
    pub run_id: String,
    /// Current project key, for `project/<topic>` paths.
    pub project_key: Option<String>,
    /// Secret scanner (the run's redactor).
    pub redactor: Arc<Redactor>,
    /// Date written into bullets; `None` means today (UTC). Fixed in tests.
    pub date: Option<Date>,
}

struct Inner {
    checkout: Arc<SessionCheckout>,
    config: MemoryToolConfig,
    usage: UsageLog,
    index: Mutex<SearchIndex>,
}

/// The memory tool for one session. Cheap to clone.
#[derive(Clone)]
pub struct MemoryTool {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for MemoryTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MemoryTool")
            .field("access", &self.inner.config.access)
            .field("run_id", &self.inner.config.run_id)
            .finish_non_exhaustive()
    }
}

impl MemoryTool {
    /// A tool over `checkout`. Prefer [`MemoryTool::for_agent`], which
    /// returns `None` for agents without memory.
    pub fn new(checkout: Arc<SessionCheckout>, config: MemoryToolConfig) -> Self {
        let internal = checkout.drive().internal_dir();
        let index = SearchIndex::load(&internal.join(INDEX_FILE));
        Self {
            inner: Arc::new(Inner {
                usage: UsageLog::new(&internal),
                checkout,
                config,
                index: Mutex::new(index),
            }),
        }
    }

    /// The tool for an agent, or `None` when `config.access` is off.
    pub fn for_agent(checkout: Arc<SessionCheckout>, config: MemoryToolConfig) -> Option<Self> {
        (config.access != MemoryAccess::Off).then(|| Self::new(checkout, config))
    }

    /// The tool spec for this tool's access.
    pub fn spec(&self) -> Option<ToolSpec> {
        spec(self.inner.config.access)
    }

    /// The access level.
    pub fn access(&self) -> MemoryAccess {
        self.inner.config.access
    }

    /// Parses `args` and runs the call. The output is text for the model.
    pub async fn call(&self, ctx: &ToolContext, args: Value) -> Result<ToolOutput, ToolError> {
        let args: MemoryArgs = parse_args(NAME, args)?;
        self.run(ctx, args)
            .await
            .map(|o| ToolOutput::Text(o.output))
    }

    /// Runs a call and returns the full outcome, including the drive change
    /// the agent should record as an `EventKind::Memory` event.
    pub async fn run(
        &self,
        _ctx: &ToolContext,
        args: MemoryArgs,
    ) -> Result<MemoryOutcome, ToolError> {
        let me = self.clone();
        tokio::task::spawn_blocking(move || me.execute(args))
            .await
            .map_err(|e| ToolError::new(ToolErrorKind::Io, format!("memory tool panicked: {e}")))?
    }

    /// Synchronous core of [`MemoryTool::run`] (runs git).
    pub fn execute(&self, args: MemoryArgs) -> Result<MemoryOutcome, ToolError> {
        let access = self.inner.config.access;
        let writes = matches!(args, MemoryArgs::Write { .. } | MemoryArgs::Forget { .. });
        if access == MemoryAccess::Off || (writes && access != MemoryAccess::ReadWrite) {
            return Err(ToolError::new(
                ToolErrorKind::Unsupported,
                "this agent has read-only memory; only search and read are available. Tell the user what you would save instead.",
            ));
        }
        match args {
            MemoryArgs::Search { query, limit } => self.search(&query, limit),
            MemoryArgs::Read { path } => self.read(&path),
            MemoryArgs::Write {
                path,
                text,
                bullets,
                title,
            } => {
                let mut items = Vec::with_capacity(bullets.len() + 1);
                if !text.trim().is_empty() {
                    items.push(text);
                }
                items.extend(bullets);
                self.write(&path, &items, title.as_deref())
            }
            MemoryArgs::Forget { path, text } => self.forget(&path, &text),
        }
    }

    fn note_path(&self, path: &str) -> Result<NotePath, ToolError> {
        let p = path.trim();
        let expanded = match (
            p.strip_prefix(PROJECT_ALIAS),
            &self.inner.config.project_key,
        ) {
            (Some(rest), Some(key)) => format!("projects/{key}/{rest}"),
            (Some(_), None) => {
                return Err(ToolError::invalid_args(
                    "there is no current project, so project/<topic> paths are unavailable. Use tools/<tool> or MEMORY.md.",
                ));
            }
            (None, _) => p.to_owned(),
        };
        NotePath::parse(&expanded).map_err(|e| {
            ToolError::invalid_args(format!(
                "{e}. Use MEMORY.md, project/<topic> or tools/<tool>."
            ))
        })
    }

    fn search(&self, query: &str, limit: Option<usize>) -> Result<MemoryOutcome, ToolError> {
        let limit = limit
            .unwrap_or(DEFAULT_SEARCH_LIMIT)
            .clamp(1, MAX_SEARCH_LIMIT);
        let hits = {
            let mut idx = self.inner.index.lock().unwrap_or_else(|p| p.into_inner());
            let stats = idx.update(self.inner.checkout.path()).map_err(drive_err)?;
            if stats.added + stats.updated + stats.removed > 0 {
                let file = self.inner.checkout.drive().internal_dir().join(INDEX_FILE);
                if let Err(e) = idx.save(&file) {
                    tracing::warn!(target: "harness::memory", "could not save the search index: {e}");
                }
            }
            idx.search(query, limit)
        };
        if hits.is_empty() {
            return Ok(text_outcome(format!(
                "No memories match `{query}`. Try other words, or continue without memory."
            )));
        }
        let mut out = String::new();
        for h in &hits {
            self.record_use(&h.path, UsageKind::SearchHit);
            let link = h.path.strip_suffix(".md").unwrap_or(&h.path);
            let _ = writeln!(
                out,
                "[[{link}]]{}",
                h.title
                    .as_deref()
                    .map(|t| format!(" {t}"))
                    .unwrap_or_default()
            );
            for s in &h.snippets {
                let _ = writeln!(out, "  - {s}");
            }
        }
        Ok(text_outcome(out))
    }

    fn read(&self, path: &str) -> Result<MemoryOutcome, ToolError> {
        let np = self.note_path(path)?;
        match read_note_at(self.inner.checkout.path(), &np).map_err(drive_err)? {
            Some(text) => {
                self.record_use(np.as_str(), UsageKind::Read);
                Ok(text_outcome(text))
            }
            None => Err(ToolError::new(
                ToolErrorKind::NotFound,
                format!("no memory note at {np}. Use the search action to find notes."),
            )),
        }
    }

    fn write(
        &self,
        path: &str,
        items: &[String],
        title: Option<&str>,
    ) -> Result<MemoryOutcome, ToolError> {
        let np = self.note_path(path)?;
        if items.is_empty() {
            return Err(ToolError::invalid_args(
                "write needs `text` (one bullet) or `bullets` (several one-line bullets for this note).",
            ));
        }
        if items.len() > MAX_BULLETS_PER_WRITE {
            return Err(ToolError::invalid_args(format!(
                "a write carries at most {MAX_BULLETS_PER_WRITE} bullets; this one has {}. Keep only the most useful lessons, or send the rest in a second write.",
                items.len()
            )));
        }
        let redactor = &self.inner.config.redactor;
        let mut accepted: Vec<String> = Vec::new();
        let mut rejected: Vec<(usize, Rejection)> = Vec::new();
        for (i, item) in items.iter().enumerate() {
            match guard::validate_for(&np, item, redactor) {
                Ok(t) if !accepted.contains(&t) => accepted.push(t),
                Ok(_) => {}
                Err(r) => rejected.push((i + 1, r)),
            }
        }
        if accepted.is_empty() {
            return Err(ToolError::invalid_args(match rejected.as_slice() {
                [(_, r)] => format!("memory not saved: {r}"),
                _ => format!(
                    "memory not saved: none of the {} bullets passed. {}",
                    items.len(),
                    describe_rejections(&rejected)
                ),
            }));
        }
        let title = match title.map(str::trim).filter(|t| !t.is_empty()) {
            Some(t) => {
                guard::check_format(t)
                    .and_then(|()| guard::check_secrets(t, redactor))
                    .map_err(|r| ToolError::invalid_args(format!("title rejected: {r}")))?;
                t.to_owned()
            }
            None => np.default_title(),
        };
        if guard::is_overview(&np) {
            self.check_overview_room(&np, &accepted)?;
        }
        let run = &self.inner.config.run_id;
        let date = self.today();
        let message = commit_message("write", &np, &accepted, run);
        let ((existed, added), published) = self
            .inner
            .checkout
            .write(&message, |dir| {
                let file = dir.join(np.as_str());
                let current = read_note_at(dir, &np)?;
                let existed = current.is_some();
                let mut note = current
                    .as_deref()
                    .map_or_else(|| Note::with_title(&title), Note::parse);
                let mut added = Vec::new();
                for text in &accepted {
                    if note.contains_text(text) {
                        continue;
                    }
                    note.push(Bullet::new(text.clone(), run_link(run), date));
                    added.push(text.clone());
                }
                if added.is_empty() {
                    return Ok((existed, added));
                }
                if let Some(parent) = file.parent() {
                    fs::create_dir_all(parent).map_err(io_err(format!("creating {np}")))?;
                }
                fs::write(&file, note.render()).map_err(io_err(format!("writing {np}")))?;
                Ok((existed, added))
            })
            .map_err(drive_err)?;
        let mut out = match &published {
            None => format!("Already in memory ({np}); nothing changed."),
            Some(commit) if items.len() == 1 => format!(
                "Saved to {np} (commit {}). It applies to future sessions.",
                short(&commit.commit)
            ),
            Some(commit) => format!(
                "Saved {} bullet(s) to {np} (commit {}). It applies to future sessions.",
                added.len(),
                short(&commit.commit)
            ),
        };
        let duplicates = accepted.len() - added.len();
        if published.is_some() && duplicates > 0 {
            let _ = write!(out, " {duplicates} already in memory.");
        }
        if !rejected.is_empty() {
            let _ = write!(
                out,
                " Not saved: {} Resend only those, fixed, or skip them.",
                describe_rejections(&rejected)
            );
        }
        let Some(commit) = published else {
            return Ok(text_outcome(out));
        };
        self.reindex(&np);
        Ok(MemoryOutcome {
            output: out,
            change: Some(MemoryChange {
                action: if existed {
                    MemoryAction::Updated
                } else {
                    MemoryAction::Written
                },
                path: np,
                bullets: added,
                commit,
            }),
        })
    }

    /// Refuses a write that would grow a project's overview note past
    /// [`guard::MAX_OVERVIEW_BULLETS`].
    fn check_overview_room(&self, np: &NotePath, new: &[String]) -> Result<(), ToolError> {
        let note = read_note_at(self.inner.checkout.path(), np)
            .map_err(drive_err)?
            .map(|t| Note::parse(&t))
            .unwrap_or_default();
        let have = note.bullets().count();
        let adding = new.iter().filter(|t| !note.contains_text(t)).count();
        if have + adding > guard::MAX_OVERVIEW_BULLETS {
            return Err(ToolError::invalid_args(format!(
                "memory not saved: {np} holds at most {} bullets and already has {have}; this write adds {adding}. Keep the overview compact: read it, forget or merge bullets, then write again. Save gotchas in another project note.",
                guard::MAX_OVERVIEW_BULLETS
            )));
        }
        Ok(())
    }

    fn forget(&self, path: &str, text: &str) -> Result<MemoryOutcome, ToolError> {
        let np = self.note_path(path)?;
        let needle = text.trim();
        if needle.is_empty() {
            return Err(ToolError::invalid_args(
                "forget needs `text`: part of the bullet to remove. Read the note first to copy it.",
            ));
        }
        let run = &self.inner.config.run_id;
        // The message lists the removed bullets, so build it from a preview.
        let preview = read_note_at(self.inner.checkout.path(), &np)
            .map_err(drive_err)?
            .map(|t| Note::parse(&t))
            .unwrap_or_default();
        let preview: Vec<String> = preview
            .matching(needle)
            .into_iter()
            .map(|b| b.text.clone())
            .collect();
        let message = commit_message("forget", &np, &preview, run);
        let (removed, published) = self
            .inner
            .checkout
            .write(&message, |dir| {
                let file = dir.join(np.as_str());
                let Some(current) = read_note_at(dir, &np)? else {
                    return Ok(Vec::new());
                };
                let mut note = Note::parse(&current);
                let removed = note.remove_matching(needle);
                if removed.is_empty() {
                    return Ok(removed);
                }
                if note.is_empty() && np.as_str() != crate::drive::MEMORY_FILE {
                    fs::remove_file(&file).map_err(io_err(format!("removing {np}")))?;
                } else {
                    fs::write(&file, note.render()).map_err(io_err(format!("writing {np}")))?;
                }
                Ok(removed)
            })
            .map_err(drive_err)?;
        let Some(commit) = published else {
            return Err(ToolError::new(
                ToolErrorKind::NoMatch,
                format!(
                    "no bullet in {np} contains `{needle}`. Read the note and copy part of the exact bullet."
                ),
            ));
        };
        self.reindex(&np);
        let bullets: Vec<String> = removed.into_iter().map(|b| b.text).collect();
        Ok(MemoryOutcome {
            output: format!(
                "Forgot {} bullet(s) from {np} (commit {}).",
                bullets.len(),
                short(&commit.commit)
            ),
            change: Some(MemoryChange {
                action: MemoryAction::Deleted,
                path: np,
                bullets,
                commit,
            }),
        })
    }

    fn reindex(&self, np: &NotePath) {
        let text = read_note_at(self.inner.checkout.path(), np).ok().flatten();
        self.inner
            .index
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .upsert(np, text.as_deref());
    }

    fn record_use(&self, note: &str, kind: UsageKind) {
        if let Err(e) = self
            .inner
            .usage
            .note_used(note, &self.inner.config.run_id, kind)
        {
            tracing::warn!(target: "harness::memory", "could not record memory usage: {e}");
        }
    }

    fn today(&self) -> Date {
        self.inner
            .config
            .date
            .unwrap_or_else(|| OffsetDateTime::now_utc().date())
    }
}

/// The commit message for a memory change. The trailer `Run:` references the
/// run link so every commit can be traced back to its run.
pub fn commit_message(verb: &str, path: &NotePath, bullets: &[String], run_id: &str) -> String {
    let mut m = format!("memory: {verb} {path}\n\n");
    for b in bullets {
        let _ = writeln!(m, "- {b}");
    }
    let _ = write!(m, "\nRun: {}", run_link(run_id));
    m
}

/// "bullet 2: <reason>; bullet 4: <reason>." for a multi-bullet write.
fn describe_rejections(rejected: &[(usize, Rejection)]) -> String {
    let parts: Vec<String> = rejected
        .iter()
        .map(|(i, r)| format!("bullet {i}: {r}"))
        .collect();
    parts.join("; ")
}

fn text_outcome(output: String) -> MemoryOutcome {
    MemoryOutcome {
        output,
        change: None,
    }
}

fn short(sha: &str) -> &str {
    sha.get(..7).unwrap_or(sha)
}

fn drive_err(e: DriveError) -> ToolError {
    ToolError::new(
        ToolErrorKind::Io,
        format!("memory drive error: {e}. Retry once; if it fails again, continue without memory."),
    )
}
