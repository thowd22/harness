//! Usage stats: every memory read, per note and run, for Dreaming.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::drive::{DriveError, io_err};

/// File under `.harness/` holding usage records (JSON lines).
pub const USAGE_FILE: &str = "usage.jsonl";

/// How a note was used.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UsageKind {
    /// Read in full with the memory tool.
    Read,
    /// Returned as a search hit.
    SearchHit,
    /// Loaded as part of the session index.
    Index,
}

/// One usage record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsageRecord {
    /// Drive-relative note path.
    pub note: String,
    /// The run that used it.
    pub run: String,
    /// When.
    #[serde(with = "time::serde::rfc3339")]
    pub ts: OffsetDateTime,
    /// How.
    pub kind: UsageKind,
}

/// Aggregated usage of one note.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UsageSummary {
    /// Full reads.
    pub reads: u64,
    /// Search hits.
    pub search_hits: u64,
    /// Most recent use of any kind.
    pub last_used: Option<OffsetDateTime>,
    /// Runs that used it.
    pub runs: BTreeSet<String>,
}

/// Append-only usage log at `<drive>/.harness/usage.jsonl`.
///
/// Each record is written with one `write` call on an `O_APPEND` file, so
/// concurrent sessions never interleave partial lines.
#[derive(Debug, Clone)]
pub struct UsageLog {
    path: PathBuf,
}

impl UsageLog {
    /// The log in `internal_dir` (a drive's `.harness/`).
    pub fn new(internal_dir: &Path) -> Self {
        Self {
            path: internal_dir.join(USAGE_FILE),
        }
    }

    /// The log file.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Appends one record.
    pub fn record(&self, rec: &UsageRecord) -> Result<(), DriveError> {
        if let Some(dir) = self.path.parent() {
            fs::create_dir_all(dir).map_err(io_err("creating the internal directory"))?;
        }
        let mut line = serde_json::to_string(rec).map_err(|e| DriveError::Io {
            context: "encoding a usage record".into(),
            source: io::Error::other(e),
        })?;
        line.push('\n');
        let mut f = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .map_err(io_err("opening the usage log"))?;
        f.write_all(line.as_bytes())
            .map_err(io_err("writing the usage log"))
    }

    /// Records `kind` use of `note` by `run`, now.
    pub fn note_used(&self, note: &str, run: &str, kind: UsageKind) -> Result<(), DriveError> {
        self.record(&UsageRecord {
            note: note.to_owned(),
            run: run.to_owned(),
            ts: OffsetDateTime::now_utc(),
            kind,
        })
    }

    /// All records in order. Unparseable lines (e.g. a torn last line) are
    /// skipped.
    pub fn records(&self) -> Result<Vec<UsageRecord>, DriveError> {
        let text = match fs::read_to_string(&self.path) {
            Ok(t) => t,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(io_err("reading the usage log")(e)),
        };
        Ok(text
            .lines()
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect())
    }

    /// Usage aggregated per note.
    pub fn summary(&self) -> Result<BTreeMap<String, UsageSummary>, DriveError> {
        let mut out: BTreeMap<String, UsageSummary> = BTreeMap::new();
        for r in self.records()? {
            let s = out.entry(r.note).or_default();
            match r.kind {
                UsageKind::Read => s.reads += 1,
                UsageKind::SearchHit => s.search_hits += 1,
                UsageKind::Index => {}
            }
            s.last_used = s.last_used.max(Some(r.ts));
            s.runs.insert(r.run);
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_and_summarises_per_note_and_run() {
        let tmp = tempfile::tempdir().unwrap();
        let log = UsageLog::new(&tmp.path().join(".harness"));
        assert!(log.records().unwrap().is_empty());
        log.note_used("tools/cargo.md", "run-a", UsageKind::Read)
            .unwrap();
        log.note_used("tools/cargo.md", "run-b", UsageKind::Read)
            .unwrap();
        log.note_used("tools/cargo.md", "run-b", UsageKind::SearchHit)
            .unwrap();
        log.note_used("MEMORY.md", "run-a", UsageKind::Read)
            .unwrap();
        // A torn line is skipped.
        OpenOptions::new()
            .append(true)
            .open(log.path())
            .unwrap()
            .write_all(b"{\"note\":")
            .unwrap();
        let recs = log.records().unwrap();
        assert_eq!(recs.len(), 4);
        assert_eq!(recs[0].note, "tools/cargo.md");
        assert_eq!(recs[0].run, "run-a");
        let s = log.summary().unwrap();
        let c = &s["tools/cargo.md"];
        assert_eq!(c.reads, 2);
        assert_eq!(c.search_hits, 1);
        assert_eq!(
            c.runs.iter().cloned().collect::<Vec<_>>(),
            vec!["run-a", "run-b"]
        );
        assert!(c.last_used.is_some());
        assert_eq!(s["MEMORY.md"].reads, 1);
    }
}
