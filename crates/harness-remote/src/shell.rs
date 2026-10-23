//! POSIX shell access and the degraded shell fallback.
//!
//! [`RemoteShell`] runs a `sh` script on a host ([`SshShell`] over ssh,
//! [`LocalShell`] locally for tests). The installer ([`crate::install`])
//! uses it to detect the architecture and upload the helper.
//!
//! Where uploading the helper is not allowed, [`ShellWorkspace`] and
//! [`ShellExec`] offer the basic operations with plain POSIX tools (`cat`,
//! `stat`, `mkdir`, `mv`, `chmod`, `kill`). The fallback is degraded:
//! - `glob` is unsupported;
//! - a missing program is reported as exit code 127, not a spawn error;
//! - a disconnect cannot be reattached, so the step fails;
//! - `stat`/`list` need GNU or BSD `stat`.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use async_trait::async_trait;
use harness_tools::exec::{
    Exec, ExecError, ExecHandle, ExecRequest, ExitOutcome, OutputChunk, StreamKind,
};
use harness_tools::local::LocalExec;
use harness_tools::workspace::{DirEntry, FileKind, FileStat, Workspace, WsError, WsResult};
use tokio::sync::{mpsc, oneshot};

use crate::ssh::{SshTarget, shell_quote};

/// Output of a finished script.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellOutput {
    /// Exit code.
    pub code: Option<i32>,
    /// stdout bytes.
    pub stdout: Vec<u8>,
    /// stderr text.
    pub stderr: String,
}

impl ShellOutput {
    /// Exit code 0.
    pub fn success(&self) -> bool {
        self.code == Some(0)
    }

    /// stdout as trimmed text.
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.stdout).trim().to_owned()
    }
}

/// Runs POSIX `sh` scripts on one host.
#[async_trait]
pub trait RemoteShell: Send + Sync {
    /// Run `script` with `stdin` and wait for it.
    async fn run(&self, script: &str, stdin: &[u8]) -> io::Result<ShellOutput>;
    /// A local command (program, args) that runs `script` on the host with
    /// streaming output.
    fn command_for(&self, script: &str) -> (String, Vec<String>);
    /// Host name for messages.
    fn host(&self) -> String;
}

/// Runs scripts locally with `sh -c` (tests, or a local "remote").
#[derive(Debug, Clone, Default)]
pub struct LocalShell {
    /// Extra environment (e.g. a fake `HOME`).
    pub env: Vec<(String, String)>,
}

#[async_trait]
impl RemoteShell for LocalShell {
    async fn run(&self, script: &str, stdin: &[u8]) -> io::Result<ShellOutput> {
        use tokio::io::AsyncWriteExt;
        let mut child = tokio::process::Command::new("sh")
            .args(["-c", script])
            .envs(self.env.iter().cloned())
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()?;
        let mut si = child.stdin.take();
        let data = stdin.to_vec();
        let writer = tokio::spawn(async move {
            if let Some(si) = si.as_mut() {
                let _ = si.write_all(&data).await;
            }
            drop(si);
        });
        let out = child.wait_with_output().await?;
        let _ = writer.await;
        Ok(ShellOutput {
            code: out.status.code(),
            stdout: out.stdout,
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        })
    }

    fn command_for(&self, script: &str) -> (String, Vec<String>) {
        let mut args = Vec::new();
        if !self.env.is_empty() {
            // env K=V ... sh -c script
            for (k, v) in &self.env {
                args.push(format!("{k}={v}"));
            }
            args.extend(["sh".into(), "-c".into(), script.into()]);
            return ("env".into(), args);
        }
        ("sh".into(), vec!["-c".into(), script.into()])
    }

    fn host(&self) -> String {
        "local".into()
    }
}

/// Runs scripts on a host over ssh (always through `sh -c`, whatever the
/// login shell is).
#[derive(Debug, Clone)]
pub struct SshShell {
    /// Destination.
    pub target: SshTarget,
}

#[async_trait]
impl RemoteShell for SshShell {
    async fn run(&self, script: &str, stdin: &[u8]) -> io::Result<ShellOutput> {
        let out = self
            .target
            .run(&["sh".into(), "-c".into(), script.into()], stdin)
            .await
            .map_err(|e| io::Error::other(e.to_string()))?;
        Ok(ShellOutput {
            code: out.code,
            stdout: out.stdout,
            stderr: out.stderr,
        })
    }

    fn command_for(&self, script: &str) -> (String, Vec<String>) {
        (
            self.target.options.program.clone(),
            self.target
                .command_args(&["sh".into(), "-c".into(), script.into()]),
        )
    }

    fn host(&self) -> String {
        self.target.host.clone()
    }
}

/// Wraps a [`RemoteShell`] so every script it runs (installs, uploads,
/// git bundle sync, shell-fallback file operations) produces an
/// [`AuditEvent`](crate::audit::AuditEvent) with op `shell` and the
/// script's first line as target.
pub struct AuditedShell<S> {
    inner: S,
    host: String,
    audit: Arc<dyn crate::audit::AuditSink>,
}

impl<S: RemoteShell> AuditedShell<S> {
    /// Audit `inner`'s scripts as host `host`.
    pub fn new(inner: S, host: impl Into<String>, audit: Arc<dyn crate::audit::AuditSink>) -> Self {
        Self {
            inner,
            host: host.into(),
            audit,
        }
    }
}

fn audit_target(script: &str) -> String {
    let first = script.lines().next().unwrap_or("").trim();
    first.chars().take(120).collect()
}

#[async_trait]
impl<S: RemoteShell> RemoteShell for AuditedShell<S> {
    async fn run(&self, script: &str, stdin: &[u8]) -> io::Result<ShellOutput> {
        let started = std::time::Instant::now();
        let at = SystemTime::now();
        let r = self.inner.run(script, stdin).await;
        let error = match &r {
            Ok(o) if o.success() => None,
            Ok(o) => Some(format!("exit {:?}: {}", o.code, o.stderr.trim())),
            Err(e) => Some(e.to_string()),
        };
        self.audit.record(&crate::audit::AuditEvent {
            host: self.host.clone(),
            op: "shell".into(),
            target: Some(audit_target(script)),
            ok: error.is_none(),
            error,
            duration: started.elapsed(),
            at,
            bytes_saved: 0,
        });
        r
    }

    fn command_for(&self, script: &str) -> (String, Vec<String>) {
        self.audit.record(&crate::audit::AuditEvent {
            host: self.host.clone(),
            op: "shell.exec".into(),
            target: Some(audit_target(script)),
            ok: true,
            error: None,
            duration: Duration::ZERO,
            at: SystemTime::now(),
            bytes_saved: 0,
        });
        self.inner.command_for(script)
    }

    fn host(&self) -> String {
        self.inner.host()
    }
}

/// Exit codes used by the fallback scripts.
mod exit {
    pub const NOT_FOUND: i32 = 2;
    pub const IS_DIR: i32 = 3;
    pub const NOT_DIR: i32 = 4;
    pub const DENIED: i32 = 5;
}

fn classify(path: &Path, out: &ShellOutput) -> WsError {
    let path = path.to_path_buf();
    match out.code {
        Some(exit::NOT_FOUND) => WsError::NotFound(path),
        Some(exit::IS_DIR) => WsError::IsDirectory(path),
        Some(exit::NOT_DIR) => WsError::NotADirectory(path),
        Some(exit::DENIED) => WsError::PermissionDenied(path),
        _ => WsError::Io {
            path,
            source: io::Error::other(format!(
                "shell fallback failed (exit {:?}): {}",
                out.code,
                out.stderr.trim()
            )),
        },
    }
}

/// A degraded [`Workspace`] built from POSIX shell commands.
pub struct ShellWorkspace {
    shell: Arc<dyn RemoteShell>,
    root: PathBuf,
}

impl std::fmt::Debug for ShellWorkspace {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ShellWorkspace")
            .field("host", &self.shell.host())
            .field("root", &self.root)
            .finish()
    }
}

impl ShellWorkspace {
    /// A workspace rooted at the absolute remote path `root`.
    pub fn new(shell: Arc<dyn RemoteShell>, root: impl Into<PathBuf>) -> Self {
        Self {
            shell,
            root: root.into(),
        }
    }

    async fn script(&self, path: &Path, body: &str, stdin: &[u8]) -> WsResult<ShellOutput> {
        let script = format!("p={}\n{body}", shell_quote(&path.to_string_lossy()));
        let out = self
            .shell
            .run(&script, stdin)
            .await
            .map_err(|e| WsError::from_io(path, e))?;
        if out.success() {
            Ok(out)
        } else {
            Err(classify(path, &out))
        }
    }
}

const STAT_FMT: &str =
    r#"s=$(stat -L -c '%s %a %Y' -- "$p" 2>/dev/null || stat -L -f '%z %Lp %m' -- "$p") || exit 1"#;

#[async_trait]
impl Workspace for ShellWorkspace {
    fn root(&self) -> &Path {
        &self.root
    }

    async fn read(&self, path: &Path) -> WsResult<Vec<u8>> {
        let p = self.resolve(path);
        let body = r#"[ -e "$p" ] || exit 2; [ -d "$p" ] && exit 3; [ -r "$p" ] || exit 5; exec cat -- "$p""#;
        Ok(self.script(&p, body, b"").await?.stdout)
    }

    async fn write(&self, path: &Path, data: &[u8]) -> WsResult<()> {
        let p = self.resolve(path);
        let body =
            r#"mkdir -p -- "$(dirname -- "$p")" || exit 1; [ -d "$p" ] && exit 3; cat > "$p""#;
        self.script(&p, body, data).await.map(|_| ())
    }

    async fn replace_atomic(&self, path: &Path, data: &[u8]) -> WsResult<()> {
        let p = self.resolve(path);
        let body = r#"[ -d "$p" ] && exit 3
[ -L "$p" ] && p=$(readlink -f -- "$p")
d=$(dirname -- "$p"); mkdir -p -- "$d" || exit 1
t="$d/.harness-tmp.$$"
cat > "$t" || { rm -f -- "$t"; exit 1; }
if [ -e "$p" ]; then m=$(stat -L -c %a -- "$p" 2>/dev/null || stat -L -f %Lp -- "$p"); chmod "$m" "$t"; else chmod 644 "$t"; fi
mv -f -- "$t" "$p""#;
        self.script(&p, body, data).await.map(|_| ())
    }

    async fn set_mode(&self, path: &Path, mode: u32) -> WsResult<()> {
        let p = self.resolve(path);
        let body = format!(
            r#"[ -e "$p" ] || exit 2; chmod -- {:o} "$p""#,
            mode & 0o7777
        );
        self.script(&p, &body, b"").await.map(|_| ())
    }

    async fn stat(&self, path: &Path) -> WsResult<FileStat> {
        let p = self.resolve(path);
        let body = format!(
            r#"[ -e "$p" ] || exit 2
if [ -d "$p" ]; then k=dir; elif [ -f "$p" ]; then k=file; else k=other; fi
{STAT_FMT}
echo "$k $s""#
        );
        let out = self.script(&p, &body, b"").await?;
        let text = out.text();
        let f: Vec<&str> = text.split_whitespace().collect();
        let bad = || WsError::Io {
            path: p.clone(),
            source: io::Error::other(format!("unexpected stat output {text:?}")),
        };
        if f.len() < 4 {
            return Err(bad());
        }
        Ok(FileStat {
            kind: match f[0] {
                "dir" => FileKind::Dir,
                "file" => FileKind::File,
                _ => FileKind::Other,
            },
            len: f[1].parse().map_err(|_| bad())?,
            mode: u32::from_str_radix(f[2], 8).ok(),
            modified: f[3]
                .parse::<u64>()
                .ok()
                .map(|s| SystemTime::UNIX_EPOCH + Duration::from_secs(s)),
        })
    }

    async fn list(&self, path: &Path) -> WsResult<Vec<DirEntry>> {
        let p = self.resolve(path);
        let body = r#"[ -e "$p" ] || exit 2; [ -d "$p" ] || exit 4; cd -- "$p" || exit 5
for f in * .[!.]* ..?*; do
  [ -e "$f" ] || [ -L "$f" ] || continue
  if [ -L "$f" ]; then k=symlink; elif [ -d "$f" ]; then k=dir; elif [ -f "$f" ]; then k=file; else k=other; fi
  s=$(stat -c %s -- "$f" 2>/dev/null || stat -f %z -- "$f")
  printf '%s %s %s\n' "$k" "$s" "$f"
done"#;
        let out = self.script(&p, body, b"").await?;
        let mut entries: Vec<DirEntry> = String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter_map(|l| {
                let mut it = l.splitn(3, ' ');
                let kind = match it.next()? {
                    "dir" => FileKind::Dir,
                    "file" => FileKind::File,
                    "symlink" => FileKind::Symlink,
                    _ => FileKind::Other,
                };
                let len = it.next()?.parse().ok()?;
                Some(DirEntry {
                    name: it.next()?.to_owned(),
                    kind,
                    len,
                })
            })
            .collect();
        entries.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(entries)
    }

    async fn glob(&self, pattern: &str) -> WsResult<Vec<PathBuf>> {
        Err(WsError::Io {
            path: PathBuf::from(pattern),
            source: io::Error::new(
                io::ErrorKind::Unsupported,
                "glob is not available in the shell fallback; use bash `find` instead",
            ),
        })
    }
}

/// Prefix of the stderr line carrying the remote process group id.
const PGID_MARK: &str = "__HARNESS_PGID__:";

/// A degraded [`Exec`] that runs commands through a [`RemoteShell`].
///
/// The remote command reports its process group first, so kill, timeout
/// and drop terminate the whole remote group (TERM, then KILL after 2 s).
pub struct ShellExec {
    shell: Arc<dyn RemoteShell>,
    local: LocalExec,
}

impl std::fmt::Debug for ShellExec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ShellExec")
            .field("host", &self.shell.host())
            .finish()
    }
}

impl ShellExec {
    /// Run processes on `shell`'s host.
    pub fn new(shell: Arc<dyn RemoteShell>) -> Self {
        Self {
            shell,
            // Sampling the local ssh process says nothing about the remote
            // command, so the shell fallback reports no liveness.
            local: LocalExec::new().with_liveness_interval(None),
        }
    }

    /// The remote script for `req`.
    pub fn script(req: &ExecRequest) -> String {
        let mut s = format!(
            "cd -- {} || exit 127\npg=$(ps -o pgid= -p $$ 2>/dev/null | tr -d ' ')\nprintf '%s%s\\n' '{PGID_MARK}' \"${{pg:-$$}}\" >&2\n",
            shell_quote(&req.cwd.to_string_lossy())
        );
        let assigns: Vec<String> = req
            .env
            .iter()
            .filter(|(k, _)| {
                !k.is_empty() && k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
            })
            .map(|(k, v)| format!("{k}={}", shell_quote(v)))
            .collect();
        let argv: Vec<String> = std::iter::once(req.program.clone())
            .chain(req.args.iter().cloned())
            .map(|a| shell_quote(&a))
            .collect();
        if req.clear_env {
            s.push_str("exec env -i HOME=\"$HOME\" PATH=\"$PATH\" ");
            for a in &assigns {
                s.push_str(a);
                s.push(' ');
            }
        } else {
            for a in &assigns {
                s.push_str("export ");
                s.push_str(a);
                s.push('\n');
            }
            s.push_str("exec ");
        }
        s.push_str(&argv.join(" "));
        s.push('\n');
        s
    }
}

/// A remote `sh` script that stops process group `pgid`: TERM, up to 2 s of
/// grace, then KILL.
///
/// The script uses the shell's builtin `kill` in the `kill -SIG -PGID` form,
/// because that is the only spelling every remote `sh` accepts: dash (Debian,
/// Ubuntu) and busybox ash (Alpine) reject `--` ("Illegal number: -",
/// "invalid number '--'"), and dash also rejects `-s SIG -PGID`. The procps-ng
/// misparse of `-169` as `-1` is in `/bin/kill`, not in shell builtins; the
/// `pgid <= 1` guard covers the dangerous targets anyway.
fn kill_group_script(pgid: u32) -> String {
    // -0 is the killing shell's own group and -1 every process of the user.
    if pgid <= 1 {
        return "true".into();
    }
    format!(
        "kill -TERM -{pgid} 2>/dev/null; i=0; while kill -0 -{pgid} 2>/dev/null && [ $i -lt 20 ]; do sleep 0.1; i=$((i+1)); done; kill -KILL -{pgid} 2>/dev/null; true"
    )
}

#[async_trait]
impl Exec for ShellExec {
    async fn spawn(&self, req: ExecRequest) -> Result<ExecHandle, ExecError> {
        let timeout = req.timeout;
        let (program, args) = self.shell.command_for(&Self::script(&req));
        let local = ExecRequest::new(program, args, std::env::temp_dir());
        let mut inner = self.local.spawn(local).await?;
        let (otx, orx) = mpsc::unbounded_channel();
        let (ktx, mut krx) = oneshot::channel::<()>();
        let (stx, srx) = oneshot::channel();
        let shell = self.shell.clone();
        tokio::spawn(async move {
            let mut pgid: Option<u32> = None;
            let mut pending_err: Vec<u8> = Vec::new();
            let mut marker_done = false;
            let (mut killed, mut timed_out) = (false, false);
            let sleep = tokio::time::sleep(timeout.unwrap_or(Duration::from_secs(u64::MAX / 4)));
            tokio::pin!(sleep);
            loop {
                tokio::select! {
                    chunk = inner.next_output() => {
                        let Some(c) = chunk else { break };
                        if c.stream == StreamKind::Stderr && !marker_done {
                            pending_err.extend_from_slice(&c.data);
                            if let Some(pos) = pending_err.iter().position(|&b| b == b'\n') {
                                let line = String::from_utf8_lossy(&pending_err[..pos]).into_owned();
                                let rest = pending_err.split_off(pos + 1);
                                match line.strip_prefix(PGID_MARK) {
                                    Some(id) => pgid = id.trim().parse().ok(),
                                    None => { let _ = otx.send(OutputChunk { stream: StreamKind::Stderr, data: format!("{line}\n").into_bytes() }); }
                                }
                                marker_done = true;
                                if !rest.is_empty() {
                                    let _ = otx.send(OutputChunk { stream: StreamKind::Stderr, data: rest });
                                }
                                pending_err.clear();
                            }
                            continue;
                        }
                        let _ = otx.send(c);
                    }
                    _ = &mut krx, if !killed && !timed_out => {
                        killed = true;
                        if let Some(pg) = pgid { let _ = shell.run(&kill_group_script(pg), b"").await; }
                        inner.kill();
                    }
                    _ = &mut sleep, if timeout.is_some() && !killed && !timed_out => {
                        timed_out = true;
                        if let Some(pg) = pgid { let _ = shell.run(&kill_group_script(pg), b"").await; }
                        inner.kill();
                    }
                }
            }
            if !pending_err.is_empty() {
                let _ = otx.send(OutputChunk {
                    stream: StreamKind::Stderr,
                    data: pending_err,
                });
            }
            drop(otx);
            let mut outcome = inner.wait().await.unwrap_or_default();
            // inner.kill() makes the local side report `killed`; what the
            // caller sees is why *we* stopped it.
            if killed || timed_out {
                outcome.killed = killed;
                outcome.timed_out = timed_out;
            }
            if killed || timed_out {
                outcome.code = None;
            }
            let _ = stx.send(ExitOutcome { ..outcome });
        });
        Ok(ExecHandle::new(orx, ktx, srx))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ws(dir: &Path) -> ShellWorkspace {
        ShellWorkspace::new(Arc::new(LocalShell::default()), dir)
    }

    #[test]
    fn kill_script_never_targets_group_0_or_1() {
        assert_eq!(kill_group_script(0), "true");
        assert_eq!(kill_group_script(1), "true");
        let s = kill_group_script(169);
        assert!(s.starts_with("kill -TERM -169 "), "{s}");
        assert!(s.contains("kill -0 -169 "), "{s}");
        assert!(s.contains("kill -KILL -169 "), "{s}");
        assert!(!s.contains("--"), "dash and busybox reject `kill --`: {s}");
    }

    /// Runs the kill script through `sh` against a group that nothing else
    /// owns, so a script that silently does nothing cannot pass. On hosts
    /// where `/bin/sh` is dash (Debian, Ubuntu) this is the shell a remote
    /// `ShellExec` kill really uses.
    #[cfg(unix)]
    #[tokio::test]
    async fn kill_script_stops_a_foreign_group_under_sh() {
        let mut cmd = tokio::process::Command::new("sh");
        cmd.args(["-c", "sleep 30 & sleep 30 & wait"])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .process_group(0)
            .kill_on_drop(true);
        let mut child = cmd.spawn().unwrap();
        let pg = child.id().unwrap();
        tokio::time::sleep(Duration::from_millis(200)).await;
        let out = LocalShell::default()
            .run(&kill_group_script(pg), b"")
            .await
            .unwrap();
        assert_eq!(out.code, Some(0));
        let st = tokio::time::timeout(Duration::from_secs(5), child.wait())
            .await
            .expect("group leader still running after the kill script")
            .unwrap();
        assert!(!st.success(), "{st:?}");
        // Orphaned members may be zombies for a moment until init reaps them.
        let mut gone = false;
        for _ in 0..50 {
            let probe = std::process::Command::new("sh")
                .args(["-c", &format!("kill -0 -{pg} 2>/dev/null")])
                .status()
                .unwrap();
            if !probe.success() {
                gone = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        assert!(gone, "group {pg} survived the kill script");
    }

    #[tokio::test]
    async fn basic_file_ops() {
        let t = tempfile::tempdir().unwrap();
        let w = ws(t.path());
        w.write(Path::new("a/b.txt"), b"hello\n").await.unwrap();
        assert_eq!(w.read(Path::new("a/b.txt")).await.unwrap(), b"hello\n");
        assert!(matches!(
            w.read(Path::new("nope")).await.unwrap_err(),
            WsError::NotFound(_)
        ));
        assert!(matches!(
            w.read(Path::new("a")).await.unwrap_err(),
            WsError::IsDirectory(_)
        ));
        w.replace_atomic(Path::new("a/r.sh"), b"v1").await.unwrap();
        w.set_mode(Path::new("a/r.sh"), 0o751).await.unwrap();
        w.replace_atomic(Path::new("a/r.sh"), b"v2").await.unwrap();
        let st = w.stat(Path::new("a/r.sh")).await.unwrap();
        assert_eq!(st.kind, FileKind::File);
        assert_eq!(st.len, 2);
        assert_eq!(st.mode, Some(0o751), "mode kept by replace");
        assert!(st.modified.is_some());
        assert_eq!(w.stat(Path::new("a")).await.unwrap().kind, FileKind::Dir);
        let names: Vec<_> = w
            .list(Path::new("a"))
            .await
            .unwrap()
            .into_iter()
            .map(|e| (e.name, e.kind))
            .collect();
        assert_eq!(
            names,
            vec![
                ("b.txt".into(), FileKind::File),
                ("r.sh".into(), FileKind::File)
            ]
        );
        assert!(matches!(
            w.list(Path::new("a/b.txt")).await.unwrap_err(),
            WsError::NotADirectory(_)
        ));
        assert!(w.glob("**").await.is_err());
    }

    #[tokio::test]
    async fn exec_output_status_cwd_env() {
        let t = tempfile::tempdir().unwrap();
        let ex = ShellExec::new(Arc::new(LocalShell::default()));
        let req = ExecRequest::new(
            "sh",
            vec![
                "-c".into(),
                "pwd -P; echo \"$FOO\"; echo err >&2; exit 3".into(),
            ],
            t.path(),
        )
        .with_clean_env(vec![("FOO".into(), "it's".into())]);
        let h = ex.spawn(req).await.unwrap();
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let st = h
            .drive(|c| match c.stream {
                StreamKind::Stdout => out.extend_from_slice(&c.data),
                StreamKind::Stderr => err.extend_from_slice(&c.data),
            })
            .await
            .unwrap();
        let want = std::fs::canonicalize(t.path()).unwrap();
        assert_eq!(
            String::from_utf8_lossy(&out),
            format!("{}\nit's\n", want.display())
        );
        assert_eq!(
            String::from_utf8_lossy(&err),
            "err\n",
            "pgid marker stripped"
        );
        assert_eq!(st.code, Some(3));
    }

    #[tokio::test]
    async fn exec_kill_and_timeout_kill_the_remote_group() {
        let t = tempfile::tempdir().unwrap();
        let ex = ShellExec::new(Arc::new(LocalShell::default()));
        for use_timeout in [false, true] {
            let mut req = ExecRequest::new(
                "sh",
                vec!["-c".into(), "sleep 30 & echo $!; wait".into()],
                t.path(),
            );
            if use_timeout {
                req = req.with_timeout(Duration::from_millis(500));
            }
            let mut h = ex.spawn(req).await.unwrap();
            let c = h.next_output().await.unwrap();
            let pid: u32 = String::from_utf8_lossy(&c.data).trim().parse().unwrap();
            if !use_timeout {
                h.kill();
            }
            let st = tokio::time::timeout(Duration::from_secs(10), h.drive(|_| {}))
                .await
                .unwrap()
                .unwrap();
            assert_eq!(st.timed_out, use_timeout, "{st:?}");
            assert_eq!(st.killed, !use_timeout, "{st:?}");
            let mut dead = false;
            for _ in 0..50 {
                if harness_tools::conformance::local_process_gone(pid) {
                    dead = true;
                    break;
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            assert!(dead, "grandchild {pid} survived");
        }
    }

    #[tokio::test]
    async fn audited_shell_records_every_script() {
        let audit = Arc::new(crate::audit::RecordingAudit::default());
        let sh = Arc::new(AuditedShell::new(
            LocalShell::default(),
            "box",
            audit.clone(),
        ));
        let t = tempfile::tempdir().unwrap();
        let w = ShellWorkspace::new(sh.clone(), t.path());
        w.write(Path::new("f"), b"x").await.unwrap();
        let _ = w.read(Path::new("missing")).await;
        let ex = ShellExec::new(sh);
        ex.spawn(ExecRequest::new("true", vec![], t.path()))
            .await
            .unwrap()
            .wait()
            .await
            .unwrap();
        let ev = audit.events();
        let ops: Vec<_> = ev.iter().map(|e| (e.op.as_str(), e.ok)).collect();
        assert_eq!(
            ops,
            vec![("shell", true), ("shell", false), ("shell.exec", true)]
        );
        assert!(ev.iter().all(|e| e.host == "box"));
    }

    #[test]
    fn script_quotes_everything() {
        let req =
            ExecRequest::new("echo", vec!["$(rm -rf /)".into()], "/tmp/a b").with_clean_env(vec![
                ("X".into(), "$HOME".into()),
                ("BAD NAME".into(), "v".into()),
            ]);
        let s = ShellExec::script(&req);
        assert!(s.contains("cd -- '/tmp/a b'"));
        assert!(s.contains("X='$HOME'"));
        assert!(s.contains("echo '$(rm -rf /)'"));
        assert!(!s.contains("BAD NAME"));
    }
}
