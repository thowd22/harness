//! The remote Workspace/Exec pass the shared harness-tools conformance
//! suites, both against an in-process helper and against the real helper
//! binary spawned over a local pipe (standing in for ssh).

use std::sync::Arc;

use harness_remote::client::{ClientOptions, CommandConnector, InProcessConnector, RemoteClient};
use harness_remote::helper::{Helper, HelperOptions};
use harness_remote::workspace::{RemoteEnv, RemoteExec, RemoteWorkspace};
use harness_tools::conformance::{exec_suite, workspace_suite};

async fn in_process() -> RemoteClient {
    let c = InProcessConnector::new(Helper::new(HelperOptions::default()));
    RemoteClient::connect(Arc::new(c), ClientOptions::new("in-process"))
        .await
        .expect("connect")
}

fn helper_bin() -> String {
    env!("CARGO_BIN_EXE_harness-remote-agent").to_owned()
}

async fn over_pipe(sock: &std::path::Path) -> RemoteClient {
    let c = CommandConnector::new(
        helper_bin(),
        vec![
            "remote-agent".into(),
            "--stdio".into(),
            "--grace".into(),
            "2".into(),
            "--socket-dir".into(),
            sock.display().to_string(),
        ],
    );
    RemoteClient::connect(Arc::new(c), ClientOptions::new("pipe"))
        .await
        .expect("connect to helper process")
}

#[tokio::test(flavor = "multi_thread")]
async fn remote_workspace_passes_conformance_in_process() {
    let dir = tempfile::tempdir().unwrap();
    let ws = RemoteWorkspace::new(in_process().await, dir.path().to_string_lossy());
    workspace_suite(&ws).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn remote_exec_passes_conformance_in_process() {
    let dir = tempfile::tempdir().unwrap();
    let ex = RemoteExec::new(in_process().await, RemoteEnv::default());
    exec_suite(&ex, dir.path()).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn remote_workspace_passes_conformance_over_helper_process() {
    let dir = tempfile::tempdir().unwrap();
    let sock = socket_dir();
    let ws = RemoteWorkspace::new(over_pipe(sock.path()).await, dir.path().to_string_lossy());
    workspace_suite(&ws).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn remote_exec_passes_conformance_over_helper_process() {
    let dir = tempfile::tempdir().unwrap();
    let sock = socket_dir();
    let ex = RemoteExec::new(over_pipe(sock.path()).await, RemoteEnv::default());
    exec_suite(&ex, dir.path()).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn helper_process_reports_version_and_probe() {
    let sock = socket_dir();
    let client = over_pipe(sock.path()).await;
    let h = client.hello().unwrap();
    assert_eq!(h.version, harness_remote::protocol::HELPER_VERSION);
    assert_eq!(h.arch, std::env::consts::ARCH);
    let p = client.probe(Some("/")).await.unwrap();
    assert!(p.cpus >= 1);
    assert_eq!(p.jobs, 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn helper_binary_prints_version() {
    let out = std::process::Command::new(helper_bin())
        .args(["remote-agent", "--version"])
        .output()
        .unwrap();
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stdout).contains(&format!(
        "protocol {}",
        harness_remote::protocol::PROTOCOL_VERSION
    )));
    let out = std::process::Command::new(helper_bin())
        .arg("--bogus")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
}

/// A short directory for the helper's reattach socket. Unix socket paths are
/// capped (104 bytes on macOS, whose `$TMPDIR` alone is ~50), and
/// `helper-<32 hex>.sock` adds 44, so sockets live under `/tmp`, not `$TMPDIR`.
fn socket_dir() -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix("hs")
        .tempdir_in("/tmp")
        .expect("create a socket dir under /tmp")
}
