//! Memory drive synchronization: one git worktree per session, and
//! publishing session commits to `main` safely.
//!
//! Each session opens a [`SessionCheckout`]: a worktree at
//! `<drive>/.harness/worktrees/<session>` on branch `session/<session>`.
//! Writes edit files in that worktree and commit there. Publishing then takes
//! the drive lock and fast-forwards `main` (and the drive root's working
//! tree) to the session commit.
//!
//! If another session published first, the write is *stale*: `main` is no
//! longer an ancestor of the session commit. The publish is rejected, the
//! session branch is rebased onto the new `main` (under the lock, so the
//! retry cannot be overtaken) and publishing is retried, up to
//! [`MAX_PUBLISH_ATTEMPTS`] times. Markdown notes use git's `union`
//! merge driver (see `.gitattributes` in the drive), so two sessions adding
//! bullets to the same spot of one note both keep their lines: edits are
//! merged, never overwritten.

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime};

use crate::drive::{Drive, DriveError, Git, INTERNAL_DIR, MAIN_BRANCH, io_err};

/// Directory under `.harness/` that holds session worktrees.
pub const WORKTREES_DIR: &str = "worktrees";
/// Prefix of session branch names.
pub const SESSION_BRANCH_PREFIX: &str = "session/";
/// Lock file under `.harness/` that serialises publishing.
pub const LOCK_FILE: &str = "drive.lock";
/// How many times a stale write is rebased and retried.
pub const MAX_PUBLISH_ATTEMPTS: u32 = 8;
/// How long to wait for the drive lock.
pub const LOCK_TIMEOUT: Duration = Duration::from_secs(30);
/// A lock older than this is considered abandoned and broken.
pub const STALE_LOCK_AGE: Duration = Duration::from_secs(120);

/// The outcome of a published write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Published {
    /// The commit now at `main`.
    pub commit: String,
    /// Publish attempts made (1 when nothing raced).
    pub attempts: u32,
    /// How many times the write was rejected as stale and rebased.
    pub rebases: u32,
}

/// A session's private worktree of the drive.
///
/// Methods take `&self` and serialise internally, so one checkout can be
/// shared (`Arc`) by an agent and its subagents.
#[derive(Debug)]
pub struct SessionCheckout {
    drive: Drive,
    session: String,
    path: PathBuf,
    branch: String,
    lock: Mutex<()>,
}

impl SessionCheckout {
    /// Opens (or re-opens) the worktree for `session`, starting at the
    /// current `main`. `session` must be a simple id (letters, digits, `-`,
    /// `_`).
    pub fn open(drive: &Drive, session: &str) -> Result<Self, DriveError> {
        if session.is_empty()
            || !session
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
        {
            return Err(DriveError::InvalidPath {
                path: session.to_owned(),
                reason: "session ids may contain only letters, digits, `-` and `_`".into(),
            });
        }
        let path = drive.internal_dir().join(WORKTREES_DIR).join(session);
        let branch = format!("{SESSION_BRANCH_PREFIX}{session}");
        let me = Self {
            drive: drive.clone(),
            session: session.to_owned(),
            path,
            branch,
            lock: Mutex::new(()),
        };
        let _g = DriveLock::acquire(drive)?;
        let root = drive.git();
        if me.path.join(".git").exists() {
            // Resume: bring it up to date with main.
            me.rebase_onto_main()?;
        } else {
            // A worktree dir removed by hand leaves stale metadata behind.
            root.run(&["worktree", "prune"])?;
            fs::create_dir_all(me.path.parent().unwrap_or(&me.path))
                .map_err(io_err("creating the worktrees directory"))?;
            let p = me.path.to_string_lossy().into_owned();
            root.run(&[
                "worktree",
                "add",
                "--quiet",
                "-B",
                &me.branch,
                &p,
                MAIN_BRANCH,
            ])?;
        }
        Ok(me)
    }

    /// The drive this checkout belongs to.
    pub fn drive(&self) -> &Drive {
        &self.drive
    }

    /// The session id.
    pub fn session(&self) -> &str {
        &self.session
    }

    /// The worktree directory. Reads for this session come from here.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The session branch name.
    pub fn branch(&self) -> &str {
        &self.branch
    }

    /// A git runner in the worktree.
    pub fn git(&self) -> Git {
        Git::new(&self.path)
    }

    /// Brings the worktree up to date with `main` (rebasing any unpublished
    /// commits).
    pub fn refresh(&self) -> Result<(), DriveError> {
        let _l = self.lock.lock().unwrap_or_else(|p| p.into_inner());
        self.rebase_onto_main()
    }

    /// Applies `edit` to the worktree, commits the result with `message` and
    /// publishes it to `main`.
    ///
    /// `edit` runs against an up-to-date worktree and receives its path. It
    /// returns a value passed back to the caller. When `edit` changes
    /// nothing, nothing is committed and `Ok((value, None))` is returned.
    pub fn write<T>(
        &self,
        message: &str,
        edit: impl FnOnce(&Path) -> Result<T, DriveError>,
    ) -> Result<(T, Option<Published>), DriveError> {
        let _l = self.lock.lock().unwrap_or_else(|p| p.into_inner());
        self.rebase_onto_main()?;
        let value = edit(&self.path)?;
        let git = self.git();
        if git.commit_all(message)?.is_none() {
            return Ok((value, None));
        }
        let published = self.publish()?;
        Ok((value, Some(published)))
    }

    /// Publishes the session branch to `main`, rebasing and retrying while
    /// the write is stale.
    ///
    /// The first attempt is optimistic: the session commit was made without
    /// holding the drive lock. If `main` moved meanwhile, the publish is
    /// rejected as stale; the retry then rebases onto the new `main` *while
    /// holding the lock*, so it cannot be overtaken again and busy drives do
    /// not livelock.
    fn publish(&self) -> Result<Published, DriveError> {
        let mut rebases = 0;
        for attempt in 1..=MAX_PUBLISH_ATTEMPTS {
            let _g = DriveLock::acquire(&self.drive)?;
            if attempt > 1 {
                tracing::debug!(
                    target: "harness::memory",
                    session = %self.session,
                    attempt,
                    "stale memory write, rebasing onto main"
                );
                self.rebase_onto_main()?;
                rebases += 1;
            }
            if let Some(commit) = self.try_publish_locked()? {
                return Ok(Published {
                    commit,
                    attempts: attempt,
                    rebases,
                });
            }
        }
        Err(DriveError::StaleWrite {
            attempts: MAX_PUBLISH_ATTEMPTS,
        })
    }

    /// One publish attempt; the caller holds the drive lock. `Ok(None)`
    /// means stale: `main` is not an ancestor of the session branch.
    fn try_publish_locked(&self) -> Result<Option<String>, DriveError> {
        let root = self.drive.git();
        if !root.succeeds(&["merge-base", "--is-ancestor", MAIN_BRANCH, &self.branch])? {
            return Ok(None);
        }
        root.run(&["merge", "--quiet", "--ff-only", &self.branch])?;
        root.rev_parse(MAIN_BRANCH).map(Some)
    }

    fn rebase_onto_main(&self) -> Result<(), DriveError> {
        let git = self.git();
        let out = git.output(&["rebase", "--quiet", MAIN_BRANCH])?;
        if out.status.success() {
            return Ok(());
        }
        // Union merges should never conflict, but never leave a half-done
        // rebase behind.
        let _ = git.output(&["rebase", "--abort"]);
        Err(DriveError::Git {
            args: format!("rebase {MAIN_BRANCH}"),
            stderr: String::from_utf8_lossy(&out.stderr).trim().to_owned(),
        })
    }

    /// Removes the worktree and its branch. Published commits stay on `main`.
    pub fn close(self) -> Result<(), DriveError> {
        let _g = DriveLock::acquire(&self.drive)?;
        let root = self.drive.git();
        let p = self.path.to_string_lossy().into_owned();
        root.run(&["worktree", "remove", "--force", &p])?;
        root.run(&["branch", "-D", "--quiet", &self.branch])?;
        Ok(())
    }
}

/// Cross-process exclusive lock on the drive (`.harness/drive.lock`).
#[derive(Debug)]
struct DriveLock {
    path: PathBuf,
}

impl DriveLock {
    fn acquire(drive: &Drive) -> Result<Self, DriveError> {
        let dir = drive.root().join(INTERNAL_DIR);
        fs::create_dir_all(&dir).map_err(io_err("creating the internal directory"))?;
        let path = dir.join(LOCK_FILE);
        let start = Instant::now();
        let mut backoff = Duration::from_millis(2);
        loop {
            match OpenOptions::new().write(true).create_new(true).open(&path) {
                Ok(mut f) => {
                    let _ = writeln!(f, "{}", std::process::id());
                    return Ok(Self { path });
                }
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                    if lock_age(&path).is_some_and(|a| a > STALE_LOCK_AGE) {
                        tracing::warn!(target: "harness::memory", "breaking abandoned memory lock {}", path.display());
                        let _ = fs::remove_file(&path);
                        continue;
                    }
                    if start.elapsed() > LOCK_TIMEOUT {
                        return Err(DriveError::LockTimeout(path));
                    }
                    std::thread::sleep(backoff);
                    backoff = (backoff * 2).min(Duration::from_millis(50));
                }
                Err(e) => return Err(io_err(format!("locking {}", path.display()))(e)),
            }
        }
    }
}

fn lock_age(path: &Path) -> Option<Duration> {
    let m = fs::metadata(path).ok()?.modified().ok()?;
    SystemTime::now().duration_since(m).ok()
}

impl Drop for DriveLock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}
