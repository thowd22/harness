//! Shared conformance suite for [`Workspace`] and [`Exec`] implementations.
//!
//! Every implementation (local, remote over SSH, ...) runs the same checks so
//! tools behave identically wherever they execute. The functions panic with a
//! descriptive message on the first violation, so call them from a test:
//!
//! ```
//! # #[tokio::main(flavor = "current_thread")]
//! # async fn main() {
//! use harness_tools::{conformance, local::LocalWorkspace};
//! let dir = tempfile::tempdir().unwrap();
//! // A remote implementation would construct its own workspace here.
//! conformance::workspace_suite(&LocalWorkspace::new(dir.path())).await;
//! # }
//! ```
//!
//! [`workspace_suite`] expects `ws.root()` to be an empty, writable directory.
//! [`exec_suite`] expects `cwd` to be an existing directory on the target host
//! that has `sh`, `sleep`, `cat` and `pwd`.

use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::exec::{Exec, ExecError, ExecRequest, StreamKind};
use crate::workspace::{FileKind, Workspace, WsError};

/// Whether local process `pid` has exited (gone, or a zombie awaiting its
/// parent). Reads `/proc` on Linux and asks `ps` elsewhere, so liveness
/// checks in tests stay real on macOS instead of passing because `/proc`
/// does not exist.
///
/// # Panics
///
/// Off Linux, panics when `ps` cannot be run: the check would otherwise be
/// meaningless.
pub fn local_process_gone(pid: u32) -> bool {
    if cfg!(target_os = "linux") {
        return match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
            Err(_) => true,
            Ok(stat) => stat.contains(") Z"),
        };
    }
    gone_via_ps(pid)
}

/// The non-Linux path of [`local_process_gone`]; also tested on Linux.
fn gone_via_ps(pid: u32) -> bool {
    let out = std::process::Command::new("ps")
        .args(["-o", "stat=", "-p", &pid.to_string()])
        .output()
        .expect("`ps` is needed to check process liveness off Linux");
    !out.status.success() || String::from_utf8_lossy(&out.stdout).contains('Z')
}

/// Run every workspace check against `ws`.
pub async fn workspace_suite(ws: &dyn Workspace) {
    read_write_roundtrip(ws).await;
    missing_and_directory_errors(ws).await;
    atomic_replace(ws).await;
    stat_and_list(ws).await;
    glob(ws).await;
    resolve(ws);
}

async fn read_write_roundtrip(ws: &dyn Workspace) {
    let p = Path::new("nested/dir/a.txt");
    ws.write(p, b"hello\n")
        .await
        .expect("write creates parents");
    assert_eq!(ws.read(p).await.expect("read back"), b"hello\n");
    ws.write(p, b"x").await.expect("write truncates");
    assert_eq!(ws.read(p).await.unwrap(), b"x", "write must truncate");
    let abs = ws.root().join("nested/dir/a.txt");
    assert_eq!(ws.read(&abs).await.unwrap(), b"x", "absolute paths work");
}

async fn missing_and_directory_errors(ws: &dyn Workspace) {
    let err = ws.read(Path::new("does/not/exist")).await.unwrap_err();
    assert!(matches!(err, WsError::NotFound(_)), "missing file: {err:?}");
    let err = ws.stat(Path::new("nope")).await.unwrap_err();
    assert!(matches!(err, WsError::NotFound(_)), "missing stat: {err:?}");
    ws.write(Path::new("d/f"), b"").await.unwrap();
    assert!(
        ws.read(Path::new("d")).await.is_err(),
        "reading a dir fails"
    );
}

async fn atomic_replace(ws: &dyn Workspace) {
    let p = Path::new("atomic/r.sh");
    ws.replace_atomic(p, b"v1").await.expect("replace creates");
    assert_eq!(ws.read(p).await.unwrap(), b"v1");
    ws.set_mode(p, 0o751).await.expect("set_mode");
    ws.replace_atomic(p, b"v2").await.expect("replace existing");
    assert_eq!(ws.read(p).await.unwrap(), b"v2");
    let st = ws.stat(p).await.unwrap();
    assert_eq!(st.mode, Some(0o751), "replace_atomic must keep permissions");
    let names: Vec<_> = ws
        .list(Path::new("atomic"))
        .await
        .unwrap()
        .into_iter()
        .map(|e| e.name)
        .collect();
    assert_eq!(names, vec!["r.sh"], "no temp files left behind");
    let err = ws
        .replace_atomic(Path::new("atomic"), b"x")
        .await
        .unwrap_err();
    assert!(
        matches!(err, WsError::IsDirectory(_)),
        "dir target: {err:?}"
    );
}

async fn stat_and_list(ws: &dyn Workspace) {
    ws.write(Path::new("ls/b.txt"), b"12345").await.unwrap();
    ws.write(Path::new("ls/a.txt"), b"1").await.unwrap();
    ws.write(Path::new("ls/sub/c"), b"").await.unwrap();
    let st = ws.stat(Path::new("ls/b.txt")).await.unwrap();
    assert_eq!(st.kind, FileKind::File);
    assert_eq!(st.len, 5);
    assert!(st.modified.is_some(), "mtime reported");
    assert_eq!(ws.stat(Path::new("ls")).await.unwrap().kind, FileKind::Dir);
    let entries = ws.list(Path::new("ls")).await.unwrap();
    let got: Vec<_> = entries.iter().map(|e| (e.name.as_str(), e.kind)).collect();
    assert_eq!(
        got,
        vec![
            ("a.txt", FileKind::File),
            ("b.txt", FileKind::File),
            ("sub", FileKind::Dir)
        ],
        "list is sorted and typed"
    );
    let err = ws.list(Path::new("ls/a.txt")).await.unwrap_err();
    assert!(
        matches!(err, WsError::NotADirectory(_)),
        "list file: {err:?}"
    );
}

async fn glob(ws: &dyn Workspace) {
    ws.write(Path::new("g/src/main.rs"), b"").await.unwrap();
    ws.write(Path::new("g/src/deep/lib.rs"), b"").await.unwrap();
    ws.write(Path::new("g/README.md"), b"").await.unwrap();
    ws.write(Path::new("g/.hidden.rs"), b"").await.unwrap();
    let all = ws.glob("g/**/*.rs").await.unwrap();
    assert_eq!(
        all,
        vec![
            PathBuf::from("g/.hidden.rs"),
            PathBuf::from("g/src/deep/lib.rs"),
            PathBuf::from("g/src/main.rs"),
        ],
        "recursive glob, sorted, hidden included"
    );
    let shallow = ws.glob("g/src/*.rs").await.unwrap();
    assert_eq!(
        shallow,
        vec![PathBuf::from("g/src/main.rs")],
        "* stays in one dir"
    );
    let err = ws.glob("g/[").await.unwrap_err();
    assert!(matches!(err, WsError::InvalidPattern { .. }), "{err:?}");
}

fn resolve(ws: &dyn Workspace) {
    let a = ws.resolve(Path::new("x/../y/./z"));
    assert_eq!(a, ws.root().join("y/z"), "resolve normalises");
    assert_eq!(
        ws.resolve(&ws.root().join("y/z")),
        a,
        "absolute == relative"
    );
}

/// Run every exec check against `exec`, using `cwd` as the working directory.
pub async fn exec_suite(exec: &dyn Exec, cwd: &Path) {
    output_and_status(exec, cwd).await;
    stdin_is_closed(exec, cwd).await;
    cwd_and_env(exec, cwd).await;
    timeout_kills_group(exec, cwd).await;
    kill_kills_group(exec, cwd).await;
    drop_kills_group(exec, cwd).await;
    spawn_failure(exec, cwd).await;
}

fn sh(cwd: &Path, script: &str) -> ExecRequest {
    ExecRequest::new("sh", vec!["-c".into(), script.into()], cwd)
        .with_timeout(Duration::from_secs(20))
}

/// Run to completion, returning (stdout, stderr, outcome).
async fn run(exec: &dyn Exec, req: ExecRequest) -> (String, String, crate::exec::ExitOutcome) {
    let handle = exec.spawn(req).await.expect("spawn");
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let outcome = handle
        .drive(|c| match c.stream {
            StreamKind::Stdout => out.extend_from_slice(&c.data),
            StreamKind::Stderr => err.extend_from_slice(&c.data),
        })
        .await
        .expect("outcome");
    (
        String::from_utf8_lossy(&out).into_owned(),
        String::from_utf8_lossy(&err).into_owned(),
        outcome,
    )
}

async fn output_and_status(exec: &dyn Exec, cwd: &Path) {
    let (out, err, st) = run(exec, sh(cwd, "echo out; echo err >&2; exit 3")).await;
    assert_eq!(out, "out\n");
    assert_eq!(err, "err\n");
    assert_eq!(st.code, Some(3));
    assert!(!st.timed_out && !st.killed && !st.success());
    let (_, _, st) = run(exec, sh(cwd, "true")).await;
    assert!(st.success(), "{st:?}");
    let (_, _, st) = run(exec, sh(cwd, "kill -TERM $$")).await;
    assert_eq!(st.signal, Some(15), "signal reported: {st:?}");
}

async fn stdin_is_closed(exec: &dyn Exec, cwd: &Path) {
    let req = sh(cwd, "cat; echo done").with_timeout(Duration::from_secs(5));
    let (out, _, st) = run(exec, req).await;
    assert_eq!(out, "done\n", "cat must see EOF on stdin");
    assert!(st.success(), "{st:?}");
}

async fn cwd_and_env(exec: &dyn Exec, cwd: &Path) {
    let (out, _, _) = run(exec, sh(cwd, "pwd -P")).await;
    let want = std::fs::canonicalize(cwd).unwrap_or_else(|_| cwd.to_path_buf());
    assert_eq!(out.trim_end(), want.to_string_lossy(), "cwd respected");
    let mut req = sh(cwd, "echo \"$FOO:${HARNESS_CONF_SECRET:-unset}\"");
    req = req.with_clean_env(vec![
        ("FOO".into(), "bar".into()),
        ("PATH".into(), "/usr/bin:/bin".into()),
    ]);
    let (out, _, _) = run(exec, req).await;
    assert_eq!(out, "bar:unset\n", "clean env only has given vars");
}

/// Shell that starts a background sleeper, prints its pid, then waits.
const SPAWN_GRANDCHILD: &str = "sleep 30 & echo $!; wait";

async fn read_pid(handle: &mut crate::exec::ExecHandle) -> u32 {
    let mut buf = Vec::new();
    while !buf.contains(&b'\n') {
        let chunk = tokio::time::timeout(Duration::from_secs(10), handle.next_output())
            .await
            .expect("pid printed in time")
            .expect("output open");
        buf.extend_from_slice(&chunk.data);
    }
    String::from_utf8_lossy(&buf)
        .trim()
        .parse()
        .expect("pid line")
}

/// Poll (through `exec` itself, so it works on remote hosts) until `pid` is
/// gone or a zombie.
async fn assert_dead(exec: &dyn Exec, cwd: &Path, pid: u32) {
    // Linux: /proc. Elsewhere (macOS): `ps`, which also reports zombies,
    // falling back to `kill -0` only when `ps` is missing.
    let probe = format!(
        "if [ -d /proc/{pid} ]; then grep -q ') Z' /proc/{pid}/stat; \
         elif command -v ps >/dev/null 2>&1; then s=$(ps -o stat= -p {pid}) || exit 0; \
         case \"$s\" in *Z*) exit 0;; *) exit 1;; esac; \
         else ! kill -0 {pid} 2>/dev/null; fi"
    );
    for _ in 0..50 {
        let (_, _, st) = run(exec, sh(cwd, &probe)).await;
        if st.success() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("grandchild {pid} survived: the whole process group must be killed");
}

async fn timeout_kills_group(exec: &dyn Exec, cwd: &Path) {
    let req = sh(cwd, SPAWN_GRANDCHILD).with_timeout(Duration::from_millis(500));
    let mut h = exec.spawn(req).await.expect("spawn");
    let pid = read_pid(&mut h).await;
    let st = h.drive(|_| {}).await.expect("outcome");
    assert!(st.timed_out, "{st:?}");
    assert_dead(exec, cwd, pid).await;
}

async fn kill_kills_group(exec: &dyn Exec, cwd: &Path) {
    let mut h = exec.spawn(sh(cwd, SPAWN_GRANDCHILD)).await.expect("spawn");
    let pid = read_pid(&mut h).await;
    h.kill();
    let st = tokio::time::timeout(Duration::from_secs(10), h.drive(|_| {}))
        .await
        .expect("kill finishes promptly")
        .expect("outcome");
    assert!(st.killed && !st.timed_out, "{st:?}");
    assert_dead(exec, cwd, pid).await;
}

async fn drop_kills_group(exec: &dyn Exec, cwd: &Path) {
    let mut h = exec.spawn(sh(cwd, SPAWN_GRANDCHILD)).await.expect("spawn");
    let pid = read_pid(&mut h).await;
    drop(h);
    assert_dead(exec, cwd, pid).await;
}

async fn spawn_failure(exec: &dyn Exec, cwd: &Path) {
    let req = ExecRequest::new("/definitely/not/a/program", vec![], cwd);
    let err = exec.spawn(req).await.unwrap_err();
    assert!(matches!(err, ExecError::Spawn { .. }), "{err:?}");
}

#[cfg(test)]
mod tests {
    use super::{gone_via_ps, local_process_gone};
    use std::process::{Command, Stdio};

    #[test]
    fn local_process_gone_tracks_running_zombie_and_reaped() {
        lifecycle(local_process_gone);
    }

    /// The `ps` path macOS uses, exercised on every OS.
    #[test]
    fn ps_path_tracks_running_zombie_and_reaped() {
        lifecycle(gone_via_ps);
    }

    fn lifecycle(local_process_gone: fn(u32) -> bool) {
        let mut child = Command::new("sleep")
            .arg("30")
            .stdin(Stdio::null())
            .spawn()
            .unwrap();
        let pid = child.id();
        assert!(!local_process_gone(pid), "a running process is alive");
        child.kill().unwrap();
        // Killed but not yet waited on: a zombie, which counts as gone.
        let mut zombie = false;
        for _ in 0..50 {
            if local_process_gone(pid) {
                zombie = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(zombie, "a killed, unreaped child counts as gone");
        child.wait().unwrap();
        assert!(local_process_gone(pid), "a reaped process is gone");
    }
}
