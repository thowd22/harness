//! Hygiene report generation.
//!
//! At the end of a node, [`HygieneReport::build`] looks at the node's net
//! changes from the [ledger](crate::ledger) and puts every changed file in
//! one or more [`Category`]s:
//!
//! - **in scope / out of scope** against the node's [`Scope`] (with no
//!   scope everything is in scope);
//! - **new untracked**: created files the user's git does not track;
//! - **temp or debug-like**: created files that look like scratch work
//!   (`debug_*.py`, `*.tmp`, `*.orig`, `out.txt`, `tmp/…`);
//! - **whitespace-only**: modifications that change nothing but whitespace.
//!
//! The [`HygieneScore`] (0–100, the share of changed files with no
//! problem) is recorded as a metric with [`record_metric`]. The user can
//! revert selected files ([`revert_files`], through a checkpoint) and
//! remove the temp files ([`remove_temp_files`]). [`HygieneReport::lines`]
//! renders the report for node detail and the summary shown before a task
//! is reported done.

use std::collections::BTreeSet;
use std::io::Write as _;
use std::path::Path;
use std::process::Command;

use harness_events::schema::{FileChangeKind, NodeId};
use serde::Serialize;

use crate::checkpoints::{CheckpointError, CheckpointStore};
use crate::ledger::{ChangeSource, NetChange};
use crate::scope::Scope;

/// File in a run directory that holds recorded metrics (JSON lines).
pub const METRICS_FILE: &str = "metrics.jsonl";
/// Metric name of the hygiene score.
pub const SCORE_METRIC: &str = "hygiene.score";

/// A hygiene category.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Category {
    /// Inside the task scope.
    InScope,
    /// Outside the task scope.
    OutOfScope,
    /// Created and not tracked by the user's git.
    NewUntracked,
    /// Looks like a temporary or debug file.
    TempOrDebug,
    /// Only whitespace changed.
    WhitespaceOnly,
}

impl Category {
    /// Label for the report.
    pub fn label(self) -> &'static str {
        match self {
            Self::InScope => "in scope",
            Self::OutOfScope => "out of scope",
            Self::NewUntracked => "new untracked",
            Self::TempOrDebug => "temp/debug",
            Self::WhitespaceOnly => "whitespace-only",
        }
    }

    /// True for categories that lower the score.
    pub fn is_problem(self) -> bool {
        matches!(
            self,
            Self::OutOfScope | Self::TempOrDebug | Self::WhitespaceOnly
        )
    }
}

/// One changed file in the report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FileReport {
    /// Workspace-relative path.
    pub path: String,
    /// Net change.
    pub kind: FileChangeKind,
    /// Categories, sorted.
    pub categories: BTreeSet<Category>,
    /// What made the change.
    pub sources: Vec<ChangeSource>,
    /// Why it was classed as temp/debug, if it was.
    pub temp_reason: Option<String>,
}

impl FileReport {
    /// True if any category lowers the score.
    pub fn has_problem(&self) -> bool {
        self.categories.iter().any(|c| c.is_problem())
    }
}

/// The hygiene score and counts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
pub struct HygieneScore {
    /// 0–100: the share of changed files without a problem (100 when
    /// nothing changed).
    pub score: u8,
    /// Changed files.
    pub total: usize,
    /// In scope.
    pub in_scope: usize,
    /// Out of scope.
    pub out_of_scope: usize,
    /// New untracked.
    pub new_untracked: usize,
    /// Temp or debug-like.
    pub temp_or_debug: usize,
    /// Whitespace-only.
    pub whitespace_only: usize,
}

/// Content before the node's changes, for the whitespace check.
pub trait BeforeContent {
    /// The text of `path` before the node started, if known.
    fn before_text(&self, path: &str) -> Option<String>;
}

/// Before-content from a checkpoint (normally the node's first turn).
#[derive(Debug, Clone, Copy)]
pub struct CheckpointBefore<'a> {
    /// The store.
    pub store: &'a CheckpointStore,
    /// The checkpoint id.
    pub id: u64,
}

impl BeforeContent for CheckpointBefore<'_> {
    fn before_text(&self, path: &str) -> Option<String> {
        self.store.file_text(self.id, path).ok().flatten()
    }
}

/// Inputs to [`HygieneReport::build`].
#[derive(Clone, Copy)]
pub struct ReportInput<'a> {
    /// Workspace root.
    pub root: &'a Path,
    /// The node.
    pub node_id: Option<&'a NodeId>,
    /// Net changes (`Ledger::net_changes(Some(node))`).
    pub changes: &'a [NetChange],
    /// The node's scope.
    pub scope: Option<&'a Scope>,
    /// Paths tracked by the user's git ([`tracked_files`]); `None` outside
    /// a git repository, where no file counts as untracked.
    pub tracked: Option<&'a BTreeSet<String>>,
    /// Content before the changes, for the whitespace check. Without it,
    /// the ledger's diffs are used.
    pub before: Option<&'a dyn BeforeContent>,
}

/// The hygiene report for one node.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HygieneReport {
    /// The node.
    pub node_id: Option<NodeId>,
    /// The scope globs used.
    pub scope: Option<Vec<String>>,
    /// One entry per changed file, sorted by path.
    pub files: Vec<FileReport>,
    /// Score and counts.
    pub score: HygieneScore,
}

impl HygieneReport {
    /// Classify the changes.
    pub fn build(input: ReportInput<'_>) -> Self {
        let mut files = Vec::new();
        for c in input.changes {
            let mut categories = BTreeSet::new();
            let in_scope = input.scope.is_none_or(|s| s.contains(&c.path));
            categories.insert(if in_scope {
                Category::InScope
            } else {
                Category::OutOfScope
            });
            let mut temp_reason = None;
            if c.kind == FileChangeKind::Created {
                if input.tracked.is_some_and(|t| !t.contains(&c.path)) {
                    categories.insert(Category::NewUntracked);
                }
                if let Some(r) = temp_or_debug(&c.path) {
                    categories.insert(Category::TempOrDebug);
                    temp_reason = Some(r);
                }
            }
            if c.kind == FileChangeKind::Modified && whitespace_only(input.root, c, input.before) {
                categories.insert(Category::WhitespaceOnly);
            }
            files.push(FileReport {
                path: c.path.clone(),
                kind: c.kind,
                categories,
                sources: c.sources.clone(),
                temp_reason,
            });
        }
        files.sort_by(|a, b| a.path.cmp(&b.path));
        let score = score(&files);
        Self {
            node_id: input.node_id.cloned(),
            scope: input.scope.map(|s| s.globs().to_vec()),
            files,
            score,
        }
    }

    /// Files in `category`.
    pub fn in_category(&self, category: Category) -> Vec<&FileReport> {
        self.files
            .iter()
            .filter(|f| f.categories.contains(&category))
            .collect()
    }

    /// The created temp/debug files (what [`remove_temp_files`] removes).
    pub fn temp_files(&self) -> Vec<&str> {
        self.files
            .iter()
            .filter(|f| {
                f.kind == FileChangeKind::Created && f.categories.contains(&Category::TempOrDebug)
            })
            .map(|f| f.path.as_str())
            .collect()
    }

    /// Text lines for node detail and the before-done summary.
    pub fn lines(&self) -> Vec<String> {
        let s = &self.score;
        let mut out = vec![format!(
            "Hygiene {}/100: {} changed, {} out of scope, {} temp/debug, {} whitespace-only, {} new untracked",
            s.score, s.total, s.out_of_scope, s.temp_or_debug, s.whitespace_only, s.new_untracked
        )];
        if let Some(scope) = &self.scope {
            out.push(format!("Scope: {}", scope.join(", ")));
        }
        for f in &self.files {
            let sign = match f.kind {
                FileChangeKind::Created => '+',
                FileChangeKind::Modified => '~',
                FileChangeKind::Deleted => '-',
            };
            let cats: Vec<&str> = f.categories.iter().map(|c| c.label()).collect();
            let mark = if f.has_problem() { "!" } else { " " };
            let mut line = format!("{mark} {sign} {}  [{}]", f.path, cats.join(", "));
            if let Some(r) = &f.temp_reason {
                line.push_str(&format!(" ({r})"));
            }
            out.push(line);
        }
        out
    }
}

fn score(files: &[FileReport]) -> HygieneScore {
    let count = |c: Category| files.iter().filter(|f| f.categories.contains(&c)).count();
    let total = files.len();
    let problems = files.iter().filter(|f| f.has_problem()).count();
    let score = if total == 0 {
        100
    } else {
        ((total - problems) * 100 / total) as u8
    };
    HygieneScore {
        score,
        total,
        in_scope: count(Category::InScope),
        out_of_scope: count(Category::OutOfScope),
        new_untracked: count(Category::NewUntracked),
        temp_or_debug: count(Category::TempOrDebug),
        whitespace_only: count(Category::WhitespaceOnly),
    }
}

/// Why `path` looks like a temporary or debug file, if it does.
pub fn temp_or_debug(path: &str) -> Option<String> {
    let name = path.rsplit('/').next().unwrap_or(path);
    let lower = name.to_ascii_lowercase();
    let dirs: Vec<&str> = path.split('/').collect();
    let dirs = &dirs[..dirs.len().saturating_sub(1)];
    if let Some(d) = dirs
        .iter()
        .find(|d| matches!(d.to_ascii_lowercase().as_str(), "tmp" | "temp" | "scratch"))
    {
        return Some(format!("inside `{d}/`"));
    }
    const SUFFIXES: &[&str] = &[
        ".tmp", ".temp", ".bak", ".orig", ".rej", ".swp", ".swo", ".log", ".pyc", "~",
    ];
    if let Some(s) = SUFFIXES.iter().find(|s| lower.ends_with(*s)) {
        return Some(format!("`{s}` file"));
    }
    const NAMES: &[&str] = &[
        "out.txt",
        "output.txt",
        "a.out",
        "nohup.out",
        "core",
        ".ds_store",
        "untitled",
    ];
    if NAMES.contains(&lower.as_str()) {
        return Some(format!("`{name}` is a typical scratch output"));
    }
    // `debug.py`, `debug_parse.py`, `tmp-1.sh`, but not `template.rs`.
    const PREFIXES: &[&str] = &["tmp", "temp", "scratch", "debug", "foo"];
    let stem = lower.split('.').next().unwrap_or(&lower);
    if let Some(p) = PREFIXES.iter().find(|p| {
        stem.strip_prefix(*p).is_some_and(|rest| {
            rest.is_empty()
                || rest.starts_with(|c: char| c == '_' || c == '-' || c.is_ascii_digit())
        })
    }) {
        return Some(format!("name starts with `{p}`"));
    }
    None
}

fn squash(s: &str) -> String {
    s.chars().filter(|c| !c.is_whitespace()).collect()
}

fn whitespace_only(root: &Path, c: &NetChange, before: Option<&dyn BeforeContent>) -> bool {
    if let Some(b) = before.and_then(|b| b.before_text(&c.path))
        && let Ok(after) = std::fs::read_to_string(root.join(&c.path))
    {
        return b != after && squash(&b) == squash(&after);
    }
    if c.diffs.is_empty() {
        return false;
    }
    let (mut removed, mut added) = (String::new(), String::new());
    for d in &c.diffs {
        for l in d.lines() {
            if l.starts_with("---") || l.starts_with("+++") {
                continue;
            }
            if let Some(r) = l.strip_prefix('-') {
                removed.push_str(r);
            } else if let Some(a) = l.strip_prefix('+') {
                added.push_str(a);
            }
        }
    }
    (!removed.is_empty() || !added.is_empty()) && squash(&removed) == squash(&added)
}

/// Paths tracked by the git repository at `root` (from its index, which is
/// only read), or `None` when `root` is not in a git work tree.
pub fn tracked_files(root: &Path) -> Option<BTreeSet<String>> {
    // Repository-location variables (a hook's GIT_DIR or GIT_INDEX_FILE)
    // would make git list another repository than the one at `root`.
    let out = Command::new("git")
        .args(["ls-files", "-z", "--full-name"])
        .current_dir(root)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .env_remove("GIT_OBJECT_DIRECTORY")
        .env_remove("GIT_COMMON_DIR")
        .stdin(std::process::Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(
        out.stdout
            .split(|b| *b == 0)
            .filter(|s| !s.is_empty())
            .map(|s| String::from_utf8_lossy(s).into_owned())
            .collect(),
    )
}

/// Revert `paths` to checkpoint `checkpoint_id` (normally the node's first
/// turn checkpoint); files that did not exist then are deleted.
pub fn revert_files(
    store: &CheckpointStore,
    checkpoint_id: u64,
    paths: &[&str],
) -> Result<(), CheckpointError> {
    store.restore_paths(checkpoint_id, paths)
}

/// Delete the created temp/debug files in `report`. Returns the paths
/// removed.
pub fn remove_temp_files(root: &Path, report: &HygieneReport) -> std::io::Result<Vec<String>> {
    let mut removed = Vec::new();
    for p in report.temp_files() {
        match std::fs::remove_file(root.join(p)) {
            Ok(()) => removed.push(p.to_string()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
    }
    Ok(removed)
}

/// One recorded metric line.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MetricRecord<'a> {
    /// Metric name ([`SCORE_METRIC`]).
    pub name: &'static str,
    /// The node.
    pub node_id: Option<&'a NodeId>,
    /// The value (the score).
    pub value: f64,
    /// Counts behind the score.
    pub counts: &'a HygieneScore,
    /// When it was recorded (RFC 3339).
    pub ts: String,
}

/// Append the report's score to `<run_dir>/metrics.jsonl`.
pub fn record_metric(run_dir: &Path, report: &HygieneReport) -> std::io::Result<()> {
    let rec = MetricRecord {
        name: SCORE_METRIC,
        node_id: report.node_id.as_ref(),
        value: f64::from(report.score.score),
        counts: &report.score,
        ts: time::OffsetDateTime::now_utc()
            .format(&time::format_description::well_known::Rfc3339)
            .unwrap_or_default(),
    };
    let mut line = serde_json::to_string(&rec).map_err(std::io::Error::other)?;
    line.push('\n');
    std::fs::create_dir_all(run_dir)?;
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(run_dir.join(METRICS_FILE))?
        .write_all(line.as_bytes())
}

#[cfg(test)]
mod tests;
