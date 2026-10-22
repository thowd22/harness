//! When dreaming runs, and the dream log.
//!
//! With `[memory] dream = daily` a pass is due when at least
//! `interval_hours` passed since the last pass **and** there was activity
//! (a run or a memory commit) since then. `manual` runs only on
//! `/memory dream`; `off` never runs (not even on request). The log of
//! passes is `.harness/dream-log.jsonl` in the drive.

use std::fs::OpenOptions;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use harness_config::ini::config::Dream;
use serde::{Deserialize, Serialize};
use time::{Duration, OffsetDateTime};

use crate::drive::{DriveError, io_err};

/// File under the drive's `.harness/` that records passes.
pub const DREAM_LOG: &str = "dream-log.jsonl";

/// One pass in the dream log.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DreamEntry {
    /// When it ran.
    #[serde(with = "time::serde::rfc3339")]
    pub at: OffsetDateTime,
    /// The commit it published (`None` when nothing changed).
    pub commit: Option<String>,
    /// One-line summary.
    pub summary: String,
    /// Whether it was requested (`/memory dream`) rather than scheduled.
    pub manual: bool,
    /// Runs reviewed for missed lessons.
    pub runs_reviewed: usize,
}

/// Append-only log of dream passes.
#[derive(Debug, Clone)]
pub struct DreamLog {
    path: PathBuf,
}

impl DreamLog {
    /// The log in the drive's internal directory.
    pub fn new(internal_dir: &Path) -> Self {
        Self {
            path: internal_dir.join(DREAM_LOG),
        }
    }

    /// The log file.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Appends an entry.
    pub fn append(&self, entry: &DreamEntry) -> Result<(), DriveError> {
        if let Some(p) = self.path.parent() {
            std::fs::create_dir_all(p).map_err(io_err("creating the dream log directory"))?;
        }
        let mut f = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .map_err(io_err("opening the dream log"))?;
        let line = serde_json::to_string(entry).unwrap_or_default();
        writeln!(f, "{line}").map_err(io_err("writing the dream log"))
    }

    /// Every entry, oldest first (unreadable lines are skipped).
    pub fn entries(&self) -> Vec<DreamEntry> {
        std::fs::read_to_string(&self.path)
            .unwrap_or_default()
            .lines()
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect()
    }

    /// The most recent entry.
    pub fn last(&self) -> Option<DreamEntry> {
        self.entries().pop()
    }
}

/// Why a pass should or should not run now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Due {
    /// Run now.
    Yes {
        /// Why.
        reason: String,
    },
    /// Do not run.
    No {
        /// Why.
        reason: String,
    },
}

impl Due {
    /// Whether a pass should run.
    pub fn is_due(&self) -> bool {
        matches!(self, Due::Yes { .. })
    }
}

/// Decides whether a pass is due.
///
/// `requested` is `/memory dream`. `last` is the previous pass and
/// `activity` the latest run or memory commit.
pub fn due(
    mode: Dream,
    requested: bool,
    last: Option<OffsetDateTime>,
    activity: Option<OffsetDateTime>,
    now: OffsetDateTime,
    interval: Duration,
) -> Due {
    let no = |r: &str| Due::No { reason: r.into() };
    match mode {
        Dream::Off => return no("dreaming is off ([memory] dream = off)"),
        _ if requested => {
            return Due::Yes {
                reason: "requested with /memory dream".into(),
            };
        }
        Dream::Manual => {
            return no("dreaming runs only on /memory dream ([memory] dream = manual)");
        }
        Dream::Daily => {}
    }
    let Some(activity) = activity else {
        return no("no activity yet");
    };
    match last {
        None => Due::Yes {
            reason: "first pass: seeding memory from recent runs".into(),
        },
        Some(l) if activity <= l => no("no activity since the last pass"),
        Some(l) if now - l < interval => no("the last pass was too recent"),
        Some(_) => Due::Yes {
            reason: "daily pass after new activity".into(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::datetime;

    const NOW: OffsetDateTime = datetime!(2026-10-22 12:00 UTC);
    const DAY: Duration = Duration::hours(20);

    #[test]
    fn daily_needs_activity_and_interval() {
        let ago = |h: i64| Some(NOW - Duration::hours(h));
        assert!(due(Dream::Daily, false, None, ago(1), NOW, DAY).is_due());
        assert!(!due(Dream::Daily, false, None, None, NOW, DAY).is_due());
        assert!(due(Dream::Daily, false, ago(25), ago(2), NOW, DAY).is_due());
        assert!(
            !due(Dream::Daily, false, ago(25), ago(26), NOW, DAY).is_due(),
            "no new activity"
        );
        assert!(
            !due(Dream::Daily, false, ago(5), ago(1), NOW, DAY).is_due(),
            "too recent"
        );
    }

    #[test]
    fn manual_and_off() {
        assert!(!due(Dream::Manual, false, None, Some(NOW), NOW, DAY).is_due());
        assert!(due(Dream::Manual, true, None, None, NOW, DAY).is_due());
        assert!(due(Dream::Daily, true, Some(NOW), None, NOW, DAY).is_due());
        assert!(!due(Dream::Off, true, None, Some(NOW), NOW, DAY).is_due());
    }

    #[test]
    fn log_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let log = DreamLog::new(dir.path());
        assert!(log.last().is_none());
        let e = DreamEntry {
            at: NOW,
            commit: Some("abc".into()),
            summary: "merged 1".into(),
            manual: false,
            runs_reviewed: 2,
        };
        log.append(&e).unwrap();
        log.append(&DreamEntry {
            manual: true,
            ..e.clone()
        })
        .unwrap();
        assert_eq!(log.entries().len(), 2);
        assert!(log.last().unwrap().manual);
    }
}
