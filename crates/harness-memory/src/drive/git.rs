//! Thin wrapper over the `git` command line, isolated from user config.
//!
//! Every invocation disables hooks, commit signing and the system config, and
//! sets a fixed committer identity, so a user's global git setup cannot make
//! memory writes fail or prompt.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use super::DriveError;

/// Author and committer name used for memory commits.
pub const AUTHOR_NAME: &str = "harness";
/// Author and committer email used for memory commits.
pub const AUTHOR_EMAIL: &str = "harness@localhost";

/// Runs `git` in one directory (a drive root or a session worktree).
#[derive(Debug, Clone)]
pub struct Git {
    dir: PathBuf,
}

impl Git {
    /// A runner for `dir`.
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    /// The working directory.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut c = Command::new("git");
        c.current_dir(&self.dir)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_AUTHOR_NAME", AUTHOR_NAME)
            .env("GIT_AUTHOR_EMAIL", AUTHOR_EMAIL)
            .env("GIT_COMMITTER_NAME", AUTHOR_NAME)
            .env("GIT_COMMITTER_EMAIL", AUTHOR_EMAIL)
            .env("GIT_EDITOR", "true")
            .env("GIT_SEQUENCE_EDITOR", "true")
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE")
            .args([
                "-c",
                "core.hooksPath=/dev/null",
                "-c",
                "commit.gpgsign=false",
                "-c",
                "tag.gpgsign=false",
                "-c",
                "core.autocrlf=false",
                "-c",
                "rebase.autoStash=false",
            ])
            .args(args)
            .stdin(Stdio::null());
        c
    }

    /// Runs git and returns the raw output, whatever the exit status.
    pub fn output(&self, args: &[&str]) -> Result<Output, DriveError> {
        self.command(args).output().map_err(|e| DriveError::Io {
            context: format!("running git {}", args.join(" ")),
            source: e,
        })
    }

    /// Runs git and returns trimmed stdout, or [`DriveError::Git`] on a
    /// non-zero exit.
    pub fn run(&self, args: &[&str]) -> Result<String, DriveError> {
        let out = self.output(args)?;
        if out.status.success() {
            Ok(String::from_utf8_lossy(&out.stdout).trim().to_owned())
        } else {
            Err(DriveError::Git {
                args: args.join(" "),
                stderr: String::from_utf8_lossy(&out.stderr).trim().to_owned(),
            })
        }
    }

    /// True when git exits with status 0.
    pub fn succeeds(&self, args: &[&str]) -> Result<bool, DriveError> {
        Ok(self.output(args)?.status.success())
    }

    /// `git rev-parse <rev>`.
    pub fn rev_parse(&self, rev: &str) -> Result<String, DriveError> {
        self.run(&["rev-parse", "--verify", "--quiet", rev])
    }

    /// Stages everything and commits with `message`. Returns the new commit
    /// id, or `None` when there was nothing to commit.
    pub fn commit_all(&self, message: &str) -> Result<Option<String>, DriveError> {
        self.run(&["add", "-A"])?;
        if self.succeeds(&["diff", "--cached", "--quiet"])? {
            return Ok(None);
        }
        self.run(&["commit", "--quiet", "-m", message])?;
        self.rev_parse("HEAD").map(Some)
    }
}
