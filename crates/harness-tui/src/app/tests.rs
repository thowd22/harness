//! App tests: the composed chat, map, detail, goal bar, panels and the
//! event loop, with mouse and keyboard paths.

use super::*;
use crate::demo::events::{EventBuilder, NodeSpec};
use crate::interaction::testing::*;
use crate::keys::HELP_CLOSE_ID;
use harness_config::ini::load_str;
use harness_events::schema::{EventKind, NodeId, NodeKind, OutputStream, RunId};
use harness_policy::approvals::{
    ApprovalAnswer, ApprovalPrompt, ApprovalRequest, ApprovalScope, ApprovalUi, ChannelUi,
};
use harness_policy::rules::{Action as PolicyAction, Decision, Layer, MatchedRule, Verdict};
use ratatui::backend::TestBackend;
use serde_json::json;
use std::time::Instant;

struct Ui {
    app: App,
    term: Terminal<TestBackend>,
    requests: mpsc::UnboundedReceiver<UiRequest>,
}

impl Ui {
    fn new(cfg: AppConfig) -> Ui {
        Ui::sized(cfg, 80, 24)
    }

    fn sized(cfg: AppConfig, w: u16, h: u16) -> Ui {
        let (tx, requests) = mpsc::unbounded_channel();
        let mut ui = Ui {
            app: App::new(cfg, tx),
            term: Terminal::new(TestBackend::new(w, h)).unwrap(),
            requests,
        };
        ui.draw();
        ui
    }

    fn draw(&mut self) {
        let app = &mut self.app;
        self.term.draw(|f| app.render(f)).unwrap();
    }

    /// Sends events, redrawing after each like the loop does.
    fn send(&mut self, events: Vec<Event>) {
        for e in events {
            self.app.handle(AppEvent::Terminal(e));
            if self.app.is_dirty() {
                self.draw();
            }
        }
    }

    fn feed(&mut self, events: &[RunEvent]) {
        for e in events {
            self.app.handle(AppEvent::Run(Box::new(e.clone())));
        }
        self.draw();
    }

    fn core(&mut self, c: CoreEvent) {
        self.app.handle(AppEvent::Core(c));
        self.draw();
    }

    fn click_text(&mut self, text: &str) {
        let (x, y) = find_text(self.term.backend().buffer(), text)
            .unwrap_or_else(|| panic!("`{text}` not on screen:\n{}", self.text()));
        self.send(vec![click(x, y)]);
    }

    fn text(&self) -> String {
        buffer_text(self.term.backend().buffer())
    }

    fn requests(&mut self) -> Vec<UiRequest> {
        let mut out = Vec::new();
        while let Ok(r) = self.requests.try_recv() {
            out.push(r);
        }
        out
    }
}

fn run_event(seq: u64, node: Option<&str>, kind: EventKind) -> AppEvent {
    AppEvent::Run(Box::new(RunEvent {
        seq,
        ts: time::OffsetDateTime::UNIX_EPOCH,
        run_id: RunId(uuid::Uuid::nil()),
        node_id: node.map(NodeId::from),
        parent_node_id: None,
        kind,
    }))
}

fn started(seq: u64) -> AppEvent {
    run_event(
        seq,
        None,
        EventKind::RunStarted {
            name: "demo".into(),
            workflow: None,
            host: None,
            title: None,
            agent_names: Default::default(),
        },
    )
}

fn node_started(seq: u64, id: &str) -> AppEvent {
    run_event(
        seq,
        Some(id),
        EventKind::NodeStarted {
            kind: NodeKind::Agent,
            name: id.into(),
            agent: None,
            model: None,
            host: None,
            needs: vec![],
            queued: false,
        },
    )
}

/// A run with a main agent, a subagent and a todo list.
fn demo_run() -> Vec<RunEvent> {
    let mut b = EventBuilder::new();
    b.run_started("fix", None);
    b.start(NodeSpec::step("main").agent("coder").model("sonnet"));
    b.start(NodeSpec::subagent("r1", "main").agent("researcher"));
    b.tool_call(
        "main",
        "t1",
        "todo",
        json!({"goal": "Fix the parser", "todos": [
            {"id": "1", "content": "Reproduce", "status": "completed"},
            {"id": "2", "content": "Fix parse_header", "status": "in_progress"},
        ]}),
    );
    b.into_events()
}

#[test]
fn renders_only_when_dirty() {
    let mut ui = Ui::new(AppConfig::default());
    assert!(!ui.app.is_dirty());
    assert_eq!(ui.app.frames(), 1);
    // Key releases and focus loss change nothing.
    let mut release = match key("f5") {
        Event::Key(k) => k,
        _ => unreachable!(),
    };
    release.kind = KeyEventKind::Release;
    ui.send(vec![Event::Key(release)]);
    assert_eq!(ui.app.frames(), 1);
    ui.app.handle(AppEvent::Terminal(Event::FocusLost));
    assert!(!ui.app.is_dirty());
    ui.app.handle(started(0));
    assert!(ui.app.is_dirty());
}

#[tokio::test]
async fn event_loop_consumes_channel_and_coalesces() {
    let (tx, mut rx) = event_channel();
    let (req_tx, mut req_rx) = mpsc::unbounded_channel();
    let mut app = App::new(
        AppConfig {
            animations: false,
            ..AppConfig::default()
        },
        req_tx,
    );
    let mut term = Terminal::new(TestBackend::new(60, 12)).unwrap();
    let mut control = RecordingControl::default();
    // A burst of events queued before the loop runs is drawn once.
    tx.send(started(0)).await.unwrap();
    tx.send(node_started(1, "planner")).await.unwrap();
    for e in type_text("hi") {
        tx.send(AppEvent::Terminal(e)).await.unwrap();
    }
    tx.send(AppEvent::Terminal(key("enter"))).await.unwrap();
    let handle = tokio::spawn(async move {
        run_loop(&mut term, &mut control, &mut app, &mut rx)
            .await
            .unwrap();
        (app, term)
    });
    tokio::task::yield_now().await;
    tx.send(AppEvent::Terminal(key("ctrl+q"))).await.unwrap();
    let (app, term) = handle.await.unwrap();
    assert!(app.should_quit());
    assert_eq!(app.run_state().nodes.len(), 1);
    assert!(app.frames() <= 2, "frames {}", app.frames());
    assert!(buffer_text(term.backend().buffer()).contains("hi"));
    assert_eq!(req_rx.recv().await, Some(UiRequest::Submit("hi".into())));
    assert_eq!(req_rx.recv().await, Some(UiRequest::Quit));
}

#[tokio::test(start_paused = true)]
async fn idles_without_ticks_and_ticks_only_while_running() {
    let (tx, mut rx) = event_channel();
    let (req_tx, _req_rx) = mpsc::unbounded_channel();
    let mut app = App::new(AppConfig::default(), req_tx);
    let mut term = Terminal::new(TestBackend::new(40, 8)).unwrap();
    let mut control = RecordingControl::default();
    let idle = tokio::time::timeout(
        Duration::from_secs(10),
        run_loop(&mut term, &mut control, &mut app, &mut rx),
    )
    .await;
    assert!(idle.is_err(), "loop should still be waiting");
    assert_eq!(app.frames(), 1, "idle loop draws once");
    assert_eq!(app.ticks(), 0, "idle loop never wakes for ticks");

    // An interactive session opens its run before the first prompt; with
    // no node running it is idle and must not animate.
    tx.send(started(0)).await.unwrap();
    let opened = tokio::time::timeout(
        Duration::from_secs(1),
        run_loop(&mut term, &mut control, &mut app, &mut rx),
    )
    .await;
    assert!(opened.is_err());
    assert!(!app.is_running(), "an open session with no turn is ready");
    assert_eq!(app.ticks(), 0, "no spinner before the first prompt");

    tx.send(node_started(1, "main")).await.unwrap();
    let busy = tokio::time::timeout(
        Duration::from_secs(1),
        run_loop(&mut term, &mut control, &mut app, &mut rx),
    )
    .await;
    assert!(busy.is_err());
    assert!(app.is_running());
    assert!(app.ticks() >= 10, "ticks {}", app.ticks());
    assert!(app.frames() >= 10);

    // Animations off: no ticks even while running.
    let (req_tx, _r) = mpsc::unbounded_channel();
    let mut app = App::new(
        AppConfig {
            animations: false,
            ..AppConfig::default()
        },
        req_tx,
    );
    tx.send(started(0)).await.unwrap();
    tx.send(node_started(1, "main")).await.unwrap();
    let _ = tokio::time::timeout(
        Duration::from_secs(1),
        run_loop(&mut term, &mut control, &mut app, &mut rx),
    )
    .await;
    assert!(app.is_running());
    assert_eq!(app.ticks(), 0);
}

#[test]
fn resize_redraws_at_new_size() {
    let mut ui = Ui::new(AppConfig::default());
    ui.term.backend_mut().resize(80, 20);
    ui.app.handle(AppEvent::Terminal(Event::Resize(80, 20)));
    assert!(ui.app.is_dirty());
    ui.draw();
    assert_eq!(ui.term.backend().buffer().area.width, 80);
    let text = ui.text();
    assert_eq!(text.lines().count(), 20);
    assert!(text.lines().last().unwrap().ends_with("keys"), "{text}");
}

#[tokio::test]
async fn suspend_resume_and_mouse_toggle_go_through_control() {
    let (tx, mut rx) = event_channel();
    let (req_tx, _r) = mpsc::unbounded_channel();
    let mut app = App::new(AppConfig::default(), req_tx);
    let mut term = Terminal::new(TestBackend::new(40, 8)).unwrap();
    let control = RecordingControl::default();
    let mut c = control.clone();
    tx.send(AppEvent::Terminal(key("ctrl+z"))).await.unwrap();
    tx.send(AppEvent::Resumed).await.unwrap();
    tx.send(AppEvent::Command(AppCommand::ToggleMouse))
        .await
        .unwrap();
    tx.send(AppEvent::Command(AppCommand::ToggleMouse))
        .await
        .unwrap();
    tx.send(AppEvent::Command(AppCommand::Quit)).await.unwrap();
    run_loop(&mut term, &mut c, &mut app, &mut rx)
        .await
        .unwrap();
    assert_eq!(
        control.calls(),
        vec!["suspend", "enter mouse=true", "mouse=false", "mouse=true"]
    );
}

#[test]
fn terminal_is_restored_on_drop_error_and_panic() {
    let control = RecordingControl::default();
    {
        let _s = TerminalSession::enter(control.clone(), true).unwrap();
    }
    assert_eq!(control.calls(), vec!["enter mouse=true", "leave"]);

    let control = RecordingControl::default();
    let c2 = control.clone();
    let result = std::panic::catch_unwind(move || {
        let _s = TerminalSession::enter(c2, false).unwrap();
        panic!("boom");
    });
    assert!(result.is_err());
    assert_eq!(control.calls(), vec!["enter mouse=false", "leave"]);

    let control = RecordingControl::default();
    TerminalSession::enter(control.clone(), true)
        .unwrap()
        .finish()
        .unwrap();
    assert_eq!(
        control.calls(),
        vec!["enter mouse=true", "leave"],
        "finish does not leave twice"
    );

    struct Failing(RecordingControl);
    impl TerminalControl for Failing {
        fn enter(&mut self, _: bool) -> io::Result<()> {
            Err(io::Error::other("no tty"))
        }
        fn leave(&mut self) -> io::Result<()> {
            self.0.leave()
        }
        fn set_mouse_capture(&mut self, _: bool) -> io::Result<()> {
            Ok(())
        }
        fn suspend(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let control = RecordingControl::default();
    assert!(TerminalSession::enter(Failing(control.clone()), true).is_err());
    assert_eq!(control.calls(), vec!["leave"]);
}

#[test]
fn shutdown_signal_quits() {
    let mut ui = Ui::new(AppConfig::default());
    ui.app.handle(AppEvent::Shutdown);
    assert!(ui.app.should_quit());
}

#[test]
fn cold_start_to_interactive_under_150ms() {
    let start = Instant::now();
    let cfg = load_str("", None).unwrap();
    let app_cfg = AppConfig::from_config_with(&cfg, ColorDepth::TrueColor).unwrap();
    let (tx, _rx) = mpsc::unbounded_channel();
    let mut app = App::new(app_cfg, tx);
    let mut term = Terminal::new(TestBackend::new(200, 60)).unwrap();
    term.draw(|f| app.render(f)).unwrap();
    // Interactive: the first key is handled and drawn.
    app.handle(AppEvent::Terminal(key("a")));
    term.draw(|f| app.render(f)).unwrap();
    let elapsed = start.elapsed();
    assert!(
        elapsed < Duration::from_millis(150),
        "cold start took {elapsed:?}"
    );
}

#[test]
fn typing_send_newline_cancel_and_palette() {
    let mut ui = Ui::new(AppConfig::default());
    ui.send(type_text("one"));
    ui.send(keys("shift+enter alt+enter"));
    ui.send(type_text("two"));
    assert_eq!(ui.app.draft(), "one\n\ntwo");
    ui.send(keys("enter"));
    assert_eq!(ui.app.draft(), "");
    ui.send(keys("esc ctrl+c"));
    assert_eq!(
        ui.requests(),
        vec![
            UiRequest::Submit("one\n\ntwo".into()),
            UiRequest::Cancel,
            UiRequest::Interrupt,
        ]
    );
    assert!(ui.text().contains("▌ one"), "{}", ui.text());
    // Ctrl+P opens the palette in place (no round trip to the core).
    ui.send(keys("ctrl+p"));
    assert!(ui.app.chat().palette().is_open());
    ui.send(keys("esc"));
    assert!(!ui.app.chat().palette().is_open());
}

#[test]
fn tab_switches_view_and_view_tabs_are_clickable() {
    let mut ui = Ui::new(AppConfig::default());
    ui.app.handle(node_started(0, "planner"));
    ui.draw();
    ui.send(keys("tab"));
    assert_eq!(ui.app.view(), View::Map);
    assert!(ui.text().contains("planner"), "{}", ui.text());
    ui.send(keys("tab"));
    assert_eq!(ui.app.view(), View::Chat, "Tab on the map goes back");
    ui.click_text(" map ");
    assert_eq!(ui.app.view(), View::Map);
    ui.click_text(" chat ");
    assert_eq!(ui.app.view(), View::Chat);
}

#[test]
fn help_overlay_by_mouse() {
    let mut ui = Ui::new(AppConfig::default());
    ui.click_text("keys");
    assert!(ui.app.help_open());
    assert!(ui.text().contains("command palette"), "{}", ui.text());
    assert_eq!(ui.app.focused(), Some(&ElementId::from(HELP_CLOSE_ID)));
    ui.click_text("[ Close ]");
    assert!(!ui.app.help_open());
    // Clicking outside the overlay also closes it.
    ui.app.command(AppCommand::ShowHelp);
    ui.draw();
    ui.send(vec![click(0, 0)]);
    assert!(!ui.app.help_open(), "{}", ui.text());
    assert_eq!(ui.app.view(), View::Chat);
}

#[test]
fn help_overlay_by_keyboard() {
    let mut ui = Ui::new(AppConfig::default());
    // The palette's "show keys" entry opens it from the keyboard.
    ui.send(keys("ctrl+p"));
    ui.send(type_text("show keys"));
    ui.send(keys("enter"));
    assert!(ui.app.help_open(), "{}", ui.text());
    // Tab stays inside the modal; Enter on Close closes it.
    ui.send(keys("tab tab"));
    assert_eq!(ui.app.focused(), Some(&ElementId::from(HELP_CLOSE_ID)));
    ui.send(keys("enter"));
    assert!(!ui.app.help_open());
    // Esc closes too and does not reach the core.
    ui.app.command(AppCommand::ShowHelp);
    ui.draw();
    ui.send(keys("esc"));
    assert!(!ui.app.help_open());
    assert!(ui.requests().is_empty());
}

#[test]
fn wheel_and_paging_scroll_the_chat() {
    let mut ui = Ui::new(AppConfig::default());
    for i in 0..40 {
        ui.send(type_text(&format!("m{i}")));
        ui.send(keys("enter"));
    }
    assert!(ui.text().contains("▌ m39"), "follows the tail");
    assert!(!ui.text().contains("▌ m5\n"));
    ui.send(vec![wheel_up(5, 3)]);
    ui.send(vec![wheel_up(5, 3)]);
    assert!(!ui.text().contains("▌ m39"), "{}", ui.text());
    ui.send(keys("pgdn pgdn pgdn"));
    assert!(ui.text().contains("▌ m39"), "{}", ui.text());
}

#[test]
fn mouse_off_ignores_clicks_and_palette_can_toggle() {
    let cfg = load_str("[ui]\nmouse = false\n", None).unwrap();
    let app_cfg = AppConfig::from_config_with(&cfg, ColorDepth::TrueColor).unwrap();
    assert!(!app_cfg.mouse);
    let control = RecordingControl::default();
    let session = TerminalSession::enter(control.clone(), app_cfg.mouse).unwrap();
    let mut ui = Ui::new(app_cfg);
    assert!(ui.text().contains("mouse off"));
    ui.click_text("keys");
    assert!(!ui.app.help_open(), "clicks are ignored without capture");
    ui.app.command(AppCommand::ToggleMouse);
    assert_eq!(ui.app.take_effects(), vec![Effect::SetMouseCapture(true)]);
    ui.draw();
    ui.click_text("keys");
    assert!(ui.app.help_open());
    drop(session);
    assert_eq!(control.calls(), vec!["enter mouse=false", "leave"]);
}

#[test]
fn config_errors_are_collected() {
    let cfg = load_str("[theme]\nnope = #000000\n[keys]\nquit = tab\n", None).unwrap();
    let errs = AppConfig::from_config_with(&cfg, ColorDepth::TrueColor).unwrap_err();
    assert_eq!(errs.len(), 2, "{errs:?}");
}

#[test]
fn map_nodes_open_detail_by_mouse_and_keyboard() {
    let mut ui = Ui::sized(AppConfig::default(), 120, 30);
    ui.feed(&demo_run());
    ui.send(keys("tab"));
    assert_eq!(ui.app.view(), View::Map);
    // Keyboard: Enter on the selected node opens its detail, Esc backs out.
    ui.send(keys("enter"));
    assert_eq!(ui.app.screen(), Some("detail"), "{}", ui.text());
    let first = ui.app.detail_node().cloned().unwrap();
    ui.send(keys("esc"));
    assert_eq!(ui.app.screen(), None);
    assert_eq!(ui.app.view(), View::Map);
    // Mouse: a click selects a node, a second click opens it.
    let name = if first.as_str() == "main" {
        "r1"
    } else {
        "main"
    };
    ui.click_text(name);
    ui.click_text(name);
    assert_eq!(ui.app.screen(), Some("detail"), "{}", ui.text());
    assert_ne!(ui.app.detail_node(), Some(&first));
    // Esc closes the detail, then the live pane, then leaves the map.
    ui.send(keys("esc esc esc"));
    assert_eq!(ui.app.view(), View::Chat);
}

#[test]
fn subagent_card_click_opens_its_detail() {
    let mut ui = Ui::sized(AppConfig::default(), 120, 30);
    ui.feed(&demo_run());
    // Subagent cards show the agent's display name.
    ui.click_text("Researcher");
    assert_eq!(ui.app.screen(), Some("detail"), "{}", ui.text());
    assert_eq!(ui.app.detail_node(), Some(&NodeId::from("r1")));
}

#[test]
fn goal_bar_shows_in_both_views_and_is_reachable_by_mouse_and_keyboard() {
    let mut ui = Ui::sized(AppConfig::default(), 120, 30);
    ui.feed(&demo_run());
    assert!(ui.app.goals().has_tasks());
    assert!(ui.text().contains("Fix the parser"), "chat: {}", ui.text());
    ui.send(keys("tab"));
    assert!(ui.text().contains("Fix the parser"), "map: {}", ui.text());
    // Mouse: clicking a task opens the node working on it.
    ui.click_text("Fix parse_header");
    assert_eq!(ui.app.screen(), Some("detail"), "{}", ui.text());
    assert_eq!(ui.app.detail_node(), Some(&NodeId::from("main")));
    ui.send(keys("esc"));
    // Keyboard: the palette focuses the bar; Enter opens, Esc leaves.
    ui.send(keys("ctrl+p"));
    ui.send(type_text("focus goal"));
    ui.send(keys("enter"));
    assert!(ui.app.goal_bar_focused());
    ui.send(keys("enter"));
    assert_eq!(ui.app.screen(), Some("detail"));
    // Narrow terminals collapse the bar into a status-line indicator.
    let mut narrow = Ui::sized(AppConfig::default(), 70, 20);
    narrow.feed(&demo_run());
    assert!(!narrow.text().contains("┌ Goal"), "{}", narrow.text());
    let status = narrow.text().lines().rev().nth(1).unwrap_or("").to_owned();
    assert!(status.contains("☰"), "{}", narrow.text());
    assert!(status.contains("1/2 Fix parse_header"), "{}", narrow.text());
}

fn request(n: usize) -> ApprovalRequest {
    ApprovalRequest {
        approval_id: format!("approval-1-{n}"),
        call_id: format!("call-{n}"),
        node_id: None,
        action: PolicyAction::Bash {
            command: "git push origin main".into(),
        },
        verdict: Verdict {
            decision: Decision::Ask,
            rule: MatchedRule {
                layer: Layer::BuiltIn,
                origin: "outward guard".into(),
                rule: "git push*".into(),
                reason: "outward action".into(),
            },
            command: Some("git push origin main".into()),
            outward: true,
        },
        preview: None,
        suggested_pattern: Some("git push *".into()),
    }
}

#[tokio::test]
async fn core_approvals_are_answered_in_the_chat_from_any_view() {
    let mut ui = Ui::new(AppConfig::default());
    let (chan, mut rx) = ChannelUi::new(2);
    let c = chan.clone();
    let task = tokio::spawn(async move {
        c.ask(ApprovalPrompt {
            batch: 1,
            requests: vec![request(0)],
        })
        .await
    });
    ui.send(keys("tab"));
    assert_eq!(ui.app.view(), View::Map);
    ui.core(CoreEvent::Approval(rx.recv().await.unwrap()));
    // Back to the chat (Esc on the map), answer by key.
    ui.send(keys("esc"));
    assert!(ui.text().contains("[y] once"), "{}", ui.text());
    ui.send(keys("s"));
    assert!(matches!(
        task.await.unwrap()[0].1,
        ApprovalAnswer::Approve(ApprovalScope::Session { .. })
    ));
    // And by mouse.
    let c = chan.clone();
    let task = tokio::spawn(async move {
        c.ask(ApprovalPrompt {
            batch: 2,
            requests: vec![request(0)],
        })
        .await
    });
    ui.core(CoreEvent::Approval(rx.recv().await.unwrap()));
    ui.click_text("[n] deny");
    assert!(matches!(
        task.await.unwrap()[0].1,
        ApprovalAnswer::Deny { .. }
    ));
}

#[test]
fn panels_show_command_output_with_buttons() {
    let mut ui = Ui::new(AppConfig::default());
    ui.core(CoreEvent::Panel {
        title: "Remote hosts".into(),
        lines: vec!["gpu  ok  12 cpus".into()],
        buttons: vec![PanelButton::new("test:gpu", "test gpu")],
    });
    assert_eq!(ui.app.screen(), Some("panel"));
    assert!(ui.text().contains("gpu  ok  12 cpus"));
    ui.click_text("[ test gpu ]");
    assert_eq!(
        ui.requests(),
        vec![UiRequest::PanelAction("test:gpu".into())]
    );
    ui.core(CoreEvent::PanelText {
        title: "Remote hosts".into(),
        lines: vec!["gpu  reachable in 12 ms".into()],
    });
    assert!(ui.text().contains("reachable in 12 ms"));
    // Keyboard: Enter on the focused button, Esc closes.
    ui.send(keys("enter"));
    assert_eq!(
        ui.requests(),
        vec![UiRequest::PanelAction("test:gpu".into())]
    );
    ui.send(keys("esc"));
    assert_eq!(ui.app.screen(), None);
}

#[test]
fn core_palette_entries_slash_commands_and_argument_prompts() {
    let mut ui = Ui::new(AppConfig::default());
    ui.core(CoreEvent::PaletteEntries(vec![PaletteEntry::new(
        "cmd.cost",
        "/cost",
        "Show session cost",
        EntryKind::Builtin,
    )]));
    ui.core(CoreEvent::SlashCommands(vec![SlashCommand::new(
        "cost",
        "Show session cost",
    )]));
    ui.send(keys("ctrl+p"));
    ui.send(type_text("/cost"));
    ui.send(keys("enter"));
    assert_eq!(
        ui.requests(),
        vec![UiRequest::Run(Invocation {
            id: "cmd.cost".into(),
            args: vec![],
        })]
    );
    // Slash completion offers the command.
    ui.send(type_text("/co"));
    assert!(ui.text().contains("Show session cost"), "{}", ui.text());
    ui.send(keys("ctrl+u esc"));
    // A workflow run without its inputs: the palette asks for them.
    ui.core(CoreEvent::PromptArgs(
        PaletteEntry::new("workflow.fix", "/fix", "Fix a bug", EntryKind::Workflow).with_args(
            vec![crate::palette::ArgSpec {
                name: "issue".into(),
                required: true,
                hint: Some("what is broken".into()),
            }],
        ),
    ));
    assert!(ui.app.chat().palette().prompting().is_some());
    ui.send(keys("enter"));
    assert!(ui.requests().is_empty(), "required input");
    ui.send(type_text("crash on empty header"));
    ui.send(keys("enter"));
    assert_eq!(
        ui.requests(),
        vec![UiRequest::Run(Invocation {
            id: "workflow.fix".into(),
            args: vec!["crash on empty header".into()],
        })]
    );
}

const SHA: &str = "0123456789abcdef0123456789abcdef01234567";

fn memory_event() -> AppEvent {
    run_event(
        5,
        None,
        EventKind::Memory {
            action: harness_events::schema::MemoryAction::Written,
            memory_id: SHA.into(),
            summary: "saved in projects/app/build: use cargo nextest".into(),
        },
    )
}

#[test]
fn memory_cards_undo_by_mouse_and_keyboard() {
    let mut ui = Ui::sized(AppConfig::default(), 90, 24);
    ui.app.handle(memory_event());
    ui.draw();
    assert!(ui.text().contains("Updated memory"), "{}", ui.text());
    ui.click_text("[ undo ]");
    assert_eq!(ui.requests(), vec![UiRequest::UndoMemory(SHA.into())]);
    ui.core(CoreEvent::MemoryUndone(SHA.into()));
    assert!(ui.text().contains("undone"), "{}", ui.text());
    assert!(!ui.text().contains("[ undo ]"));

    // Keyboard: Shift+Tab from the input reaches the undo button.
    let mut ui = Ui::sized(AppConfig::default(), 90, 24);
    ui.app.handle(memory_event());
    ui.draw();
    ui.send(keys("shift+tab"));
    ui.send(keys("enter"));
    assert_eq!(ui.requests(), vec![UiRequest::UndoMemory(SHA.into())]);
}

struct FakeReverter(std::sync::Arc<std::sync::Mutex<Vec<String>>>);

impl FileReverter for FakeReverter {
    fn revert(&self, paths: &[&str]) -> Result<(), String> {
        self.0
            .lock()
            .unwrap()
            .extend(paths.iter().map(|p| (*p).to_owned()));
        Ok(())
    }
}

#[test]
fn diff_screen_reverts_files_and_detail_asks_for_it() {
    let mut b = EventBuilder::new();
    b.start(NodeSpec::step("main").agent("coder"));
    b.file_changed(
        "main",
        "src/a.rs",
        harness_events::schema::FileChangeKind::Modified,
        Some("--- a/src/a.rs\n+++ b/src/a.rs\n@@ -1 +1 @@\n-old\n+new\n"),
    );
    let mut ui = Ui::sized(AppConfig::default(), 110, 30);
    ui.feed(b.events());
    let reverted = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    ui.core(CoreEvent::Diff(DiffSpec {
        root: PathBuf::from("/p"),
        node: None,
        report: None,
        reverter: Some(Box::new(FakeReverter(reverted.clone()))),
    }));
    assert_eq!(ui.app.screen(), Some("diff"));
    assert!(ui.text().contains("src/a.rs"), "{}", ui.text());
    assert_eq!(ui.app.diff().unwrap().files().len(), 1);
    ui.click_text("revert");
    assert_eq!(reverted.lock().unwrap().as_slice(), ["src/a.rs"]);
    ui.send(keys("esc"));
    assert_eq!(ui.app.screen(), None);
    // Node detail's diff action goes to the core, which answers with Diff.
    ui.app.open_detail(NodeId::from("main"));
    ui.draw();
    assert_eq!(
        ui.requests(),
        vec![UiRequest::Artifacts(NodeId::from("main"))]
    );
    ui.send(keys("ctrl+p"));
    ui.send(type_text("diff of this node"));
    ui.send(keys("enter"));
    assert_eq!(
        ui.requests(),
        vec![UiRequest::OpenDiff(Some(NodeId::from("main")))]
    );
}

#[test]
fn live_tool_output_streams_into_the_card() {
    let mut b = EventBuilder::new();
    b.start(NodeSpec::step("main").agent("coder"));
    b.model_call("main", "c1", "sonnet");
    b.tool_call("main", "b1", "bash", json!({"command": "cargo test"}));
    b.push(
        Some("main"),
        None,
        EventKind::ToolOutput {
            call_id: "b1".into(),
            stream: OutputStream::Stdout,
            text: "running 3 tests\n".into(),
        },
    );
    let mut ui = Ui::sized(AppConfig::default(), 100, 24);
    ui.feed(b.events());
    assert!(ui.text().contains("running 3 tests"), "{}", ui.text());
}

#[test]
fn tiny_terminals_render_every_view_without_panicking() {
    let (tx, _rx) = mpsc::unbounded_channel();
    let mut app = App::new(AppConfig::default(), tx);
    for e in demo_run() {
        app.handle(AppEvent::Run(Box::new(e)));
    }
    app.handle(AppEvent::Terminal(type_text("draft").remove(0)));
    for (w, h) in [(0, 0), (1, 1), (2, 3), (10, 2), (20, 5), (40, 3), (200, 4)] {
        let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
        term.draw(|f| app.render(f)).unwrap();
        app.handle(AppEvent::Command(AppCommand::SwitchView(View::Map)));
        term.draw(|f| app.render(f)).unwrap();
        app.open_detail(NodeId::from("main"));
        term.draw(|f| app.render(f)).unwrap();
        app.handle(AppEvent::Core(CoreEvent::Panel {
            title: "Panel".into(),
            lines: vec!["x".into(); 5],
            buttons: vec![PanelButton::new("a", "a")],
        }));
        term.draw(|f| app.render(f)).unwrap();
        app.handle(AppEvent::Terminal(key("esc")));
        app.handle(AppEvent::Command(AppCommand::ShowHelp));
        term.draw(|f| app.render(f)).unwrap();
        app.handle(AppEvent::Terminal(key("esc")));
        app.handle(AppEvent::Command(AppCommand::SwitchView(View::Chat)));
        app.handle(AppEvent::Terminal(key("ctrl+p")));
        term.draw(|f| app.render(f)).unwrap();
        app.handle(AppEvent::Terminal(key("esc")));
    }
}

fn snapshot(depth: ColorDepth) -> String {
    let cfg = AppConfig {
        theme: Theme::default().with_depth(depth),
        animations: false,
        ..AppConfig::default()
    };
    let mut ui = Ui::new(cfg);
    ui.app.handle(started(0));
    ui.app.handle(node_started(1, "planner"));
    ui.app.command(AppCommand::ShowHelp);
    ui.draw();
    styled_snapshot(ui.term.backend().buffer())
}

#[test]
fn snapshot_app_truecolor() {
    insta::assert_snapshot!(snapshot(ColorDepth::TrueColor));
}

#[test]
fn snapshot_app_256() {
    insta::assert_snapshot!(snapshot(ColorDepth::Ansi256));
}

#[test]
fn snapshot_app_no_color() {
    insta::assert_snapshot!(snapshot(ColorDepth::None));
}

#[tokio::test]
async fn waiting_gates_are_highlighted_on_the_map_and_open_their_prompt() {
    let mut b = EventBuilder::new();
    b.run_started("plan-approve-implement", Some("plan-approve-implement"));
    b.start(
        NodeSpec::step("wf/approve")
            .kind(NodeKind::Approval)
            .name("approve"),
    );
    b.approval("wf/approve", "gate-1", "approve step approve");
    let mut ui = Ui::sized(AppConfig::default(), 120, 30);
    ui.feed(b.events());
    let (chan, mut rx) = ChannelUi::new(2);
    let c = chan.clone();
    let task = tokio::spawn(async move {
        c.ask(ApprovalPrompt {
            batch: 1,
            requests: vec![ApprovalRequest {
                node_id: Some(NodeId::from("wf/approve")),
                ..request(0)
            }],
        })
        .await
    });
    ui.core(CoreEvent::Approval(rx.recv().await.unwrap()));
    ui.send(keys("tab"));
    let map = ui.text();
    assert!(map.contains("approve"), "{map}");
    assert!(
        map.contains("⏸") || map.to_lowercase().contains("approval"),
        "the waiting gate is highlighted: {map}"
    );
    // Keyboard: Enter on the waiting node goes straight to its prompt.
    ui.send(keys("enter"));
    assert_eq!(ui.app.view(), View::Chat);
    assert!(ui.text().contains("[y] once"), "{}", ui.text());
    // Mouse: back on the map, clicking the node twice does the same.
    ui.send(keys("tab"));
    ui.click_text("⏸ approve ");
    ui.click_text("⏸ approve ");
    assert_eq!(ui.app.view(), View::Chat, "{}", ui.text());
    ui.send(keys("y"));
    assert!(matches!(
        task.await.unwrap()[0].1,
        ApprovalAnswer::Approve(ApprovalScope::Once)
    ));
}

#[test]
fn map_nodes_restore_their_checkpoint_by_key_or_palette() {
    let mut ui = Ui::sized(AppConfig::default(), 120, 30);
    ui.feed(&demo_run());
    ui.send(keys("tab"));
    let selected = ui.app.map().selected().cloned().unwrap();
    ui.send(keys("r"));
    assert_eq!(
        ui.requests(),
        vec![UiRequest::RestoreCheckpoint(selected.clone())]
    );
    // The palette entry (clickable row) does the same for the selection.
    ui.send(keys("ctrl+p"));
    ui.send(type_text("restore checkpoint"));
    ui.click_text("Restore checkpoint");
    assert_eq!(ui.requests(), vec![UiRequest::RestoreCheckpoint(selected)]);
}

#[test]
fn node_detail_shows_the_ledger_and_hygiene_report_with_actions() {
    let mut ui = Ui::sized(AppConfig::default(), 120, 50);
    ui.feed(&demo_run());
    ui.core(CoreEvent::Hygiene {
        node: NodeId::from("main"),
        ledger: vec!["+ debug.log (bash: python run.py)".into()],
        report: vec!["Hygiene 80/100: 1 changed, 0 out of scope, 1 temp/debug".into()],
        temp_files: 1,
    });
    ui.app.open_detail(NodeId::from("main"));
    ui.draw();
    assert_eq!(
        ui.requests(),
        vec![UiRequest::Artifacts(NodeId::from("main"))]
    );
    let text = ui.text();
    assert!(text.contains("debug.log (bash: python run.py)"), "{text}");
    assert!(text.contains("Hygiene 80/100"), "{text}");
    ui.click_text("[remove 1 temp files]");
    assert_eq!(
        ui.requests(),
        vec![UiRequest::RemoveTempFiles(NodeId::from("main"))]
    );
    ui.click_text("[review diff]");
    assert_eq!(
        ui.requests(),
        vec![UiRequest::OpenDiff(Some(NodeId::from("main")))]
    );
}

#[test]
fn host_badges_show_remote_health_on_map_nodes() {
    let mut b = EventBuilder::new();
    b.start(NodeSpec::step("main").agent("coder").host("gpu-box"));
    let mut ui = Ui::sized(AppConfig::default(), 120, 30);
    ui.feed(b.events());
    ui.core(CoreEvent::HostBadge {
        host: "gpu-box".into(),
        badge: "down".into(),
        token: Token::Error,
    });
    ui.send(keys("tab"));
    let map = ui.text();
    assert!(map.contains("gpu-box"), "{map}");
    assert!(map.contains("down"), "badge on the node: {map}");
}

#[test]
fn the_inspector_shows_the_provider_rendered_body() {
    let mut b = EventBuilder::new();
    b.start(NodeSpec::step("main").agent("coder"));
    b.model_call("main", "c1", "sonnet");
    b.finish_call("main", "c1", crate::demo::events::usage(10, 0, 5, 0), 3);
    let (tx, requests) = mpsc::unbounded_channel();
    let app = App::new(AppConfig::default(), tx).with_request_body(Arc::new(|provider, req| {
        Some(json!({"rendered_by": provider, "model": req.model, "marker": "EXACT-BODY"}))
    }));
    let mut ui = Ui {
        app,
        term: Terminal::new(TestBackend::new(120, 40)).unwrap(),
        requests,
    };
    ui.feed(b.events());
    ui.app.open_detail(NodeId::from("main"));
    ui.draw();
    ui.send(keys("ctrl+p"));
    ui.send(type_text("inspect"));
    ui.send(keys("enter"));
    assert_eq!(ui.app.screen(), Some("inspector"), "{}", ui.text());
    assert!(ui.text().contains("EXACT-BODY"), "{}", ui.text());
}

#[test]
fn node_detail_lists_artifacts_and_opens_them_by_mouse_and_keyboard() {
    let mut ui = Ui::sized(AppConfig::default(), 120, 40);
    ui.feed(&demo_run());
    ui.app.open_detail(NodeId::from("main"));
    ui.draw();
    assert_eq!(
        ui.requests(),
        vec![UiRequest::Artifacts(NodeId::from("main"))]
    );
    ui.core(CoreEvent::Artifacts {
        node: NodeId::from("main"),
        names: vec!["plan".into(), "report".into()],
    });
    assert!(ui.text().contains("Artifacts (2)"), "{}", ui.text());
    ui.click_text("[plan]");
    assert_eq!(
        ui.requests(),
        vec![UiRequest::OpenArtifact {
            node: NodeId::from("main"),
            name: "plan".into()
        }]
    );
    // Keyboard: Tab to the second artifact, Enter.
    let target = ElementId(crate::detail::artifact_id(1));
    for _ in 0..40 {
        if ui.app.detail_focused() == Some(&target) {
            break;
        }
        ui.send(keys("tab"));
    }
    assert_eq!(ui.app.detail_focused(), Some(&target), "{}", ui.text());
    ui.send(keys("enter"));
    assert_eq!(
        ui.requests(),
        vec![UiRequest::OpenArtifact {
            node: NodeId::from("main"),
            name: "report".into()
        }]
    );
}

#[test]
fn budget_badges_show_on_step_nodes_and_the_workflow_in_the_header() {
    let mut b = EventBuilder::new();
    b.run_started("ship", Some("ship"));
    b.start(NodeSpec::step("ship").kind(NodeKind::Workflow).name("ship"));
    b.start(
        NodeSpec::step("ship/build")
            .agent("coder")
            .name("build")
            .needs(&[]),
    );
    let mut ui = Ui::sized(AppConfig::default(), 120, 30);
    ui.feed(b.events());
    ui.core(CoreEvent::NodeBadge {
        node: NodeId::from("ship"),
        badge: "$0.42/$5.00 8%".into(),
        token: Token::Number,
    });
    ui.core(CoreEvent::NodeBadge {
        node: NodeId::from("ship/build"),
        badge: "$0.90/$1.00 90%".into(),
        token: Token::Warn,
    });
    ui.send(keys("tab"));
    let map = ui.text();
    assert!(map.contains("budget $0.42/$5.00 8%"), "header: {map}");
    assert!(map.contains("$0.90/$1.00 90%"), "step box: {map}");
}

fn memory_data() -> crate::memory::browser::MemoryData {
    use crate::memory::browser::{CommitRow, FileRow, MemoryData};
    MemoryData {
        drive: "/data/memory".into(),
        enabled: true,
        files: vec![
            FileRow {
                path: "MEMORY.md".into(),
                title: "Memory".into(),
                memories: 1,
            },
            FileRow {
                path: "tools/cargo.md".into(),
                title: "Cargo".into(),
                memories: 1,
            },
        ],
        recent: vec![CommitRow {
            commit: "0123456789abcdef0123456789abcdef01234567".into(),
            at: time::OffsetDateTime::UNIX_EPOCH,
            subject: "memory: write tools/cargo.md".into(),
        }],
        dreams: vec![],
    }
}

#[test]
fn memory_browser_by_mouse_and_keyboard() {
    let mut ui = Ui::sized(AppConfig::default(), 110, 30);
    ui.core(CoreEvent::Memory(memory_data()));
    assert_eq!(ui.app.screen(), Some("memory"));
    assert_eq!(
        ui.requests(),
        vec![UiRequest::Memory(MemoryRequest::Preview(
            "MEMORY.md".into()
        ))]
    );
    ui.core(CoreEvent::MemoryDetail {
        key: "MEMORY.md".into(),
        text: "# Memory\n- prefer tabs".into(),
    });
    assert!(ui.text().contains("- prefer tabs"), "{}", ui.text());
    // Keyboard: down to the next note previews it.
    ui.send(keys("down"));
    assert_eq!(
        ui.requests(),
        vec![UiRequest::Memory(MemoryRequest::Preview(
            "tools/cargo.md".into()
        ))]
    );
    // Mouse: open in the editor suspends the UI with the note's file.
    ui.click_text("open in editor");
    assert_eq!(
        ui.app.take_effects(),
        vec![Effect::Edit(PathBuf::from("/data/memory/tools/cargo.md"))]
    );
    // Ask the agent to change it: the request lands in the input.
    ui.click_text("ask to change");
    assert_eq!(ui.app.screen(), None);
    assert!(ui.app.draft().contains("tools/cargo"), "{}", ui.app.draft());
    // Search by keyboard; the toggle and dream go to the core.
    ui.core(CoreEvent::Memory(memory_data()));
    ui.requests();
    ui.send(keys("right"));
    ui.send(type_text("tabs"));
    ui.send(keys("enter"));
    assert_eq!(
        ui.requests(),
        vec![UiRequest::Memory(MemoryRequest::Search("tabs".into()))]
    );
    ui.click_text("memory off");
    assert_eq!(
        ui.requests(),
        vec![UiRequest::Memory(MemoryRequest::SetEnabled(false))]
    );
    ui.send(keys("esc"));
    assert_eq!(ui.app.screen(), None);
}

// ---- copy and paste ----

/// A chat with an assistant answer (with a secret) and an edit with a diff.
fn copy_run() -> Vec<RunEvent> {
    use harness_events::schema::DeltaChannel;
    let mut b = EventBuilder::new();
    b.start(NodeSpec::step("main").agent("coder"));
    b.model_call("main", "c1", "sonnet");
    b.delta(
        "main",
        "c1",
        DeltaChannel::Text,
        "Use token sk-live-SECRET123 here:\n\n```sh\ncurl -H \"x: 1\"\n```\n",
    );
    b.finish_call("main", "c1", crate::demo::events::usage(10, 0, 5, 0), 20);
    b.tool_call(
        "main",
        "e1",
        "edit",
        json!({"path": "src/a.rs", "old_string": "old", "new_string": "new"}),
    );
    b.tool_result("main", "e1", true, "edited src/a.rs", 1);
    b.file_changed(
        "main",
        "src/a.rs",
        harness_events::schema::FileChangeKind::Modified,
        Some("--- a/src/a.rs\n+++ b/src/a.rs\n@@ -1 +1 @@\n-old\n+new\n"),
    );
    b.into_events()
}

impl Ui {
    /// Performs pending effects through `control`, like the loop.
    fn flush(&mut self, control: &mut RecordingControl) {
        flush(&mut self.term, control, &mut self.app).unwrap();
    }
}

#[test]
fn copy_writes_the_exact_osc52_sequence_redacted_and_toasts() {
    let redactor = std::sync::Arc::new(harness_events::redact::Redactor::new([
        "sk-live-SECRET123".to_owned(),
    ]));
    let (tx, requests) = mpsc::unbounded_channel();
    let mut ui = Ui {
        app: App::new(AppConfig::default(), tx).with_redactor(redactor.clone()),
        term: Terminal::new(TestBackend::new(100, 30)).unwrap(),
        requests,
    };
    ui.app
        .set_clipboard(Clipboard::osc52_only().with_redactor(redactor));
    ui.feed(&copy_run());
    let mut control = RecordingControl::default();
    // Keyboard: the copy key copies the last assistant message.
    ui.send(keys("ctrl+y"));
    ui.flush(&mut control);
    let masked = harness_events::redact::Redactor::new(["sk-live-SECRET123".to_owned()])
        .redact("Use token sk-live-SECRET123 here:\n\n```sh\ncurl -H \"x: 1\"\n```\n")
        .into_owned();
    assert!(!masked.contains("SECRET123"), "{masked}");
    assert_eq!(
        control.calls(),
        vec![format!(
            "osc {}",
            crate::clipboard::osc52(&masked).escape_debug()
        )]
    );
    assert!(ui.text().contains("copied message"), "{}", ui.text());
    // Click: the code block's button copies just the code.
    let (x, y) = find_text(ui.term.backend().buffer(), "sh  ⧉ copy").unwrap();
    ui.send(vec![click(x + 4, y)]);
    ui.flush(&mut control);
    assert_eq!(
        control.calls().last().unwrap(),
        &format!(
            "osc {}",
            crate::clipboard::osc52("curl -H \"x: 1\"").escape_debug()
        )
    );
    // Palette: the diff.
    ui.send(keys("ctrl+p"));
    ui.send(type_text("copy diff"));
    ui.send(keys("enter"));
    ui.flush(&mut control);
    assert_eq!(
        control.calls().last().unwrap(),
        &format!(
            "osc {}",
            crate::clipboard::osc52("--- a/src/a.rs\n+++ b/src/a.rs\n@@ -1 +1 @@\n-old\n+new\n")
                .escape_debug()
        )
    );
    assert!(ui.text().contains("copied diff"), "{}", ui.text());
}

#[test]
fn copy_from_detail_and_diff_screens_by_key_click_and_palette() {
    let mut ui = Ui::sized(AppConfig::default(), 120, 30);
    ui.app.set_clipboard(Clipboard::osc52_only());
    ui.feed(&copy_run());
    let mut control = RecordingControl::default();
    // Node detail: focus the turn, then `y`, the copy key and the button.
    ui.app.open_detail(NodeId::from("main"));
    ui.draw();
    ui.send(keys("tab"));
    assert!(ui.app.detail_focused().is_some());
    ui.send(keys("y"));
    ui.flush(&mut control);
    assert_eq!(control.calls().len(), 1, "{:?}", control.calls());
    ui.send(keys("ctrl+y"));
    ui.flush(&mut control);
    assert_eq!(control.calls().len(), 2);
    assert_eq!(control.calls()[0], control.calls()[1]);
    ui.click_text("[⧉ copy entry]");
    ui.flush(&mut control);
    assert_eq!(control.calls().len(), 3);
    assert_eq!(
        control.calls()[2],
        control.calls()[0],
        "the last focused entry"
    );
    assert!(ui.text().contains("copied log entry"), "{}", ui.text());
    ui.send(keys("ctrl+p"));
    ui.send(type_text("copy entry"));
    ui.send(keys("enter"));
    ui.flush(&mut control);
    assert_eq!(control.calls().len(), 4);
    ui.send(keys("esc"));
    // Diff review: `y`, the copy key, the button and the palette.
    ui.core(CoreEvent::Diff(DiffSpec {
        root: PathBuf::from("/p"),
        node: None,
        report: None,
        reverter: None,
    }));
    assert_eq!(ui.app.screen(), Some("diff"));
    let want = format!(
        "osc {}",
        crate::clipboard::osc52("--- a/src/a.rs\n+++ b/src/a.rs\n@@ -1 +1 @@\n-old\n+new\n")
            .escape_debug()
    );
    for how in ["y", "ctrl+y", "click", "palette"] {
        match how {
            "click" => ui.click_text("[⧉ copy diff]"),
            "palette" => {
                ui.send(keys("ctrl+p"));
                ui.send(type_text("copy diff"));
                ui.send(keys("enter"));
            }
            k => ui.send(keys(k)),
        }
        ui.flush(&mut control);
        assert_eq!(control.calls().last().unwrap(), &want, "{how}");
    }
    assert!(
        ui.text().contains("copied diff of src/a.rs"),
        "{}",
        ui.text()
    );
    // Toasts over a screen are dismissed with a click, like in the chat.
    for _ in 0..20 {
        if !ui.text().contains("copied") {
            break;
        }
        ui.click_text("copied");
    }
    assert_eq!(
        ui.app.screen(),
        Some("diff"),
        "the clicks went to the toasts"
    );
    assert!(ui.app.chat_mut().notifications_mut().toasts().is_empty());
}

#[test]
fn system_clipboard_tool_receives_the_text() {
    let tmp = tempfile::tempdir().unwrap();
    let out = tmp.path().join("clip");
    let tool = crate::clipboard::SystemTool {
        program: "sh".into(),
        args: vec!["-c".into(), format!("cat > '{}'", out.display())],
    };
    let mut ui = Ui::sized(AppConfig::default(), 100, 30);
    ui.app.set_clipboard(
        Clipboard::from_env(ClipboardMode::System, &|_| None, &|_| false).with_tool(Some(tool)),
    );
    ui.feed(&copy_run());
    let mut control = RecordingControl::default();
    ui.send(keys("ctrl+p"));
    ui.send(type_text("copy last code block"));
    ui.send(keys("enter"));
    ui.flush(&mut control);
    assert!(
        control.calls().is_empty(),
        "no OSC 52: {:?}",
        control.calls()
    );
    assert_eq!(std::fs::read_to_string(out).unwrap(), "curl -H \"x: 1\"");
    assert!(ui.text().contains("copied code block"), "{}", ui.text());
}

#[test]
fn pastes_go_to_the_palette_over_any_view_dialog_fields_and_the_input() {
    let mut ui = Ui::sized(AppConfig::default(), 110, 30);
    ui.feed(&copy_run());
    // Palette over the map.
    ui.send(keys("tab"));
    assert_eq!(ui.app.view(), View::Map);
    ui.send(keys("ctrl+p"));
    ui.send(vec![Event::Paste("show\nkeys".into())]);
    assert_eq!(ui.app.chat().palette().query(), "show keys");
    ui.send(keys("esc"));
    // A paste on the map goes to the input and shows the chat.
    ui.send(vec![Event::Paste("a\tb\nc".into())]);
    assert_eq!(ui.app.view(), View::Chat);
    assert_eq!(ui.app.draft(), "a\tb\nc");
    // Detail has no text field: the paste is dropped, nothing is sent.
    ui.app.open_detail(NodeId::from("main"));
    ui.draw();
    ui.send(vec![Event::Paste("zzz\n".into())]);
    assert_eq!(ui.app.draft(), "a\tb\nc");
    ui.send(keys("esc"));
    // The memory browser's search box takes the paste.
    ui.core(CoreEvent::Memory(memory_data()));
    ui.send(vec![Event::Paste("cargo\nbuild".into())]);
    match &ui.app.screen {
        Some(Screen::Memory(m)) => assert_eq!(m.browser.query(), "cargo build"),
        _ => panic!("memory screen"),
    }
    ui.send(keys("esc"));
    // Help open: dropped.
    let _ = ui.requests();
    assert!(
        !ui.requests()
            .iter()
            .any(|r| matches!(r, UiRequest::Submit(_))),
        "a paste never sends"
    );
}

#[test]
fn mouse_off_keeps_paste_and_keyboard_copy_working() {
    let cfg = load_str("[ui]\nmouse = false\nclipboard = osc52\n", None).unwrap();
    let app_cfg = AppConfig::from_config_with(&cfg, ColorDepth::TrueColor).unwrap();
    assert!(!app_cfg.mouse);
    assert_eq!(app_cfg.clipboard, ClipboardMode::Osc52);
    let mut ui = Ui::sized(app_cfg, 100, 30);
    assert!(!ui.app.mouse_enabled());
    ui.feed(&copy_run());
    ui.send(vec![Event::Paste("pasted\ttext".into())]);
    assert_eq!(ui.app.draft(), "pasted\ttext");
    let mut control = RecordingControl::default();
    ui.send(keys("ctrl+y"));
    ui.flush(&mut control);
    let copies: Vec<String> = control
        .calls()
        .into_iter()
        .filter(|c| c.starts_with("osc \\u{1b}]52;"))
        .collect();
    assert_eq!(copies.len(), 1, "{:?}", control.calls());
}

fn open_models(ui: &mut Ui) {
    ui.core(CoreEvent::ModelPicker {
        catalog: crate::model_picker::tests::sample(true),
        current: Some("anthropic/claude-sonnet-5-5".into()),
    });
}

#[test]
fn model_picker_opens_over_every_view_and_switches_by_keyboard() {
    let mut ui = Ui::sized(AppConfig::default(), 120, 30);
    ui.send(type_text("draft"));
    open_models(&mut ui);
    assert!(ui.app.model_picker().is_some());
    let text = ui.text();
    assert!(text.contains("Switch model"), "{text}");
    assert!(text.contains(crate::model_picker::REFRESHING), "{text}");
    // Typing filters the picker, not the chat input.
    ui.send(type_text("haiku"));
    assert_eq!(ui.app.model_picker().unwrap().query(), "haiku");
    assert_eq!(ui.app.draft(), "draft");
    // A finished refresh replaces the catalog in place.
    ui.core(CoreEvent::ModelCatalog(crate::model_picker::tests::sample(
        false,
    )));
    assert!(!ui.text().contains(crate::model_picker::REFRESHING));
    ui.send(keys("enter"));
    assert!(ui.app.model_picker().is_none());
    assert_eq!(
        ui.requests(),
        vec![UiRequest::SetModel("anthropic/claude-haiku-5-5".into())]
    );
    // Esc closes without a request; a catalog update while closed is a no-op.
    open_models(&mut ui);
    ui.send(keys("down esc"));
    assert!(ui.app.model_picker().is_none());
    ui.core(CoreEvent::ModelCatalog(crate::model_picker::tests::sample(
        false,
    )));
    assert!(ui.app.model_picker().is_none());
    assert!(ui.requests().is_empty());
    // Over the map too.
    ui.send(keys("tab"));
    assert_eq!(ui.app.view(), View::Map);
    open_models(&mut ui);
    assert!(ui.text().contains("Switch model"));
    ui.send(keys("esc"));
    assert_eq!(ui.app.view(), View::Map);
}

#[test]
fn model_picker_rows_are_clickable_and_the_wheel_scrolls() {
    let mut ui = Ui::sized(AppConfig::default(), 120, 30);
    open_models(&mut ui);
    let before = ui
        .app
        .model_picker()
        .unwrap()
        .selected_entry()
        .unwrap()
        .reference
        .to_string();
    let (x, y) = find_text(ui.term.backend().buffer(), "claude-haiku-5-5").unwrap();
    ui.send(vec![wheel_down(x, y)]);
    let after = ui
        .app
        .model_picker()
        .unwrap()
        .selected_entry()
        .unwrap()
        .reference
        .to_string();
    assert_ne!(before, after);
    ui.click_text("claude-opus-5-5");
    assert!(ui.app.model_picker().is_none());
    assert_eq!(
        ui.requests(),
        vec![UiRequest::SetModel("anthropic/claude-opus-5-5".into())]
    );
    // A click outside closes it; pasted text goes to the filter.
    open_models(&mut ui);
    ui.app
        .handle(AppEvent::Terminal(Event::Paste("gemini".into())));
    assert_eq!(ui.app.model_picker().unwrap().query(), "gemini");
    ui.send(vec![click(0, 0)]);
    assert!(ui.app.model_picker().is_none());
    assert!(ui.requests().is_empty());
}

#[test]
fn model_picker_is_ninety_percent_of_the_terminal_at_several_sizes() {
    for (w, h) in [(80, 24), (120, 30), (200, 50)] {
        let mut ui = Ui::sized(AppConfig::default(), w, h);
        open_models(&mut ui);
        let buf = ui.term.backend().buffer();
        let (_, top) = find_text(buf, "╭ Switch model").unwrap();
        let (_, bottom) = find_text(buf, "╰").unwrap();
        assert_eq!(
            bottom - top + 1,
            crate::model_picker::dialog_height(h),
            "{w}x{h}"
        );
    }
}

fn open_sessions(ui: &mut Ui) {
    ui.core(CoreEvent::SessionPicker(
        crate::session_picker::tests::sample(),
    ));
}

#[test]
fn session_picker_filters_and_resumes_by_keyboard_over_every_view() {
    let mut ui = Ui::sized(AppConfig::default(), 120, 30);
    ui.send(type_text("draft"));
    open_sessions(&mut ui);
    assert!(ui.app.session_picker().is_some());
    let text = ui.text();
    assert!(text.contains(crate::session_picker::TITLE), "{text}");
    assert!(text.contains("Fix header parser"), "{text}");
    assert!(text.contains("yesterday 09:10"), "{text}");
    // Typing filters the picker, not the chat input.
    ui.send(type_text("release"));
    assert_eq!(ui.app.session_picker().unwrap().query(), "release");
    assert_eq!(ui.app.draft(), "draft");
    ui.send(keys("enter"));
    assert!(ui.app.session_picker().is_none());
    assert_eq!(
        ui.requests(),
        vec![UiRequest::Resume("s-000000cccccc".into())]
    );
    // Arrows move; Esc closes without a request.
    open_sessions(&mut ui);
    ui.send(keys("down down esc"));
    assert!(ui.app.session_picker().is_none());
    assert!(ui.requests().is_empty());
    open_sessions(&mut ui);
    ui.send(keys("down enter"));
    assert_eq!(
        ui.requests(),
        vec![UiRequest::Resume("s-000000cccccc".into())]
    );
    // Over the map too.
    ui.send(keys("tab"));
    assert_eq!(ui.app.view(), View::Map);
    open_sessions(&mut ui);
    assert!(ui.text().contains(crate::session_picker::TITLE));
    ui.send(keys("esc"));
    assert_eq!(ui.app.view(), View::Map);
    assert!(ui.app.session_picker().is_none());
}

#[test]
fn session_picker_rows_are_clickable_and_the_wheel_scrolls() {
    let mut ui = Ui::sized(AppConfig::default(), 120, 30);
    open_sessions(&mut ui);
    let before = ui
        .app
        .session_picker()
        .unwrap()
        .selected()
        .unwrap()
        .id
        .clone();
    let (x, y) = find_text(ui.term.backend().buffer(), "Release notes").unwrap();
    ui.send(vec![wheel_down(x, y)]);
    let after = ui
        .app
        .session_picker()
        .unwrap()
        .selected()
        .unwrap()
        .id
        .clone();
    assert_ne!(before, after);
    // A click on the description row resumes that session.
    ui.click_text("parser docs and the guide");
    assert!(ui.app.session_picker().is_none());
    assert_eq!(
        ui.requests(),
        vec![UiRequest::Resume("s-000000dddddd".into())]
    );
    // Clicking the name row works too.
    open_sessions(&mut ui);
    ui.click_text("Fix header parser");
    assert_eq!(
        ui.requests(),
        vec![UiRequest::Resume("s-000000bbbbbb".into())]
    );
    // A click outside closes it; pasted text goes to the filter.
    open_sessions(&mut ui);
    ui.app
        .handle(AppEvent::Terminal(Event::Paste("docs".into())));
    assert_eq!(ui.app.session_picker().unwrap().query(), "docs");
    ui.send(vec![click(0, 0)]);
    assert!(ui.app.session_picker().is_none());
    assert!(ui.requests().is_empty());
}

#[test]
fn palette_run_entry_follows_the_live_run() {
    use crate::history::RunSummary;
    let mut ui = Ui::new(AppConfig::default());
    let mut b = EventBuilder::new();
    b.run_started("coder", None);
    b.start(NodeSpec::step("main").agent("coder"));
    // The core lists the run as soon as it begins: no title yet, 0ms.
    let stale = RunSummary::from_state(&RunState::from_events(b.events()), b.now()).unwrap();
    assert_eq!(stale.duration_ms, Some(0));
    ui.core(CoreEvent::PaletteEntries(crate::palette::run_entries(
        &[stale],
        UtcOffset::UTC,
        b.now(),
    )));
    b.user_message("main", "fix the header parser");
    ui.feed(b.events());
    ui.send(keys("ctrl+p"));
    let runs: Vec<&PaletteEntry> = ui
        .app
        .chat()
        .palette()
        .entries()
        .iter()
        .filter(|e| e.kind == EntryKind::Run)
        .collect();
    assert_eq!(runs.len(), 1, "{runs:?}");
    assert!(
        runs[0].title.contains("· fix the header parser ·"),
        "{}",
        runs[0].title
    );
    assert!(
        runs[0].description.starts_with("Running"),
        "{}",
        runs[0].description
    );
    assert!(
        !runs[0].description.contains("0ms"),
        "{}",
        runs[0].description
    );
}
