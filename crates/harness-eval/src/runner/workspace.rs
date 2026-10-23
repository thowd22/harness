//! Isolated temporary workspaces for eval attempts.

use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::suite::{Suite, Task};

/// Longest a setup command may run.
pub const SETUP_TIMEOUT: Duration = Duration::from_secs(300);

/// An attempt's workspace: a fresh temporary directory holding the fixture.
/// The directory is removed on drop unless [`Workspace::keep`] was called.
#[derive(Debug)]
pub struct Workspace {
    dir: Option<tempfile::TempDir>,
    kept: Option<PathBuf>,
}

impl Workspace {
    /// Creates the workspace for `task`: copies the fixture, writes inline
    /// files and runs the setup commands.
    pub async fn prepare(suite: &Suite, task: &Task) -> Result<Workspace, String> {
        let dir = tempfile::Builder::new()
            .prefix(&format!("harness-eval-{}-", sanitize(&task.id)))
            .tempdir()
            .map_err(|e| format!("creating a temporary workspace: {e}"))?;
        let root = dir.path().to_owned();
        if let Some(fixture) = &task.fixture {
            let src = suite.resolve_path(fixture);
            copy_dir(&src, &root).map_err(|e| format!("copying fixture {}: {e}", src.display()))?;
        }
        for (rel, text) in &task.files {
            let p = root.join(rel);
            if let Some(parent) = p.parent() {
                std::fs::create_dir_all(parent).map_err(|e| format!("{rel}: {e}"))?;
            }
            std::fs::write(&p, text).map_err(|e| format!("{rel}: {e}"))?;
        }
        for cmd in &task.setup {
            let out = run_shell(&root, cmd, SETUP_TIMEOUT).await?;
            if out.code != Some(0) {
                return Err(format!(
                    "setup command `{cmd}` failed (exit {:?}): {}",
                    out.code,
                    out.output.trim()
                ));
            }
        }
        Ok(Workspace {
            dir: Some(dir),
            kept: None,
        })
    }

    /// The workspace root.
    pub fn path(&self) -> &Path {
        match (&self.dir, &self.kept) {
            (Some(d), _) => d.path(),
            (None, Some(p)) => p,
            (None, None) => Path::new(""),
        }
    }

    /// Keeps the directory after drop (for debugging failed attempts) and
    /// returns its path.
    pub fn keep(&mut self) -> PathBuf {
        if let Some(d) = self.dir.take() {
            self.kept = Some(d.keep());
        }
        self.kept.clone().unwrap_or_default()
    }
}

/// Output of a shell command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellOutput {
    /// Exit code (`None` when killed or timed out).
    pub code: Option<i32>,
    /// Combined stdout and stderr (truncated to 8 KiB).
    pub output: String,
    /// Whether the command hit its timeout.
    pub timed_out: bool,
}

/// Most output kept from a shell command.
pub const MAX_OUTPUT: usize = 8 * 1024;

/// Runs `cmd` with `sh -c` in `cwd`, with stdin closed.
///
/// The command leads its own process group. On timeout the whole group is
/// killed, so a compound command (`make`, `a && b`, `x &`) cannot leave
/// children running that hold CPU or the cargo target lock.
pub async fn run_shell(cwd: &Path, cmd: &str, timeout: Duration) -> Result<ShellOutput, String> {
    use tokio::io::AsyncReadExt;
    let mut command = tokio::process::Command::new("sh");
    command
        .arg("-c")
        .arg(cmd)
        .current_dir(cwd)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .env("GIT_AUTHOR_NAME", "harness-eval")
        .env("GIT_AUTHOR_EMAIL", "eval@localhost")
        .env("GIT_COMMITTER_NAME", "harness-eval")
        .env("GIT_COMMITTER_EMAIL", "eval@localhost")
        .kill_on_drop(true);
    #[cfg(unix)]
    command.process_group(0);
    let mut child = command
        .spawn()
        .map_err(|e| format!("starting `{cmd}`: {e}"))?;
    let pid = child.id();
    let mut stdout = child.stdout.take().expect("stdout is piped");
    let mut stderr = child.stderr.take().expect("stderr is piped");
    let finished = tokio::time::timeout(timeout, async {
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let (o, e, status) = tokio::join!(
            stdout.read_to_end(&mut out),
            stderr.read_to_end(&mut err),
            child.wait()
        );
        o.and(e).and(status).map(|status| (status, out, err))
    })
    .await;
    match finished {
        Ok(Ok((status, out, err))) => {
            let mut text = String::from_utf8_lossy(&out).into_owned();
            text.push_str(&String::from_utf8_lossy(&err));
            Ok(ShellOutput {
                code: status.code(),
                output: truncate(text, MAX_OUTPUT),
                timed_out: false,
            })
        }
        Ok(Err(e)) => Err(format!("running `{cmd}`: {e}")),
        Err(_) => {
            super::exec::kill_group(pid, &mut child).await;
            Ok(ShellOutput {
                code: None,
                output: format!("timed out after {}s", timeout.as_secs()),
                timed_out: true,
            })
        }
    }
}

fn truncate(mut s: String, max: usize) -> String {
    if s.len() > max {
        let mut cut = max;
        while !s.is_char_boundary(cut) {
            cut -= 1;
        }
        s.truncate(cut);
        s.push_str("\n[output truncated]");
    }
    s
}

fn sanitize(id: &str) -> String {
    id.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '_'
            }
        })
        .take(40)
        .collect()
}

/// Recursively copies `src` into `dst` (files, directories and symlinks).
pub fn copy_dir(src: &Path, dst: &Path) -> io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let ty = entry.file_type()?;
        let to = dst.join(entry.file_name());
        if ty.is_dir() {
            copy_dir(&entry.path(), &to)?;
        } else if ty.is_symlink() {
            #[cfg(unix)]
            std::os::unix::fs::symlink(std::fs::read_link(entry.path())?, &to)?;
        } else {
            std::fs::copy(entry.path(), &to)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn prepares_fixture_files_and_setup() {
        let fixtures = tempfile::tempdir().unwrap();
        let fx = fixtures.path().join("fx");
        std::fs::create_dir_all(fx.join("src")).unwrap();
        std::fs::write(fx.join("src/lib.rs"), "pub fn a() {}\n").unwrap();
        let yaml = r#"
name: s
tasks:
  - id: t/1
    fixture: fx
    files: { "notes/a.txt": "hi" }
    setup: ["echo ok > setup.txt"]
    prompt: p
"#;
        let suite = Suite::parse(yaml, None, fixtures.path().to_owned()).unwrap();
        let mut ws = Workspace::prepare(&suite, &suite.tasks[0]).await.unwrap();
        let root = ws.path().to_owned();
        assert!(
            root.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("harness-eval-t_1-")
        );
        assert_eq!(
            std::fs::read_to_string(root.join("src/lib.rs")).unwrap(),
            "pub fn a() {}\n"
        );
        assert_eq!(
            std::fs::read_to_string(root.join("notes/a.txt")).unwrap(),
            "hi"
        );
        assert_eq!(
            std::fs::read_to_string(root.join("setup.txt")).unwrap(),
            "ok\n"
        );
        // Workspaces are isolated and removed on drop unless kept.
        let kept = ws.keep();
        drop(ws);
        assert!(kept.exists());
        std::fs::remove_dir_all(kept).unwrap();
        let ws2 = Workspace::prepare(&suite, &suite.tasks[0]).await.unwrap();
        let p2 = ws2.path().to_owned();
        assert_ne!(p2, root);
        drop(ws2);
        assert!(!p2.exists());
    }

    #[tokio::test]
    async fn failing_setup_is_an_error() {
        let suite = Suite::parse(
            "name: s\ntasks: [{id: t, prompt: p, setup: [\"exit 3\"]}]\n",
            None,
            ".".into(),
        )
        .unwrap();
        let e = Workspace::prepare(&suite, &suite.tasks[0])
            .await
            .unwrap_err();
        assert!(e.contains("exit 3"), "{e}");
    }

    #[tokio::test]
    async fn shell_timeout_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        let out = run_shell(dir.path(), "sleep 5", Duration::from_millis(50))
            .await
            .unwrap();
        assert!(out.timed_out);
        assert_eq!(out.code, None);
    }

    /// A timeout kills the command's whole group, not just `sh`: a
    /// background child must not outlive it.
    #[cfg(unix)]
    #[tokio::test]
    async fn shell_timeout_kills_background_children() {
        let dir = tempfile::tempdir().unwrap();
        let pid_file = dir.path().join("bg.pid");
        let cmd = format!(
            "sleep 30 & echo $! > {}; sleep 30",
            pid_file.to_string_lossy()
        );
        let out = run_shell(dir.path(), &cmd, Duration::from_millis(500))
            .await
            .unwrap();
        assert!(out.timed_out);
        let pid: u32 = std::fs::read_to_string(&pid_file)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        let mut gone = false;
        for _ in 0..50 {
            if harness_tools::conformance::local_process_gone(pid) {
                gone = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        assert!(gone, "background child {pid} outlived the timeout");
    }
}
