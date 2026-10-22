//! Live streams and the persisted event log show identical content after
//! the turn completes (TASK-120 AC#6).

use std::sync::Arc;

use harness_events::redact::Redactor;
use harness_events::schema::{DeltaChannel, EventKind, NodeId, NodeKind, RunEvent, RunId, Usage};
use harness_events::store::{EventDraft, RecorderOptions, RunRecorder, read_run};
use harness_tui::demo::events::T0;
use harness_tui::detail::DetailView;
use harness_tui::interaction::testing::buffer_text;
use harness_tui::map::LiveStreams;
use harness_tui::theme::Theme;
use ratatui::Terminal;
use ratatui::backend::TestBackend;

fn node(kind: EventKind) -> EventDraft {
    EventDraft::node(NodeId::from("a"), kind)
}

fn detail_text(events: &[RunEvent]) -> String {
    let mut d = DetailView::from_events(
        NodeId::from("a"),
        Theme::default(),
        Default::default(),
        false,
        events,
    );
    let mut term = Terminal::new(TestBackend::new(100, 30)).unwrap();
    term.draw(|f| d.render(f, f.area(), T0)).unwrap();
    buffer_text(term.backend().buffer())
}

#[tokio::test]
async fn live_and_logged_streams_match() {
    let dir = tempfile::tempdir().unwrap();
    let rec = RunRecorder::create(
        dir.path(),
        RunId::new(),
        Arc::new(Redactor::new(["hunter2-secret"])),
        RecorderOptions::default(),
    )
    .await
    .unwrap();
    let mut rx = rec.subscribe();
    rec.emit(node(EventKind::NodeStarted {
        kind: NodeKind::Agent,
        name: "a".into(),
        agent: None,
        model: Some("sonnet".into()),
        host: None,
        needs: vec![],
        queued: false,
    }))
    .unwrap();
    rec.emit(node(EventKind::ModelCallStarted {
        call_id: "c1".into(),
        provider: "fake".into(),
        model: "sonnet".into(),
        request: harness_events::schema::Payload::text("{}"),
    }))
    .unwrap();
    // Token-by-token deltas, including a secret split across deltas.
    for chunk in [
        "Thinking about ",
        "the parser",
        " and hunter",
        "2-secret",
        " now",
    ] {
        rec.emit(node(EventKind::Delta {
            call_id: "c1".into(),
            channel: DeltaChannel::Reasoning,
            text: chunk.into(),
        }))
        .unwrap();
    }
    for chunk in ["I'll ", "fix ", "parse_header", ".\nDone"] {
        rec.emit(node(EventKind::Delta {
            call_id: "c1".into(),
            channel: DeltaChannel::Text,
            text: chunk.into(),
        }))
        .unwrap();
    }
    rec.emit(node(EventKind::ModelCallFinished {
        call_id: "c1".into(),
        usage: Usage {
            input: 10,
            cached_input: 0,
            output: 20,
            reasoning: 5,
        },
        stop_reason: Some("end_turn".into()),
        response: None,
        duration_ms: 42,
    }))
    .unwrap();
    rec.finish().await.unwrap();

    let mut live_events = Vec::new();
    while let Ok(e) = rx.try_recv() {
        live_events.push(e);
    }
    let logged = read_run(rec.dir()).unwrap().events;

    let mut live = LiveStreams::new();
    for e in &live_events {
        live.apply(e);
    }
    let mut replay = LiveStreams::new();
    for e in &logged {
        replay.apply(e);
    }
    let a = live.stream(&NodeId::from("a")).unwrap().transcript();
    let b = replay.stream(&NodeId::from("a")).unwrap().transcript();
    assert_eq!(a, b);
    assert!(a.contains("I'll fix parse_header"), "{a}");
    assert!(!a.contains("hunter2-secret"), "redacted in both: {a}");
    assert_eq!(detail_text(&live_events), detail_text(&logged));
}
