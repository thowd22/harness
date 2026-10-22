//! Git-backed Markdown memory drive.
//!
//! The drive is a git repository (default `<data_dir>/memory`, or
//! `[memory] drive`):
//!
//! ```text
//! memory/
//!   MEMORY.md                 general preferences + index of other notes
//!   projects/<project-key>/   per-project notes
//!   tools/                    per-tool notes
//!   .harness/                 internal: usage stats, search index, worktrees
//! ```
//!
//! `.harness/` is git-ignored and is a protected path for agent tools (see
//! [`Drive::protected_paths`]); only harness code writes there. Markdown is
//! merged with git's `union` driver, so concurrent bullet additions are kept
//! from both sides instead of conflicting.

pub mod git;
pub mod note;
pub mod project;

use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};

pub use git::Git;
pub use note::{Bullet, Line, Note, links, run_link};
pub use project::{KeySource, ProjectKey, project_key};

/// The always-loaded note at the drive root.
pub const MEMORY_FILE: &str = "MEMORY.md";
/// Directory of per-project notes.
pub const PROJECTS_DIR: &str = "projects";
/// Directory of per-tool notes.
pub const TOOLS_DIR: &str = "tools";
/// Internal directory (protected, git-ignored).
pub const INTERNAL_DIR: &str = ".harness";
/// The branch that holds the published memory.
pub const MAIN_BRANCH: &str = "main";
/// Note file extension.
pub const NOTE_EXT: &str = "md";

const GITIGNORE: &str = "/.harness/\n";
const GITATTRIBUTES: &str = "*.md merge=union\n";
const MEMORY_TEMPLATE: &str = "# Memory\n\nGeneral preferences and an index of other notes.\n";
const KEEP_FILE: &str = ".gitkeep";

/// Errors from drive operations.
#[derive(Debug, thiserror::Error)]
pub enum DriveError {
    /// Filesystem error.
    #[error("{context}: {source}")]
    Io {
        /// What was being done.
        context: String,
        /// The error.
        #[source]
        source: io::Error,
    },
    /// A git command failed.
    #[error("git {args} failed: {stderr}")]
    Git {
        /// Arguments.
        args: String,
        /// Its stderr.
        stderr: String,
    },
    /// A note path is not allowed.
    #[error("invalid note path `{path}`: {reason}")]
    InvalidPath {
        /// The path as given.
        path: String,
        /// Why it was rejected.
        reason: String,
    },
    /// The directory exists but is not a memory drive.
    #[error(
        "{0} exists but is not a git repository; move it away or point [memory] drive elsewhere"
    )]
    NotADrive(PathBuf),
    /// Another process held the drive lock for too long.
    #[error("timed out waiting for the memory drive lock {0}")]
    LockTimeout(PathBuf),
    /// A write could not be published after the maximum number of attempts.
    #[error("memory write still stale after {attempts} attempts")]
    StaleWrite {
        /// Attempts made.
        attempts: u32,
    },
}

pub(crate) fn io_err(context: impl Into<String>) -> impl FnOnce(io::Error) -> DriveError {
    let context = context.into();
    move |source| DriveError::Io { context, source }
}

/// A validated note path, relative to the drive root, always ending in `.md`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NotePath(String);

impl NotePath {
    /// Validates `path`. Accepts `MEMORY.md`, `projects/<key>/<name>.md` and
    /// `tools/<name>.md` (sub-directories allowed), with or without the `.md`
    /// suffix and with an optional `[[ ]]` wrapper. Rejects absolute paths,
    /// `..`, hidden components and the internal directory.
    pub fn parse(path: &str) -> Result<Self, DriveError> {
        let bad = |reason: &str| DriveError::InvalidPath {
            path: path.to_owned(),
            reason: reason.to_owned(),
        };
        let p = path
            .trim()
            .trim_start_matches("[[")
            .trim_end_matches("]]")
            .trim();
        if p.is_empty() {
            return Err(bad("empty"));
        }
        let p = if p.ends_with(".md") {
            p.to_owned()
        } else {
            format!("{p}.{NOTE_EXT}")
        };
        if p == MEMORY_FILE {
            return Ok(Self(p));
        }
        let mut comps = Vec::new();
        for c in Path::new(&p).components() {
            match c {
                Component::Normal(s) => {
                    let s = s.to_str().ok_or_else(|| bad("not UTF-8"))?;
                    if s.starts_with('.') {
                        return Err(bad("hidden and internal paths are not notes"));
                    }
                    if !s
                        .chars()
                        .all(|c| c.is_alphanumeric() || matches!(c, '-' | '_' | '.'))
                    {
                        return Err(bad("use letters, digits, `-`, `_` and `.` only"));
                    }
                    comps.push(s);
                }
                _ => return Err(bad("must be a relative path without `..`")),
            }
        }
        let ok = match comps.first().copied() {
            Some(PROJECTS_DIR) => comps.len() >= 3,
            Some(TOOLS_DIR) => comps.len() >= 2,
            _ => false,
        };
        if !ok {
            return Err(bad(
                "notes live in MEMORY.md, projects/<project-key>/<name>.md or tools/<name>.md",
            ));
        }
        Ok(Self(comps.join("/")))
    }

    /// The path with `.md`.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The `[[link]]` form: the path without `.md`.
    pub fn link(&self) -> &str {
        self.0.strip_suffix(".md").unwrap_or(&self.0)
    }

    /// The project key when this is a project note.
    pub fn project(&self) -> Option<&str> {
        let mut it = self.0.split('/');
        (it.next() == Some(PROJECTS_DIR))
            .then(|| it.next())
            .flatten()
    }

    /// A default title for a new note: the file stem, plus the project key
    /// for project notes.
    pub fn default_title(&self) -> String {
        let stem = Path::new(&self.0)
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        let mut t = stem.replace(['-', '_'], " ");
        if let Some(first) = t.get(..1) {
            t = first.to_uppercase() + &t[1..];
        }
        match self.project() {
            Some(p) => format!("{t} ({p})"),
            None => t,
        }
    }
}

impl std::fmt::Display for NotePath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// A memory drive: a git repository with `main` checked out at its root.
#[derive(Debug, Clone)]
pub struct Drive {
    root: PathBuf,
}

impl Drive {
    /// Opens the drive at `root`, initialising it first if the directory is
    /// missing or empty. Initialising creates the git repository on
    /// [`MAIN_BRANCH`], `MEMORY.md`, `projects/`, `tools/` and `.harness/`,
    /// and makes the first commit.
    pub fn open_or_init(root: impl Into<PathBuf>) -> Result<Self, DriveError> {
        let root = root.into();
        fs::create_dir_all(&root).map_err(io_err(format!("creating {}", root.display())))?;
        let drive = Self {
            root: root
                .canonicalize()
                .map_err(io_err(format!("resolving {}", root.display())))?,
        };
        let git = drive.git();
        if drive.root.join(".git").exists() {
            fs::create_dir_all(drive.internal_dir())
                .map_err(io_err("creating the internal directory"))?;
            return Ok(drive);
        }
        let empty = fs::read_dir(&drive.root)
            .map_err(io_err(format!("listing {}", drive.root.display())))?
            .next()
            .is_none();
        if !empty {
            return Err(DriveError::NotADrive(drive.root));
        }
        git.run(&["init", "--quiet", "-b", MAIN_BRANCH])?;
        let write = |rel: &str, text: &str| {
            let p = drive.root.join(rel);
            if let Some(parent) = p.parent() {
                fs::create_dir_all(parent).map_err(io_err(format!("creating {rel}")))?;
            }
            fs::write(&p, text).map_err(io_err(format!("writing {rel}")))
        };
        write(MEMORY_FILE, MEMORY_TEMPLATE)?;
        write(&format!("{PROJECTS_DIR}/{KEEP_FILE}"), "")?;
        write(&format!("{TOOLS_DIR}/{KEEP_FILE}"), "")?;
        write(".gitignore", GITIGNORE)?;
        write(".gitattributes", GITATTRIBUTES)?;
        fs::create_dir_all(drive.internal_dir())
            .map_err(io_err("creating the internal directory"))?;
        git.commit_all("memory: initialise drive")?;
        Ok(drive)
    }

    /// Opens the drive configured in `harness.ini` (`[memory] drive`, or
    /// `<data_dir>/memory`).
    pub fn from_config(config: &harness_config::ini::Config) -> Result<Self, DriveError> {
        Self::open_or_init(harness_config::paths::DataDir::from_config(config).memory())
    }

    /// The drive root (canonical).
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// `<root>/.harness`.
    pub fn internal_dir(&self) -> PathBuf {
        self.root.join(INTERNAL_DIR)
    }

    /// `<root>/MEMORY.md`.
    pub fn memory_file(&self) -> PathBuf {
        self.root.join(MEMORY_FILE)
    }

    /// Paths agent tools must never write: the internal directory. Add these
    /// to `harness_policy::rules::PolicyConfig::protected_roots`; the data
    /// dir is already protected, but a `[memory] drive` outside it is not.
    pub fn protected_paths(&self) -> Vec<PathBuf> {
        vec![self.internal_dir()]
    }

    /// A git runner at the drive root.
    pub fn git(&self) -> Git {
        Git::new(&self.root)
    }

    /// The commit `main` points to.
    pub fn head(&self) -> Result<String, DriveError> {
        self.git().rev_parse(MAIN_BRANCH)
    }

    /// Reads a note from the published drive. `Ok(None)` when it does not
    /// exist.
    pub fn read_note(&self, path: &NotePath) -> Result<Option<String>, DriveError> {
        read_note_at(&self.root, path)
    }
}

/// Reads `path` under `checkout` (a drive root or a session worktree).
pub fn read_note_at(checkout: &Path, path: &NotePath) -> Result<Option<String>, DriveError> {
    match fs::read_to_string(checkout.join(path.as_str())) {
        Ok(s) => Ok(Some(s)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(io_err(format!("reading {path}"))(e)),
    }
}

/// Every note under `checkout`, sorted.
pub fn list_notes(checkout: &Path) -> Result<Vec<NotePath>, DriveError> {
    let mut out = Vec::new();
    if checkout.join(MEMORY_FILE).is_file() {
        out.push(NotePath(MEMORY_FILE.to_owned()));
    }
    for dir in [PROJECTS_DIR, TOOLS_DIR] {
        walk(checkout, &checkout.join(dir), &mut out)?;
    }
    out.sort();
    Ok(out)
}

fn walk(base: &Path, dir: &Path, out: &mut Vec<NotePath>) -> Result<(), DriveError> {
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(io_err(format!("listing {}", dir.display()))(e)),
    };
    for e in entries {
        let e = e.map_err(io_err("listing notes"))?;
        let name = e.file_name();
        if name.to_string_lossy().starts_with('.') {
            continue;
        }
        let ty = e.file_type().map_err(io_err("listing notes"))?;
        let p = e.path();
        if ty.is_dir() {
            walk(base, &p, out)?;
        } else if ty.is_file()
            && p.extension().is_some_and(|x| x == NOTE_EXT)
            && let Some(rel) = p.strip_prefix(base).ok().and_then(Path::to_str)
            && let Ok(np) = NotePath::parse(rel)
        {
            out.push(np);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn init_creates_layout_and_commit() {
        let tmp = tempfile::tempdir().unwrap();
        let d = Drive::open_or_init(tmp.path().join("memory")).unwrap();
        assert!(d.root().join(".git").is_dir());
        assert!(d.memory_file().is_file());
        assert!(d.root().join(PROJECTS_DIR).is_dir());
        assert!(d.root().join(TOOLS_DIR).is_dir());
        assert!(d.internal_dir().is_dir());
        assert_eq!(d.git().run(&["branch", "--show-current"]).unwrap(), "main");
        let files = d.git().run(&["ls-files"]).unwrap();
        assert!(files.contains("MEMORY.md"));
        assert!(
            !files.contains(".harness"),
            "internal dir must be untracked"
        );
        assert_eq!(d.git().run(&["status", "--porcelain"]).unwrap(), "");
        // Reopening is idempotent.
        let head = d.head().unwrap();
        let d2 = Drive::open_or_init(d.root()).unwrap();
        assert_eq!(d2.head().unwrap(), head);
    }

    #[test]
    fn refuses_non_empty_non_repo() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join("x"), "").unwrap();
        assert!(matches!(
            Drive::open_or_init(tmp.path()),
            Err(DriveError::NotADrive(_))
        ));
    }

    #[test]
    fn note_paths_are_validated() {
        assert_eq!(NotePath::parse("MEMORY.md").unwrap().as_str(), "MEMORY.md");
        let p = NotePath::parse("[[projects/my-app-1234/testing]]").unwrap();
        assert_eq!(p.as_str(), "projects/my-app-1234/testing.md");
        assert_eq!(p.link(), "projects/my-app-1234/testing");
        assert_eq!(p.project(), Some("my-app-1234"));
        assert_eq!(p.default_title(), "Testing (my-app-1234)");
        assert!(NotePath::parse("tools/cargo").is_ok());
        for bad in [
            "../x",
            "/etc/passwd",
            ".harness/usage",
            "tools/.hidden",
            "projects/x.md",
            "other/x",
            "tools/a b",
            "",
        ] {
            assert!(NotePath::parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn lists_notes() {
        let tmp = tempfile::tempdir().unwrap();
        let d = Drive::open_or_init(tmp.path()).unwrap();
        fs::create_dir_all(d.root().join("projects/k")).unwrap();
        fs::write(d.root().join("projects/k/a.md"), "# A\n").unwrap();
        fs::write(d.root().join("tools/cargo.md"), "# Cargo\n").unwrap();
        let names: Vec<_> = list_notes(d.root())
            .unwrap()
            .into_iter()
            .map(|n| n.0)
            .collect();
        assert_eq!(
            names,
            vec!["MEMORY.md", "projects/k/a.md", "tools/cargo.md"]
        );
    }
}
