use super::*;
use crate::demo::events::{EventBuilder, NodeSpec, T0, usage};
use crate::interaction::testing::{buffer_text, click, find_text, key, styled_snapshot};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use serde_json::json;

struct H {
    d: DetailView,
    term: Terminal<TestBackend>,
}

impl H {
    fn new(events: &[RunEvent], w: u16, h: u16) -> H {
        H {
            d: DetailView::from_events(
                NodeId::from("fix"),
                Theme::default(),
                ViewSettings::default(),
                false,
                events,
            ),
            term: Terminal::new(TestBackend::new(w, h)).unwrap(),
        }
    }

    fn draw(&mut self) -> String {
        let d = &mut self.d;
        self.term
            .draw(|f| d.render(f, f.area(), T0 + time::Duration::seconds(60)))
            .unwrap();
        buffer_text(self.term.backend().buffer())
    }

    fn send(&mut self, ev: &Event) -> Option<DetailOutput> {
        let out = self.d.handle(ev);
        self.draw();
        out
    }

    fn click_on(&mut self, needle: &str) -> Option<DetailOutput> {
        let (x, y) = find_text(self.term.backend().buffer(), needle).unwrap_or_else(|| {
            panic!(
                "{needle} not on screen:\n{}",
                buffer_text(self.term.backend().buffer())
            )
        });
        self.send(&click(x, y))
    }
}

fn log() -> EventBuilder {
    let mut b = EventBuilder::new();
    b.start(NodeSpec::step("fix").agent("implementer").model("sonnet"));
    // Turn 1: an edit, succeeded.
    b.model_call("fix", "c1", "sonnet");
    b.delta("fix", "c1", DeltaChannel::Text, "Editing the parser.");
    b.advance(4_200);
    b.finish_call("fix", "c1", usage(500, 11_900, 812, 120), 4_200);
    b.cost("fix", 0.021);
    b.tool_call(
        "fix",
        "t1",
        "edit",
        json!({"path": "src/parse.rs", "old_string": "a", "new_string": "b"}),
    );
    b.file_changed(
        "fix",
        "src/parse.rs",
        FileChangeKind::Modified,
        Some("--- a/src/parse.rs\n+++ b/src/parse.rs\n@@ -40,1 +40,1 @@\n-    .unwrap();\n+    .ok_or(ParseError::MissingColon)?;\n"),
    );
    b.tool_result("fix", "t1", true, "ok", 3);
    // Turn 2: a failing bash call.
    b.model_call("fix", "c2", "sonnet");
    b.delta("fix", "c2", DeltaChannel::Text, "Running the tests.");
    b.advance(1_000);
    b.finish_call("fix", "c2", usage(300, 12_000, 100, 0), 1_000);
    b.cost("fix", 0.01);
    b.tool_call("fix", "t2", "bash", json!({"command": "cargo test parse"}));
    b.advance(8_100);
    b.tool_result(
        "fix",
        "t2",
        false,
        "test parse::header_colon ... FAILED\nexit code 101",
        8_100,
    );
    // Turn 3: finished, nothing failed.
    b.model_call("fix", "c3", "sonnet");
    b.delta("fix", "c3", DeltaChannel::Text, "Trying again.");
    b.finish_call("fix", "c3", usage(10, 0, 5, 0), 500);
    b.verify("fix", "cargo test", false, "1 failed", 3_000);
    b
}

#[test]
fn sidebar_lists_turns_tool_calls_verify_runs_and_hygiene() {
    let mut h = H::new(log().events(), 120, 40);
    h.d.set_hygiene(
        vec!["~ src/parse.rs (edit)".into()],
        vec!["Hygiene 100/100: 1 changed".into()],
        0,
    );
    let text = h.draw();
    for needle in [
        "✔ Turn 1 4.2s",
        "✔ edit src/parse.rs",
        "✗ Turn 2",
        "✗ bash $ cargo test parse",
        "✗ verify cargo test",
        "◆ Hygiene report",
    ] {
        assert!(text.contains(needle), "{needle}\n{text}");
    }
    // Activating a sidebar item jumps to (and expands) its group.
    h.click_on("✔ Turn 1 4.2s");
    let text = h.draw();
    assert!(text.contains("▾ ✔ Turn 1"), "{text}");
    assert_eq!(
        h.d.interaction.focused().map(|f| f.0.as_str()),
        Some("detail.turn.0")
    );
}

#[test]
fn groups_collapse_and_failures_auto_expand() {
    let mut h = H::new(log().events(), 120, 40);
    let text = h.draw();
    // Turn 1 succeeded and is not the last turn: collapsed.
    assert!(text.contains("▸ ✔ Turn 1"), "{text}");
    // Turn 2 has a failed call: expanded, with the failed card expanded.
    assert!(text.contains("▾ ✗ Turn 2"), "{text}");
    assert!(
        text.contains("test parse::header_colon ... FAILED"),
        "{text}"
    );
    // Failed verify is expanded.
    assert!(text.contains("1 failed"), "{text}");
}

#[test]
fn groups_and_entries_toggle_by_click_and_keyboard() {
    let mut h = H::new(log().events(), 120, 40);
    h.draw();
    // Mouse: expand turn 1, then collapse its edit card... (card starts collapsed preview)
    h.click_on("▸ ✔ Turn 1");
    assert!(h.draw().contains("▾ ✔ Turn 1"));
    h.click_on("▾ ✔ Turn 1");
    assert!(h.draw().contains("▸ ✔ Turn 1"));
    // Keyboard: Tab to the turn 2 header, Enter collapses it.
    for _ in 0..40 {
        if h.d
            .interaction
            .focused()
            .is_some_and(|f| f.0 == "detail.turn.1")
        {
            break;
        }
        h.send(&key("tab"));
    }
    assert_eq!(h.d.interaction.focused().unwrap().0, "detail.turn.1");
    h.send(&key("enter"));
    assert!(h.draw().contains("▸ ✗ Turn 2"));
    h.send(&key("enter"));
    assert!(h.draw().contains("▾ ✗ Turn 2"));
    // Tool card toggles too (keyboard).
    for _ in 0..40 {
        if h.d.interaction.focused().is_some_and(|f| f.0 == "card.t2") {
            break;
        }
        h.send(&key("tab"));
    }
    assert_eq!(h.d.interaction.focused().unwrap().0, "card.t2");
    let before = h.d.turns[1].cards[0].expanded;
    h.send(&key("enter"));
    assert_ne!(h.d.turns[1].cards[0].expanded, before);
    // And by mouse.
    h.click_on("╭ ✗ bash");
    assert_eq!(h.d.turns[1].cards[0].expanded, before);
    assert_eq!(h.send(&key("esc")), Some(DetailOutput::Back));
}

#[test]
fn entries_show_duration_tokens_and_cost_and_reuse_chat_cards_and_meta() {
    let mut h = H::new(log().events(), 140, 40);
    h.draw();
    h.click_on("▸ ✔ Turn 1");
    let text = h.draw();
    assert!(
        text.contains("in 12.4k (cached 11.9k) out 812 reasoning 120  $0.021  4.2s"),
        "{text}"
    );
    // Tool card from the chat view, with its duration.
    assert!(text.contains("╭ ✔ edit"), "{text}");
    assert!(text.contains("3ms"), "{text}");
    // The chat's per-turn metadata line.
    assert!(
        text.contains(
            "sonnet · ↑12.4k (cache 96%) ↓812 · reasoning 120 · $0.021 · 4.2s · end_turn"
        ),
        "{text}"
    );
}

#[test]
fn follow_mode_tails_a_running_node() {
    let mut b = EventBuilder::new();
    b.start(NodeSpec::step("fix"));
    b.model_call("fix", "c1", "sonnet");
    let mut h = H::new(b.events(), 100, 12);
    for i in 0..50 {
        b.delta("fix", "c1", DeltaChannel::Text, &format!("line {i}\n"));
        h.d.apply_run_event(b.events().last().unwrap());
    }
    let text = h.draw();
    assert!(h.d.is_following());
    assert!(text.contains("line 49"), "{text}");
    assert!(!text.contains("line 10\n"), "{text}");
    h.send(&key("pageup"));
    assert!(!h.d.is_following());
    assert!(!h.draw().contains("line 49"));
    h.send(&key("f"));
    assert!(h.d.is_following());
    assert!(h.draw().contains("line 49"));
}

#[test]
fn current_turn_streams_reasoning_then_collapses_and_streams_bash_output() {
    let mut b = EventBuilder::new();
    b.start(NodeSpec::step("fix"));
    b.model_call("fix", "c1", "sonnet");
    b.delta(
        "fix",
        "c1",
        DeltaChannel::Reasoning,
        "Consider the colon case",
    );
    let mut h = H::new(b.events(), 100, 20);
    let text = h.draw();
    assert!(text.contains("▾ thinking"), "{text}");
    assert!(text.contains("┊ Consider the colon case"), "{text}");
    b.delta("fix", "c1", DeltaChannel::Text, "Fixing it");
    b.finish_call("fix", "c1", usage(1, 0, 1, 5), 10);
    b.tool_call("fix", "t1", "bash", json!({"command": "cargo test"}));
    for e in &b.events()[3..] {
        h.d.apply_run_event(e);
    }
    let text = h.draw();
    assert!(text.contains("▸ thinking (1 lines, collapsed)"), "{text}");
    assert!(text.contains("Fixing it"), "{text}");
    h.d.tool_output("t1", "running 2 tests\n");
    assert!(h.draw().contains("running 2 tests"));
    // The reasoning toggle works by click.
    h.click_on("▸ thinking");
    assert!(h.draw().contains("┊ Consider the colon case"));
}

#[test]
fn hidden_reasoning_is_stated() {
    let mut b = EventBuilder::new();
    b.start(NodeSpec::step("fix"));
    b.model_call("fix", "c1", "o3");
    b.delta("fix", "c1", DeltaChannel::Text, "answer");
    b.finish_call("fix", "c1", usage(10, 0, 50, 40), 10);
    let mut h = H::new(b.events(), 100, 20);
    let text = h.draw();
    assert!(
        text.contains("ⓘ reasoning not exposed by provider"),
        "{text}"
    );
}

#[test]
fn palette_actions_search_copy_diff_and_inspector() {
    let mut h = H::new(log().events(), 120, 30);
    h.draw();
    let ids: Vec<String> = palette_entries().into_iter().map(|e| e.id).collect();
    for id in [ids::SEARCH, ids::COPY, ids::DIFF, ids::INSPECT] {
        assert!(ids.iter().any(|i| i == id));
    }
    let inv = |id: &str, args: Vec<String>| Invocation {
        id: id.into(),
        args,
    };
    assert_eq!(h.d.search("editing the parser"), 1);
    h.d.run_palette(&inv(ids::SEARCH, vec!["editing the parser".into()]));
    let text = h.draw();
    assert!(text.contains("Editing the parser."), "{text}");
    let buf = h.term.backend().buffer();
    let (x, y) = find_text(buf, "Editing the parser.").unwrap();
    assert_eq!(buf[(x, y)].fg, Theme::default().color(Token::Highlight));
    // Copy the focused entry.
    h.d.interaction.set_focus("card.t2");
    match h.d.run_palette(&inv(ids::COPY, vec![])) {
        Some(DetailOutput::Copy(t)) => assert!(t.contains("cargo test parse"), "{t}"),
        other => panic!("{other:?}"),
    }
    assert_eq!(
        h.d.run_palette(&inv(ids::INSPECT, vec![])),
        Some(DetailOutput::OpenInspector {
            node: NodeId::from("fix"),
            call_id: "c2".into()
        })
    );
    assert_eq!(
        h.d.run_palette(&inv(ids::DIFF, vec![])),
        Some(DetailOutput::OpenDiff(NodeId::from("fix")))
    );
}

#[test]
fn hygiene_actions_by_click_and_keyboard() {
    let mut h = H::new(log().events(), 120, 60);
    h.d.set_hygiene(
        vec!["+ debug.log (bash)".into()],
        vec!["Hygiene 80/100: 1 changed, 0 out of scope, 1 temp/debug".into()],
        1,
    );
    let text = h.draw();
    assert!(text.contains("Hygiene 80/100"), "{text}");
    assert_eq!(
        h.click_on("[review diff]"),
        Some(DetailOutput::OpenDiff(NodeId::from("fix")))
    );
    h.d.interaction.set_focus("detail.hygiene.temp");
    h.draw();
    assert_eq!(
        h.send(&key("enter")),
        Some(DetailOutput::RemoveTempFiles(NodeId::from("fix")))
    );
}

#[test]
fn rendering_cost_does_not_grow_with_log_length() {
    let mut b = EventBuilder::new();
    b.start(NodeSpec::step("fix"));
    for i in 0..2_000 {
        let call = format!("c{i}");
        b.model_call("fix", &call, "sonnet");
        b.finish_call("fix", &call, usage(1, 0, 1, 0), 1);
        b.tool_call("fix", &format!("t{i}"), "read", json!({"path": "a.rs"}));
        b.tool_result("fix", &format!("t{i}"), true, "x", 1);
    }
    let mut h = H::new(b.events(), 100, 30);
    h.draw();
    let first = h.d.layouts();
    h.draw();
    assert_eq!(h.d.layouts(), first, "second frame fully cached");
}

#[test]
fn snapshot_detail() {
    let mut h = H::new(log().events(), 110, 30);
    h.draw();
    insta::assert_snapshot!("detail_gallery", styled_snapshot(h.term.backend().buffer()));
}

#[test]
fn mcp_calls_show_server_tool_duration_and_result_and_toggle() {
    let mut b = EventBuilder::new();
    b.start(NodeSpec::step("fix").agent("implementer").model("sonnet"));
    b.model_call("fix", "c1", "sonnet");
    b.finish_call("fix", "c1", usage(10, 0, 5, 0), 500);
    b.tool_call(
        "fix",
        "m1",
        "mcp__github__create_issue",
        json!({"title": "Crash"}),
    );
    b.push(
        Some("fix"),
        None,
        EventKind::McpCall {
            call_id: "m1".into(),
            server: "github".into(),
            tool: "create_issue".into(),
        },
    );
    b.advance(1_234);
    b.tool_result(
        "fix",
        "m1",
        true,
        "line 1\nline 2\nline 3\nline 4\nline 5\nline 6\nline 7\nline 8\nline 9\nline 10",
        1_234,
    );
    let mut h = H::new(b.events(), 120, 40);
    let text = h.draw();
    // The sidebar names the server tool too.
    assert!(text.contains("✔ mcp github › create_issue"), "{text}");
    assert!(
        text.contains("╭ ✔ mcp github › create_issue  title=Crash"),
        "{text}"
    );
    assert!(text.contains("1.2s"), "{text}");
    let card = |h: &H| h.d.turns[0].cards[0].expanded;
    let before = card(&h);
    // Keyboard.
    for _ in 0..40 {
        if h.d.interaction.focused().is_some_and(|f| f.0 == "card.m1") {
            break;
        }
        h.send(&key("tab"));
    }
    assert_eq!(h.d.interaction.focused().unwrap().0, "card.m1");
    h.send(&key("enter"));
    assert_ne!(card(&h), before);
    // Mouse, on the card header.
    h.click_on("╭ ✔ mcp github");
    assert_eq!(card(&h), before);
    assert_eq!(h.send(&key("esc")), Some(DetailOutput::Back));
}
