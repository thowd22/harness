//! Local implementations of [`Workspace`] and [`Exec`].

use std::io::Write as _;
use std::os::unix::fs::PermissionsExt as _;
use std::os::unix::process::ExitStatusExt as _;
use std::path::{Path, PathBuf};
use std::process::{ExitStatus, Stdio};
use std::time::Duration;

use async_trait::async_trait;
use nix::sys::signal::{Signal, killpg};
use nix::unistd::Pid;
use tokio::io::{AsyncRead, AsyncReadExt as _};
use tokio::process::{Child, Command};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;

use crate::exec::{Exec, ExecError, ExecHandle, ExecRequest, ExitOutcome, OutputChunk, StreamKind};
use crate::workspace::{DirEntry, FileKind, FileStat, Workspace, WsError, WsResult};

/// Permission bits for files created by [`Workspace::replace_atomic`] when no
/// file existed before.
pub const NEW_FILE_MODE: u32 = 0o644;

/// How long a process group gets between SIGTERM and SIGKILL.
pub const KILL_GRACE: Duration = Duration::from_secs(2);

/// How long output readers may keep draining after the process exits and
/// its group has been killed (covers daemons that escaped the group).
const READER_GRACE: Duration = Duration::from_millis(500);

/// [`Workspace`] backed by the local filesystem.
#[derive(Debug, Clone)]
pub struct LocalWorkspace {
    root: PathBuf,
}

impl LocalWorkspace {
    /// A workspace rooted at `root` (typically the project directory).
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }
}

fn kind_of(ft: std::fs::FileType) -> FileKind {
    if ft.is_symlink() {
        FileKind::Symlink
    } else if ft.is_dir() {
        FileKind::Dir
    } else if ft.is_file() {
        FileKind::File
    } else {
        FileKind::Other
    }
}

fn replace_atomic_blocking(path: &Path, data: &[u8]) -> WsResult<()> {
    let io_err = |e| WsError::from_io(path, e);
    // Follow symlinks so the link survives and its target is replaced.
    let target = match std::fs::canonicalize(path) {
        Ok(p) => p,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => path.to_path_buf(),
        Err(e) => return Err(io_err(e)),
    };
    let existing = match std::fs::metadata(&target) {
        Ok(m) if m.is_dir() => return Err(WsError::IsDirectory(path.to_path_buf())),
        Ok(m) => Some(m.permissions()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(io_err(e)),
    };
    let parent = target
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    std::fs::create_dir_all(parent).map_err(io_err)?;
    let mut tmp = tempfile::Builder::new()
        .prefix(".harness-tmp-")
        .tempfile_in(parent)
        .map_err(io_err)?;
    tmp.write_all(data).map_err(io_err)?;
    let perms = existing.unwrap_or_else(|| std::fs::Permissions::from_mode(NEW_FILE_MODE));
    tmp.as_file().set_permissions(perms).map_err(io_err)?;
    tmp.as_file().sync_all().map_err(io_err)?;
    tmp.persist(&target).map_err(|e| io_err(e.error))?;
    Ok(())
}

fn glob_blocking(root: &Path, pattern: &str) -> WsResult<Vec<PathBuf>> {
    let glob = globset::GlobBuilder::new(pattern)
        .literal_separator(true)
        .build()
        .map_err(|e| WsError::InvalidPattern {
            pattern: pattern.to_string(),
            reason: e.kind().to_string(),
        })?
        .compile_matcher();
    let walker = ignore::WalkBuilder::new(root)
        .hidden(false)
        .require_git(false)
        .filter_entry(|e| e.file_name() != ".git")
        .build();
    let mut out = Vec::new();
    for entry in walker {
        let entry = match entry {
            Ok(e) => e,
            Err(err) => {
                tracing::debug!(%err, "glob: skipping unreadable entry");
                continue;
            }
        };
        if !entry.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        if let Ok(rel) = entry.path().strip_prefix(root)
            && glob.is_match(rel)
        {
            out.push(rel.to_path_buf());
        }
    }
    out.sort();
    Ok(out)
}

#[async_trait]
impl Workspace for LocalWorkspace {
    fn root(&self) -> &Path {
        &self.root
    }

    async fn read(&self, path: &Path) -> WsResult<Vec<u8>> {
        let p = self.resolve(path);
        tokio::fs::read(&p)
            .await
            .map_err(|e| WsError::from_io(&p, e))
    }

    async fn write(&self, path: &Path, data: &[u8]) -> WsResult<()> {
        let p = self.resolve(path);
        if let Some(parent) = p.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|e| WsError::from_io(parent, e))?;
        }
        tokio::fs::write(&p, data)
            .await
            .map_err(|e| WsError::from_io(&p, e))
    }

    async fn replace_atomic(&self, path: &Path, data: &[u8]) -> WsResult<()> {
        let p = self.resolve(path);
        let data = data.to_vec();
        tokio::task::spawn_blocking(move || replace_atomic_blocking(&p, &data))
            .await
            .map_err(|e| WsError::Io {
                path: path.to_path_buf(),
                source: std::io::Error::other(e),
            })?
    }

    async fn set_mode(&self, path: &Path, mode: u32) -> WsResult<()> {
        let p = self.resolve(path);
        tokio::fs::set_permissions(&p, std::fs::Permissions::from_mode(mode))
            .await
            .map_err(|e| WsError::from_io(&p, e))
    }

    async fn stat(&self, path: &Path) -> WsResult<FileStat> {
        let p = self.resolve(path);
        let m = tokio::fs::metadata(&p)
            .await
            .map_err(|e| WsError::from_io(&p, e))?;
        Ok(FileStat {
            kind: kind_of(m.file_type()),
            len: m.len(),
            mode: Some(m.permissions().mode() & 0o7777),
            modified: m.modified().ok(),
        })
    }

    async fn list(&self, path: &Path) -> WsResult<Vec<DirEntry>> {
        let p = self.resolve(path);
        let mut rd = tokio::fs::read_dir(&p)
            .await
            .map_err(|e| WsError::from_io(&p, e))?;
        let mut out = Vec::new();
        while let Some(entry) = rd.next_entry().await.map_err(|e| WsError::from_io(&p, e))? {
            let meta = entry
                .metadata()
                .await
                .map_err(|e| WsError::from_io(entry.path(), e))?;
            out.push(DirEntry {
                name: entry.file_name().to_string_lossy().into_owned(),
                kind: kind_of(meta.file_type()),
                len: meta.len(),
            });
        }
        out.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(out)
    }

    async fn glob(&self, pattern: &str) -> WsResult<Vec<PathBuf>> {
        let root = self.root.clone();
        let pattern = pattern.to_string();
        let p2 = pattern.clone();
        tokio::task::spawn_blocking(move || glob_blocking(&root, &pattern))
            .await
            .map_err(|e| WsError::Io {
                path: PathBuf::from(p2),
                source: std::io::Error::other(e),
            })?
    }
}

/// [`Exec`] that runs processes on this machine.
///
/// Each process leads a new session and process group (`setsid`), with
/// stdin attached to `/dev/null` and no controlling terminal. On timeout, kill or drop of the handle the
/// group receives SIGTERM, then SIGKILL after [`KILL_GRACE`]. When the leader
/// exits on its own, any processes left in its group are killed too, so
/// stray background jobs cannot outlive a tool call.
#[derive(Debug, Clone, Default)]
pub struct LocalExec;

impl LocalExec {
    /// A local executor.
    pub fn new() -> Self {
        Self
    }
}

fn signal_group(pgid: i32, sig: Signal) {
    // ESRCH just means the group is already gone.
    let _ = killpg(Pid::from_raw(pgid), sig);
}

/// Starts the child in a new session (`setsid`), which also makes it the
/// leader of a new process group, so its pid is the group id to signal.
///
/// A plain `setpgid` group would still belong to the TUI's session and be a
/// *background* group of the user's terminal: anything that opens
/// `/dev/tty` (an ssh or sudo password prompt, pinentry-tty) would be
/// stopped by SIGTTIN/SIGTTOU and hang until the timeout. In its own session
/// the child has no controlling terminal, so such an open fails at once
/// with ENXIO and the prompt errors out instead.
#[allow(unsafe_code)]
fn new_session(cmd: &mut Command) {
    // SAFETY: the closure runs in the forked child before exec and only
    // calls setsid(2), which is async-signal-safe; it allocates nothing and
    // touches no locks. (`process_group(0)` cannot be combined with it:
    // setsid fails with EPERM in a process that already leads a group.)
    unsafe {
        cmd.pre_exec(|| {
            nix::unistd::setsid()
                .map(|_| ())
                .map_err(std::io::Error::from)
        });
    }
}

async fn terminate_group(pgid: i32, child: &mut Child) -> std::io::Result<ExitStatus> {
    signal_group(pgid, Signal::SIGTERM);
    // Stopped members (SIGSTOP, job-control stops) act on TERM only once
    // they are continued.
    signal_group(pgid, Signal::SIGCONT);
    match tokio::time::timeout(KILL_GRACE, child.wait()).await {
        Ok(status) => status,
        Err(_) => {
            signal_group(pgid, Signal::SIGKILL);
            child.wait().await
        }
    }
}

fn pump(
    mut reader: impl AsyncRead + Unpin + Send + 'static,
    stream: StreamKind,
    tx: mpsc::UnboundedSender<OutputChunk>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut buf = vec![0u8; 8192];
        loop {
            match reader.read(&mut buf).await {
                Ok(0) | Err(_) => break,
                // Keep draining even if the receiver is gone, so the
                // process never blocks on a full pipe.
                Ok(n) => {
                    let _ = tx.send(OutputChunk {
                        stream,
                        data: buf[..n].to_vec(),
                    });
                }
            }
        }
    })
}

async fn supervise(
    mut child: Child,
    pgid: i32,
    timeout: Option<Duration>,
    mut kill_rx: oneshot::Receiver<()>,
    readers: Vec<JoinHandle<()>>,
    status_tx: oneshot::Sender<ExitOutcome>,
) {
    let deadline = async {
        match timeout {
            Some(d) => tokio::time::sleep(d).await,
            None => std::future::pending().await,
        }
    };
    tokio::pin!(deadline);
    let mut outcome = ExitOutcome::default();
    let status = tokio::select! {
        status = child.wait() => status,
        () = &mut deadline => {
            outcome.timed_out = true;
            terminate_group(pgid, &mut child).await
        }
        // Fires on an explicit kill and when the handle is dropped.
        _ = &mut kill_rx => {
            outcome.killed = true;
            terminate_group(pgid, &mut child).await
        }
    };
    // Leader is gone: make sure nothing else in its group survives.
    signal_group(pgid, Signal::SIGKILL);
    for mut reader in readers {
        if tokio::time::timeout(READER_GRACE, &mut reader)
            .await
            .is_err()
        {
            reader.abort();
        }
    }
    if let Ok(status) = status {
        outcome.code = status.code();
        outcome.signal = status.signal();
    }
    let _ = status_tx.send(outcome);
}

#[async_trait]
impl Exec for LocalExec {
    async fn spawn(&self, req: ExecRequest) -> Result<ExecHandle, ExecError> {
        let mut cmd = Command::new(&req.program);
        cmd.args(&req.args)
            .current_dir(&req.cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        new_session(&mut cmd);
        if req.clear_env {
            cmd.env_clear();
        }
        cmd.envs(req.env.iter().map(|(k, v)| (k, v)));
        let mut child = cmd.spawn().map_err(|source| ExecError::Spawn {
            program: req.program.clone(),
            source,
        })?;
        let pgid = child
            .id()
            .and_then(|id| i32::try_from(id).ok())
            .ok_or(ExecError::Lost)?;

        let (out_tx, out_rx) = mpsc::unbounded_channel();
        let mut readers = Vec::with_capacity(2);
        if let Some(stdout) = child.stdout.take() {
            readers.push(pump(stdout, StreamKind::Stdout, out_tx.clone()));
        }
        if let Some(stderr) = child.stderr.take() {
            readers.push(pump(stderr, StreamKind::Stderr, out_tx));
        }
        let (kill_tx, kill_rx) = oneshot::channel();
        let (status_tx, status_rx) = oneshot::channel();
        tokio::spawn(supervise(
            child,
            pgid,
            req.timeout,
            kill_rx,
            readers,
            status_tx,
        ));
        Ok(ExecHandle::new(out_rx, kill_tx, status_rx))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::conformance;

    #[tokio::test]
    async fn local_workspace_passes_conformance_suite() {
        let dir = tempfile::tempdir().unwrap();
        conformance::workspace_suite(&LocalWorkspace::new(dir.path())).await;
    }

    #[tokio::test]
    async fn local_exec_passes_conformance_suite() {
        let dir = tempfile::tempdir().unwrap();
        conformance::exec_suite(&LocalExec::new(), dir.path()).await;
    }

    async fn run(cmd: &str, timeout: Duration) -> (ExitOutcome, String) {
        let dir = tempfile::tempdir().unwrap();
        let req =
            ExecRequest::new("sh", vec!["-c".into(), cmd.into()], dir.path()).with_timeout(timeout);
        let h = LocalExec::new().spawn(req).await.unwrap();
        let mut out = Vec::new();
        let st = h.drive(|c| out.extend_from_slice(&c.data)).await.unwrap();
        (st, String::from_utf8_lossy(&out).into_owned())
    }

    /// A command that opens `/dev/tty` (a password prompt) fails at once
    /// instead of being stopped as a background job of the user's terminal.
    #[tokio::test]
    async fn tty_open_fails_fast_instead_of_stopping() {
        let started = std::time::Instant::now();
        let (st, out) = run(
            "if ( : < /dev/tty ) 2>/dev/null; then echo opened; else echo no-tty; fi",
            Duration::from_secs(20),
        )
        .await;
        assert!(!st.timed_out, "{st:?} {out}");
        assert!(out.contains("no-tty"), "{out}");
        assert!(started.elapsed() < Duration::from_secs(10));
    }

    /// The child leads its own session (so it has no controlling terminal)
    /// and its own process group (so killpg reaches its children).
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn exec_child_leads_its_own_session() {
        // /proc/<pid>/stat fields after the command name: state ppid pgrp session.
        let ids = |stat: &str| -> (String, String) {
            let f: Vec<&str> = stat
                .rsplit_once(')')
                .unwrap()
                .1
                .split_whitespace()
                .collect();
            (f[2].to_string(), f[3].to_string())
        };
        let (st, out) = run("echo $$; cat /proc/$$/stat", Duration::from_secs(20)).await;
        assert_eq!(st.code, Some(0), "{out}");
        let (pid, stat) = out.split_once('\n').unwrap();
        let (pgrp, sid) = ids(stat);
        assert_eq!(pgrp, pid.trim(), "group leader");
        assert_eq!(sid, pid.trim(), "session leader");
        let own = std::fs::read_to_string("/proc/self/stat").unwrap();
        assert_ne!(ids(&own).1, sid, "not the caller's session");
    }

    /// A stopped command still ends at the TERM of a kill, not only at the
    /// SIGKILL after the grace period.
    #[tokio::test]
    async fn kill_continues_a_stopped_group() {
        let dir = tempfile::tempdir().unwrap();
        let req = ExecRequest::new(
            "sh",
            vec!["-c".into(), "echo up; kill -STOP $$; sleep 30".into()],
            dir.path(),
        );
        let mut h = LocalExec::new().spawn(req).await.unwrap();
        h.next_output().await.unwrap();
        tokio::time::sleep(Duration::from_millis(200)).await;
        let started = std::time::Instant::now();
        h.kill();
        let st = h.drive(|_| {}).await.unwrap();
        assert!(st.killed, "{st:?}");
        assert_eq!(st.signal, Some(15), "ended by TERM: {st:?}");
        assert!(started.elapsed() < KILL_GRACE, "{:?}", started.elapsed());
    }

    #[tokio::test]
    async fn new_files_get_default_mode() {
        let dir = tempfile::tempdir().unwrap();
        let ws = LocalWorkspace::new(dir.path());
        ws.replace_atomic(Path::new("n.txt"), b"x").await.unwrap();
        let st = ws.stat(Path::new("n.txt")).await.unwrap();
        assert_eq!(st.mode, Some(NEW_FILE_MODE));
    }

    #[tokio::test]
    async fn replace_atomic_follows_symlinks() {
        let dir = tempfile::tempdir().unwrap();
        let ws = LocalWorkspace::new(dir.path());
        ws.write(Path::new("real.txt"), b"old").await.unwrap();
        std::os::unix::fs::symlink(dir.path().join("real.txt"), dir.path().join("link.txt"))
            .unwrap();
        ws.replace_atomic(Path::new("link.txt"), b"new")
            .await
            .unwrap();
        assert_eq!(ws.read(Path::new("real.txt")).await.unwrap(), b"new");
        let meta = std::fs::symlink_metadata(dir.path().join("link.txt")).unwrap();
        assert!(meta.file_type().is_symlink());
    }

    #[tokio::test]
    async fn glob_honours_gitignore() {
        let dir = tempfile::tempdir().unwrap();
        let ws = LocalWorkspace::new(dir.path());
        ws.write(Path::new(".gitignore"), b"target/\n")
            .await
            .unwrap();
        ws.write(Path::new("target/x.rs"), b"").await.unwrap();
        ws.write(Path::new("src/a.rs"), b"").await.unwrap();
        assert_eq!(
            ws.glob("**/*.rs").await.unwrap(),
            vec![PathBuf::from("src/a.rs")]
        );
    }
}
