//! Sandbox integration tests: real processes, the `harness-sandbox`
//! launcher binary and the local executor. Assertions adapt to what the
//! kernel supports, but always check that unsupported features warn.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use harness_policy::sandbox::{
    LandlockState, Launcher, NetworkPolicy, SandboxConfig, SandboxStatus, SandboxedExec,
    WARNING_PREFIX,
};
use harness_tools::bash::{BashTool, EnvScrubber};
use harness_tools::config::ToolsConfig;
use harness_tools::exec::{Exec, ExecRequest};
use harness_tools::local::LocalExec;
use harness_tools::spill::{MemoryBlobSink, Spiller};
use harness_tools::tool::ToolContext;

fn launcher() -> Launcher {
    Launcher::binary(env!("CARGO_BIN_EXE_harness-sandbox"))
}

/// Directories outside the temp dirs (target/tmp), so writes there are
/// really "outside the workspace and temp dirs".
fn dirs(name: &str) -> (PathBuf, PathBuf) {
    let base = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("policy-sandbox-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let ws = base.join("ws");
    let outside = base.join("outside");
    std::fs::create_dir_all(&ws).unwrap();
    std::fs::create_dir_all(&outside).unwrap();
    (ws, outside)
}

/// Logs (to the test's stderr, shown with `--nocapture` and on failure) that
/// a host feature is missing, so the enforcement assertion is replaced by
/// the degraded-path assertion rather than silently passing.
fn skip_note(reason: &str) {
    eprintln!("SKIP enforcement check: {reason}");
}

async fn run(exec: &dyn Exec, cwd: &Path, script: &str) -> (bool, String) {
    let req = ExecRequest::new("bash", vec!["-c".into(), script.into()], cwd);
    let handle = exec.spawn(req).await.unwrap();
    let mut out = Vec::new();
    let status = handle
        .drive(|c| out.extend_from_slice(&c.data))
        .await
        .unwrap();
    (status.success(), String::from_utf8_lossy(&out).into_owned())
}

#[tokio::test]
async fn landlock_restricts_writes_to_workspace_and_temp() {
    let (ws, outside) = dirs("fs");
    let cfg = SandboxConfig::for_workspace(&ws, true, NetworkPolicy::Allow);
    let exec = SandboxedExec::new(Arc::new(LocalExec::new()), launcher(), cfg);
    let tmp = tempfile::NamedTempFile::new().unwrap();

    let (ok, out) = run(&exec, &ws, "echo hi > inside.txt && echo done").await;
    assert!(ok, "{out}");
    assert!(ws.join("inside.txt").exists());
    let (ok, out) = run(&exec, &ws, &format!("echo t > {}", tmp.path().display())).await;
    assert!(ok, "temp dir writable: {out}");
    let (ok, _) = run(&exec, &ws, "ls / > /dev/null").await;
    assert!(ok, "/dev/null writable and reads unrestricted");

    let (ok, out) = run(&exec, &ws, &format!("touch {}/x", outside.display())).await;
    match &exec.status().landlock {
        LandlockState::Full | LandlockState::Partial => {
            assert!(!ok, "write outside the workspace must fail: {out}");
            assert!(out.contains("Permission denied"), "{out}");
            assert!(!outside.join("x").exists());
        }
        other => {
            // Degraded: still runs, but warns visibly.
            skip_note(&format!(
                "Landlock not enforced here ({other:?}); checking the degraded path warns instead"
            ));
            assert!(ok, "{other:?}: {out}");
            assert!(out.contains(WARNING_PREFIX), "{out}");
            assert!(
                exec.status()
                    .warnings
                    .iter()
                    .any(|w| w.contains("Landlock"))
            );
        }
    }
}

#[tokio::test]
async fn bash_tool_runs_inside_the_sandbox() {
    let (ws, outside) = dirs("bash");
    let cfg = SandboxConfig::for_workspace(&ws, true, NetworkPolicy::Allow);
    let exec = Arc::new(SandboxedExec::new(
        Arc::new(LocalExec::new()),
        launcher(),
        cfg,
    ));
    let enforced = matches!(
        exec.status().landlock,
        LandlockState::Full | LandlockState::Partial
    );
    if !enforced {
        skip_note(&format!(
            "Landlock not enforced here ({:?}); asserting the outside write is NOT blocked",
            exec.status().landlock
        ));
    }
    let tools = ToolsConfig::default();
    let spiller = Spiller::new(Arc::new(MemoryBlobSink::new()), &tools);
    let bash = BashTool::new(exec, &ws, tools, EnvScrubber::new([], []), spiller);
    let ctx = ToolContext::new("c1");
    let out = bash
        .call(&ctx, serde_json::json!({"command": format!("touch a.txt; touch {}/b.txt", outside.display())}))
        .await
        .unwrap()
        .text()
        .to_string();
    assert!(ws.join("a.txt").exists(), "{out}");
    assert_eq!(outside.join("b.txt").exists(), !enforced, "{out}");
}

#[tokio::test]
async fn network_deny_blocks_connections() {
    let (ws, _) = dirs("net");
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let probe = format!("exec 3<>/dev/tcp/127.0.0.1/{port} && echo connected");

    let allow = SandboxedExec::new(
        Arc::new(LocalExec::new()),
        launcher(),
        SandboxConfig::for_workspace(&ws, true, NetworkPolicy::Allow),
    );
    let (ok, out) = run(&allow, &ws, &probe).await;
    assert!(ok && out.contains("connected"), "network allowed: {out}");

    let deny = SandboxedExec::new(
        Arc::new(LocalExec::new()),
        launcher(),
        SandboxConfig::for_workspace(&ws, true, NetworkPolicy::Deny),
    );
    let (ok, out) = run(&deny, &ws, &probe).await;
    if deny.status().net_tool.is_some() {
        assert!(
            !ok && !out.contains("connected"),
            "network must be blocked: {out}"
        );
        // The filesystem sandbox still applies inside the namespace.
        let (ok, out) = run(&deny, &ws, "echo x > f && cat f").await;
        assert!(ok && out.contains('x'), "{out}");
    } else {
        skip_note(
            "neither bwrap nor unshare works here; checking network = deny degrades with a warning",
        );
        assert!(ok, "degrades: {out}");
        assert!(
            deny.status()
                .warnings
                .iter()
                .any(|w| w.contains("network = deny"))
        );
    }
}

#[test]
fn doctor_reports_status() {
    let (ws, _) = dirs("doctor");
    let s = SandboxStatus::probe(
        &SandboxConfig::for_workspace(&ws, true, NetworkPolicy::Deny),
        &launcher(),
    );
    let lines = s.doctor_lines();
    assert_eq!(lines.len(), 2);
    assert!(lines[0].starts_with("filesystem sandbox:"), "{lines:?}");
    assert!(lines[1].starts_with("network:"), "{lines:?}");
    // On this kernel family the probe really runs the launcher.
    assert!(
        !matches!(&s.landlock, LandlockState::Unsupported { reason } if reason.contains("cannot run"))
    );
}

#[test]
fn launcher_rejects_bad_usage() {
    let status = std::process::Command::new(env!("CARGO_BIN_EXE_harness-sandbox"))
        .arg("--bogus")
        .output()
        .unwrap();
    assert_eq!(status.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&status.stderr).contains(WARNING_PREFIX));
    let missing = std::process::Command::new(env!("CARGO_BIN_EXE_harness-sandbox"))
        .args(["--", "/nonexistent/program"])
        .output()
        .unwrap();
    assert_eq!(missing.status.code(), Some(127));
}
