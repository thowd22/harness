//! Remote operations and connection changes reach the run log, tagged with
//! the host.

use std::sync::Arc;

use harness_events::redact::Redactor;
use harness_events::schema::{EventKind, NodeId, RunId};
use harness_events::store::{RecorderOptions, RunRecorder, read_run};
use harness_remote::audit::{RecorderAudit, record_connection};
use harness_remote::client::{ClientOptions, InProcessConnector, RemoteClient};
use harness_remote::helper::{Helper, HelperOptions};
use harness_remote::workspace::RemoteWorkspace;
use harness_tools::workspace::Workspace;

#[tokio::test(flavor = "multi_thread")]
async fn remote_ops_and_connection_changes_are_recorded_with_the_host() {
    let data = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    std::fs::write(work.path().join("a.txt"), "hello\n").unwrap();
    let recorder = RunRecorder::create(
        data.path(),
        RunId::new(),
        Arc::new(Redactor::default()),
        RecorderOptions::default(),
    )
    .await
    .unwrap();
    let node = NodeId::from("step/remote");
    let opts = ClientOptions::new("gpu-box").with_audit(Arc::new(RecorderAudit::new(
        recorder.clone(),
        Some(node.clone()),
    )));
    let connector = Arc::new(InProcessConnector::new(Helper::new(
        HelperOptions::default(),
    )));
    let client = RemoteClient::connect(connector.clone(), opts)
        .await
        .unwrap();
    let watch = record_connection(&client, recorder.clone());
    let ws = RemoteWorkspace::new(client.clone(), work.path().to_str().unwrap());
    assert_eq!(
        ws.read(std::path::Path::new("a.txt")).await.unwrap(),
        b"hello\n"
    );
    assert!(ws.read(std::path::Path::new("missing.txt")).await.is_err());
    // A dropped connection is reconnected and recorded.
    connector.disconnect_all();
    assert!(ws.read(std::path::Path::new("a.txt")).await.is_ok());
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    client.close();
    recorder.finish().await.unwrap();
    watch.abort();

    let events = read_run(recorder.dir()).unwrap().events;
    let ops: Vec<(String, String, bool)> = events
        .iter()
        .filter_map(|e| match &e.kind {
            EventKind::RemoteOp { host, op, ok, .. } => {
                assert_eq!(e.node_id.as_ref(), Some(&node));
                Some((host.clone(), op.clone(), *ok))
            }
            _ => None,
        })
        .collect();
    assert!(ops.iter().all(|(h, _, _)| h == "gpu-box"), "{ops:?}");
    assert!(
        ops.iter().any(|(_, op, ok)| op == "fs.read" && *ok),
        "{ops:?}"
    );
    assert!(
        ops.iter().any(|(_, op, ok)| op == "fs.read" && !*ok),
        "{ops:?}"
    );
    let states: Vec<String> = events
        .iter()
        .filter_map(|e| match &e.kind {
            EventKind::RemoteConnection { host, state, .. } => {
                assert_eq!(host, "gpu-box");
                Some(state.clone())
            }
            _ => None,
        })
        .collect();
    assert!(
        states
            .iter()
            .any(|s| s == "disconnected" || s == "reconnecting"),
        "{states:?}"
    );
    assert!(states.iter().any(|s| s == "connected"), "{states:?}");
}
