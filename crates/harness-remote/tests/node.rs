//! A node on a remote host end to end over the real helper binary on a
//! local pipe (standing in for ssh): connect, push, tools on the host,
//! pull, gated apply, cleanup, with every remote operation and connection
//! change recorded on the node and tagged with the host.

use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use harness_events::redact::Redactor;
use harness_events::schema::{EventKind, NodeId, RunId};
use harness_events::store::{RecorderOptions, RunRecorder, read_run};
use harness_remote::audit::AuditSink;
use harness_remote::client::{ClientOptions, CommandConnector, RemoteClient};
use harness_remote::node::{HostConnect, HostSession, RemoteNode};
use harness_remote::sync::{ApplyOptions, CleanupPolicy, HygieneGate, Pusher};
use harness_remote::workspace::RemoteEnv;
use harness_tools::exec::{Exec, ExecRequest};
use harness_tools::workspace::Workspace;

/// Runs `harness-remote-agent remote-agent --stdio` as a local child
/// process; the "host" is a temp workdir on this machine.
struct PipeHosts {
    sock: std::path::PathBuf,
    workdir: std::path::PathBuf,
}

#[async_trait]
impl HostConnect for PipeHosts {
    async fn connect(&self, host: &str, audit: Arc<dyn AuditSink>) -> Result<HostSession, String> {
        let c = CommandConnector::new(
            env!("CARGO_BIN_EXE_harness-remote-agent"),
            vec![
                "remote-agent".into(),
                "--stdio".into(),
                "--grace".into(),
                "2".into(),
                "--socket-dir".into(),
                self.sock.display().to_string(),
            ],
        );
        let client = RemoteClient::connect(Arc::new(c), ClientOptions::new(host).with_audit(audit))
            .await
            .map_err(|e| e.to_string())?;
        Ok(HostSession {
            client,
            workdir: Some(self.workdir.display().to_string()),
            pusher: Pusher::Rsync(None),
            cleanup: CleanupPolicy::OnSuccess,
        })
    }
}

fn has_rsync() -> bool {
    std::process::Command::new("rsync")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

#[tokio::test(flavor = "multi_thread")]
async fn node_runs_tools_on_the_host_and_brings_changes_back() {
    if !has_rsync() {
        eprintln!("rsync not installed; push is covered by tests/sync.rs");
        return;
    }
    let t = tempfile::tempdir().unwrap();
    let sock = socket_dir();
    let local = t.path().join("project");
    std::fs::create_dir_all(local.join("src")).unwrap();
    std::fs::write(local.join("src/lib.rs"), "pub fn a() {}\n").unwrap();
    let hosts = PipeHosts {
        sock: sock.path().to_path_buf(),
        workdir: t.path().join("remote-work"),
    };
    let recorder = RunRecorder::create(
        &t.path().join("data"),
        RunId::new(),
        Arc::new(Redactor::default()),
        RecorderOptions::default(),
    )
    .await
    .unwrap();
    let node = NodeId::from("wf/build");
    let rn = RemoteNode::open(&hosts, "gpu-box", &local, &recorder, &node, 1)
        .await
        .unwrap();
    assert_eq!(rn.host(), "gpu-box");
    let root = rn.root();
    assert!(root.starts_with(&hosts.workdir), "{}", root.display());
    assert!(root.join("src/lib.rs").exists(), "worktree pushed");

    let ws = rn.workspace();
    ws.replace_atomic(Path::new("src/lib.rs"), b"pub fn a() { b() }\n")
        .await
        .unwrap();
    let exec = rn.exec(RemoteEnv::default());
    let mut req = ExecRequest::new(
        "sh",
        vec![
            "-c".into(),
            "echo 'pub fn b() {}' > src/b.rs && echo report > \"$ART/r.txt\"".into(),
        ],
        root.clone(),
    );
    req.env = vec![("ART".into(), rn.artifacts_dir().display().to_string())];
    let st = exec.spawn(req).await.unwrap().wait().await.unwrap();
    assert!(st.success());
    assert_eq!(rn.artifact("r.txt").await.unwrap(), b"report\n");

    let ret = rn
        .finish(true, &[&HygieneGate::default()], ApplyOptions::default())
        .await
        .unwrap();
    assert!(ret.applied(), "{:?}", ret.report.problems());
    assert_eq!(ret.changes.changes.len(), 2, "artifacts are not changes");
    assert!(ret.removed);
    assert!(!root.exists(), "run dir removed after success");
    assert_eq!(
        std::fs::read_to_string(local.join("src/lib.rs")).unwrap(),
        "pub fn a() { b() }\n"
    );
    assert_eq!(
        std::fs::read_to_string(local.join("src/b.rs")).unwrap(),
        "pub fn b() {}\n"
    );

    recorder.flush().await.unwrap();
    let evs = read_run(recorder.dir()).unwrap().events;
    let ops: Vec<&str> = evs
        .iter()
        .filter_map(|e| match &e.kind {
            EventKind::RemoteOp { host, op, .. } => {
                assert_eq!(host, "gpu-box");
                assert_eq!(e.node_id.as_ref(), Some(&node), "{op} is on the node");
                Some(op.as_str())
            }
            _ => None,
        })
        .collect();
    for want in [
        "sync.push",
        "fs.replace_atomic",
        "exec.start",
        "sync.changes",
        "sync.apply",
    ] {
        assert!(ops.contains(&want), "{want} missing from {ops:?}");
    }
    let states: Vec<&str> = evs
        .iter()
        .filter_map(|e| match &e.kind {
            EventKind::RemoteConnection { host, state, .. } => {
                assert_eq!(host, "gpu-box");
                Some(state.as_str())
            }
            _ => None,
        })
        .collect();
    assert_eq!(states.first(), Some(&"connecting"));
    assert!(states.contains(&"connected"), "{states:?}");
    assert_eq!(states.last(), Some(&"disconnected"), "{states:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failed_connection_is_recorded_on_the_node() {
    struct Down;
    #[async_trait]
    impl HostConnect for Down {
        async fn connect(&self, host: &str, _: Arc<dyn AuditSink>) -> Result<HostSession, String> {
            Err(format!("ssh: {host} is unreachable"))
        }
    }
    let t = tempfile::tempdir().unwrap();
    let recorder = RunRecorder::create(
        t.path(),
        RunId::new(),
        Arc::new(Redactor::default()),
        RecorderOptions::default(),
    )
    .await
    .unwrap();
    let node = NodeId::from("wf/x");
    let err = RemoteNode::open(&Down, "gpu-box", t.path(), &recorder, &node, 1)
        .await
        .unwrap_err();
    assert!(err.contains("unreachable"));
    recorder.flush().await.unwrap();
    let evs = read_run(recorder.dir()).unwrap().events;
    let last = evs
        .iter()
        .rev()
        .find_map(|e| match &e.kind {
            EventKind::RemoteConnection {
                host,
                state,
                detail,
            } => Some((
                host.clone(),
                state.clone(),
                detail.clone(),
                e.node_id.clone(),
            )),
            _ => None,
        })
        .unwrap();
    assert_eq!(last.0, "gpu-box");
    assert_eq!(last.1, "failed");
    assert!(last.2.unwrap().contains("unreachable"));
    assert_eq!(last.3, Some(node));
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
