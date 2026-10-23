//! Reducer tests: snapshot of a recorded run, determinism, live-vs-replay
//! equality and O(1) amortized apply.

use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use harness_events::redact::Redactor;
use harness_events::reducer::{EdgeKind, RunState};
use harness_events::schema::{
    EventKind, NodeId, NodeKind, NodeStatus, RunEvent, RunId, RunStatus, Usage,
};
use harness_events::store::{EventDraft, RecorderOptions, RunRecorder, read_run};

fn fixture() -> Vec<RunEvent> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/fix-bug");
    let replay = read_run(&dir).unwrap();
    assert!(!replay.truncated);
    replay.events
}

#[test]
fn recorded_run_snapshot() {
    let state = RunState::from_events(&fixture());
    insta::assert_yaml_snapshot!("fix_bug_run_state", state);
}

#[test]
fn recorded_run_has_expected_shape() {
    let s = RunState::from_events(&fixture());
    assert_eq!(s.status, RunStatus::Failed);
    let ids: Vec<&str> = s.nodes.iter().map(|n| n.id.as_str()).collect();
    assert_eq!(ids, ["repro", "prove-fail", "fix", "proof", "r1", "r2"]);
    let needs = s.edges.iter().filter(|e| e.kind == EdgeKind::Needs).count();
    let spawns = s.edges.iter().filter(|e| e.kind == EdgeKind::Spawn).count();
    assert_eq!((needs, spawns), (3, 2));

    let fix = s.node(&"fix".into()).unwrap();
    assert_eq!(fix.children, vec![NodeId::from("r1"), NodeId::from("r2")]);
    assert_eq!(fix.host.as_deref(), Some("gpu-box"));
    assert_eq!(fix.tokens.input, 25400);
    assert!((fix.cost_usd - 0.31).abs() < 1e-9);
    assert_eq!(fix.duration_ms, Some(62_000));

    let prove = s.node(&"prove-fail".into()).unwrap();
    assert_eq!((prove.retries, prove.last_verify_passed), (1, Some(true)));
    assert_eq!(
        s.node(&"r1".into())
            .unwrap()
            .parent
            .as_ref()
            .unwrap()
            .as_str(),
        "fix"
    );
    assert_eq!(s.node(&"proof".into()).unwrap().status, NodeStatus::Failed);
    assert!((s.cost_usd - 0.40).abs() < 1e-9);
    assert_eq!(s.roots().count(), 4);
}

#[test]
fn reducer_is_deterministic() {
    let events = fixture();
    let a = RunState::from_events(&events);
    let b = RunState::from_events(&events);
    assert_eq!(a, b);
    assert_eq!(
        serde_json::to_string(&a).unwrap(),
        serde_json::to_string(&b).unwrap()
    );

    // Incremental application gives the same result as a fresh fold of each prefix.
    let mut live = RunState::new();
    for (i, e) in events.iter().enumerate() {
        live.apply(e);
        if i % 10 == 0 {
            assert_eq!(live, RunState::from_events(&events[..=i]));
        }
    }
    assert_eq!(live, a);
}

#[tokio::test]
async fn live_fold_equals_replay_fold() {
    let tmp = tempfile::tempdir().unwrap();
    let rec = RunRecorder::create(
        tmp.path(),
        RunId::new(),
        Arc::new(Redactor::default()),
        RecorderOptions::default(),
    )
    .await
    .unwrap();
    let mut live = RunState::new();
    let mut emit = |d: EventDraft| {
        for e in rec.emit(d).unwrap() {
            live.apply(&e);
        }
    };
    emit(EventDraft::run(EventKind::RunStarted {
        name: "r".into(),
        workflow: None,
        host: None,
        title: None,
        agent_names: Default::default(),
    }));
    emit(EventDraft::node(
        "a",
        EventKind::NodeStarted {
            kind: NodeKind::Agent,
            name: "a".into(),
            agent: None,
            model: Some("m".into()),
            host: None,
            needs: vec![],
            queued: false,
        },
    ));
    emit(EventDraft::node(
        "a",
        EventKind::ModelCallFinished {
            call_id: "c".into(),
            usage: Usage {
                input: 10,
                cached_input: 0,
                output: 5,
                reasoning: 0,
                cache_write: 0,
            },
            stop_reason: None,
            response: None,
            duration_ms: 1,
        },
    ));
    emit(EventDraft::node(
        "a",
        EventKind::NodeFinished {
            status: NodeStatus::Succeeded,
            error: None,
        },
    ));
    emit(EventDraft::run(EventKind::RunFinished {
        status: RunStatus::Succeeded,
        error: None,
    }));
    rec.finish().await.unwrap();

    let replayed = RunState::from_events(&read_run(rec.dir()).unwrap().events);
    assert_eq!(live, replayed);
}

fn synthetic(n_nodes: usize, per_node: usize) -> Vec<RunEvent> {
    let run_id = RunId(uuid::Uuid::nil());
    let ts = time::macros::datetime!(2026-10-22 12:00:00 UTC);
    let mut seq = 0;
    let mut out = Vec::with_capacity(n_nodes * (per_node + 1));
    let mut push = |node: usize, parent: Option<usize>, kind| {
        out.push(RunEvent {
            seq,
            ts,
            run_id,
            node_id: Some(NodeId(format!("n{node}"))),
            parent_node_id: parent.map(|p| NodeId(format!("n{p}"))),
            kind,
        });
        seq += 1;
    };
    for i in 0..n_nodes {
        push(
            i,
            (i > 0).then_some(i / 2),
            EventKind::NodeStarted {
                kind: NodeKind::Subagent,
                name: format!("n{i}"),
                agent: None,
                model: None,
                host: None,
                needs: if i > 0 {
                    vec![NodeId(format!("n{}", i - 1))]
                } else {
                    vec![]
                },
                queued: false,
            },
        );
    }
    for j in 0..per_node {
        for i in 0..n_nodes {
            push(
                i,
                None,
                if j % 2 == 0 {
                    EventKind::Cost { usd: 0.001 }
                } else {
                    EventKind::ToolCall {
                        call_id: format!("t{j}"),
                        tool: "bash".into(),
                        args: harness_events::schema::Payload::text("ls"),
                    }
                },
            );
        }
    }
    out
}

fn ns_per_event(events: &[RunEvent]) -> f64 {
    // Best of three to reduce noise from other processes.
    (0..3)
        .map(|_| {
            let start = Instant::now();
            let s = RunState::from_events(events);
            std::hint::black_box(&s);
            start.elapsed().as_nanos() as f64 / events.len() as f64
        })
        .fold(f64::INFINITY, f64::min)
}

#[test]
fn apply_is_constant_time_per_event() {
    let small = synthetic(500, 20);
    let large = synthetic(8000, 20);
    let (s, l) = (ns_per_event(&small), ns_per_event(&large));
    // 16x more nodes and events: linear-time apply would be ~16x slower per
    // event. Allow generous noise for hashing/cache effects.
    assert!(l < s * 5.0, "per-event cost grew from {s:.0}ns to {l:.0}ns");
    let state = RunState::from_events(&large);
    assert_eq!(state.nodes.len(), 8000);
    assert_eq!(state.edges.len(), 2 * 7999);
}
