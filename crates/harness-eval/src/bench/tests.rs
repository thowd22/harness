use super::*;
use harness_events::schema::{NodeId, Payload, RunId};
use harness_providers::model::Usage;

const EPOCH: u64 = 1_800_000_000_000_000;

fn ev(us: u64, kind: EventKind) -> RunEvent {
    RunEvent {
        seq: 0,
        ts: time::OffsetDateTime::from_unix_timestamp_nanos(((EPOCH + us) * 1000) as i128).unwrap(),
        run_id: RunId::new(),
        node_id: Some(NodeId::random()),
        parent_node_id: None,
        kind,
    }
}

fn span(name: &str, a: u64, b: u64) -> Span {
    Span {
        name: name.into(),
        start_us: a,
        end_us: b,
    }
}

fn call(index: usize, start: u64, end: u64) -> CallTiming {
    CallTiming {
        index,
        model: "m".into(),
        start_us: start,
        first_event_us: Some(start + 500),
        first_content_us: Some(start + 1000),
        end_us: Some(end),
        usage: Some(Usage {
            output_tokens: 100,
            input_tokens: 10,
            cache_read_tokens: 90,
            ..Usage::default()
        }),
        ..CallTiming::default()
    }
}

fn fixture() -> (ProbeData, Vec<RunEvent>) {
    let mut c0 = call(0, 1000, 3000);
    c0.tool_block_stops_us = vec![2500];
    let compaction = call(1, 4350, 4450);
    let c1 = call(2, 4800, 6000);
    let probe = ProbeData {
        epoch_unix_us: EPOCH,
        calls: vec![c0, compaction, c1],
        spans: vec![span("hook:context", 4300, 4500), span("turn:1", 0, 10_000)],
    };
    let events = vec![
        ev(
            3100,
            EventKind::ModelCallFinished {
                call_id: "c0".into(),
                usage: Default::default(),
                stop_reason: None,
                response: None,
                duration_ms: 2,
            },
        ),
        ev(
            3200,
            EventKind::ToolCall {
                call_id: "t1".into(),
                tool: "read".into(),
                args: Payload::Text(String::new()),
            },
        ),
        ev(
            4200,
            EventKind::ToolResult {
                call_id: "t1".into(),
                ok: true,
                output: Payload::Text(String::new()),
                duration_ms: 1,
            },
        ),
        ev(
            4700,
            EventKind::ModelCallStarted {
                call_id: "c1".into(),
                provider: "fake".into(),
                model: "m".into(),
                request: Payload::Text(String::new()),
            },
        ),
        ev(6500, EventKind::Cost { usd: 0.25 }),
    ];
    (probe, events)
}

fn close(a: f64, b: f64) {
    assert!((a - b).abs() < 1e-6, "{a} != {b}");
}

#[test]
fn analyze_splits_calls_gaps_and_turns() {
    let (probe, events) = fixture();
    let r = analyze("x1", &probe, &events);
    assert_eq!(r.calls.len(), 3);
    let c0 = &r.calls[0];
    assert_eq!((c0.kind.as_str(), c0.turn), ("agent", 1));
    close(c0.ttfb_ms.unwrap(), 0.5);
    close(c0.ttft_ms.unwrap(), 1.0);
    close(c0.total_ms.unwrap(), 2.0);
    close(c0.output_tps.unwrap(), 100_000.0);
    close(c0.tool_early_start_ms.unwrap(), 0.5);
    assert_eq!(r.calls[1].kind, "compaction");

    assert_eq!(r.gaps.len(), 2);
    let start = &r.gaps[0];
    assert!(start.turn_start);
    close(start.gap_ms, 1.0);
    let g = &r.gaps[1];
    assert!(!g.turn_start);
    assert_eq!(g.before_call, 2);
    close(g.gap_ms, 1.8);
    close(g.tools_ms, 1.0);
    close(g.tool_sum_ms, 1.0);
    assert_eq!(g.tool_calls, 1);
    close(g.hooks_ms, 0.2);
    close(g.compaction_ms, 0.1);
    close(g.overhead_ms, 0.6);
    close(g.finish_ms.unwrap(), 0.1);
    close(g.build_ms.unwrap(), 0.2);
    close(g.send_ms.unwrap(), 0.1);

    let t = &r.turns[0];
    close(t.wall_ms, 10.0);
    close(t.model_ms, 3.2);
    close(t.tools_ms, 1.0);
    close(t.hooks_ms, 0.2);
    close(t.overhead_ms, 5.6);
    close(t.start_ms, 1.0);
    close(t.end_ms, 4.0);
    assert_eq!(t.calls, 3);

    assert_eq!(r.tools.len(), 1);
    assert_eq!((r.tools[0].tool.as_str(), r.tools[0].calls), ("read", 1));
    assert_eq!(r.hooks[0].hook, "context");
    close(r.cost_usd, 0.25);
}

#[test]
fn a_background_compaction_counts_only_while_the_hook_waits() {
    // The summary starts inside the context hook and keeps running past it,
    // during the next agent call: only the 0.15 ms inside the hook block.
    let (mut probe, events) = fixture();
    probe.calls[1].end_us = Some(9000);
    let r = analyze("x1", &probe, &events);
    assert_eq!(r.calls[1].kind, "compaction");
    close(r.gaps[1].compaction_ms, 0.15);
    let s = summarize(&[r]);
    close(s.compaction_call_ms.unwrap().p50, 4.65);
    close(s.compaction_blocking_ms.unwrap().p50, 0.15);
}

#[test]
fn summarize_takes_percentiles_and_counts_prefix_breaks() {
    let (mut probe, events) = fixture();
    probe.calls[2].body = Some(BodySizes {
        prefix_shared: 50,
        prefix_prev_total: 100,
        prefix_total: 150,
        ..BodySizes::default()
    });
    let r = analyze("x1", &probe, &events);
    let s = summarize(&[r.clone(), r]);
    assert_eq!(s.ttfb_ms.as_ref().unwrap().n, 4);
    close(s.ttfb_ms.unwrap().p50, 0.5);
    close(s.overhead_ms.unwrap().p50, 0.6);
    close(s.prefix_shared_pct.unwrap().p50, 50.0);
    assert_eq!(s.prefix_breaks, 2);
    // Calls after the first: 90 of 100 input tokens from the cache.
    close(s.cache_hit_pct.unwrap().p50, 90.0);
    close(s.cost_usd, 0.5);
}

#[test]
fn stat_percentiles() {
    let s = Stat::of(&[5.0, 1.0, 3.0, 2.0, 4.0]).unwrap();
    assert_eq!((s.p50, s.min, s.max, s.n), (3.0, 1.0, 5.0, 5));
    close(s.mean, 3.0);
    assert!(Stat::of(&[]).is_none());
}

#[test]
fn table_lists_runs_and_summary() {
    let (probe, events) = fixture();
    let run = analyze("x1", &probe, &events);
    let report = BenchReport {
        schema: BENCH_SCHEMA,
        mode: "fake".into(),
        summary: summarize(std::slice::from_ref(&run)),
        runs: vec![run],
        ..BenchReport::default()
    };
    let t = render_table(&report);
    for needle in [
        "run x1",
        "ttfb ms",
        "turn-start",
        "turn 1: wall 10.0 ms",
        "summary over 1 run(s)",
    ] {
        assert!(t.contains(needle), "{needle}: {t}");
    }
}

#[test]
fn scenario_has_two_prompts_a_parsable_script_and_writes_the_repo() {
    assert_eq!(scenario::prompts().len(), 2);
    let script = harness_providers::fake::FakeScript::from_yaml(scenario::FAKE_SCRIPT).unwrap();
    assert_eq!(script.responses.len(), 10);
    let dir = tempfile::tempdir().unwrap();
    scenario::write_repo(dir.path()).unwrap();
    let stock = std::fs::read_to_string(dir.path().join("inventory/stock.py")).unwrap();
    // The bug the first prompt asks to fix, exactly as the fake edit expects.
    assert!(stock.contains("total += self._items[sku].unit_price_cents\n"));
    assert!(dir.path().join("tests/test_stock.py").exists());
    let compaction =
        harness_providers::fake::FakeScript::from_yaml(scenario::COMPACTION_SCRIPT).unwrap();
    assert_eq!(compaction.capabilities.unwrap().context_window, 20_000);
    assert_eq!(
        compaction
            .responses
            .iter()
            .filter(|t| t.match_system.is_some())
            .count(),
        1,
        "one handoff summary reply"
    );
}

#[tokio::test]
async fn emit_costs_are_grouped_by_event_type() {
    let (_, events) = fixture();
    let costs = emit_costs(&events, Arc::new(Redactor::default())).await;
    let names: Vec<&str> = costs.iter().map(|c| c.event.as_str()).collect();
    assert!(
        names.contains(&"tool_call") && names.contains(&"cost"),
        "{names:?}"
    );
    assert_eq!(costs.iter().map(|c| c.count).sum::<u32>(), 5);
}
