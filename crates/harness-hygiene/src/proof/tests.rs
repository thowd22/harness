use super::*;
use crate::events::RecordingSink;
use harness_events::schema::{RunEvent, RunId};
use harness_tools::local::LocalExec;
use std::path::Path;
use std::time::Duration;

fn write(root: &Path, rel: &str, text: &str) {
    let p = root.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, text).unwrap();
}

fn read(root: &Path, rel: &str) -> String {
    std::fs::read_to_string(root.join(rel)).unwrap()
}

struct Fixture {
    work: tempfile::TempDir,
    _data: tempfile::TempDir,
    store: Arc<CheckpointStore>,
    original: u64,
}

/// `src/value.txt` holds the "code"; the test passes when it says `fixed`.
/// `test.sh` is the repro test, written before the fix.
fn fixture(test: &str) -> Fixture {
    let work = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    let root = work.path();
    write(root, "src/value.txt", "buggy\n");
    write(root, "tests/test.sh", test);
    let store = Arc::new(CheckpointStore::open(data.path(), root).unwrap());
    let original = store
        .snapshot(
            CheckpointKind::Turn,
            Some(&NodeId::from("fix")),
            Some(1),
            "before fix",
        )
        .unwrap()
        .id;
    // The fix (and a tweak to the test, which must be kept).
    write(root, "src/value.txt", "fixed\n");
    write(root, "tests/test.sh", &format!("{test}# tweaked\n"));
    Fixture {
        work,
        _data: data,
        store,
        original,
    }
}

const GOOD_TEST: &str = "grep -q fixed src/value.txt\n";

fn proof(f: &Fixture, test_cmd: &str, sink: Arc<RecordingSink>) -> BugfixProof {
    let wt =
        CheckpointWorktree::prepare(f.store.clone(), f.original, &["tests/**".into()]).unwrap();
    assert_eq!(wt.paths(), ["src/value.txt".to_string()]);
    let runner = VerifyRunner::new(Arc::new(LocalExec::new()), f.work.path())
        .with_timeout(Duration::from_secs(30))
        .with_sink(sink);
    BugfixProof::new(runner, Arc::new(wt), test_cmd, NodeId::from("proof"))
}

#[tokio::test]
async fn proves_a_real_fix_and_restores_it() {
    let f = fixture(GOOD_TEST);
    let sink = Arc::new(RecordingSink::default());
    let p = proof(&f, "sh tests/test.sh", sink.clone());
    let report = p.run(&CancellationToken::new()).await.unwrap();
    assert!(report.proven, "{report:?}");
    let got: Vec<(Phase, Option<bool>)> = report
        .phases
        .iter()
        .map(|r| (r.phase, r.run.as_ref().map(|x| x.passed)))
        .collect();
    assert_eq!(
        got,
        vec![
            (Phase::Original, Some(false)),
            (Phase::Fixed, Some(true)),
            (Phase::Reverted, Some(false)),
            (Phase::Restore, None),
        ]
    );
    assert_eq!(read(f.work.path(), "src/value.txt"), "fixed\n");
    assert!(read(f.work.path(), "tests/test.sh").contains("# tweaked"));
    assert_eq!(report.outputs()["proven"], true);
}

#[tokio::test]
async fn a_test_that_always_passes_is_not_proof() {
    let f = fixture("true\n");
    let p = proof(&f, "sh tests/test.sh", Arc::new(RecordingSink::default()));
    let report = p.run(&CancellationToken::new()).await.unwrap();
    assert!(!report.proven);
    assert!(!report.phases[0].ok && report.phases[1].ok && !report.phases[2].ok);
    assert_eq!(read(f.work.path(), "src/value.txt"), "fixed\n");
}

#[tokio::test]
async fn each_phase_is_a_visible_child_node_with_logs() {
    let f = fixture(GOOD_TEST);
    let sink = Arc::new(RecordingSink::default());
    let p = proof(
        &f,
        "sh tests/test.sh; rc=$?; echo phase-log; exit $rc",
        sink.clone(),
    );
    p.run(&CancellationToken::new()).await.unwrap();

    // Replay the drafts through a real recorder and the reducer.
    let data = tempfile::tempdir().unwrap();
    let run_id = RunId::new();
    let rec = harness_events::store::RunRecorder::create(
        data.path(),
        run_id,
        Arc::new(harness_events::redact::Redactor::default()),
        Default::default(),
    )
    .await
    .unwrap();
    rec.emit(harness_events::store::EventDraft::node(
        "proof",
        EventKind::NodeStarted {
            kind: NodeKind::Workflow,
            name: "proof".into(),
            agent: None,
            model: None,
            host: None,
            needs: vec![],
            queued: false,
        },
    ))
    .unwrap();
    for d in sink.drafts() {
        rec.emit(d).unwrap();
    }
    rec.finish().await.unwrap();
    let replay =
        harness_events::store::read_run(&harness_events::store::run_dir(data.path(), run_id))
            .unwrap();
    let state = harness_events::reducer::RunState::from_events(&replay.events);
    let parent = state.node(&NodeId::from("proof")).unwrap();
    let kids: Vec<&str> = parent.children.iter().map(NodeId::as_str).collect();
    assert_eq!(
        kids,
        vec![
            "proof.original",
            "proof.fixed",
            "proof.reverted",
            "proof.restore"
        ]
    );
    for (id, verify_ok) in [
        ("proof.original", Some(false)),
        ("proof.fixed", Some(true)),
        ("proof.reverted", Some(false)),
        ("proof.restore", None),
    ] {
        let n = state.node(&NodeId::from(id)).unwrap();
        assert_eq!(n.status, NodeStatus::Succeeded, "{id}");
        assert_eq!(n.last_verify_passed, verify_ok, "{id}");
    }
    // Logs: each test phase has its own verify output.
    let logs: Vec<&RunEvent> = replay
        .events
        .iter()
        .filter(|e| matches!(&e.kind, EventKind::VerifyFinished { output, .. } if matches!(output, harness_events::schema::Payload::Text(t) if t.contains("phase-log"))))
        .collect();
    assert_eq!(logs.len(), 3);
}

#[tokio::test]
async fn cancel_restores_the_fix() {
    let f = fixture(GOOD_TEST);
    let sink = Arc::new(RecordingSink::default());
    // The original phase blocks; cancel while it runs.
    let p = proof(
        &f,
        "if grep -q buggy src/value.txt; then sleep 30; fi; sh tests/test.sh",
        sink.clone(),
    );
    let cancel = CancellationToken::new();
    let c2 = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(300)).await;
        c2.cancel();
    });
    let started = std::time::Instant::now();
    let err = p.run(&cancel).await.unwrap_err();
    assert!(matches!(err, ProofError::Cancelled));
    assert!(started.elapsed() < Duration::from_secs(10));
    assert_eq!(read(f.work.path(), "src/value.txt"), "fixed\n");
    assert!(sink.kinds().iter().any(|k| matches!(
        k,
        EventKind::NodeFinished {
            status: NodeStatus::Cancelled,
            ..
        }
    )));
}

#[tokio::test]
async fn dropping_the_proof_future_restores_the_fix() {
    let f = fixture(GOOD_TEST);
    let p = proof(
        &f,
        "if grep -q buggy src/value.txt; then sleep 30; fi",
        Arc::new(RecordingSink::default()),
    );
    let cancel = CancellationToken::new();
    let r = tokio::time::timeout(Duration::from_millis(300), p.run(&cancel)).await;
    assert!(r.is_err(), "timed out and dropped");
    assert_eq!(read(f.work.path(), "src/value.txt"), "fixed\n");
}

#[test]
fn no_fix_is_an_error() {
    let work = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    write(work.path(), "a.txt", "x");
    let store = Arc::new(CheckpointStore::open(data.path(), work.path()).unwrap());
    let cp = store
        .snapshot(CheckpointKind::Manual, None, None, "o")
        .unwrap()
        .id;
    assert!(matches!(
        CheckpointWorktree::prepare(store, cp, &[]),
        Err(ProofError::NoFix)
    ));
}
