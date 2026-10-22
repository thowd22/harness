use super::*;
use crate::demo::events::{EventBuilder, NodeSpec, usage};
use crate::interaction::testing::{buffer_text, click, find_text, key};
use ratatui::Terminal;
use ratatui::backend::TestBackend;

fn user(text: &str) -> Value {
    json!({"role": "user", "content": [{"type": "text", "text": text}]})
}

fn assistant(text: &str) -> Value {
    json!({"role": "assistant", "content": [{"type": "text", "text": text}]})
}

fn events() -> Vec<RunEvent> {
    let mut b = EventBuilder::new();
    b.start(NodeSpec::step("main"));
    b.model_call_with(
        "main",
        "c1",
        "sonnet",
        json!({
            "provider": "anthropic",
            "model": "claude-sonnet-5-5",
            "system_sha256": "abc",
            "system": "You are a coding agent.",
            "tools": ["read", "bash"],
            "messages_from": 0,
            "messages": [user("fix the parser, token sk-ant-api03-SECRETSECRETSECRETSECRET")],
        }),
    );
    b.delta("main", "c1", DeltaChannel::Reasoning, "Look at parse.rs");
    b.delta("main", "c1", DeltaChannel::Text, "Reading it.");
    b.finish_call_with(
        "main",
        "c1",
        usage(100, 0, 20, 5),
        900,
        Some(assistant("Reading it.")),
    );
    b.model_call_with(
        "main",
        "c2",
        "sonnet",
        json!({
            "provider": "anthropic",
            "model": "claude-sonnet-5-5",
            "system_sha256": "abc",
            "tools": ["read", "bash"],
            "messages_from": 1,
            "messages": [assistant("Reading it."), user("continue")],
        }),
    );
    b.finish_call_with(
        "main",
        "c2",
        usage(10, 120, 30, 0),
        500,
        Some(assistant("Done.")),
    );
    b.into_events()
}

#[test]
fn rebuilds_the_exact_request_and_raw_response() {
    let ev = events();
    let i = Inspection::from_events(&ev, "c2", None).unwrap();
    assert_eq!(i.recorded["messages_from"], 1);
    assert_eq!(i.request["system"], "You are a coding agent.");
    let msgs = i.request["messages"].as_array().unwrap();
    assert_eq!(msgs.len(), 3, "full history: user, assistant, user");
    assert_eq!(i.request["model"], "claude-sonnet-5-5");
    assert_eq!(i.response.as_ref().unwrap()["content"][0]["text"], "Done.");
    assert_eq!(i.usage.unwrap().cached_input, 120);
    let first = Inspection::from_events(&ev, "c1", None).unwrap();
    assert_eq!(first.streamed_reasoning, "Look at parse.rs");
    assert_eq!(first.streamed_text, "Reading it.");
    assert!(Inspection::from_events(&ev, "missing", None).is_none());
}

#[test]
fn payloads_are_redacted() {
    let ev = events();
    let i = Inspection::from_events(&ev, "c1", None).unwrap();
    let r = Arc::new(Redactor::default());
    let v = InspectorView::new(i, Theme::default(), std::env::temp_dir(), Some(r));
    let text = v.data().to_json().to_string();
    assert!(!text.contains("SECRETSECRET"), "{text}");
    assert!(text.contains("[REDACTED]"), "{text}");
}

#[test]
fn layers_show_tokens_and_cache_breakpoints() {
    let ev = events();
    let i = Inspection::from_events(&ev, "c2", None).unwrap();
    let body = i.provider_body.as_ref().expect("anthropic body rebuilt");
    assert!(body["messages"].is_array());
    let labels: Vec<&str> = i.layers.iter().map(|l| l.label.as_str()).collect();
    assert!(labels[0].starts_with("tools (2): read, bash"), "{labels:?}");
    assert_eq!(labels[1], "system prompt");
    assert!(labels[2].starts_with("#1 user"), "{labels:?}");
    assert!(i.layers.iter().any(|l| l.breakpoint), "{:?}", i.layers);
    assert_eq!(
        i.layers[1].tokens,
        estimate_tokens("You are a coding agent.")
    );
    // Messages new in this call are marked.
    assert!(!i.layers[2].new);
    assert!(i.layers[3].new && i.layers[4].new);
}

#[test]
fn export_writes_the_payloads() {
    let dir = tempfile::tempdir().unwrap();
    let i = Inspection::from_events(&events(), "c2", None).unwrap();
    let mut v = InspectorView::new(i, Theme::default(), dir.path().to_path_buf(), None);
    let mut term = Terminal::new(TestBackend::new(120, 30)).unwrap();
    term.draw(|f| v.render(f, f.area())).unwrap();
    let (x, y) = find_text(term.backend().buffer(), "[export]").unwrap();
    let out = v.handle(&click(x, y));
    let Some(InspectorOutput::Exported(path)) = out else {
        panic!("{out:?}")
    };
    let written: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(written["call_id"], "c2");
    assert_eq!(written["request"]["messages"].as_array().unwrap().len(), 3);
    assert!(written["provider_body"].is_object());
    // Keyboard export too.
    assert!(matches!(
        v.handle(&key("e")),
        Some(InspectorOutput::Exported(_))
    ));
}

#[test]
fn tabs_switch_by_click_and_keyboard() {
    let i = Inspection::from_events(&events(), "c1", None).unwrap();
    let mut v = InspectorView::new(i, Theme::default(), std::env::temp_dir(), None);
    let mut term = Terminal::new(TestBackend::new(120, 40)).unwrap();
    term.draw(|f| v.render(f, f.area())).unwrap();
    assert!(buffer_text(term.backend().buffer()).contains("recorded request (as logged)"));
    let (x, y) = find_text(term.backend().buffer(), "[Response]").unwrap();
    v.handle(&click(x, y));
    assert_eq!(v.tab(), Tab::Response);
    term.draw(|f| v.render(f, f.area())).unwrap();
    let text = buffer_text(term.backend().buffer());
    assert!(text.contains("streamed reasoning"), "{text}");
    assert!(text.contains("raw response message"), "{text}");
    // Keyboard: focus the Layers button and press Enter.
    v.handle(&key("tab"));
    v.handle(&key("enter"));
    assert_eq!(v.tab(), Tab::Layers);
    term.draw(|f| v.render(f, f.area())).unwrap();
    assert!(buffer_text(term.backend().buffer()).contains("◆ cache breakpoint"));
    v.handle(&key("1"));
    assert_eq!(v.tab(), Tab::Request);
    assert_eq!(v.handle(&key("esc")), Some(InspectorOutput::Back));
}

#[test]
fn request_tab_shows_raw_and_wire_body_size() {
    let notice = harness_providers::openai::notice::Notice::RequestBody {
        provider: "anthropic".into(),
        raw_bytes: 286_255,
        wire_bytes: 47_303,
        encoding: Some("gzip".into()),
    };
    let mut ev = events();
    for e in &mut ev {
        if let EventKind::ModelCallFinished {
            call_id, response, ..
        } = &mut e.kind
            && call_id == "c2"
        {
            let mut m = assistant("Done.");
            m["content"]
                .as_array_mut()
                .unwrap()
                .push(serde_json::to_value(notice.to_block()).unwrap());
            *response = Some(Payload::Json(m));
        }
    }
    let i = Inspection::from_events(&ev, "c2", None).unwrap();
    let w = i.wire.clone().unwrap();
    assert_eq!((w.raw_bytes, w.wire_bytes), (286_255, 47_303));
    assert_eq!(i.to_json()["request_body"]["wire_bytes"], 47_303);
    let mut v = InspectorView::new(i, Theme::default(), std::env::temp_dir(), None);
    let mut term = Terminal::new(TestBackend::new(120, 40)).unwrap();
    term.draw(|f| v.render(f, f.area())).unwrap();
    let text = buffer_text(term.backend().buffer());
    assert!(
        text.contains("request body 279.5 KB raw · 46.2 KB on the wire (gzip, 83% smaller)"),
        "{text}"
    );
    // Calls without the notice show no size line.
    let plain = Inspection::from_events(&events(), "c1", None).unwrap();
    assert!(plain.wire.is_none());
    assert!(plain.to_json()["request_body"].is_null());
}
