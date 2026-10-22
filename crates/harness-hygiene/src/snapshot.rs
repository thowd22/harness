//! Worktree snapshots: what every file looked like at one moment.
//!
//! The ledger takes a [`Snapshot`] before and after each bash command and
//! compares them to find the files the command created, modified or
//! deleted. A snapshot walks the worktree with the same rules git uses for
//! untracked files (`.gitignore`, `.git/info/exclude`), always skips `.git`,
//! and includes hidden files. Symlinks are not followed.
//!
//! Hashing every file before and after every command would be slow, so
//! [`Snapshot::take_with_cache`] reuses the hash of a file whose length and
//! modification time did not change since the previous snapshot.

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

use harness_events::schema::FileChangeKind;
use sha2::{Digest, Sha256};

/// Limits for [`Snapshot`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SnapshotOptions {
    /// Text files up to this size keep their content, so diffs can be
    /// produced for bash-made changes. Default 64 KiB.
    pub text_file_bytes: u64,
    /// Total content kept per snapshot. Default 32 MiB.
    pub text_total_bytes: u64,
}

impl Default for SnapshotOptions {
    fn default() -> Self {
        Self {
            text_file_bytes: 64 * 1024,
            text_total_bytes: 32 * 1024 * 1024,
        }
    }
}

/// One file in a snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileState {
    /// Size in bytes.
    pub len: u64,
    /// Last modification time, when the platform reports it.
    pub modified: Option<SystemTime>,
    /// sha256 of the content (hex).
    pub hash: String,
    /// The content, for small UTF-8 files (see [`SnapshotOptions`]).
    pub text: Option<Arc<str>>,
}

/// The state of every visible file under a root.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Snapshot {
    root: PathBuf,
    files: BTreeMap<String, FileState>,
}

/// One difference between two snapshots.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathChange {
    /// Root-relative path with `/` separators.
    pub path: String,
    /// What happened.
    pub kind: FileChangeKind,
    /// State before (`None` when created).
    pub before: Option<FileState>,
    /// State after (`None` when deleted).
    pub after: Option<FileState>,
}

impl PathChange {
    /// A unified diff (`a/` and `b/` headers), when both sides' text is
    /// known (an absent side counts as empty).
    pub fn diff(&self) -> Option<String> {
        let side = |s: &Option<FileState>| match s {
            None => Some(Arc::<str>::from("")),
            Some(f) => f.text.clone(),
        };
        let before = side(&self.before)?;
        let after = side(&self.after)?;
        Some(harness_tools::write::unified_diff(
            &self.path, &before, &after,
        ))
    }
}

impl Snapshot {
    /// Snapshot `root` from scratch.
    pub fn take(root: &Path, opts: &SnapshotOptions) -> io::Result<Snapshot> {
        Self::take_with_cache(root, opts, None)
    }

    /// Snapshot `root`, reusing hashes and text from `previous` for files
    /// whose length and modification time are unchanged.
    pub fn take_with_cache(
        root: &Path,
        opts: &SnapshotOptions,
        previous: Option<&Snapshot>,
    ) -> io::Result<Snapshot> {
        let mut files = BTreeMap::new();
        let mut text_budget = opts.text_total_bytes;
        let walker = ignore::WalkBuilder::new(root)
            .hidden(false)
            .git_ignore(true)
            .git_exclude(true)
            .git_global(false)
            .ignore(false)
            .parents(false)
            .require_git(false)
            .follow_links(false)
            .filter_entry(|e| e.file_name() != ".git")
            .build();
        for entry in walker {
            let entry = entry.map_err(io::Error::other)?;
            if !entry.file_type().is_some_and(|t| t.is_file()) {
                continue;
            }
            let Some(rel) = relative(root, entry.path()) else {
                continue;
            };
            let meta = match entry.metadata() {
                Ok(m) => m,
                Err(_) => continue,
            };
            let len = meta.len();
            let modified = meta.modified().ok();
            let cached = previous
                .and_then(|p| p.files.get(&rel))
                .filter(|f| f.len == len && f.modified.is_some() && f.modified == modified);
            let state = match cached {
                Some(f) => f.clone(),
                None => match std::fs::read(entry.path()) {
                    Ok(bytes) => {
                        let keep = len <= opts.text_file_bytes && len <= text_budget;
                        let text = keep
                            .then(|| std::str::from_utf8(&bytes).ok().map(Arc::<str>::from))
                            .flatten();
                        FileState {
                            len,
                            modified,
                            hash: hex::encode(Sha256::digest(&bytes)),
                            text,
                        }
                    }
                    // Vanished or unreadable between listing and reading.
                    Err(_) => continue,
                },
            };
            if let Some(t) = &state.text {
                text_budget = text_budget.saturating_sub(t.len() as u64);
            }
            files.insert(rel, state);
        }
        Ok(Snapshot {
            root: root.to_path_buf(),
            files,
        })
    }

    /// The snapshot root.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Every file, keyed by root-relative path.
    pub fn files(&self) -> &BTreeMap<String, FileState> {
        &self.files
    }

    /// One file's state.
    pub fn get(&self, rel: &str) -> Option<&FileState> {
        self.files.get(rel)
    }

    /// The files that differ between `self` (before) and `after`, sorted
    /// by path.
    pub fn diff(&self, after: &Snapshot) -> Vec<PathChange> {
        let mut out = Vec::new();
        for (path, b) in &self.files {
            match after.files.get(path) {
                None => out.push(PathChange {
                    path: path.clone(),
                    kind: FileChangeKind::Deleted,
                    before: Some(b.clone()),
                    after: None,
                }),
                Some(a) if a.hash != b.hash => out.push(PathChange {
                    path: path.clone(),
                    kind: FileChangeKind::Modified,
                    before: Some(b.clone()),
                    after: Some(a.clone()),
                }),
                Some(_) => {}
            }
        }
        for (path, a) in &after.files {
            if !self.files.contains_key(path) {
                out.push(PathChange {
                    path: path.clone(),
                    kind: FileChangeKind::Created,
                    before: None,
                    after: Some(a.clone()),
                });
            }
        }
        out.sort_by(|x, y| x.path.cmp(&y.path));
        out
    }
}

/// `path` relative to `root` with `/` separators, or `None` if it is not
/// beneath `root`.
pub fn relative(root: &Path, path: &Path) -> Option<String> {
    let rel = path.strip_prefix(root).ok()?;
    let parts: Vec<String> = rel
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    (!parts.is_empty()).then(|| parts.join("/"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(root: &Path, rel: &str, text: &str) {
        let p = root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    }

    #[test]
    fn detects_created_modified_deleted() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(root, "a.txt", "one\n");
        write(root, "b.txt", "keep\n");
        write(root, "c.txt", "gone\n");
        let opts = SnapshotOptions::default();
        let before = Snapshot::take(root, &opts).unwrap();
        write(root, "a.txt", "two\n");
        std::fs::remove_file(root.join("c.txt")).unwrap();
        write(root, "sub/new.txt", "new\n");
        let after = Snapshot::take_with_cache(root, &opts, Some(&before)).unwrap();
        let changes = before.diff(&after);
        let got: Vec<(&str, FileChangeKind)> =
            changes.iter().map(|c| (c.path.as_str(), c.kind)).collect();
        assert_eq!(
            got,
            vec![
                ("a.txt", FileChangeKind::Modified),
                ("c.txt", FileChangeKind::Deleted),
                ("sub/new.txt", FileChangeKind::Created),
            ]
        );
        let d = changes[0].diff().unwrap();
        assert!(d.contains("-one") && d.contains("+two"), "{d}");
    }

    #[test]
    fn honours_gitignore_and_skips_git_dir() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(root, ".gitignore", "target/\n");
        write(root, "target/out.bin", "x");
        write(root, ".git/config", "x");
        write(root, ".hidden", "x");
        let s = Snapshot::take(root, &SnapshotOptions::default()).unwrap();
        let names: Vec<&str> = s.files().keys().map(String::as_str).collect();
        assert_eq!(names, vec![".gitignore", ".hidden"]);
    }

    #[test]
    fn text_is_kept_only_for_small_utf8_files() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(root, "small.txt", "hi");
        write(root, "big.txt", &"x".repeat(100));
        std::fs::write(root.join("bin"), [0xff, 0xfe]).unwrap();
        let opts = SnapshotOptions {
            text_file_bytes: 10,
            ..Default::default()
        };
        let s = Snapshot::take(root, &opts).unwrap();
        assert_eq!(s.get("small.txt").unwrap().text.as_deref(), Some("hi"));
        assert!(s.get("big.txt").unwrap().text.is_none());
        assert!(s.get("bin").unwrap().text.is_none());
    }
}
