//! Workspace sync end to end: push (rsync or git bundle), remote edits
//! through the remote tools, pull, gated apply, conflicts and cleanup. The
//! "remote host" is an in-process helper on local temp dirs.

use std::path::Path;
use std::sync::Arc;

use harness_remote::client::{ClientOptions, InProcessConnector, RemoteClient};
use harness_remote::helper::{Helper, HelperOptions};
use harness_remote::shell::LocalShell;
use harness_remote::sync::{
    ApplyOptions, CleanupPolicy, DeltaKind, HygieneGate, Pusher, RemoteRun,
};
use harness_remote::workspace::RemoteWorkspace;
use harness_tools::workspace::Workspace;

fn project() -> tempfile::TempDir {
    let t = tempfile::tempdir().unwrap();
    let r = t.path();
    std::fs::create_dir_all(r.join("src")).unwrap();
    std::fs::create_dir_all(r.join("target")).unwrap();
    std::fs::write(r.join(".gitignore"), "target/\n").unwrap();
    std::fs::write(r.join("src/lib.rs"), "pub fn a() {}\n").unwrap();
    std::fs::write(r.join("notes.txt"), "untracked\n").unwrap();
    std::fs::write(r.join("target/big.o"), "junk").unwrap();
    t
}

async fn client() -> RemoteClient {
    let c = InProcessConnector::new(Helper::new(HelperOptions::default()));
    RemoteClient::connect(Arc::new(c), ClientOptions::new("box"))
        .await
        .unwrap()
}

fn has_tool(name: &str) -> bool {
    std::process::Command::new(name)
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

async fn roundtrip(pusher: Pusher, local: &Path, work: &Path) {
    let client = client().await;
    let run = RemoteRun::push(
        &client,
        local,
        Some(&work.to_string_lossy()),
        "run-1",
        &pusher,
    )
    .await
    .unwrap();
    let rdir = Path::new(run.remote_dir()).to_path_buf();
    assert!(rdir.starts_with(work));
    assert!(rdir.join("src/lib.rs").exists());
    assert!(rdir.join("notes.txt").exists(), "untracked files synced");
    assert!(!rdir.join("target").exists(), "gitignored files not synced");

    // The agent edits on the remote host through the remote workspace.
    let ws = RemoteWorkspace::new(client.clone(), run.remote_dir());
    ws.replace_atomic(Path::new("src/lib.rs"), b"pub fn a() { b() }\n")
        .await
        .unwrap();
    ws.write(Path::new("src/b.rs"), b"pub fn b() {}\n")
        .await
        .unwrap();

    let set = run.pull().await.unwrap();
    let kinds: Vec<_> = set
        .changes
        .iter()
        .map(|d| (d.path.as_str(), d.kind))
        .collect();
    assert_eq!(
        kinds,
        vec![
            ("src/b.rs", DeltaKind::Added),
            ("src/lib.rs", DeltaKind::Modified)
        ]
    );
    assert!(set.to_patch(local).contains("+pub fn a() { b() }"));
    let rep = run
        .apply(&set, &[&HygieneGate::default()], ApplyOptions::default())
        .unwrap();
    assert!(rep.is_applied(), "{:?}", rep.problems());
    assert_eq!(
        std::fs::read_to_string(local.join("src/lib.rs")).unwrap(),
        "pub fn a() { b() }\n"
    );
    assert!(local.join("src/b.rs").exists());

    assert!(run.cleanup(CleanupPolicy::OnSuccess, true).await.unwrap());
    assert!(!rdir.exists(), "run dir removed after success");
}

#[tokio::test(flavor = "multi_thread")]
async fn rsync_push_pull_apply_cleanup() {
    if !has_tool("rsync") {
        eprintln!("rsync missing; skipping");
        return;
    }
    let local = project();
    let work = tempfile::tempdir().unwrap();
    roundtrip(Pusher::Rsync(None), local.path(), work.path()).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn git_bundle_push_pull_apply_cleanup() {
    if !has_tool("git") {
        eprintln!("git missing; skipping");
        return;
    }
    let local = project();
    let ok = std::process::Command::new("git")
        .args(["init", "-q"])
        .current_dir(local.path())
        .status()
        .unwrap()
        .success();
    assert!(ok);
    let work = tempfile::tempdir().unwrap();
    roundtrip(
        Pusher::GitBundle(Arc::new(LocalShell::default())),
        local.path(),
        work.path(),
    )
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn conflicting_local_edit_is_surfaced_not_overwritten() {
    if !has_tool("rsync") {
        return;
    }
    let local = project();
    let work = tempfile::tempdir().unwrap();
    let client = client().await;
    let run = RemoteRun::push(
        &client,
        local.path(),
        Some(&work.path().to_string_lossy()),
        "run-2",
        &Pusher::Rsync(None),
    )
    .await
    .unwrap();
    let ws = RemoteWorkspace::new(client.clone(), run.remote_dir());
    ws.write(Path::new("src/lib.rs"), b"remote\n")
        .await
        .unwrap();
    std::fs::write(local.path().join("src/lib.rs"), "local edit\n").unwrap();
    let set = run.pull().await.unwrap();
    let rep = run.apply(&set, &[], ApplyOptions::default()).unwrap();
    assert!(!rep.is_applied());
    assert_eq!(rep.conflicts[0].path, "src/lib.rs");
    assert_eq!(
        std::fs::read_to_string(local.path().join("src/lib.rs")).unwrap(),
        "local edit\n"
    );
    // Failed runs are kept for inspection under the default policy...
    assert!(!run.cleanup(CleanupPolicy::OnSuccess, false).await.unwrap());
    assert!(Path::new(run.remote_dir()).exists());
    // ...and pruned later.
    let removed = run.prune(0, None).await.unwrap();
    assert_eq!(removed, vec!["run-2"]);
    assert!(!Path::new(run.remote_dir()).exists());
}

#[tokio::test(flavor = "multi_thread")]
async fn cleanup_refuses_paths_outside_the_project_dir() {
    let client = client().await;
    let t = tempfile::tempdir().unwrap();
    let base = t.path().join("proj");
    std::fs::create_dir_all(&base).unwrap();
    let err = client
        .remove_run_dir(&t.path().to_string_lossy(), &base.to_string_lossy())
        .await
        .unwrap_err();
    assert!(err.to_string().contains("refusing"), "{err}");
    assert!(t.path().exists());
}

#[tokio::test(flavor = "multi_thread")]
async fn push_is_audited_with_the_host() {
    if !has_tool("rsync") {
        return;
    }
    let audit = Arc::new(harness_remote::audit::RecordingAudit::default());
    let mut opts = ClientOptions::new("box");
    opts.audit = audit.clone();
    let c = InProcessConnector::new(Helper::new(HelperOptions::default()));
    let client = RemoteClient::connect(Arc::new(c), opts).await.unwrap();
    let local = project();
    let work = tempfile::tempdir().unwrap();
    let run = RemoteRun::push(
        &client,
        local.path(),
        Some(&work.path().to_string_lossy()),
        "run-a",
        &Pusher::Rsync(None),
    )
    .await
    .unwrap();
    run.pull().await.unwrap();
    run.cleanup(CleanupPolicy::Always, false).await.unwrap();
    let ops: Vec<String> = audit.events().iter().map(|e| e.op.clone()).collect();
    assert_eq!(
        ops,
        vec!["sync.push", "sync.manifest", "sync.changes", "sync.remove"]
    );
    assert!(audit.events().iter().all(|e| e.host == "box" && e.ok));
}
