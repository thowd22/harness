//! Worktree snapshots: what every file looked like at one moment.
//!
//! The ledger takes a [`Snapshot`] before and after each bash command and
//! compares them to find the files the command created, modified or
//! deleted. A snapshot walks the worktree with the same rules git uses for
//! untracked files (`.gitignore`, also in parent directories up to the
//! nearest repository root but not above it, and `.git/info/exclude`),
//! always skips `.git`, and includes hidden files. Symlinks are not
//! followed. Tracked files are always included, even when an ignore rule
//! matches them, and a workspace its enclosing repository ignores (under a
//! dotfiles home whose `.gitignore` is `*`) uses only its own ignore files.
//! Ignored untracked files are not part of a snapshot, so a command that
//! only writes ignored files changes nothing (see [`crate::artifacts`]).
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
        // A workspace below its repository root also honours the
        // `.gitignore` files above it, up to that root and no further, as
        // git does. With `require_git(true)` the ignore crate stops at the
        // first ancestor with a `.git`, so a `.gitignore` above the
        // repository (a dotfiles home with `*`, a stray one in a parent
        // directory) never hides files. At the repository root, or outside
        // a repository, only the workspace's own ignore files count.
        //
        // When the enclosing repository ignores the workspace directory
        // itself (a project or a force-added config directory under a
        // dotfiles home whose `.gitignore` is `*`), its rules say nothing
        // useful about the workspace, so only the workspace's own ignore
        // files count, as outside a repository.
        let repo = repository_root(root);
        let below_repo_root = matches!(repo, Some(r) if r != root && !ignored_by_parents(r, root));
        let walker = ignore::WalkBuilder::new(root)
            .hidden(false)
            .git_ignore(true)
            .git_exclude(true)
            .git_global(false)
            .ignore(false)
            .parents(below_repo_root)
            .require_git(below_repo_root)
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
            if let Some(state) =
                file_state(entry.path(), &meta, &rel, opts, &mut text_budget, previous)
            {
                files.insert(rel, state);
            }
        }
        // Git never ignores a tracked file: add the tracked files an
        // ignore rule hid from the walk.
        if repo.is_some() {
            for rel in crate::artifacts::tracked_but_ignored(root) {
                if files.contains_key(&rel) || rel.split('/').any(|c| c == ".git") {
                    continue;
                }
                let path = root.join(&rel);
                let Ok(meta) = std::fs::symlink_metadata(&path) else {
                    continue;
                };
                if !meta.is_file() {
                    continue;
                }
                if let Some(state) =
                    file_state(&path, &meta, &rel, opts, &mut text_budget, previous)
                {
                    files.insert(rel, state);
                }
            }
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

/// The state of the file at `path` (workspace-relative `rel`), reusing
/// `previous` when its length and modification time are unchanged; `None`
/// when it vanished or cannot be read. Kept text is charged to
/// `text_budget`.
fn file_state(
    path: &Path,
    meta: &std::fs::Metadata,
    rel: &str,
    opts: &SnapshotOptions,
    text_budget: &mut u64,
    previous: Option<&Snapshot>,
) -> Option<FileState> {
    let len = meta.len();
    let modified = meta.modified().ok();
    let cached = previous
        .and_then(|p| p.files.get(rel))
        .filter(|f| f.len == len && f.modified.is_some() && f.modified == modified);
    let state = match cached {
        Some(f) => f.clone(),
        None => {
            // Vanished or unreadable between listing and reading.
            let bytes = std::fs::read(path).ok()?;
            let keep = len <= opts.text_file_bytes && len <= *text_budget;
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
    };
    if let Some(t) = &state.text {
        *text_budget = text_budget.saturating_sub(t.len() as u64);
    }
    Some(state)
}

/// True if the repository at `repo` ignores `dir` (a directory below
/// `repo`) or one of the directories between them, by the `.gitignore`
/// files from `repo` down to `dir`'s parent and `.git/info/exclude`. A
/// deeper `.gitignore` takes precedence, and git does not look inside an
/// ignored directory, so the first ignored component decides.
fn ignored_by_parents(repo: &Path, dir: &Path) -> bool {
    use ignore::gitignore::{Gitignore, GitignoreBuilder};
    let Ok(rel) = dir.strip_prefix(repo) else {
        return false;
    };
    // Lowest precedence first.
    let mut matchers: Vec<Gitignore> = Vec::new();
    let mut exclude = GitignoreBuilder::new(repo);
    if exclude.add(repo.join(".git/info/exclude")).is_none()
        && let Ok(g) = exclude.build()
    {
        matchers.push(g);
    }
    let mut current = repo.to_path_buf();
    for component in rel.components() {
        let file = current.join(".gitignore");
        if file.is_file() {
            let (g, _) = Gitignore::new(&file);
            matchers.push(g);
        }
        current.push(component);
        for m in matchers.iter().rev() {
            match m.matched(&current, true) {
                ignore::Match::Ignore(_) => return true,
                ignore::Match::Whitelist(_) => break,
                ignore::Match::None => {}
            }
        }
    }
    false
}

/// The nearest enclosing repository root of `dir`: the first of `dir` and
/// its ancestors that has a `.git` (directory or file).
fn repository_root(dir: &Path) -> Option<&Path> {
    dir.ancestors().find(|a| a.join(".git").exists())
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
    fn a_workspace_below_the_repository_root_honours_parent_gitignores() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path();
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        write(repo, ".gitignore", "*.log\nbuild/\n");
        let ws = repo.join("app");
        write(&ws, "main.py", "x");
        write(&ws, "run.log", "x");
        write(&ws, "build/out.bin", "x");
        let s = Snapshot::take(&ws, &SnapshotOptions::default()).unwrap();
        let names: Vec<&str> = s.files().keys().map(String::as_str).collect();
        assert_eq!(names, vec!["main.py"]);

        // Outside a repository, a parent's .gitignore does not apply.
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), ".gitignore", "*.log\n");
        let ws = dir.path().join("app");
        write(&ws, "run.log", "x");
        let s = Snapshot::take(&ws, &SnapshotOptions::default()).unwrap();
        assert!(s.get("run.log").is_some());
    }

    fn names(root: &Path) -> Vec<String> {
        let s = Snapshot::take(root, &SnapshotOptions::default()).unwrap();
        s.files().keys().cloned().collect()
    }

    #[test]
    fn a_gitignore_above_the_repository_root_does_not_apply() {
        // A dotfiles home: the home directory is a repository whose
        // .gitignore ignores everything; projects below are their own
        // repositories.
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        std::fs::create_dir_all(home.join(".git")).unwrap();
        write(home, ".gitignore", "*\n!.bashrc\n");
        let proj = home.join("code/proj");
        std::fs::create_dir_all(proj.join(".git")).unwrap();
        write(&proj, ".gitignore", "*.log\n");
        write(&proj, "main.py", "x");
        write(&proj, "run.log", "x");
        // The workspace at the nested repository's root.
        assert_eq!(names(&proj), vec![".gitignore", "main.py"]);
        // A workspace below the nested repository's root honours the
        // nested repository's .gitignore, but not the home one.
        let app = proj.join("app");
        write(&app, "new.py", "x");
        write(&app, "app.log", "x");
        assert_eq!(names(&app), vec!["new.py"]);

        // A plain directory with a stray .gitignore above a repository.
        let dir = tempfile::tempdir().unwrap();
        let outer = dir.path();
        write(outer, ".gitignore", "*.txt\n*.py\n");
        let repo = outer.join("repo");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        write(&repo, ".gitignore", "build/\n");
        let ws = repo.join("app");
        write(&ws, "notes.txt", "x");
        write(&ws, "main.py", "x");
        write(&ws, "build/out.bin", "x");
        assert_eq!(names(&ws), vec!["main.py", "notes.txt"]);
        assert_eq!(
            names(&repo),
            vec![".gitignore", "app/main.py", "app/notes.txt"]
        );
    }

    fn git(root: &Path, args: &[&str]) {
        let st = std::process::Command::new("git")
            .args(["-c", "user.name=t", "-c", "user.email=t@t.invalid"])
            .args(args)
            .current_dir(root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .stdout(std::process::Stdio::null())
            .status()
            .unwrap();
        assert!(st.success(), "git {args:?}");
    }

    #[test]
    fn a_workspace_ignored_by_its_enclosing_repository_sees_its_files() {
        // A dotfiles home whose .gitignore ignores everything, with
        // force-added (tracked) files and a plain project below it.
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        git(home, &["init", "-q"]);
        write(home, ".gitignore", "*\n");
        write(home, ".config/nvim/init.lua", "x");
        write(home, ".config/nvim/lua/plug.lua", "x");
        git(home, &["add", "-f", ".config/nvim/init.lua"]);
        write(home, "scratch/proj/main.py", "x");
        write(home, "scratch/proj/.gitignore", "*.log\n");
        write(home, "scratch/proj/run.log", "x");
        // The workspace is a directory the home repository ignores, so
        // its ignore rules say nothing about the workspace's files: a
        // tracked dotfile and an untracked one both count.
        assert_eq!(
            names(&home.join(".config/nvim")),
            vec!["init.lua", "lua/plug.lua"]
        );
        // A plain project under such a home keeps only its own rules.
        assert_eq!(
            names(&home.join("scratch/proj")),
            vec![".gitignore", "main.py"]
        );
    }

    #[test]
    fn tracked_files_are_listed_even_when_an_ignore_rule_matches_them() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path();
        git(repo, &["init", "-q"]);
        write(repo, ".gitignore", "*.log\n");
        write(repo, "app/keep.log", "x");
        write(repo, "app/main.py", "x");
        write(repo, "top.log", "x");
        git(repo, &["add", "-f", "app/keep.log", "top.log"]);
        write(repo, "app/other.log", "x");
        write(repo, "new.log", "x");
        // Below the repository root (parent .gitignore applies).
        assert_eq!(names(&repo.join("app")), vec!["keep.log", "main.py"]);
        // At the repository root (its own .gitignore applies).
        assert_eq!(
            names(repo),
            vec![".gitignore", "app/keep.log", "app/main.py", "top.log"]
        );
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
