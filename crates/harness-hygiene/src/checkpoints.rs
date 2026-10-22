//! Shadow-git checkpoints and `/undo`.
//!
//! A [`CheckpointStore`] keeps a private git repository per project, under
//! the data dir (`DataDir::checkpoints()/<project-hash>.git`), and points it
//! at the project's worktree with `--git-dir`/`--work-tree`. The user's own
//! `.git` (index, branches, stash, hooks, config) is never read or written:
//!
//! - every git call gets an explicit `--git-dir` and `--work-tree`, all
//!   inherited `GIT_*` variables are removed, and global and system git
//!   config are ignored;
//! - git never descends into the worktree's `.git` directory, so the user's
//!   repository is not part of a snapshot;
//! - snapshots are made with plumbing (`add -A` into the shadow index,
//!   `write-tree`, `commit-tree`), so no hooks run.
//!
//! Files ignored by the project's `.gitignore` are not snapshotted and are
//! never touched by a restore. Nested repositories are recorded as gitlinks
//! only (their contents are not snapshotted).
//!
//! The agent calls [`TurnCheckpointer::before_mutation`] before the first
//! mutating tool call of each turn. `/undo` is [`CheckpointStore::undo`];
//! the map's "restore checkpoint" action is [`CheckpointStore::restore`]
//! with an id from [`CheckpointStore::for_node`]. Every restore first takes
//! a [`CheckpointKind::Restore`] snapshot, so a restore can be undone too.
//!
//! All methods run git synchronously; call them from
//! `tokio::task::spawn_blocking` in async code.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;

use harness_events::schema::NodeId;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use time::OffsetDateTime;

/// File in the shadow git dir that lists checkpoints.
pub const INDEX_FILE: &str = "harness-checkpoints.json";
/// Ref that keeps the newest checkpoint reachable.
pub const HEAD_REF: &str = "refs/harness/checkpoints";
/// Author used for checkpoint commits.
pub const AUTHOR: &str = "harness checkpoints";
/// Author email used for checkpoint commits.
pub const AUTHOR_EMAIL: &str = "checkpoints@harness.invalid";

/// Why a checkpoint was taken.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckpointKind {
    /// Before a mutating turn.
    Turn,
    /// Before a restore (so it can be undone).
    Restore,
    /// Requested explicitly (bug-fix proof, user).
    Manual,
}

/// One snapshot of the worktree.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Checkpoint {
    /// Sequential id, from 1.
    pub id: u64,
    /// Shadow commit.
    pub commit: String,
    /// Tree of the commit.
    pub tree: String,
    /// Why it was taken.
    pub kind: CheckpointKind,
    /// Node whose turn this precedes.
    pub node_id: Option<NodeId>,
    /// Turn number within the node.
    pub turn: Option<u32>,
    /// Human label.
    pub label: String,
    /// When it was taken.
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    /// Set once `/undo` has restored this checkpoint.
    #[serde(default)]
    pub undone: bool,
}

/// One path that differs between a checkpoint and now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangedPath {
    /// Git status letter (`A`, `M`, `D`, `T`).
    pub status: char,
    /// Worktree-relative path.
    pub path: String,
}

/// Checkpoint failures.
#[derive(Debug, thiserror::Error)]
pub enum CheckpointError {
    /// git could not be started.
    #[error("could not run git: {0}")]
    Spawn(#[source] std::io::Error),
    /// A git command failed.
    #[error("git {args} failed: {stderr}")]
    Git {
        /// The arguments.
        args: String,
        /// git's stderr.
        stderr: String,
    },
    /// Reading or writing the checkpoint list failed.
    #[error("checkpoint store I/O: {0}")]
    Io(#[from] std::io::Error),
    /// The checkpoint list is unreadable.
    #[error("checkpoint list is corrupt: {0}")]
    Corrupt(#[from] serde_json::Error),
    /// No checkpoint with this id.
    #[error("no checkpoint #{0}")]
    NotFound(u64),
    /// `/undo` has nothing left to restore.
    #[error("nothing to undo: no earlier turn checkpoint")]
    NothingToUndo,
}

/// A shadow git repository for one worktree.
#[derive(Debug)]
pub struct CheckpointStore {
    git_dir: PathBuf,
    work_tree: PathBuf,
    lock: Mutex<()>,
}

/// A stable directory name for a project root (first 16 hex chars of the
/// sha256 of the canonical path).
pub fn project_key(work_tree: &Path) -> String {
    let canon = std::fs::canonicalize(work_tree).unwrap_or_else(|_| work_tree.to_path_buf());
    let digest = Sha256::digest(canon.to_string_lossy().as_bytes());
    hex::encode(digest)[..16].to_string()
}

impl CheckpointStore {
    /// Open (creating if needed) the shadow repository for `work_tree`
    /// under `checkpoints_dir` (normally `DataDir::checkpoints()`).
    pub fn open(checkpoints_dir: &Path, work_tree: &Path) -> Result<Self, CheckpointError> {
        let work_tree = std::fs::canonicalize(work_tree)?;
        let git_dir = checkpoints_dir.join(format!("{}.git", project_key(&work_tree)));
        let store = Self {
            git_dir,
            work_tree,
            lock: Mutex::new(()),
        };
        if !store.git_dir.join("HEAD").exists() {
            std::fs::create_dir_all(checkpoints_dir)?;
            store.git_plain(&["init", "--quiet"])?;
            for (k, v) in [
                ("core.autocrlf", "false"),
                ("core.hooksPath", "/dev/null"),
                ("gc.auto", "0"),
                ("core.quotePath", "false"),
            ] {
                store.git_plain(&["config", k, v])?;
            }
        }
        Ok(store)
    }

    /// The shadow git dir.
    pub fn git_dir(&self) -> &Path {
        &self.git_dir
    }

    /// The worktree.
    pub fn work_tree(&self) -> &Path {
        &self.work_tree
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut cmd = Command::new("git");
        for (k, _) in std::env::vars_os() {
            if k.to_string_lossy().starts_with("GIT_") {
                cmd.env_remove(&k);
            }
        }
        cmd.env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_AUTHOR_NAME", AUTHOR)
            .env("GIT_AUTHOR_EMAIL", AUTHOR_EMAIL)
            .env("GIT_COMMITTER_NAME", AUTHOR)
            .env("GIT_COMMITTER_EMAIL", AUTHOR_EMAIL)
            .env("GIT_TERMINAL_PROMPT", "0")
            .arg("--git-dir")
            .arg(&self.git_dir)
            .arg("--work-tree")
            .arg(&self.work_tree)
            .args(args)
            .current_dir(&self.work_tree)
            .stdin(Stdio::null());
        cmd
    }

    fn git_plain(&self, args: &[&str]) -> Result<String, CheckpointError> {
        let out = self
            .command(args)
            .output()
            .map_err(CheckpointError::Spawn)?;
        if !out.status.success() {
            return Err(CheckpointError::Git {
                args: args.join(" "),
                stderr: String::from_utf8_lossy(&out.stderr).trim().to_string(),
            });
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }

    fn git(&self, args: &[&str]) -> Result<String, CheckpointError> {
        Ok(self.git_plain(args)?.trim().to_string())
    }

    fn index_path(&self) -> PathBuf {
        self.git_dir.join(INDEX_FILE)
    }

    /// Every checkpoint, oldest first.
    pub fn list(&self) -> Result<Vec<Checkpoint>, CheckpointError> {
        match std::fs::read(self.index_path()) {
            Ok(bytes) => Ok(serde_json::from_slice(&bytes)?),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(e) => Err(e.into()),
        }
    }

    fn save(&self, list: &[Checkpoint]) -> Result<(), CheckpointError> {
        let tmp = self.git_dir.join(format!("{INDEX_FILE}.tmp"));
        std::fs::write(&tmp, serde_json::to_vec_pretty(list)?)?;
        std::fs::rename(tmp, self.index_path())?;
        Ok(())
    }

    /// One checkpoint by id.
    pub fn get(&self, id: u64) -> Result<Checkpoint, CheckpointError> {
        self.list()?
            .into_iter()
            .find(|c| c.id == id)
            .ok_or(CheckpointError::NotFound(id))
    }

    /// Checkpoints taken before `node`'s turns, oldest first (for the map).
    pub fn for_node(&self, node: &NodeId) -> Result<Vec<Checkpoint>, CheckpointError> {
        Ok(self
            .list()?
            .into_iter()
            .filter(|c| c.node_id.as_ref() == Some(node))
            .collect())
    }

    /// Stage the whole worktree into the shadow index and return its tree.
    fn stage_all(&self) -> Result<String, CheckpointError> {
        self.git(&["add", "-A", "--ignore-errors", "--", "."])?;
        self.git(&["write-tree"])
    }

    /// Snapshot the worktree now.
    pub fn snapshot(
        &self,
        kind: CheckpointKind,
        node: Option<&NodeId>,
        turn: Option<u32>,
        label: &str,
    ) -> Result<Checkpoint, CheckpointError> {
        let _guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        self.snapshot_locked(kind, node, turn, label)
    }

    fn snapshot_locked(
        &self,
        kind: CheckpointKind,
        node: Option<&NodeId>,
        turn: Option<u32>,
        label: &str,
    ) -> Result<Checkpoint, CheckpointError> {
        let mut list = self.list()?;
        let tree = self.stage_all()?;
        let commit = match list.last() {
            Some(prev) if prev.tree == tree => prev.commit.clone(),
            prev => {
                let msg = if label.is_empty() {
                    "checkpoint"
                } else {
                    label
                };
                let mut args = vec!["commit-tree", tree.as_str(), "-m", msg];
                if let Some(p) = prev {
                    args.extend(["-p", p.commit.as_str()]);
                }
                self.git(&args)?
            }
        };
        self.git(&["update-ref", HEAD_REF, &commit])?;
        let cp = Checkpoint {
            id: list.last().map_or(1, |c| c.id + 1),
            commit,
            tree,
            kind,
            node_id: node.cloned(),
            turn,
            label: label.to_string(),
            created_at: OffsetDateTime::now_utc(),
            undone: false,
        };
        list.push(cp.clone());
        self.save(&list)?;
        Ok(cp)
    }

    /// Restore the whole worktree to checkpoint `id`. Files the checkpoint
    /// did not have are removed; ignored files are left alone. Returns the
    /// safety checkpoint taken just before.
    pub fn restore(&self, id: u64) -> Result<Checkpoint, CheckpointError> {
        let _guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        self.restore_locked(id)
    }

    fn restore_locked(&self, id: u64) -> Result<Checkpoint, CheckpointError> {
        let target = self
            .list()?
            .into_iter()
            .find(|c| c.id == id)
            .ok_or(CheckpointError::NotFound(id))?;
        let safety = self.snapshot_locked(
            CheckpointKind::Restore,
            None,
            None,
            &format!("before restore to #{id}"),
        )?;
        // The shadow index now matches the worktree, so `read-tree -u`
        // removes exactly the files the target does not have.
        self.git(&["read-tree", "-u", "--reset", &target.commit])?;
        Ok(safety)
    }

    /// `/undo`: restore the most recent turn checkpoint that has not been
    /// undone yet, and mark it undone. Repeating walks further back.
    pub fn undo(&self) -> Result<Checkpoint, CheckpointError> {
        let _guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        let target = self
            .list()?
            .into_iter()
            .rev()
            .find(|c| c.kind == CheckpointKind::Turn && !c.undone)
            .ok_or(CheckpointError::NothingToUndo)?;
        self.restore_locked(target.id)?;
        let mut list = self.list()?;
        if let Some(c) = list.iter_mut().find(|c| c.id == target.id) {
            c.undone = true;
        }
        self.save(&list)?;
        Ok(target)
    }

    /// Restore only `paths` (worktree-relative) to their state in
    /// checkpoint `id`; paths the checkpoint did not have are deleted.
    pub fn restore_paths(&self, id: u64, paths: &[&str]) -> Result<(), CheckpointError> {
        let _guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        let cp = self.get(id)?;
        let mut present = Vec::new();
        for p in paths {
            if self.has_path(&cp.commit, p)? {
                present.push(*p);
            } else {
                match std::fs::remove_file(self.work_tree.join(p)) {
                    Ok(()) => {}
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(e) => return Err(e.into()),
                }
            }
        }
        if !present.is_empty() {
            let mut args = vec!["checkout", cp.commit.as_str(), "--"];
            args.extend(present);
            self.git(&args)?;
        }
        Ok(())
    }

    fn has_path(&self, commit: &str, path: &str) -> Result<bool, CheckpointError> {
        let out = self
            .command(&["cat-file", "-e", &format!("{commit}:{path}")])
            .output()
            .map_err(CheckpointError::Spawn)?;
        Ok(out.status.success())
    }

    /// The content of `path` in checkpoint `id`, if it existed and is
    /// UTF-8.
    pub fn file_text(&self, id: u64, path: &str) -> Result<Option<String>, CheckpointError> {
        let cp = self.get(id)?;
        let out = self
            .command(&["cat-file", "blob", &format!("{}:{path}", cp.commit)])
            .output()
            .map_err(CheckpointError::Spawn)?;
        if !out.status.success() {
            return Ok(None);
        }
        Ok(String::from_utf8(out.stdout).ok())
    }

    /// Paths that differ between checkpoint `id` and the current worktree.
    pub fn changed_since(&self, id: u64) -> Result<Vec<ChangedPath>, CheckpointError> {
        let _guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        let cp = self.get(id)?;
        self.stage_all()?;
        let out = self.git_plain(&[
            "diff",
            "--cached",
            "--name-status",
            "--no-renames",
            &cp.commit,
        ])?;
        Ok(out
            .lines()
            .filter_map(|l| {
                let (st, path) = l.split_once('\t')?;
                Some(ChangedPath {
                    status: st.chars().next()?,
                    path: path.to_string(),
                })
            })
            .collect())
    }

    /// Unified diff from checkpoint `id` to the current worktree.
    pub fn diff_since(&self, id: u64) -> Result<String, CheckpointError> {
        let _guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        let cp = self.get(id)?;
        self.stage_all()?;
        self.git_plain(&["diff", "--cached", "--no-renames", "--no-color", &cp.commit])
    }
}

/// Takes one checkpoint per mutating turn.
#[derive(Debug)]
pub struct TurnCheckpointer {
    store: std::sync::Arc<CheckpointStore>,
    last: Mutex<Option<(Option<NodeId>, u32)>>,
}

impl TurnCheckpointer {
    /// Wrap a store.
    pub fn new(store: std::sync::Arc<CheckpointStore>) -> Self {
        Self {
            store,
            last: Mutex::new(None),
        }
    }

    /// The store.
    pub fn store(&self) -> &std::sync::Arc<CheckpointStore> {
        &self.store
    }

    /// Call before every mutating tool call (write, edit, bash). The first
    /// call for a `(node, turn)` snapshots the worktree; later calls in the
    /// same turn return `None`.
    pub fn before_mutation(
        &self,
        node: Option<&NodeId>,
        turn: u32,
    ) -> Result<Option<Checkpoint>, CheckpointError> {
        let key = (node.cloned(), turn);
        let mut last = self.last.lock().unwrap_or_else(|e| e.into_inner());
        if last.as_ref() == Some(&key) {
            return Ok(None);
        }
        let label = match node {
            Some(n) => format!("before turn {turn} of {}", n.as_str()),
            None => format!("before turn {turn}"),
        };
        let cp = self
            .store
            .snapshot(CheckpointKind::Turn, node, Some(turn), &label)?;
        *last = Some(key);
        Ok(Some(cp))
    }
}

#[cfg(test)]
mod tests;
