use super::*;
use crate::demo::events::{EventBuilder, NodeSpec, T0, usage};
use crate::interaction::testing::{buffer_text, click, find_text, key, styled_snapshot};
use harness_events::schema::DeltaChannel;
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use serde_json::json;

struct H {
    map: MapView,
    term: Terminal<TestBackend>,
}

impl H {
    fn new(w: u16, h: u16) -> H {
        H {
            map: MapView::new(MapConfig {
                animations: false,
                ..MapConfig::default()
            }),
            term: Terminal::new(TestBackend::new(w, h)).unwrap(),
        }
    }

    fn feed(&mut self, events: &[RunEvent]) {
        for e in events {
            self.map.apply_run_event(e);
        }
    }

    fn draw(&mut self) -> String {
        let map = &mut self.map;
        self.term
            .draw(|f| map.render(f, f.area(), T0 + time::Duration::seconds(134)))
            .unwrap();
        buffer_text(self.term.backend().buffer())
    }

    fn send(&mut self, ev: &Event) -> Option<MapOutput> {
        let out = self.map.handle(ev);
        self.draw();
        out
    }
}

fn workflow() -> EventBuilder {
    let mut b = EventBuilder::new();
    b.run_started("fix-bug", Some("fix-bug"));
    b.start(NodeSpec::step("repro").agent("implementer").model("sonnet"));
    b.advance(41_000);
    b.cost("repro", 0.08);
    b.finish("repro", NodeStatus::Succeeded, None);
    b.start(
        NodeSpec::step("fix")
            .agent("implementer")
            .model("sonnet")
            .host("gpu-box")
            .needs(&["repro"]),
    );
    b.model_call("fix", "c1", "sonnet");
    b.advance(2_000);
    b.finish_call("fix", "c1", usage(500, 11_900, 812, 0), 2_000);
    b.cost("fix", 0.31);
    b.retry("fix", 1, "429 rate limited");
    b.start(
        NodeSpec::step("proof")
            .agent("bugfix-proof")
            .needs(&["fix"])
            .queued(),
    );
    b
}

#[test]
fn nodes_show_status_name_agent_model_host_duration_cost_retries_and_tokens() {
    let mut h = H::new(100, 16);
    h.feed(workflow().events());
    let text = h.draw();
    assert!(text.contains("fix-bug"), "{text}");
    assert!(text.contains("✔ repro"), "{text}");
    assert!(text.contains("Implementer · sonnet"), "{text}");
    assert!(text.contains("⧉gpu-box"), "{text}");
    assert!(text.contains("41.0s $0.080"), "{text}");
    assert!(text.contains("$0.31 ↻1"), "{text}");
    assert!(text.contains("↑12.4k ↓812 cache 96%"), "{text}");
    assert!(text.contains("○ proof"), "{text}");
    // Running node uses the shared indicator glyph (static spinner).
    assert!(text.contains("◐ fix"), "{text}");
    // Edges use box drawing characters and arrows.
    assert!(text.contains("│────►│"), "{text}");
}

#[test]
fn header_shows_totals_and_critical_path() {
    let mut h = H::new(120, 16);
    h.feed(workflow().events());
    let text = h.draw();
    let first = text.lines().next().unwrap();
    assert!(first.contains("fix-bug"), "{first}");
    assert!(first.contains("$0.39"), "{first}");
    assert!(first.contains("cache 96%"), "{first}");
    assert!(first.contains("critical path 2m14s"), "{first}");
}

#[test]
fn arrows_move_selection_and_selection_follows_active_node_until_moved() {
    let mut h = H::new(100, 16);
    h.feed(workflow().events());
    h.draw();
    assert!(h.map.is_following());
    assert_eq!(h.map.selected(), Some(&NodeId::from("fix")));
    h.send(&key("left"));
    assert_eq!(h.map.selected(), Some(&NodeId::from("repro")));
    assert!(!h.map.is_following());
    // New activity no longer steals the selection.
    let mut b = workflow();
    b.running("proof");
    h.map.apply_run_event(b.events().last().unwrap());
    assert_eq!(h.map.selected(), Some(&NodeId::from("repro")));
    h.send(&key("l"));
    assert_eq!(h.map.selected(), Some(&NodeId::from("fix")));
    h.send(&key("f"));
    assert!(h.map.is_following());
    assert_eq!(h.map.selected(), Some(&NodeId::from("proof")));
}

#[test]
fn viewport_pans_to_the_selection() {
    let mut b = EventBuilder::new();
    b.run_started("long", None);
    let mut prev: Option<String> = None;
    for i in 0..8 {
        let id = format!("s{i}");
        let needs: Vec<&str> = prev.iter().map(String::as_str).collect();
        b.start(NodeSpec::step(&id).needs(&needs).queued());
        prev = Some(id);
    }
    let mut h = H::new(60, 12);
    h.feed(b.events());
    h.draw();
    assert_eq!(h.map.pan(), (0, 0));
    h.map.select(NodeId::from("s0"));
    for _ in 0..7 {
        h.send(&key("right"));
    }
    assert_eq!(h.map.selected(), Some(&NodeId::from("s7")));
    assert!(h.map.pan().0 > 0, "panned right: {:?}", h.map.pan());
    let text = h.draw();
    assert!(text.contains("s7"), "{text}");
}

#[test]
fn large_graphs_switch_to_compact_nodes() {
    let mut b = EventBuilder::new();
    b.run_started("fan", None);
    b.start(NodeSpec::step("root"));
    for i in 0..12 {
        b.start(NodeSpec::step(&format!("w{i}")).needs(&["root"]).queued());
    }
    let mut h = H::new(100, 30);
    h.feed(b.events());
    let text = h.draw();
    assert_eq!(h.map.density(), Density::Compact);
    assert!(text.contains("○ w11"), "{text}");
    let mut small = H::new(100, 30);
    small.feed(workflow().events());
    small.draw();
    assert_eq!(small.map.density(), Density::Full);
}

#[test]
fn enter_opens_detail_and_tab_is_left_to_the_app() {
    let mut h = H::new(100, 16);
    h.feed(workflow().events());
    h.draw();
    assert_eq!(
        h.send(&key("enter")),
        Some(MapOutput::OpenDetail(NodeId::from("fix")))
    );
    assert_eq!(h.send(&key("tab")), None);
    assert_eq!(h.send(&key("esc")), Some(MapOutput::Back));
}

#[test]
fn every_node_is_clickable() {
    let mut h = H::new(100, 16);
    h.feed(workflow().events());
    let text = h.draw();
    let _ = text;
    let (x, y) = find_text(h.term.backend().buffer(), "repro").unwrap();
    assert_eq!(h.send(&click(x, y)), None);
    assert_eq!(h.map.selected(), Some(&NodeId::from("repro")));
    assert!(h.map.live_open(), "click opens the live pane");
    // A second click on the selected node opens detail.
    let (x, y) = find_text(h.term.backend().buffer(), "repro").unwrap();
    assert_eq!(
        h.send(&click(x, y)),
        Some(MapOutput::OpenDetail(NodeId::from("repro")))
    );
    let (x, y) = find_text(h.term.backend().buffer(), "proof").unwrap();
    h.send(&click(x, y));
    assert_eq!(h.map.selected(), Some(&NodeId::from("proof")));
}

#[test]
fn blocked_nodes_are_highlighted_and_enter_jumps_to_the_prompt() {
    let mut b = workflow();
    b.approval("fix", "approval-1-1", "git push origin main");
    let mut h = H::new(110, 16);
    h.feed(b.events());
    let text = h.draw();
    assert!(text.contains("⏸ 1 waiting"), "{text}");
    assert!(text.contains("⏸ approval · Enter"), "{text}");
    assert_eq!(h.map.selected(), Some(&NodeId::from("fix")));
    // The blocked node's border uses the approval token.
    let (x, y) = find_text(h.term.backend().buffer(), "⏸ fix").unwrap();
    let cell = &h.term.backend().buffer()[(x - 1, y)];
    assert_eq!(cell.fg, h.map.cfg.theme.color(Token::Approval));
    assert_eq!(
        h.send(&key("enter")),
        Some(MapOutput::OpenPrompt {
            node: NodeId::from("fix"),
            blocked: Blocked::Approval
        })
    );
    // Budget stops are blocked too.
    let mut b = workflow();
    b.finish(
        "fix",
        NodeStatus::Failed,
        Some("budget: node budget $0.30 reached"),
    );
    let mut h = H::new(110, 16);
    h.feed(b.events());
    h.draw();
    h.map.select(NodeId::from("fix"));
    assert_eq!(
        h.send(&key("enter")),
        Some(MapOutput::OpenPrompt {
            node: NodeId::from("fix"),
            blocked: Blocked::Budget
        })
    );
}

#[test]
fn running_nodes_show_a_live_preview_dimmed_for_reasoning() {
    let mut b = workflow();
    b.model_call("fix", "c2", "sonnet");
    b.delta("fix", "c2", DeltaChannel::Reasoning, "checking callers");
    let mut h = H::new(100, 16);
    h.feed(b.events());
    h.draw();
    let buf = h.term.backend().buffer();
    let (x, y) = find_text(buf, "checking callers").unwrap();
    assert_eq!(buf[(x, y)].fg, h.map.cfg.theme.color(Token::Reasoning));
    assert!(buf[(x, y)].modifier.contains(Modifier::ITALIC));
    b.delta("fix", "c2", DeltaChannel::Text, "Updating parse_header");
    h.map.apply_run_event(b.events().last().unwrap());
    let text = h.draw();
    assert!(text.contains("Updating parse_header"), "{text}");
    // Previews can be turned off.
    let mut off = H::new(100, 16);
    off.map.cfg.live_preview = false;
    off.feed(b.events());
    assert!(!off.draw().contains("Updating parse_header"));
}

#[test]
fn space_and_click_open_the_live_pane_esc_and_close_button_close_it() {
    let mut b = workflow();
    b.model_call("fix", "c2", "sonnet");
    b.delta("fix", "c2", DeltaChannel::Reasoning, "plan the change");
    b.delta("fix", "c2", DeltaChannel::Text, "Editing src/parse.rs now");
    b.tool_call("fix", "t9", "bash", json!({"command": "cargo test parse"}));
    let mut h = H::new(100, 30);
    h.feed(b.events());
    h.draw();
    h.send(&key("space"));
    assert!(h.map.live_open());
    let text = h.draw();
    assert!(text.contains("live · fix"), "{text}");
    assert!(text.contains("┊ plan the change"), "{text}");
    assert!(text.contains("→ bash $ cargo test parse"), "{text}");
    h.map
        .tool_output(&NodeId::from("fix"), "t9", "test parse::ok ... ok\n");
    assert!(h.draw().contains("test parse::ok ... ok"));
    h.send(&key("esc"));
    assert!(!h.map.live_open());
    // Click opens it; the [x] button closes it.
    let (x, y) = find_text(h.term.backend().buffer(), "◐ fix").unwrap();
    h.send(&click(x + 2, y));
    assert!(h.map.live_open());
    let (x, y) = find_text(h.term.backend().buffer(), "[x]").unwrap();
    h.send(&click(x, y));
    assert!(!h.map.live_open());
}

#[test]
fn layout_is_only_recomputed_on_topology_changes() {
    let mut b = workflow();
    let mut h = H::new(100, 16);
    h.feed(b.events());
    h.draw();
    let n = h.map.layout_computes();
    b.cost("fix", 0.01);
    b.delta("fix", "c1", DeltaChannel::Text, "more");
    for e in &b.events()[b.events().len() - 2..] {
        h.map.apply_run_event(e);
    }
    h.draw();
    assert_eq!(h.map.layout_computes(), n);
}

#[test]
fn snapshot_map_with_subagents() {
    let mut h = H::new(120, 22);
    h.feed(&crate::demo::scripted_run());
    h.draw();
    insta::assert_snapshot!("map_gallery", styled_snapshot(h.term.backend().buffer()));
}

#[test]
fn running_nodes_and_goal_bar_tasks_spin() {
    use crate::indicators::SPINNER_FRAMES;
    let mut map = MapView::new(MapConfig::default());
    for e in workflow().events() {
        map.apply_run_event(e);
    }
    let mut term = Terminal::new(TestBackend::new(100, 16)).unwrap();
    let mut frame_text = |map: &mut MapView| {
        term.draw(|f| map.render(f, f.area(), T0)).unwrap();
        buffer_text(term.backend().buffer())
    };
    let first = frame_text(&mut map);
    assert!(
        first.contains(&format!("{} fix", SPINNER_FRAMES[0])),
        "{first}"
    );
    map.tick();
    let second = frame_text(&mut map);
    assert!(
        second.contains(&format!("{} fix", SPINNER_FRAMES[1])),
        "{second}"
    );
    // Goal-bar tasks in progress use the same spinner.
    let spinner = crate::indicators::Spinner::new(true);
    let (glyph, token) = crate::goalbar::TaskStatus::InProgress.glyph(&spinner);
    assert_eq!(
        (glyph, token),
        crate::indicators::node_glyph(NodeStatus::Running, &spinner)
    );
    assert_eq!(glyph, SPINNER_FRAMES[0]);
}

#[test]
fn header_and_nodes_use_run_titles_and_agent_display_names() {
    let mut h = H::new(100, 12);
    let mut b = EventBuilder::new();
    b.push(
        None,
        None,
        harness_events::schema::EventKind::RunStarted {
            name: "coder".into(),
            workflow: None,
            host: None,
            title: None,
            agent_names: [("coder".to_owned(), "Code Smith".to_owned())].into(),
        },
    );
    b.start(NodeSpec::step("coder").agent("coder").model("sonnet"));
    b.user_message("coder", "Fix the header parser\nplease");
    // A node with a raw UUID id that is only referenced so far.
    b.start(
        NodeSpec::subagent("0199f1a2-3b4c-7d5e-8f60-123456abcdef", "coder")
            .name("0199f1a2-3b4c-7d5e-8f60-123456abcdef"),
    );
    h.feed(b.events());
    let text = h.draw();
    assert!(
        text.contains(" Code Smith · Fix the header parser "),
        "{text}"
    );
    assert!(text.contains("Code Smith"), "{text}");
    assert!(text.contains("#abcdef"), "{text}");
    assert!(
        harness_events::names::find_raw_id(&text).is_none(),
        "{text}"
    );
}

#[test]
fn a_long_title_is_cut_before_the_header_metrics_at_80_columns() {
    let mut h = H::new(80, 12);
    let mut b = EventBuilder::new();
    b.run_started("coder", None);
    b.start(NodeSpec::step("coder").agent("coder").model("sonnet"));
    b.user_message(
        "coder",
        "Refactor the configuration loader so that every layer reports precise errors",
    );
    b.cost("coder", 0.42);
    b.approval("coder", "approval-1-1", "git push origin main");
    h.feed(b.events());
    let text = h.draw();
    let header = text.lines().next().unwrap();
    assert!(header.starts_with(" Coder · Refa"), "{header}");
    assert!(header.contains('…'), "the title is cut: {header}");
    for needle in ["running", "$0.42", "⏸ 1 waiting"] {
        assert!(header.contains(needle), "{needle} in {header}");
    }
}

#[test]
fn mcp_calls_show_server_tool_duration_and_result_in_the_live_pane() {
    let mut b = workflow();
    b.model_call("fix", "c2", "sonnet");
    b.tool_call(
        "fix",
        "t5",
        "mcp__github__create_issue",
        json!({"title": "Crash"}),
    );
    b.push(
        Some("fix"),
        None,
        harness_events::schema::EventKind::McpCall {
            call_id: "t5".into(),
            server: "github".into(),
            tool: "create_issue".into(),
        },
    );
    b.tool_result("fix", "t5", true, "created issue #12", 1_234);
    let mut h = H::new(100, 30);
    h.feed(b.events());
    h.draw();
    // Keyboard opens the live pane of the selected node.
    h.send(&key("space"));
    assert!(h.map.live_open());
    let text = h.draw();
    assert!(
        text.contains("✔ mcp github › create_issue title=Crash · 1.2s"),
        "{text}"
    );
    assert!(text.contains("created issue #12"), "{text}");
    let (x, y) = find_text(h.term.backend().buffer(), "github").unwrap();
    assert_eq!(
        h.term.backend().buffer()[(x, y)].fg,
        h.map.cfg.theme.color(Token::Accent)
    );
    h.send(&key("esc"));
    assert!(!h.map.live_open());
    // Mouse opens it too.
    let (x, y) = find_text(h.term.backend().buffer(), "◐ fix").unwrap();
    h.send(&click(x + 2, y));
    assert!(h.map.live_open());
    assert!(h.draw().contains("mcp github › create_issue"));
}
