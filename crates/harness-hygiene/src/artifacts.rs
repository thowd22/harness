//! Generated build and test artifacts.
//!
//! Running a test suite or a build writes files nobody edited:
//! `__pycache__/*.pyc`, `.pytest_cache/`, `target/`, `node_modules/`,
//! coverage data. The ledger records such bash-made changes as
//! *generated*: they stay visible in the event log and the diff view,
//! labelled `generated`, but they are not counted as changes to verify (the
//! `unverified` notice and `verify_when = on_changes` ignore them).
//!
//! Two sources decide what is generated:
//!
//! - the project's ignore rules. A [`Snapshot`](crate::snapshot::Snapshot)
//!   never lists untracked files ignored by `.gitignore` (also in parent
//!   directories up to the repository root) or `.git/info/exclude`, as
//!   `git status` hides them, so they never become changes at all;
//! - the short built-in list [`BUILTIN_ARTIFACTS`], for projects without a
//!   `.gitignore` (or with one that misses them). Matches are classified
//!   by [`Artifacts::is_generated`].
//!
//! Files the user's git tracks are never generated, even when they match
//! the list (a source directory named `target/`, a vendored and committed
//! `node_modules/`, a committed `coverage.xml`): [`tracked_among`] checks
//! the candidates. Files changed by the `write` and `edit` tools are never
//! generated either: the model changed them on purpose.

use std::collections::BTreeSet;
use std::path::Path;
use std::process::{Command, Stdio};

use ignore::gitignore::{Gitignore, GitignoreBuilder};

/// Built-in gitignore-style patterns for well-known build and test output.
pub const BUILTIN_ARTIFACTS: &[&str] = &[
    "__pycache__/",
    "*.pyc",
    "*.pyo",
    ".pytest_cache/",
    ".mypy_cache/",
    ".ruff_cache/",
    ".tox/",
    ".nox/",
    ".coverage",
    ".coverage.*",
    "coverage.xml",
    "htmlcov/",
    ".nyc_output/",
    "node_modules/",
    "target/",
    ".DS_Store",
];

/// Classifies workspace-relative paths as generated artifacts.
#[derive(Debug, Clone)]
pub struct Artifacts {
    builtin: Gitignore,
}

impl Default for Artifacts {
    fn default() -> Self {
        Self::builtin()
    }
}

impl Artifacts {
    /// The [`BUILTIN_ARTIFACTS`] matcher.
    pub fn builtin() -> Self {
        let mut b = GitignoreBuilder::new("");
        for line in BUILTIN_ARTIFACTS {
            // The patterns are constants; a bad one is a bug caught by the
            // tests below.
            let _ = b.add_line(None, line);
        }
        Self {
            builtin: b.build().unwrap_or_else(|_| Gitignore::empty()),
        }
    }

    /// True if the file at workspace-relative `rel` (`/` separators) is a
    /// well-known generated artifact, itself or through a parent directory
    /// (`calc/__pycache__/m.cpython-312.pyc`, `web/node_modules/x/i.js`).
    pub fn is_generated(&self, rel: &str) -> bool {
        let rel = rel.trim_start_matches("./");
        if rel.is_empty() || rel.starts_with('/') {
            return false;
        }
        self.builtin
            .matched_path_or_any_parents(Path::new(rel), false)
            .is_ignore()
    }

    /// The files under `root` (workspace-relative) that the user's git
    /// tracks and that match [`BUILTIN_ARTIFACTS`]. Empty outside a
    /// repository or when git cannot run.
    pub fn tracked_in(&self, root: &Path) -> BTreeSet<String> {
        ls_files(root, &[], None)
            .into_iter()
            .filter(|p| self.is_generated(p))
            .collect()
    }
}

/// The paths among `paths` (workspace-relative, `/` separators) that the
/// user's git tracks in the repository containing `root`, as one
/// `git ls-files` call. Empty outside a repository, when git cannot run,
/// or when `paths` is empty.
pub fn tracked_among(root: &Path, paths: &[&str]) -> BTreeSet<String> {
    if paths.is_empty() {
        return BTreeSet::new();
    }
    ls_files(root, &[], Some(paths))
}

/// The files under `root` (workspace-relative) that the user's git tracks
/// although an ignore rule matches them (force-added files, or files
/// committed before the rule), as one `git ls-files` call. Git never
/// ignores a tracked file, so a [`Snapshot`](crate::snapshot::Snapshot)
/// lists these too. Empty outside a repository or when git cannot run.
pub fn tracked_but_ignored(root: &Path) -> BTreeSet<String> {
    ls_files(root, &["--cached", "--ignored", "--exclude-standard"], None)
}

/// `git ls-files` in `root` (paths relative to `root`) with the `extra`
/// options, limited to the
/// literal `paths` when given (in chunks, so any number fits on the
/// command line). Inherited `GIT_*` variables are removed so a hook's
/// `GIT_DIR` or `GIT_INDEX_FILE` cannot point git at another repository.
fn ls_files(root: &Path, extra: &[&str], paths: Option<&[&str]>) -> BTreeSet<String> {
    let run = |chunk: &[&str]| -> Option<Vec<u8>> {
        let mut cmd = Command::new("git");
        for (k, _) in std::env::vars_os() {
            if k.to_string_lossy().starts_with("GIT_") {
                cmd.env_remove(&k);
            }
        }
        let out = cmd
            .env("GIT_OPTIONAL_LOCKS", "0")
            .env("GIT_TERMINAL_PROMPT", "0")
            .args(["--literal-pathspecs", "ls-files", "-z"])
            .args(extra)
            .arg("--")
            .args(chunk)
            .current_dir(root)
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .ok()?;
        out.status.success().then_some(out.stdout)
    };
    let mut stdout = Vec::new();
    match paths {
        None => stdout = run(&[]).unwrap_or_default(),
        Some(paths) => {
            // Well below every platform's argument limit.
            const CHUNK_BYTES: usize = 64 * 1024;
            let mut start = 0;
            while start < paths.len() {
                let mut end = start;
                let mut bytes = 0;
                while end < paths.len() && (end == start || bytes + paths[end].len() < CHUNK_BYTES)
                {
                    bytes += paths[end].len() + 1;
                    end += 1;
                }
                match run(&paths[start..end]) {
                    Some(out) => stdout.extend(out),
                    // Not a repository (or git is missing): nothing is
                    // tracked.
                    None => return BTreeSet::new(),
                }
                start = end;
            }
        }
    }
    stdout
        .split(|b| *b == 0)
        .filter(|s| !s.is_empty())
        .map(|s| String::from_utf8_lossy(s).into_owned())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_builtin_pattern_parses() {
        let mut b = GitignoreBuilder::new("");
        for line in BUILTIN_ARTIFACTS {
            b.add_line(None, line).unwrap();
        }
        assert_eq!(b.build().unwrap().len(), BUILTIN_ARTIFACTS.len());
    }

    #[test]
    fn well_known_build_and_test_output_is_generated() {
        let a = Artifacts::builtin();
        for p in [
            "__pycache__/m.cpython-312.pyc",
            "calc/__pycache__/__init__.cpython-314.pyc",
            "legacy/m.pyc",
            ".pytest_cache/v/cache/lastfailed",
            "pkg/.mypy_cache/3.12/x.json",
            ".ruff_cache/0.1/a",
            ".tox/py312/log",
            ".coverage",
            ".coverage.host.123",
            "coverage.xml",
            "htmlcov/index.html",
            ".nyc_output/out.json",
            "node_modules/left-pad/index.js",
            "web/node_modules/x/i.js",
            "target/debug/app",
            "crates/x/target/release/x",
            ".DS_Store",
            "docs/.DS_Store",
        ] {
            assert!(a.is_generated(p), "{p} should be generated");
        }
    }

    fn git(root: &Path, args: &[&str]) {
        let st = Command::new("git")
            .args(["-c", "user.name=t", "-c", "user.email=t@t.invalid"])
            .args(args)
            .current_dir(root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .stdout(Stdio::null())
            .status()
            .unwrap();
        assert!(st.success(), "git {args:?}");
    }

    fn write(root: &Path, rel: &str) {
        let p = root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, "x\n").unwrap();
    }

    #[test]
    fn tracked_files_matching_the_list_are_found() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path();
        git(repo, &["init", "-q"]);
        for p in [
            "app/infra/target/config.txt",
            "app/src/target/mod.rs",
            "app/coverage.xml",
            "app/main.py",
        ] {
            write(repo, p);
        }
        git(repo, &["add", "."]);
        write(repo, "app/target/debug/out");
        write(repo, "app/__pycache__/m.pyc");
        let ws = repo.join("app");
        let tracked = tracked_among(
            &ws,
            &[
                "infra/target/config.txt",
                "coverage.xml",
                "target/debug/out",
                "__pycache__/m.pyc",
                "gone/never.txt",
            ],
        );
        let want: BTreeSet<String> = ["coverage.xml", "infra/target/config.txt"]
            .map(String::from)
            .into();
        assert_eq!(tracked, want);
        let want: BTreeSet<String> = [
            "coverage.xml",
            "infra/target/config.txt",
            "src/target/mod.rs",
        ]
        .map(String::from)
        .into();
        assert_eq!(Artifacts::builtin().tracked_in(&ws), want);
        assert!(tracked_among(&ws, &[]).is_empty());
        // Outside a repository nothing is tracked.
        let plain = tempfile::tempdir().unwrap();
        write(plain.path(), "target/x");
        assert!(tracked_among(plain.path(), &["target/x"]).is_empty());
        assert!(Artifacts::builtin().tracked_in(plain.path()).is_empty());
    }

    #[test]
    fn source_files_are_not_generated() {
        let a = Artifacts::builtin();
        for p in [
            "calc/__init__.py",
            "tests/test_calc.py",
            "run_tests.sh",
            "src/target.rs",
            "src/targets/mod.rs",
            "coverage.md",
            "node_modules.txt",
            "pycache.py",
            ".gitignore",
            "",
        ] {
            assert!(!a.is_generated(p), "{p} should not be generated");
        }
    }
}
