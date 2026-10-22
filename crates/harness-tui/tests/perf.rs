//! Frame-budget benchmarks for the map, node detail, live streams and the
//! goal bar (TASK-71, TASK-120 AC#4).
//!
//! Each benchmark renders steady-state frames into a ratatui `TestBackend`
//! and compares the median frame time with the budget: 16 ms in release
//! builds. Debug builds (plain `cargo test`, as CI runs it) are several
//! times slower, so the budget is scaled by `DEBUG_FACTOR` there; set
//! `HARNESS_PERF_BUDGET_MS` to override. A regression that pushes a frame
//! over the budget fails the test, which is the CI regression threshold.

use std::time::{Duration, Instant};

use harness_config::ini::config::GoalBar as GoalBarMode;
use harness_events::schema::{DeltaChannel, NodeStatus, RunEvent};
use harness_tui::demo::events::{EventBuilder, NodeSpec, T0, usage};
use harness_tui::detail::DetailView;
use harness_tui::goalbar::{GoalBar, Goals};
use harness_tui::map::{MapConfig, MapView};
use harness_tui::theme::Theme;
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::layout::Rect;
use serde_json::json;

/// Frame budget in release builds.
const BUDGET_MS: f64 = 16.0;
/// Debug builds are this much slower than release.
const DEBUG_FACTOR: f64 = 4.0;

fn budget() -> Duration {
    let ms = std::env::var("HARNESS_PERF_BUDGET_MS")
        .ok()
        .and_then(|v| v.parse::<f64>().ok())
        .unwrap_or(if cfg!(debug_assertions) {
            BUDGET_MS * DEBUG_FACTOR
        } else {
            BUDGET_MS
        });
    Duration::from_secs_f64(ms / 1000.0)
}

fn median(mut v: Vec<Duration>) -> Duration {
    v.sort();
    v[v.len() / 2]
}

fn check(name: &str, frames: Vec<Duration>) {
    let m = median(frames);
    println!(
        "{name}: median frame {:.2} ms (budget {:.1} ms)",
        m.as_secs_f64() * 1e3,
        budget().as_secs_f64() * 1e3
    );
    assert!(
        m <= budget(),
        "{name}: {m:?} over the {:?} frame budget",
        budget()
    );
}

fn big_run(nodes: usize) -> Vec<RunEvent> {
    let mut b = EventBuilder::new();
    b.run_started("big", Some("big"));
    // A 10-wide matrix of layers, each step needing one of the previous
    // layer, plus a subagent on every fifth step.
    for i in 0..nodes {
        let id = format!("s{i}");
        let needs: Vec<String> = if i >= 10 {
            vec![format!("s{}", i - 10 + (i % 3))]
        } else {
            vec![]
        };
        let needs: Vec<&str> = needs.iter().map(String::as_str).collect();
        if i % 5 == 4 && i >= 10 {
            b.start(NodeSpec::subagent(&id, &format!("s{}", i - 1)).model("haiku"));
        } else {
            b.start(
                NodeSpec::step(&id)
                    .agent("worker")
                    .model("sonnet")
                    .needs(&needs),
            );
        }
        b.cost(&id, 0.01);
        if i % 2 == 0 {
            b.finish(&id, NodeStatus::Succeeded, None);
        }
    }
    b.into_events()
}

#[test]
fn map_renders_200_nodes_within_budget() {
    let events = big_run(200);
    let mut map = MapView::new(MapConfig::default());
    for e in &events {
        map.apply_run_event(e);
    }
    let mut term = Terminal::new(TestBackend::new(200, 60)).unwrap();
    let now = T0 + time::Duration::seconds(30);
    term.draw(|f| map.render(f, f.area(), now)).unwrap();
    let mut frames = Vec::new();
    for _ in 0..30 {
        map.tick();
        let t = Instant::now();
        term.draw(|f| map.render(f, f.area(), now)).unwrap();
        frames.push(t.elapsed());
    }
    assert_eq!(map.run_state().nodes.len(), 200);
    check("map 200 nodes", frames);
}

#[test]
fn detail_renders_10k_entries_within_budget() {
    let mut b = EventBuilder::new();
    b.start(NodeSpec::step("n"));
    // 10k log entries: 2.5k turns with 3 tool calls each, plus text.
    for i in 0..2_500 {
        let call = format!("c{i}");
        b.model_call("n", &call, "sonnet");
        b.delta("n", &call, DeltaChannel::Text, "working on it\n");
        b.finish_call("n", &call, usage(10, 100, 5, 0), 10);
        for k in 0..3 {
            let t = format!("t{i}.{k}");
            b.tool_call("n", &t, "read", json!({"path": "src/lib.rs"}));
            b.tool_result("n", &t, true, "fn main() {}", 1);
        }
    }
    let mut d = DetailView::from_events(
        "n".into(),
        Theme::default(),
        Default::default(),
        true,
        b.events(),
    );
    let mut term = Terminal::new(TestBackend::new(160, 50)).unwrap();
    let now = T0 + time::Duration::seconds(30);
    term.draw(|f| d.render(f, f.area(), now)).unwrap();
    let mut frames = Vec::new();
    for _ in 0..20 {
        d.tick();
        let t = Instant::now();
        term.draw(|f| d.render(f, f.area(), now)).unwrap();
        frames.push(t.elapsed());
    }
    assert!(d.group_count() >= 2_500);
    check("detail 10k entries", frames);
}

#[test]
fn ten_live_streams_and_the_goal_bar_render_within_budget() {
    let mut b = EventBuilder::new();
    b.run_started("parallel", None);
    b.start(NodeSpec::step("main"));
    for i in 0..10 {
        b.start(NodeSpec::subagent(&format!("a{i}"), "main").model("haiku"));
        b.model_call(&format!("a{i}"), &format!("c{i}"), "haiku");
    }
    b.tool_call(
        "main",
        "todo1",
        "todo",
        json!({"goal": "ship it", "todos": (0..10).map(|i| json!({"content": format!("task {i}"), "status": "in_progress"})).collect::<Vec<_>>()}),
    );
    let mut map = MapView::new(MapConfig::default());
    let mut goals = Goals::new();
    for e in b.events() {
        map.apply_run_event(e);
        goals.apply(e);
    }
    map.set_live_open(true);
    let mut bar = GoalBar::new(Theme::default(), GoalBarMode::Auto, 36, true);
    let mut term = Terminal::new(TestBackend::new(200, 60)).unwrap();
    let now = T0 + time::Duration::seconds(5);
    let mut frames = Vec::new();
    let mut n = b.events().len();
    for round in 0..40 {
        // Every agent streams a few tokens per frame.
        for i in 0..10 {
            let ch = if round % 4 == 0 {
                DeltaChannel::Reasoning
            } else {
                DeltaChannel::Text
            };
            b.delta(
                &format!("a{i}"),
                &format!("c{i}"),
                ch,
                &format!("token {round} "),
            );
        }
        for e in &b.events()[n..] {
            map.apply_run_event(e);
        }
        n = b.events().len();
        map.tick();
        bar.tick();
        let t = Instant::now();
        term.draw(|f| {
            let (main, pane) = bar.split(f.area(), &goals);
            map.render(f, main, now);
            if let Some(p) = pane {
                bar.render(f, p, &goals, map.run_state());
            }
        })
        .unwrap();
        frames.push(t.elapsed());
    }
    assert_eq!(
        map.live().len(),
        11,
        "ten agents plus the todo call of main"
    );
    check("10 live streams + goal bar", frames);
}

#[test]
fn spinner_animates_only_while_something_runs() {
    let mut b = EventBuilder::new();
    b.run_started("r", None);
    b.start(NodeSpec::step("a"));
    let mut map = MapView::new(MapConfig::default());
    for e in b.events() {
        map.apply_run_event(e);
    }
    assert!(map.needs_tick(true), "running and visible: animate");
    assert!(!map.needs_tick(false), "not visible: no ticks");
    // Animation frames redraw cheaply.
    let mut term = Terminal::new(TestBackend::new(120, 30)).unwrap();
    let mut frames = Vec::new();
    for _ in 0..20 {
        map.tick();
        let t = Instant::now();
        term.draw(|f| map.render(f, f.area(), T0)).unwrap();
        frames.push(t.elapsed());
    }
    check("spinner frames", frames);
    let layouts = map.layout_computes();
    assert!(layouts <= 2, "ticks never recompute the layout: {layouts}");
    // Idle: nothing running, so no ticks and no redraws are requested.
    let n = b.events().len();
    b.finish("a", NodeStatus::Succeeded, None);
    b.run_finished(harness_events::schema::RunStatus::Succeeded);
    for e in &b.events()[n..] {
        map.apply_run_event(e);
    }
    assert!(
        !map.needs_tick(true),
        "idle map requests no animation ticks"
    );
    let _ = map.take_stream_changes();
    assert!(!map.take_stream_changes(), "no stream changes while idle");
    let _ = Rect::default();
}
