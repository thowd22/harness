//! Subagent cards in the chat (TASK-125): live status, agent, model, host,
//! last action, duration and cost; the final summary once finished; click
//! or Enter opens the subagent.

use std::path::Path;
use std::time::Instant;

use harness_events::schema::{DeltaChannel, NodeId, NodeStatus};
use harness_tui::chat::view::{ChatConfig, ChatOutput, ChatView};
use harness_tui::demo::events::{EventBuilder, NodeSpec, T0};
use harness_tui::interaction::testing::{buffer_text, click, find_text, key};
use harness_tui::status::StatusLine;
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use serde_json::json;

struct Ui {
    view: ChatView,
    term: Terminal<TestBackend>,
    now: Instant,
}

impl Ui {
    fn new() -> Ui {
        let cfg = ChatConfig {
            animations: false,
            ..ChatConfig::default()
        };
        let status = StatusLine::new(Path::new("/w"), None, cfg.mode);
        let now = Instant::now();
        Ui {
            view: ChatView::new(cfg, status, now),
            term: Terminal::new(TestBackend::new(100, 30)).unwrap(),
            now,
        }
    }

    fn feed(&mut self, b: &EventBuilder, from: usize) {
        for e in &b.events()[from..] {
            self.view.apply_run_event(e, self.now);
        }
    }

    fn draw(&mut self) -> String {
        let (view, now) = (&mut self.view, self.now);
        self.term.draw(|f| view.render(f, f.area(), now)).unwrap();
        buffer_text(self.term.backend().buffer())
    }

    fn send(&mut self, ev: crossterm::event::Event) -> Vec<ChatOutput> {
        let out = self.view.handle(&ev, self.now, T0);
        self.draw();
        out
    }
}

fn spawn() -> EventBuilder {
    let mut b = EventBuilder::new();
    b.start(NodeSpec::step("main").agent("coder"));
    b.start(
        NodeSpec::subagent("r1", "main")
            .name("survey")
            .agent("researcher")
            .model("haiku")
            .host("gpu-box"),
    );
    b
}

#[test]
fn card_shows_status_agent_model_host_last_action_duration_and_cost_live() {
    let mut ui = Ui::new();
    let mut b = spawn();
    ui.feed(&b, 0);
    let text = ui.draw();
    assert!(
        text.contains("◐ survey (Researcher) · haiku · ⧉ gpu-box"),
        "{text}"
    );
    let n = b.events().len();
    b.advance(12_000);
    b.tool_call("r1", "t1", "read", json!({"path": "src/net/mod.rs"}));
    b.cost("r1", 0.004);
    ui.feed(&b, n);
    let text = ui.draw();
    assert!(text.contains("read src/net/mod.rs"), "{text}");
    assert!(text.contains("$0.004"), "{text}");
    let n = b.events().len();
    b.model_call("r1", "c1", "haiku");
    b.delta("r1", "c1", DeltaChannel::Text, "Found 3 callers");
    ui.feed(&b, n);
    assert!(ui.draw().contains("writing: Found 3 callers"));
}

#[test]
fn finished_card_shows_the_final_summary() {
    let mut ui = Ui::new();
    let mut b = spawn();
    b.model_call("r1", "c1", "haiku");
    b.delta(
        "r1",
        "c1",
        DeltaChannel::Text,
        "Callers:\n- net::read\n- net::write\n",
    );
    b.advance(9_000);
    b.finish("r1", NodeStatus::Succeeded, None);
    ui.feed(&b, 0);
    let text = ui.draw();
    assert!(text.contains("✔ survey (Researcher)"), "{text}");
    assert!(text.contains("│ Callers:"), "{text}");
    assert!(text.contains("│ - net::write"), "{text}");
    assert!(text.contains("9.0s"), "{text}");
}

#[test]
fn click_or_enter_opens_the_subagent() {
    let mut ui = Ui::new();
    ui.feed(&spawn(), 0);
    ui.draw();
    let (x, y) = find_text(ui.term.backend().buffer(), "survey").unwrap();
    let out = ui.send(click(x, y));
    assert!(
        out.contains(&ChatOutput::OpenNode(NodeId::from("r1"))),
        "{out:?}"
    );
    // Keyboard: the card is focused now; Enter opens it again.
    let out = ui.send(key("enter"));
    assert!(
        out.contains(&ChatOutput::OpenNode(NodeId::from("r1"))),
        "{out:?}"
    );
}
