//! Interactive session state: persistence, listing and resume.
//!
//! A session is a conversation with one agent in one project. It may span
//! several runs (one per process lifetime): resuming starts a *new* run in
//! the same session, so an `events.jsonl` that was torn by a crash is never
//! appended to.
//!
//! - [`SessionStore`] keeps `<data_dir>/sessions/<id>/session.json`
//!   ([`SessionMeta`]: name, description, project, times, message count,
//!   cost, runs, file hashes and the `/effort` override).
//! - Names and descriptions are derived locally, without a model call:
//!   the name from the first prompt ([`names::session_name`], until the
//!   user sets one with `/rename`), the description from the first and
//!   latest requests and the files edited ([`describe`]). An optional
//!   cheap-model summary ([`summary_request`], [`parse_summary`]) can
//!   replace the description after each turn.
//! - [`SessionStore::find`] resolves `last`, an id, an id prefix, a short
//!   tag (`#1a2b3c`) or a session name.
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

use harness_config::agents::Effort;
use harness_events::blobs::BlobStore;
use harness_events::names;
use harness_events::schema::{BlobEncoding, EventKind, Payload, RunEvent, RunId};
use harness_events::store::{self, StoreError};
use harness_policy::provenance;
use harness_providers::model::{ChatRequest, ContentBlock, Message, Role, ToolResultPart};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use time::{OffsetDateTime, UtcOffset};

use crate::agent_loop::{Conversation, LoopOutcome, VERIFY_FEEDBACK_PREFIX};
use crate::compaction::HANDOFF_OPEN;
use crate::robustness;

/// File name of the session metadata.
pub const SESSION_FILE: &str = "session.json";

/// Maximum title length in characters.
pub const TITLE_MAX_CHARS: usize = 60;

/// Longest name `/rename` accepts, in characters.
pub const NAME_MAX_CHARS: usize = 60;

/// Longest description, in characters.
pub const DESCRIPTION_MAX_CHARS: usize = 120;

/// The title of a session whose first prompt is not known yet.
pub const UNTITLED: &str = "(untitled)";

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
    /// A name for `/rename` that is empty after trimming.
    #[error("a session name needs at least one visible character")]
    EmptyName,
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
    /// Short name (a few words). Derived from the first prompt until the
    /// user sets one with `/rename`. Empty in sessions saved before names
    /// existed; [`SessionMeta::display_name`] fills it in.
    #[serde(default)]
    pub name: String,
    /// True when the user chose the name; it is then never re-derived.
    #[serde(default)]
    pub renamed: bool,
    /// One line on what the session is about, updated after every turn.
    #[serde(default)]
    pub description: String,
    /// When the last message (prompt or reply) was recorded.
    #[serde(default, with = "time::serde::rfc3339::option")]
    pub last_message_at: Option<OffsetDateTime>,
    /// Prompts and replies in the conversation (tool results not counted).
    #[serde(default)]
    pub messages: usize,
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
    /// Reasoning effort chosen with `/effort` for the session's agent;
    /// `None` uses the agent YAML's `effort` or the model default. Restored
    /// on resume.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<Effort>,
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
            name: names::session_name(prompt),
            renamed: false,
            description: names::title_from_text(prompt).unwrap_or_default(),
            last_message_at: None,
            messages: 0,
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
            effort: None,
        }
    }

    /// The name to show: the saved name, else one derived from the title
    /// (sessions saved before names existed).
    pub fn display_name(&self) -> String {
        if !self.name.trim().is_empty() {
            return self.name.clone();
        }
        if self.title == UNTITLED {
            return names::UNNAMED_SESSION.to_owned();
        }
        names::session_name(&self.title)
    }

    /// The description to show: the saved one, else the title.
    pub fn display_description(&self) -> String {
        if !self.description.trim().is_empty() {
            return self.description.clone();
        }
        if self.title == UNTITLED {
            return String::new();
        }
        self.title.clone()
    }

    /// A session that never got a prompt or a name (the TUI saves one at
    /// startup). Lists leave these out.
    pub fn is_empty(&self) -> bool {
        self.title == UNTITLED && self.messages == 0 && !self.renamed
    }

    /// When the session was last active: its last message, else (older
    /// sessions, or none sent yet) its last save.
    pub fn last_activity(&self) -> OffsetDateTime {
        self.last_message_at.unwrap_or(self.updated_at)
    }

    /// The cost as shown in lists: `$0.0412`, `$0.0412+` when some calls
    /// had no price, `unknown` when none had.
    pub fn cost_label(&self) -> String {
        if self.cost_unknown && self.cost_usd == 0.0 {
            "unknown".to_owned()
        } else {
            format!(
                "${:.4}{}",
                self.cost_usd,
                if self.cost_unknown { "+" } else { "" }
            )
        }
    }

    /// `1 msg`, `12 msgs`.
    pub fn messages_label(&self) -> String {
        format!(
            "{} msg{}",
            self.messages,
            if self.messages == 1 { "" } else { "s" }
        )
    }

    /// The short tag of the id (`#1a2b3c`), accepted by
    /// [`SessionStore::find`].
    pub fn tag(&self) -> String {
        names::short_tag(&self.id)
    }

    /// One line for `harness sessions`: name, last activity
    /// ([`names::activity_label`] in `offset`), project, model, message
    /// count, cost, tag and description.
    pub fn listing_line(&self, offset: UtcOffset, now: OffsetDateTime) -> String {
        let when = names::activity_label(self.last_activity(), offset, now);
        let mut line = format!(
            "{:<24}  {when:<17}  {}  {}  {:>8}  {:>9}  {}",
            names::shorten(&self.display_name(), 24),
            self.project.display(),
            self.model,
            self.messages_label(),
            self.cost_label(),
            self.tag(),
        );
        let desc = self.display_description();
        if !desc.is_empty() {
            line.push_str("  ");
            line.push_str(&desc);
        }
        line
    }

    /// Applies a reply of the optional summary model: the description
    /// (when the reply had one), the name unless the user chose one, and
    /// the call's tokens and cost.
    pub fn apply_summary(&mut self, update: &SummaryUpdate) {
        if let Some(n) = update.name.clone().filter(|_| !self.renamed) {
            self.name = n;
        }
        if let Some(d) = &update.description {
            self.description = d.clone();
        }
        self.tokens += update.tokens;
        if let Some(usd) = update.cost_usd {
            self.cost_usd += usd;
        }
        self.cost_unknown |= update.cost_unknown;
    }

    /// Updates name, description, message count and last-message time
    /// from `conversation` (after a turn). The name is derived only while
    /// the session has none (and never after `/rename`).
    pub fn refresh_from(&mut self, conversation: &Conversation, now: OffsetDateTime) {
        let prompts = user_prompts(conversation);
        if let Some(first) = prompts.first() {
            if self.title == UNTITLED {
                self.title = title_from_prompt(first);
            }
            if !self.renamed && (self.name.trim().is_empty() || self.name == names::UNNAMED_SESSION)
            {
                self.name = names::session_name(first);
            }
        }
        self.description = describe(conversation, &self.project, &self.title);
        let count = message_count(conversation);
        if count != self.messages || self.last_message_at.is_none() {
            self.last_message_at = Some(now);
        }
        self.messages = count;
    }
}

/// True for user-role text the harness wrote rather than the user: a
/// provenance-marked block (`<tool_result ...>` such as the verifier's fix
/// feedback, `<project_file ...>`, `<web_content ...>`), the compaction
/// handoff, or unmarked verifier feedback. Such text is not a request.
pub fn is_harness_text(text: &str) -> bool {
    let t = text.trim_start();
    if t.starts_with(HANDOFF_OPEN) || t.starts_with(VERIFY_FEEDBACK_PREFIX) {
        return true;
    }
    let Some(rest) = t.strip_prefix('<') else {
        return false;
    };
    provenance::TAGS.iter().any(|tag| {
        rest.strip_prefix(tag)
            .is_some_and(|r| r.starts_with(|c: char| c == '>' || c.is_whitespace()))
    })
}

/// The user's own text in `message`: its text blocks without the ones
/// the harness wrote ([`is_harness_text`]). `None` for assistant
/// messages, tool results and harness-only messages.
fn request_text(message: &Message) -> Option<String> {
    if message.role != Role::User {
        return None;
    }
    let text: String = message
        .content
        .iter()
        .filter_map(|b| match b {
            ContentBlock::Text { text } if !is_harness_text(text) => Some(text.as_str()),
            _ => None,
        })
        .collect();
    (!text.trim().is_empty()).then_some(text)
}

/// Text of every user request in `conversation`, oldest first: user
/// messages with text the user wrote. Tool results, verifier feedback and
/// compaction handoffs are not requests ([`is_harness_text`]).
pub fn user_prompts(conversation: &Conversation) -> Vec<String> {
    conversation
        .messages
        .iter()
        .filter_map(request_text)
        .collect()
}

/// Prompts and replies in `conversation`: assistant messages and user
/// messages the user wrote (text or images), not tool results, verifier
/// feedback or compaction handoffs.
pub fn message_count(conversation: &Conversation) -> usize {
    conversation
        .messages
        .iter()
        .filter(|m| {
            m.role == Role::Assistant
                || m.content.iter().any(|b| match b {
                    ContentBlock::Text { text } => {
                        !text.trim().is_empty() && !is_harness_text(text)
                    }
                    ContentBlock::ToolResult { .. } => false,
                    _ => true,
                })
        })
        .count()
}

/// A one-line description of a session, derived locally: the first
/// request (`title`, else the first prompt), the latest request when
/// there were several, and the files the agent wrote or edited, cut to
/// [`DESCRIPTION_MAX_CHARS`].
///
/// `fix the header parser → add tests for colons · edited src/parse.rs,
/// tests/parse.rs`
pub fn describe(conversation: &Conversation, project: &Path, title: &str) -> String {
    let prompts = user_prompts(conversation);
    let first = (title != UNTITLED && !title.trim().is_empty())
        .then(|| title.to_owned())
        .or_else(|| prompts.first().and_then(|p| names::title_from_text(p)));
    let mut out = first.clone().unwrap_or_default();
    if prompts.len() > 1
        && let Some(latest) = prompts.last().and_then(|p| names::title_from_text(p))
        && Some(&latest) != first.as_ref()
    {
        if !out.is_empty() {
            out.push_str(" → ");
        }
        out.push_str(&latest);
    }
    let edited = edited_paths(conversation, project);
    if edited.is_empty() {
        return names::shorten(&out, DESCRIPTION_MAX_CHARS);
    }
    const SHOWN: usize = 3;
    let mut files = format!(
        "edited {}",
        edited
            .iter()
            .take(SHOWN)
            .cloned()
            .collect::<Vec<_>>()
            .join(", ")
    );
    if edited.len() > SHOWN {
        files.push_str(&format!(" +{}", edited.len() - SHOWN));
    }
    let files = names::shorten(&files, DESCRIPTION_MAX_CHARS / 2);
    if out.is_empty() {
        return files;
    }
    // The requests give way first, so the edited files stay visible.
    let room = DESCRIPTION_MAX_CHARS.saturating_sub(files.chars().count() + 3);
    format!("{} · {files}", names::shorten(&out, room))
}

/// Project-relative paths the conversation wrote or edited, in the order
/// first touched.
fn edited_paths(conversation: &Conversation, project: &Path) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for msg in &conversation.messages {
        for (_, name, input) in msg.tool_uses() {
            if !matches!(name, "write" | "edit") {
                continue;
            }
            let Some(path) = input.get("path").and_then(Value::as_str) else {
                continue;
            };
            let abs = harness_tools::workspace::normalize(&project.join(path));
            let shown = match abs.strip_prefix(project) {
                Ok(rel) => rel.display().to_string(),
                Err(_) => path.to_owned(),
            };
            if !out.contains(&shown) {
                out.push(shown);
            }
        }
    }
    out
}

/// A name for `/rename`: whitespace collapsed, at most
/// [`NAME_MAX_CHARS`] characters.
pub fn clean_name(name: &str) -> Result<String, SessionError> {
    let collapsed = name.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.is_empty() {
        return Err(SessionError::EmptyName);
    }
    Ok(names::shorten(&collapsed, NAME_MAX_CHARS))
}

/// System prompt of the optional session summary call.
pub const SUMMARY_SYSTEM: &str = "You name and describe coding sessions for a session list. \
Reply with exactly two lines:\nname: <2 to 5 words>\ndescription: <one sentence, at most 15 words, on what the session did>";

/// Most transcript characters sent to the summary model.
pub const SUMMARY_TRANSCRIPT_CHARS: usize = 6000;

/// A compact transcript for the summary model: each prompt and reply as
/// `user:` / `assistant:` lines, tool calls by name, newest kept when it
/// is longer than `max_chars`. Redact it before sending.
pub fn summary_transcript(conversation: &Conversation, max_chars: usize) -> String {
    let mut lines: Vec<String> = Vec::new();
    for m in &conversation.messages {
        match m.role {
            Role::User => {
                if let Some(text) = request_text(m) {
                    lines.push(format!("user: {}", text.trim()));
                }
            }
            Role::Assistant => {
                let text = m.text();
                let text = text.trim();
                if !text.is_empty() {
                    lines.push(format!("assistant: {text}"));
                }
                for (_, name, input) in m.tool_uses() {
                    let path = input.get("path").and_then(Value::as_str).unwrap_or("");
                    lines.push(format!("tool: {name} {path}").trim_end().to_owned());
                }
            }
        }
    }
    let all = lines.join("\n");
    let n = all.chars().count();
    if n <= max_chars {
        return all;
    }
    all.chars().skip(n - max_chars).collect()
}

/// The request of the optional session summary call to `model` (a cheap
/// model; `[general] session_summary_model`).
pub fn summary_request(model: impl Into<String>, transcript: &str) -> ChatRequest {
    let mut req = ChatRequest::new(model.into());
    req.system = Some(SUMMARY_SYSTEM.to_owned());
    req.messages = vec![Message::user(format!(
        "Session transcript:\n{transcript}\n\nName and describe this session."
    ))];
    req.max_tokens = Some(120);
    req
}

/// What one call of the optional summary model changes in a session
/// ([`SessionMeta::apply_summary`]).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SummaryUpdate {
    /// New name (ignored after `/rename`).
    pub name: Option<String>,
    /// New description; `None` when the reply had none.
    pub description: Option<String>,
    /// Tokens the call used.
    pub tokens: u64,
    /// Known cost of the call in USD.
    pub cost_usd: Option<f64>,
    /// True when the call was answered but has no price.
    pub cost_unknown: bool,
}

/// Parses a summary reply (`name: ...` and `description: ...` lines, in
/// any order and case). Returns `(name, description)`; `None` when the
/// reply has no description.
pub fn parse_summary(reply: &str) -> Option<(Option<String>, String)> {
    let mut name = None;
    let mut description = None;
    for line in reply.lines() {
        let line = line.trim().trim_start_matches(['-', '*', ' ']);
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim().trim_matches(['"', '*', '`', ' ']);
        if value.is_empty() {
            continue;
        }
        match key.trim().trim_matches('*').to_lowercase().as_str() {
            "name" => {
                name = names::title_from_text(value).map(|n| names::shorten(&n, NAME_MAX_CHARS))
            }
            "description" => {
                description =
                    names::title_from_text(value).map(|d| names::shorten(&d, DESCRIPTION_MAX_CHARS))
            }
            _ => {}
        }
    }
    description.map(|d| (name, d))
}

/// A title from the first prompt: first line, whitespace collapsed,
/// at most [`TITLE_MAX_CHARS`] characters.
pub fn title_from_prompt(prompt: &str) -> String {
    let line = prompt.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
    let words: Vec<&str> = line.split_whitespace().collect();
    let title = words.join(" ");
    if title.is_empty() {
        UNTITLED.into()
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

    /// Loads session `id`, changes it with `f` and saves it.
    pub fn update(
        &self,
        id: &str,
        f: impl FnOnce(&mut SessionMeta),
    ) -> Result<SessionMeta, SessionError> {
        let mut meta = self.load(id)?;
        f(&mut meta);
        meta.updated_at = OffsetDateTime::now_utc();
        self.save(&meta)?;
        Ok(meta)
    }

    /// Every readable session, most recently active first
    /// ([`SessionMeta::last_activity`]). Unreadable entries are skipped
    /// with a warning.
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
        out.sort_by(|a, b| {
            b.last_activity()
                .cmp(&a.last_activity())
                .then(b.id.cmp(&a.id))
        });
        Ok(out)
    }

    /// Finds a session. `query` is, in this order of precedence:
    ///
    /// 1. `last`: the most recently active session that has a prompt,
    /// 2. a full id,
    /// 3. a name, ignoring case (`/resume header parser`),
    /// 4. a unique id prefix,
    /// 5. a short tag (`#1a2b3c` or `1a2b3c`),
    /// 6. a unique name prefix, ignoring case.
    ///
    /// Several sessions with the same name or prefix are
    /// [`SessionError::Ambiguous`]; use the tag then.
    pub fn find(&self, query: &str) -> Result<SessionMeta, SessionError> {
        let all = self.list()?;
        let q = query.trim();
        if q == "last" {
            let pos = all.iter().position(|m| !m.is_empty()).unwrap_or(0);
            return all
                .into_iter()
                .nth(pos)
                .ok_or_else(|| SessionError::NotFound("last".into()));
        }
        if q.is_empty() {
            return Err(SessionError::NotFound(String::new()));
        }
        if let Some(m) = all.iter().find(|m| m.id == q) {
            return Ok(m.clone());
        }
        let lower = q.to_lowercase();
        let named = |m: &SessionMeta| !m.is_empty();
        let stages: [&dyn Fn(&SessionMeta) -> bool; 4] = [
            &|m| named(m) && m.display_name().to_lowercase() == lower,
            &|m| m.id.starts_with(q),
            &|m| names::matches_tag(&m.id, q),
            &|m| named(m) && m.display_name().to_lowercase().starts_with(&lower),
        ];
        for stage in stages {
            let matches: Vec<&SessionMeta> = all.iter().filter(|m| stage(m)).collect();
            match matches.len() {
                0 => continue,
                1 => return Ok(matches[0].clone()),
                count => {
                    return Err(SessionError::Ambiguous {
                        prefix: q.into(),
                        count,
                    });
                }
            }
        }
        Err(SessionError::NotFound(q.into()))
    }

    /// Renames the session `query` finds ([`SessionStore::find`]) and
    /// saves it. The name is kept from then on.
    pub fn rename(&self, query: &str, name: &str) -> Result<SessionMeta, SessionError> {
        let mut meta = self.find(query)?;
        meta.name = clean_name(name)?;
        meta.renamed = true;
        self.save(&meta)?;
        Ok(meta)
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

    /// Sets the name (`/rename`) and saves. The name is never re-derived
    /// afterwards.
    pub fn rename(&mut self, name: &str) -> Result<(), SessionError> {
        self.meta.name = clean_name(name)?;
        self.meta.renamed = true;
        self.touch_and_save()
    }

    /// Sets the session's reasoning effort override (`/effort`; `None`
    /// restores the agent's or model's default) and saves.
    pub fn set_effort(&mut self, effort: Option<Effort>) -> Result<(), SessionError> {
        self.meta.effort = effort;
        self.touch_and_save()
    }

    /// Applies a summary from the optional summary model
    /// ([`SessionMeta::apply_summary`]) and saves.
    pub fn apply_summary(&mut self, update: &SummaryUpdate) -> Result<(), SessionError> {
        self.meta.apply_summary(update);
        self.touch_and_save()
    }

    /// The store the session is saved in.
    pub fn store(&self) -> &SessionStore {
        &self.store
    }

    /// Records a finished loop run: totals, stop code, file hashes, and
    /// the name, description, message count and last-message time.
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
        self.meta
            .refresh_from(conversation, OffsetDateTime::now_utc());
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
