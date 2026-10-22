use super::*;
use harness_events::schema::{NodeId, NodeKind, Payload};
use serde_json::json;
use time::macros::datetime;

struct Log {
    run: RunId,
    seq: u64,
    events: Vec<RunEvent>,
}

impl Log {
    fn new() -> Self {
        Self {
            run: RunId::new(),
            seq: 0,
            events: Vec::new(),
        }
    }

    fn push(&mut self, ts: OffsetDateTime, node: Option<&str>, kind: EventKind) {
        self.events.push(RunEvent {
            seq: self.seq,
            ts,
            run_id: self.run,
            node_id: node.map(NodeId::from),
            parent_node_id: None,
            kind,
        });
        self.seq += 1;
    }

    fn node(&mut self, ts: OffsetDateTime, node: &str, agent: &str) {
        self.push(
            ts,
            Some(node),
            EventKind::NodeStarted {
                kind: NodeKind::Agent,
                name: agent.into(),
                agent: Some(agent.into()),
                model: None,
                host: None,
                needs: vec![],
                queued: false,
            },
        );
    }

    fn call(
        &mut self,
        ts: OffsetDateTime,
        node: &str,
        model: &str,
        usage: Usage,
        cost: Option<f64>,
    ) {
        let call_id = format!("c{}", self.seq);
        self.push(
            ts,
            Some(node),
            EventKind::ModelCallStarted {
                call_id: call_id.clone(),
                provider: "anthropic".into(),
                model: model.into(),
                request: Payload::Json(json!({})),
            },
        );
        self.push(
            ts,
            Some(node),
            EventKind::ModelCallFinished {
                call_id,
                usage,
                stop_reason: None,
                response: None,
                duration_ms: 5,
            },
        );
        if let Some(usd) = cost {
            self.push(ts, Some(node), EventKind::Cost { usd });
        }
    }
}

fn usage(input: u64, cached: u64, output: u64) -> Usage {
    Usage {
        input,
        cached_input: cached,
        output,
        reasoning: 0,
    }
}

#[test]
fn records_usage_per_call_and_aggregates_per_node_run_and_day() {
    let d1 = datetime!(2026-10-21 23:50 UTC);
    let d2 = datetime!(2026-10-22 00:10 UTC);
    let mut a = Log::new();
    a.node(d1, "n1", "coder");
    a.call(d1, "n1", "m1", usage(100, 0, 10), Some(0.5));
    a.call(d2, "n1", "m1", usage(20, 100, 5), Some(0.25));
    a.node(d2, "n2", "reviewer");
    a.call(d2, "n2", "m2", usage(50, 0, 50), None);
    let mut b = Log::new();
    b.node(d2, "n3", "coder");
    b.call(d2, "n3", "m1", usage(10, 0, 1), Some(1.0));

    let ledger = UsageLedger::from_events(a.events.iter().chain(&b.events));
    // Per call.
    let calls = ledger.calls();
    assert_eq!(calls.len(), 4);
    assert_eq!(calls[1].usage, usage(20, 100, 5));
    assert_eq!(calls[1].cost_usd, Some(0.25));
    assert_eq!(calls[1].agent.as_deref(), Some("coder"));
    assert_eq!(calls[2].cost_usd, None);
    assert!((calls[1].cache_hit_rate().unwrap() - 100.0 / 120.0).abs() < 1e-9);
    // Per node.
    let nodes = ledger.by_node();
    assert_eq!(nodes["n1"].calls, 2);
    assert!((nodes["n1"].cost_usd - 0.75).abs() < 1e-9);
    assert!(nodes["n2"].cost_unknown());
    // Per run (and so per session of two runs).
    let runs = ledger.by_run();
    assert_eq!(runs.len(), 2);
    assert_eq!(runs[&b.run.to_string()].calls, 1);
    let total = ledger.total();
    assert_eq!(total.calls, 4);
    assert!((total.cost_usd - 1.75).abs() < 1e-9);
    assert_eq!(total.cost_label(), "$1.7500+?");
    // Per day.
    let days = ledger.by_day();
    assert_eq!(days[&d1.date()].calls, 1);
    assert_eq!(days[&d2.date()].calls, 3);
    assert!((ledger.day_total(d2.date()).cost_usd - 1.25).abs() < 1e-9);
}

#[test]
fn tracks_context_window_use_per_node() {
    let t = datetime!(2026-10-22 10:00 UTC);
    let mut a = Log::new();
    a.node(t, "n1", "coder");
    a.call(t, "n1", "m", usage(1000, 0, 10), None);
    a.call(t, "n1", "m", usage(200, 3000, 10), None);
    a.call(t, "n1", "m", usage(100, 1500, 10), None);
    let ctx = UsageLedger::from_events(&a.events).context_by_node();
    let n1 = &ctx["n1"];
    assert_eq!(n1.last_tokens, 1600);
    assert_eq!(n1.peak_tokens, 3200);
    assert_eq!(n1.calls, 3);
    assert_eq!(n1.fraction(3200), Some(0.5));
}

#[test]
fn cost_report_breaks_down_by_model_agent_and_node() {
    let t = datetime!(2026-10-22 10:00 UTC);
    let mut a = Log::new();
    a.node(t, "aaaaaaaaaaaa", "coder");
    a.call(t, "aaaaaaaaaaaa", "opus", usage(10, 0, 10), Some(2.0));
    a.node(t, "bbbbbbbbbbbb", "reviewer");
    a.call(t, "bbbbbbbbbbbb", "haiku", usage(10, 0, 10), Some(0.1));
    let report = CostReport::new(&UsageLedger::from_events(&a.events));
    assert_eq!(report.by_model[0].label, "anthropic/opus");
    assert_eq!(report.by_agent[0].label, "coder");
    assert_eq!(report.by_agent[1].label, "reviewer");
    assert_eq!(report.by_node[0].label, "coder (aaaaaaaa)");
    let text = report.lines(None).join("\n");
    assert!(text.starts_with("total: $2.1000"), "{text}");
    assert!(text.contains("by model:") && text.contains("by agent:") && text.contains("by node:"));
    let only = report.lines(CostGroup::parse("agent"));
    assert!(only.iter().any(|l| l == "by agent:"));
    assert!(!only.iter().any(|l| l == "by model:"));
}

#[tokio::test]
async fn daily_spend_reads_every_run_of_the_day() {
    use harness_events::redact::Redactor;
    use harness_events::store::{EventDraft, RecorderOptions, RunRecorder};
    use std::sync::Arc;
    let dir = tempfile::tempdir().unwrap();
    for usd in [1.0, 2.5] {
        let rec = RunRecorder::create(
            dir.path(),
            RunId::new(),
            Arc::new(Redactor::default()),
            RecorderOptions::default(),
        )
        .await
        .unwrap();
        let node = NodeId::from("n");
        for kind in [
            EventKind::ModelCallStarted {
                call_id: "c".into(),
                provider: "p".into(),
                model: "m".into(),
                request: Payload::Json(json!({})),
            },
            EventKind::ModelCallFinished {
                call_id: "c".into(),
                usage: usage(1, 0, 1),
                stop_reason: None,
                response: None,
                duration_ms: 1,
            },
            EventKind::Cost { usd },
        ] {
            rec.emit(EventDraft::node(node.clone(), kind)).unwrap();
        }
        rec.finish().await.unwrap();
    }
    let today = OffsetDateTime::now_utc().date();
    let spent = daily_spend(dir.path(), today).unwrap();
    assert_eq!(spent.calls, 2);
    assert!((spent.cost_usd - 3.5).abs() < 1e-9);
    let other = daily_spend(dir.path(), today.previous_day().unwrap()).unwrap();
    assert_eq!(other.calls, 0);
}
