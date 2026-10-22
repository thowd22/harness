//! Dreaming: offline memory consolidation.
//!
//! A pass ([`Dreamer::dream`]) runs in its own drive checkout and
//!
//! 1. merges duplicate bullets (exact duplicates anywhere, near duplicates
//!    within a note) and deletes notes left empty, re-pointing `[[links]]`;
//! 2. removes transient details (task state, session summaries);
//! 3. prunes stale notes no session used within the grace period
//!    (usage stats from [`crate::index::UsageLog`]);
//! 4. adds lessons from recent runs the agents missed, proposed by the
//!    `[memory] dream_model` through [`crate::extract::Extractor`] and
//!    screened by the memory write guard;
//! 5. regenerates the index block of `MEMORY.md`.
//!
//! Source links and explicit preferences are always preserved
//! ([`consolidate::check_invariants`] runs before anything is written), and
//! the result is **one commit** (subject [`DREAM_SUBJECT`]) whose diff is
//! shown by [`history::diff`] and undone by [`history::undo`]. Each pass is
//! recorded in the dream log ([`schedule::DreamLog`]); [`schedule::due`]
//! decides when the next one runs. The first pass seeds memory from recent
//! runs.
//!
//! Settings come from the built-in, user-overridable `dream` workflow
//! (`workflows/dream.yaml`, read with [`DreamSettings::from_workflow`]).

pub mod consolidate;
pub mod history;
pub mod runs;
pub mod schedule;

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::fs;
use std::sync::Arc;

use harness_events::redact::Redactor;
use serde::{Deserialize, Serialize};
use time::{Date, Duration, OffsetDateTime};
use tokio_util::sync::CancellationToken;

use crate::drive::note::{Bullet, Note, run_link};
use crate::drive::{DriveError, MEMORY_FILE, NotePath, io_err, list_notes, read_note_at};
use crate::extract::Extractor;
use crate::index::UsageLog;
use crate::sync::{Published, SessionCheckout};
use consolidate::{Notes, PassOptions, PassReport, RemovalReason};
use runs::RunTranscript;
use schedule::{DreamEntry, DreamLog};

/// Subject of dream commits.
pub const DREAM_SUBJECT: &str = "memory: dream";
/// Step `type` of the dream step in a workflow file.
pub const STEP_TYPE: &str = "dream";

/// Settings of a pass (the `with:` map of the `type: dream` step).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DreamSettings {
    /// Unused notes older than this many days are pruned.
    pub grace_days: u32,
    /// Most lessons added from run logs per pass.
    pub max_new_lessons: usize,
    /// Most recent runs reviewed per pass.
    pub recent_runs: usize,
    /// Minimum hours between scheduled passes.
    pub interval_hours: u32,
}

impl Default for DreamSettings {
    fn default() -> Self {
        Self {
            grace_days: 30,
            max_new_lessons: 5,
            recent_runs: 20,
            interval_hours: 20,
        }
    }
}

impl DreamSettings {
    /// Reads the settings from a workflow document (the `with:` of its
    /// first `type: dream` step). Missing keys keep their defaults.
    pub fn from_workflow(document: &serde_json::Value) -> Result<Self, String> {
        let step = document
            .get("steps")
            .and_then(|s| s.as_array())
            .and_then(|steps| {
                steps
                    .iter()
                    .find(|s| s.get("type").and_then(|t| t.as_str()) == Some(STEP_TYPE))
            })
            .ok_or_else(|| format!("the dream workflow has no `type: {STEP_TYPE}` step"))?;
        match step.get("with") {
            None => Ok(Self::default()),
            Some(w) => {
                serde_json::from_value(w.clone()).map_err(|e| format!("dream step `with`: {e}"))
            }
        }
    }

    /// Minimum time between scheduled passes.
    pub fn interval(&self) -> Duration {
        Duration::hours(i64::from(self.interval_hours))
    }
}

/// An error during a pass.
#[derive(Debug, thiserror::Error)]
pub enum DreamError {
    /// The drive failed.
    #[error(transparent)]
    Drive(#[from] DriveError),
    /// The pass would break an invariant; nothing was written.
    #[error("dreaming aborted: {0}")]
    Invariant(String),
}

/// A lesson added from a run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Lesson {
    /// The note it went to.
    pub note: String,
    /// The bullet text.
    pub text: String,
    /// The run it came from.
    pub run: String,
}

/// What a pass did.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DreamReport {
    /// Deterministic changes.
    pub pass: PassReport,
    /// Lessons added from runs.
    pub added: Vec<Lesson>,
    /// Lesson proposals that were dropped (and why).
    pub skipped: Vec<String>,
    /// Runs reviewed.
    pub runs_reviewed: usize,
    /// Whether this was the first pass (seeding).
    pub first: bool,
    /// The published commit (`None` when nothing changed).
    #[serde(skip)]
    pub commit: Option<Published>,
}

impl DreamReport {
    /// One-line summary.
    pub fn summary(&self) -> String {
        let p = &self.pass;
        let mut parts = Vec::new();
        let mut push = |n: usize, what: &str| {
            if n > 0 {
                parts.push(format!("{n} {what}"));
            }
        };
        push(p.count(RemovalReason::Merged), "merged");
        push(p.count(RemovalReason::Transient), "transient removed");
        push(p.count(RemovalReason::Stale), "stale pruned");
        push(p.notes_deleted.len(), "notes deleted");
        push(self.added.len(), "lessons added");
        if p.index_changed {
            parts.push("index rebuilt".into());
        }
        if parts.is_empty() {
            "no changes".into()
        } else {
            parts.join(", ")
        }
    }

    /// The commit message: subject, summary and every change.
    pub fn commit_message(&self) -> String {
        let mut m = format!("{DREAM_SUBJECT}\n\n{}\n", self.summary());
        for r in &self.pass.removed {
            let what = match r.reason {
                RemovalReason::Merged => "merged",
                RemovalReason::Transient => "transient",
                RemovalReason::Stale => "stale",
            };
            let _ = writeln!(m, "- {what}: {} ({})", r.text, r.note);
        }
        for l in &self.added {
            let _ = writeln!(
                m,
                "- added: {} ({}, from {})",
                l.text,
                l.note,
                run_link(&l.run)
            );
        }
        m
    }
}

/// Runs consolidation passes.
pub struct Dreamer {
    settings: DreamSettings,
    redactor: Arc<Redactor>,
    lessons: Option<Extractor>,
    project_key: Option<String>,
    now: Option<OffsetDateTime>,
}

impl std::fmt::Debug for Dreamer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Dreamer")
            .field("settings", &self.settings)
            .field("lessons", &self.lessons.is_some())
            .field("project_key", &self.project_key)
            .finish_non_exhaustive()
    }
}

impl Dreamer {
    /// A dreamer without a model (deterministic steps only).
    pub fn new(settings: DreamSettings, redactor: Arc<Redactor>) -> Self {
        Self {
            settings,
            redactor,
            lessons: None,
            project_key: None,
            now: None,
        }
    }

    /// Adds missed lessons with `extractor` (built with the provider of the
    /// registry-resolved `[memory] dream_model`).
    pub fn with_lessons(mut self, extractor: Extractor) -> Self {
        self.lessons = Some(extractor);
        self
    }

    /// Project key used for `project/<topic>` lesson paths.
    pub fn with_project_key(mut self, key: impl Into<String>) -> Self {
        self.project_key = Some(key.into());
        self
    }

    /// Fixes "now" (tests).
    pub fn with_now(mut self, now: OffsetDateTime) -> Self {
        self.now = Some(now);
        self
    }

    fn now(&self) -> OffsetDateTime {
        self.now.unwrap_or_else(OffsetDateTime::now_utc)
    }

    /// The runs a pass should review: everything recent on the first pass
    /// (seeding), otherwise runs since the last pass.
    pub fn select_runs(&self, data_dir: &std::path::Path, log: &DreamLog) -> Vec<RunTranscript> {
        let since = log.last().map(|e| e.at);
        runs::recent(data_dir, since, self.settings.recent_runs)
    }

    async fn propose(
        &self,
        runs: &[RunTranscript],
        existing: &str,
        cancel: &CancellationToken,
    ) -> (Vec<Lesson>, Vec<String>) {
        let mut added = Vec::new();
        let mut skipped = Vec::new();
        let Some(ex) = &self.lessons else {
            return (added, skipped);
        };
        for run in runs {
            if added.len() >= self.settings.max_new_lessons {
                break;
            }
            match ex.extract(&run.messages, existing, cancel.clone()).await {
                Ok(e) => {
                    for (c, r) in e.rejected {
                        skipped.push(format!("{}: {r}", c.text));
                    }
                    for p in e.proposals {
                        if added.len() >= self.settings.max_new_lessons {
                            break;
                        }
                        match self.lesson_path(&p.path) {
                            Ok(note) => added.push(Lesson {
                                note: note.to_string(),
                                text: p.text,
                                run: run.run_id.clone(),
                            }),
                            Err(e) => skipped.push(format!("{}: {e}", p.text)),
                        }
                    }
                }
                Err(e) => skipped.push(format!("run {}: {e}", run.run_id)),
            }
        }
        (added, skipped)
    }

    fn lesson_path(&self, path: &str) -> Result<NotePath, String> {
        let p = path.trim();
        let expanded = match (
            p.strip_prefix(crate::tool::PROJECT_ALIAS),
            &self.project_key,
        ) {
            (Some(rest), Some(key)) => format!("projects/{key}/{rest}"),
            (Some(_), None) => return Err("no current project for a project/ path".into()),
            (None, _) => p.to_owned(),
        };
        NotePath::parse(&expanded).map_err(|e| e.to_string())
    }

    /// Computes a pass over `before` (no I/O).
    fn plan(
        &self,
        before: &Notes,
        memory_text: &str,
        lessons: &[Lesson],
        usage: &BTreeMap<String, crate::index::UsageSummary>,
        now: OffsetDateTime,
    ) -> Result<Plan, DreamError> {
        let mut after = before.clone();
        let mut pass = PassReport::default();
        let mut added = Vec::new();
        let mut skipped = Vec::new();
        // Lessons first, so they are deduplicated like everything else.
        for l in lessons {
            match crate::tool::guard::validate(&l.text, &self.redactor) {
                Ok(text) => {
                    let np = NotePath::parse(&l.note)?;
                    let note = after
                        .entry(np.clone())
                        .or_insert_with(|| Note::with_title(&np.default_title()));
                    if !note.contains_text(&text) {
                        note.push(Bullet::new(text.clone(), run_link(&l.run), now.date()));
                        added.push(Lesson { text, ..l.clone() });
                    }
                }
                Err(r) => skipped.push(format!("{}: {r}", l.text)),
            }
        }
        let with_lessons = after.clone();
        let opts = PassOptions {
            grace: Duration::days(i64::from(self.settings.grace_days)),
            now,
        };
        consolidate::merge_duplicates(&mut after, &mut pass);
        consolidate::remove_transient(&mut after, &mut pass);
        consolidate::prune_stale(&mut after, usage, &opts, &mut pass);
        consolidate::drop_empty(&mut after, &mut pass);
        consolidate::check_invariants(&with_lessons, &after, &pass)
            .map_err(DreamError::Invariant)?;
        let (memory, index_changed) = consolidate::render_memory(&after, memory_text);
        pass.index_changed = index_changed;
        Ok(Plan {
            after,
            memory,
            pass,
            added,
            skipped,
        })
    }

    /// Runs one pass in `checkout` over `runs` and publishes the result as
    /// one commit. `manual` marks `/memory dream` passes in the log.
    ///
    /// The pass is computed from the up-to-date checkout, then written; if
    /// another session published in between, it is recomputed (up to
    /// [`MAX_ATTEMPTS`] times), so the commit message always describes the
    /// commit exactly.
    pub async fn dream(
        &self,
        checkout: &SessionCheckout,
        runs: &[RunTranscript],
        manual: bool,
        cancel: &CancellationToken,
    ) -> Result<DreamReport, DreamError> {
        let drive = checkout.drive().clone();
        let log = DreamLog::new(&drive.internal_dir());
        let first = log.last().is_none();
        checkout.refresh()?;
        let existing = read_note_at(checkout.path(), &NotePath::parse(MEMORY_FILE)?)?
            .map(|t| consolidate::strip_index(&t))
            .unwrap_or_default();
        let (lessons, skipped) = self.propose(runs, &existing, cancel).await;
        let usage = UsageLog::new(&drive.internal_dir()).summary()?;
        let now = self.now();

        for _ in 0..MAX_ATTEMPTS {
            checkout.refresh()?;
            let (before, memory_text) = load(checkout.path())?;
            let Plan {
                after,
                memory,
                pass,
                added,
                skipped: skipped_now,
            } = self.plan(&before, &memory_text, &lessons, &usage, now)?;
            let mut report = DreamReport {
                pass,
                added,
                skipped: skipped.clone(),
                runs_reviewed: runs.len(),
                first,
                commit: None,
            };
            report.skipped.extend(skipped_now);
            let message = report.commit_message();
            let result = checkout.write(&message, |dir| {
                let (now_before, now_memory) = load(dir)?;
                if now_before != before || now_memory != memory_text {
                    return Err(DriveError::InvalidPath {
                        path: RETRY.into(),
                        reason: "the drive changed while dreaming".into(),
                    });
                }
                save(dir, &before, &after, &memory)
            });
            match result {
                Ok((_, published)) => {
                    report.commit = published;
                    log.append(&DreamEntry {
                        at: now,
                        commit: report.commit.as_ref().map(|c| c.commit.clone()),
                        summary: report.summary(),
                        manual,
                        runs_reviewed: report.runs_reviewed,
                    })?;
                    return Ok(report);
                }
                Err(DriveError::InvalidPath { path, .. }) if path == RETRY => {
                    continue;
                }
                Err(e) => return Err(e.into()),
            }
        }
        Err(DreamError::Drive(DriveError::StaleWrite {
            attempts: MAX_ATTEMPTS,
        }))
    }
}

/// A computed pass, before it is written.
struct Plan {
    after: Notes,
    memory: String,
    pass: PassReport,
    added: Vec<Lesson>,
    skipped: Vec<String>,
}

/// How many times a pass is recomputed when the drive changes under it.
pub const MAX_ATTEMPTS: u32 = 3;
const RETRY: &str = "<dream-retry>";

/// Loads every note (with the `MEMORY.md` index block stripped) and the
/// raw `MEMORY.md` text.
fn load(dir: &std::path::Path) -> Result<(Notes, String), DriveError> {
    let mut notes = BTreeMap::new();
    let mut memory_text = String::new();
    for p in list_notes(dir)? {
        let text = read_note_at(dir, &p)?.unwrap_or_default();
        let parsed = if p.as_str() == MEMORY_FILE {
            memory_text = text.clone();
            Note::parse(&consolidate::strip_index(&text))
        } else {
            Note::parse(&text)
        };
        notes.insert(p, parsed);
    }
    Ok((notes, memory_text))
}

/// Writes changed notes and deletes removed ones.
fn save(
    dir: &std::path::Path,
    before: &Notes,
    after: &Notes,
    memory: &str,
) -> Result<(), DriveError> {
    for p in before.keys().filter(|p| !after.contains_key(*p)) {
        fs::remove_file(dir.join(p.as_str())).map_err(io_err(format!("removing {p}")))?;
    }
    for (p, n) in after {
        let text = if p.as_str() == MEMORY_FILE {
            memory.to_owned()
        } else {
            n.render()
        };
        if before.get(p).map(Note::render) == Some(text.clone()) && p.as_str() != MEMORY_FILE {
            continue;
        }
        let file = dir.join(p.as_str());
        if let Some(parent) = file.parent() {
            fs::create_dir_all(parent).map_err(io_err(format!("creating {p}")))?;
        }
        fs::write(&file, text).map_err(io_err(format!("writing {p}")))?;
    }
    if !after.contains_key(&NotePath::parse(MEMORY_FILE)?) {
        fs::write(dir.join(MEMORY_FILE), memory).map_err(io_err("writing MEMORY.md"))?;
    }
    Ok(())
}

/// A [`Date`] helper for callers building bullets by hand.
pub fn today() -> Date {
    OffsetDateTime::now_utc().date()
}
