//! Terminal ioctls under the sandbox, with real ptys: `/dev/tty` and pty
//! slaves accept `TIOCGWINSZ` / `tcgetattr`, other devices refuse ioctls on
//! Landlock ABI 5+, older ABIs degrade to "ioctls unrestricted", and a
//! denied command's tool result carries the sandbox note.
//!
//! Linux-only (Landlock); elsewhere a single test reports the skip.

#[cfg(not(target_os = "linux"))]
#[test]
fn terminal_ioctl_tests_are_linux_only() {
    eprintln!("SKIP: the Landlock sandbox and these pty tests are Linux-only");
}

#[cfg(target_os = "linux")]
mod linux {
    use std::path::{Path, PathBuf};
    use std::process::{Command, Stdio};
    use std::sync::Arc;
    use std::time::Duration;

    use harness_policy::sandbox::{
        ABI_FLAG, DENIAL_NOTE, LandlockState, Launcher, NetworkPolicy, SandboxConfig,
        SandboxedExec, WRITE_FLAG, device_ioctls_restricted,
    };
    use harness_tools::bash::{BashTool, EnvScrubber};
    use harness_tools::config::ToolsConfig;
    use harness_tools::exec::{Exec, ExecHandle, ExecRequest};
    use harness_tools::local::LocalExec;
    use harness_tools::spill::{MemoryBlobSink, Spiller};
    use harness_tools::tool::ToolContext;
    use nix::pty::{OpenptyResult, Winsize, openpty};

    const LAUNCHER: &str = env!("CARGO_BIN_EXE_harness-sandbox");

    fn skip_note(reason: &str) {
        eprintln!("SKIP enforcement check: {reason}");
    }

    /// Workspace and an "outside" dir, both outside the temp dirs.
    fn dirs(name: &str) -> (PathBuf, PathBuf) {
        let base = Path::new(env!("CARGO_TARGET_TMPDIR"))
            .join(format!("policy-tty-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let ws = base.join("ws");
        let outside = base.join("outside");
        std::fs::create_dir_all(&ws).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        (ws, outside)
    }

    fn enforced() -> bool {
        let out = Command::new(LAUNCHER).arg("--probe").output().unwrap();
        String::from_utf8_lossy(&out.stdout).starts_with("landlock=full")
            || String::from_utf8_lossy(&out.stdout).starts_with("landlock=partial")
    }

    fn pty() -> (OpenptyResult, PathBuf) {
        let size = Winsize {
            ws_row: 40,
            ws_col: 120,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        let pty = openpty(Some(&size), None).expect("openpty");
        let name = nix::unistd::ttyname(&pty.slave).unwrap();
        (pty, name)
    }

    /// Runs `script` under the launcher (writable: `ws` and the standard
    /// devices), as the session leader of a real pty whose slave is its
    /// controlling terminal (`setsid -c`). Returns the script's stdout,
    /// written to a file in `ws`.
    fn run_on_pty(ws: &Path, extra: &[&str], script: &str) -> String {
        let (pty, slave) = pty();
        let out = ws.join("out.txt");
        let mut cmd = Command::new("setsid");
        cmd.arg("-c").arg(LAUNCHER).arg(WRITE_FLAG).arg(ws);
        for d in harness_policy::sandbox::WRITABLE_DEVICES {
            cmd.args([WRITE_FLAG, d]);
        }
        cmd.args(extra)
            .args(["--", "sh", "-c", script])
            .env("SLAVE", &slave)
            .env("OUT", &out)
            .stdin(Stdio::from(pty.slave.try_clone().unwrap()))
            .stdout(Stdio::from(pty.slave.try_clone().unwrap()))
            .stderr(Stdio::from(pty.slave));
        let mut child = cmd.spawn().expect("setsid from util-linux");
        let status = wait(&mut child, Duration::from_secs(60));
        // The master stays open until the child is gone (no SIGHUP).
        drop(pty.master);
        assert!(status.success(), "{status:?}");
        std::fs::read_to_string(&out).unwrap()
    }

    fn wait(child: &mut std::process::Child, limit: Duration) -> std::process::ExitStatus {
        let start = std::time::Instant::now();
        loop {
            if let Some(s) = child.try_wait().unwrap() {
                return s;
            }
            if start.elapsed() > limit {
                let _ = child.kill();
                let _ = child.wait();
                panic!("sandboxed pty command did not finish in {limit:?}");
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// Probes: window size and termios on `/dev/tty` and the slave path
    /// (both opened after Landlock applies), an ioctl on `/dev/null`, and
    /// a write outside the workspace.
    fn probe_script(outside: &Path) -> String {
        format!(
            "exec >\"$OUT\" 2>&1
             echo \"tty=$(stty -F /dev/tty size)\"
             echo \"slave=$(stty -F \"$SLAVE\" size)\"
             stty -F /dev/tty -a >/dev/null && echo tty_attr=ok
             stty -F \"$SLAVE\" -a >/dev/null && echo slave_attr=ok
             stty -F /dev/null size; echo null_rc=$?
             touch {}/x; echo outside_rc=$?
             true",
            outside.display()
        )
    }

    #[test]
    fn terminal_ioctls_work_and_writes_outside_stay_denied() {
        let (ws, outside) = dirs("pty");
        eprintln!(
            "host: landlock enforced={}, device ioctls restricted (ABI 5+)={}",
            enforced(),
            device_ioctls_restricted()
        );
        let out = run_on_pty(&ws, &[], &probe_script(&outside));
        assert!(out.contains("tty=40 120"), "{out}");
        assert!(out.contains("slave=40 120"), "{out}");
        assert!(
            out.contains("tty_attr=ok") && out.contains("slave_attr=ok"),
            "{out}"
        );
        if enforced() {
            assert!(out.contains("outside_rc=1"), "{out}");
            assert!(!outside.join("x").exists());
        } else {
            skip_note("Landlock not enforced here; write denial not checked");
        }
        if device_ioctls_restricted() && enforced() {
            assert!(out.contains("/dev/null: Permission denied"), "{out}");
        } else {
            skip_note("kernel Landlock ABI < 5: device ioctls are not restricted here");
            assert!(out.contains("Inappropriate ioctl"), "{out}");
        }
    }

    #[test]
    fn older_abi_leaves_ioctls_alone_but_still_denies_writes() {
        let (ws, outside) = dirs("abi4");
        let out = run_on_pty(&ws, &[ABI_FLAG, "4"], &probe_script(&outside));
        assert!(
            out.contains("tty=40 120") && out.contains("slave=40 120"),
            "{out}"
        );
        // ABI 4 cannot express device ioctls: the driver answers instead.
        assert!(out.contains("Inappropriate ioctl"), "{out}");
        if enforced() {
            assert!(out.contains("outside_rc=1"), "{out}");
        }
    }

    #[test]
    fn pty_created_inside_the_sandbox_without_a_controlling_terminal() {
        // The reported failure: bash has no controlling terminal, and the
        // command opens a pty itself (as a TUI test harness does).
        if which("script").is_none() {
            skip_note("util-linux script not installed");
            return;
        }
        let (ws, _) = dirs("inner");
        let out = ws.join("typescript");
        let status = Command::new("setsid")
            .arg("-w")
            .arg(LAUNCHER)
            .args([WRITE_FLAG, ws.to_str().unwrap(), WRITE_FLAG, "/dev/null"])
            .args([
                "--",
                "script",
                "-qec",
                "stty -F /dev/tty -a >/dev/null && echo TTY_OK",
            ])
            .arg(&out)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .unwrap();
        let text = std::fs::read_to_string(&out).unwrap_or_default();
        assert!(
            status.success() && text.contains("TTY_OK"),
            "{status:?}: {text}"
        );
    }

    fn which(name: &str) -> Option<PathBuf> {
        std::env::var_os("PATH").and_then(|p| {
            std::env::split_paths(&p)
                .map(|d| d.join(name))
                .find(|p| p.is_file())
        })
    }

    async fn bash(exec: Arc<SandboxedExec>, ws: &Path, command: &str) -> String {
        let tools = ToolsConfig::default();
        let spiller = Spiller::new(Arc::new(MemoryBlobSink::new()), &tools);
        let bash = BashTool::new(exec, ws, tools, EnvScrubber::new([], []), spiller);
        bash.call(
            &ToolContext::new("c1"),
            serde_json::json!({ "command": command }),
        )
        .await
        .unwrap()
        .text()
        .to_string()
    }

    #[tokio::test]
    async fn denied_commands_get_a_short_sandbox_note() {
        let (ws, outside) = dirs("note");
        let exec = Arc::new(SandboxedExec::new(
            Arc::new(LocalExec::new()),
            Launcher::binary(LAUNCHER),
            SandboxConfig::for_workspace(&ws, true, NetworkPolicy::Allow),
        ));
        let note = DENIAL_NOTE.trim_end();
        let ok = bash(exec.clone(), &ws, "touch inside && echo fine").await;
        assert!(!ok.contains(note), "{ok}");
        let own = bash(exec.clone(), &ws, "touch ro && chmod 0 ro && echo x > ro").await;
        if !matches!(
            exec.status().landlock,
            LandlockState::Full | LandlockState::Partial
        ) {
            skip_note("Landlock not enforced here; no sandbox denials to explain");
            let out = bash(exec, &ws, &format!("touch {}/x", outside.display())).await;
            assert!(!out.contains(note), "{out}");
            return;
        }
        let out = bash(exec.clone(), &ws, &format!("touch {}/x", outside.display())).await;
        assert!(out.contains("Permission denied"), "{out}");
        assert_eq!(out.matches(note).count(), 1, "{out}");
        // A permission error on a writable path is not blamed on the
        // sandbox (unless running as root, where chmod 0 does not deny).
        if own.contains("Permission denied") {
            assert!(!own.contains(note), "{own}");
        }
        if device_ioctls_restricted() {
            let out = bash(exec, &ws, "stty -F /dev/null size").await;
            assert!(out.contains("/dev/null: Permission denied"), "{out}");
            assert!(out.contains(note), "{out}");
        } else {
            skip_note("kernel Landlock ABI < 5: no ioctl denials to explain");
        }
    }

    /// Starts a backgrounded grandchild and returns its pid.
    async fn grandchild(exec: &SandboxedExec, ws: &Path) -> (ExecHandle, u32) {
        let req = ExecRequest::new(
            "sh",
            vec!["-c".into(), "sleep 60 & echo $!; wait".into()],
            ws,
        );
        let mut h = exec.spawn(req).await.unwrap();
        let mut text = String::new();
        while !text.contains('\n') {
            let chunk = tokio::time::timeout(Duration::from_secs(20), h.next_output())
                .await
                .expect("pid printed")
                .expect("output open");
            text.push_str(&String::from_utf8_lossy(&chunk.data));
        }
        (h, text.trim().parse().unwrap())
    }

    async fn assert_gone(pid: u32) {
        let deadline = std::time::Instant::now() + Duration::from_secs(20);
        while !harness_tools::conformance::local_process_gone(pid) {
            assert!(
                std::time::Instant::now() < deadline,
                "pid {pid} outlived its handle"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    #[tokio::test]
    async fn kill_and_drop_still_end_the_process_group() {
        let (ws, _) = dirs("kill");
        let exec = SandboxedExec::new(
            Arc::new(LocalExec::new()),
            Launcher::binary(LAUNCHER),
            SandboxConfig::for_workspace(&ws, true, NetworkPolicy::Allow),
        );
        let (mut h, pid) = grandchild(&exec, &ws).await;
        h.kill();
        let st = tokio::time::timeout(Duration::from_secs(20), h.drive(|_| {}))
            .await
            .expect("kill finishes")
            .unwrap();
        assert!(st.killed, "{st:?}");
        assert_gone(pid).await;

        let (h, pid) = grandchild(&exec, &ws).await;
        drop(h);
        assert_gone(pid).await;
    }

    /// Starts a command that closes stdout and stderr and keeps running,
    /// and returns its pid once its output has had time to close.
    async fn silent(exec: &SandboxedExec, ws: &Path) -> (ExecHandle, u32) {
        let pidfile = ws.join("silent.pid");
        let _ = std::fs::remove_file(&pidfile);
        let script = format!(
            "echo $$ > {}; exec >/dev/null 2>&1; exec sleep 60",
            pidfile.display()
        );
        let req = ExecRequest::new("sh", vec!["-c".into(), script], ws);
        let h = exec.spawn(req).await.unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(20);
        let pid = loop {
            if let Some(pid) = std::fs::read_to_string(&pidfile)
                .ok()
                .and_then(|t| t.trim().parse().ok())
            {
                break pid;
            }
            assert!(std::time::Instant::now() < deadline, "pid written");
            tokio::time::sleep(Duration::from_millis(20)).await;
        };
        // Give the redirect time to close the pipes (if it has not yet, the
        // kill below takes the earlier path and the test still holds).
        tokio::time::sleep(Duration::from_millis(300)).await;
        (h, pid)
    }

    #[tokio::test]
    async fn kill_and_drop_end_the_process_after_its_output_closed() {
        let (ws, _) = dirs("kill-eof");
        let exec = SandboxedExec::new(
            Arc::new(LocalExec::new()),
            Launcher::binary(LAUNCHER),
            SandboxConfig::for_workspace(&ws, true, NetworkPolicy::Allow),
        );
        let (mut h, pid) = silent(&exec, &ws).await;
        h.kill();
        let st = tokio::time::timeout(Duration::from_secs(20), h.drive(|_| {}))
            .await
            .expect("kill finishes")
            .unwrap();
        assert!(st.killed, "{st:?}");
        assert_gone(pid).await;

        let (h, pid) = silent(&exec, &ws).await;
        drop(h);
        assert_gone(pid).await;
    }
}
