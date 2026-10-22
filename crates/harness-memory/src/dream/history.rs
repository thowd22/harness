//! Memory history: recent writes and dream passes from the drive's git log,
//! their diffs, and undo (revert).
//!
//! Every change to the drive is a commit on `main`, so the history is the
//! log. Commits are classified by their subject (see
//! [`crate::tool::commit_message`] and [`super::DREAM_SUBJECT`]). Undo is a
//! `git revert` published like any other write, so it is itself one commit
//! that can be reverted again.

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::drive::{Drive, DriveError, MAIN_BRANCH};
use crate::sync::{Published, SessionCheckout};

/// Subject prefix of undo commits.
pub const UNDO_SUBJECT: &str = "memory: undo";

/// What a commit did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommitKind {
    /// A memory tool `write`.
    Write,
    /// A memory tool `forget`.
    Forget,
    /// A dreaming pass.
    Dream,
    /// An undo of another commit.
    Undo,
    /// Anything else (drive creation, manual edits).
    Other,
}

/// One commit of the drive history.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryEntry {
    /// Full commit id.
    pub commit: String,
    /// Commit time.
    #[serde(with = "time::serde::rfc3339")]
    pub at: OffsetDateTime,
    /// Classification.
    pub kind: CommitKind,
    /// Subject line.
    pub subject: String,
    /// Body without trailers.
    pub body: String,
    /// The run (`Run: harness://run/<id>` trailer), if any.
    pub run: Option<String>,
}

impl HistoryEntry {
    /// Abbreviated commit id.
    pub fn short(&self) -> &str {
        self.commit.get(..7).unwrap_or(&self.commit)
    }
}

fn classify(subject: &str) -> CommitKind {
    if subject.starts_with(UNDO_SUBJECT) {
        CommitKind::Undo
    } else if subject.starts_with(super::DREAM_SUBJECT) {
        CommitKind::Dream
    } else if subject.starts_with("memory: write") {
        CommitKind::Write
    } else if subject.starts_with("memory: forget") {
        CommitKind::Forget
    } else {
        CommitKind::Other
    }
}

/// The newest `limit` commits on `main`, newest first.
pub fn history(drive: &Drive, limit: usize) -> Result<Vec<HistoryEntry>, DriveError> {
    let n = format!("-n{}", limit.max(1));
    let out = drive
        .git()
        .run(&["log", MAIN_BRANCH, &n, "--format=%H%x1f%cI%x1f%s%x1f%b%x1e"])?;
    let mut entries = Vec::new();
    for rec in out.split('\x1e') {
        let rec = rec.trim_start_matches('\n');
        if rec.trim().is_empty() {
            continue;
        }
        let mut f = rec.splitn(4, '\x1f');
        let (Some(commit), Some(at), Some(subject)) = (f.next(), f.next(), f.next()) else {
            continue;
        };
        let body_raw = f.next().unwrap_or("").trim();
        let mut run = None;
        let mut body = Vec::new();
        for line in body_raw.lines() {
            if let Some(r) = line.strip_prefix("Run: ") {
                run = Some(r.trim().to_owned());
            } else {
                body.push(line);
            }
        }
        let at = OffsetDateTime::parse(at, &time::format_description::well_known::Rfc3339)
            .unwrap_or(OffsetDateTime::UNIX_EPOCH);
        entries.push(HistoryEntry {
            commit: commit.to_owned(),
            at,
            kind: classify(subject),
            subject: subject.to_owned(),
            body: body.join("\n").trim().to_owned(),
            run,
        });
    }
    Ok(entries)
}

/// Recent memory writes and forgets (not dreams), newest first.
pub fn recent_writes(drive: &Drive, limit: usize) -> Result<Vec<HistoryEntry>, DriveError> {
    Ok(history(drive, limit.saturating_mul(4).max(limit))?
        .into_iter()
        .filter(|e| {
            matches!(
                e.kind,
                CommitKind::Write | CommitKind::Forget | CommitKind::Undo
            )
        })
        .take(limit)
        .collect())
}

/// Dream passes, newest first.
pub fn dreams(drive: &Drive, limit: usize) -> Result<Vec<HistoryEntry>, DriveError> {
    Ok(history(drive, limit.saturating_mul(8).max(limit))?
        .into_iter()
        .filter(|e| e.kind == CommitKind::Dream)
        .take(limit)
        .collect())
}

/// The unified diff of `commit` (what it changed).
pub fn diff(drive: &Drive, commit: &str) -> Result<String, DriveError> {
    check_commit(commit)?;
    drive
        .git()
        .run(&["show", "--format=", "--patch", "--no-color", commit])
}

fn check_commit(commit: &str) -> Result<(), DriveError> {
    if commit.is_empty() || !commit.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(DriveError::InvalidPath {
            path: commit.to_owned(),
            reason: "not a commit id".into(),
        });
    }
    Ok(())
}

/// Undoes `commit` by reverting it in `checkout` and publishing the revert
/// as one new commit. Returns `None` when the revert changes nothing (it was
/// already undone).
pub fn undo(checkout: &SessionCheckout, commit: &str) -> Result<Option<Published>, DriveError> {
    check_commit(commit)?;
    let short = commit.get(..7).unwrap_or(commit).to_owned();
    let subject = checkout
        .drive()
        .git()
        .run(&["log", "-1", "--format=%s", commit])
        .unwrap_or_default();
    let message = format!("{UNDO_SUBJECT} {short}\n\nReverts {commit} ({subject}).");
    let (_, published) = checkout.write(&message, |dir| {
        let git = crate::drive::Git::new(dir);
        let out = git.output(&["revert", "--no-commit", "--no-edit", commit])?;
        if !out.status.success() {
            let _ = git.output(&["revert", "--abort"]);
            let _ = git.output(&["reset", "--hard", "--quiet", "HEAD"]);
            return Err(DriveError::Git {
                args: format!("revert {commit}"),
                stderr: String::from_utf8_lossy(&out.stderr).trim().to_owned(),
            });
        }
        Ok(())
    })?;
    Ok(published)
}
