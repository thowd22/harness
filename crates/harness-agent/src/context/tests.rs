use super::*;
use harness_providers::model::{ChatRequest, ContentBlock, Message, Role, ToolSpec, Usage};
use serde_json::json;

fn tool() -> ToolSpec {
    ToolSpec {
        name: "read".into(),
        description: "Read a file".into(),
        input_schema: json!({"type": "object", "properties": {"path": {"type": "string"}}}),
    }
}

#[test]
fn estimates_request_size_by_part_before_sending() {
    let request = ChatRequest {
        system: Some("x".repeat(400)),
        tools: vec![tool()],
        messages: vec![Message::user("y".repeat(800)), Message::assistant("ok")],
        ..ChatRequest::new("m")
    };
    let est = estimate_request(&request);
    assert_eq!(est.system, 100);
    assert!(est.tools > 8);
    assert_eq!(est.messages, 4 + 200 + 4 + 1);
    assert_eq!(est.total(), est.system + est.tools + est.messages);
}

#[test]
fn tracker_anchors_on_reported_usage_and_estimates_only_new_messages() {
    let mut t = ContextTracker::new(10_000);
    let mut messages = vec![Message::user("hello")];
    let first = t.estimate(Some("sys"), &[tool()], &messages);
    assert!(!first.calibrated);
    // The provider reports the real size, which differs from the estimate.
    messages.push(Message::assistant("answer"));
    t.observe_usage(
        &Usage {
            input_tokens: 1000,
            cache_read_tokens: 3000,
            output_tokens: 50,
            ..Usage::default()
        },
        messages.len(),
    );
    assert_eq!(t.peak(), 4000);
    messages.push(Message::user("z".repeat(400)));
    let next = t.estimate(Some("sys"), &[tool()], &messages);
    assert!(next.calibrated);
    assert_eq!(next.tokens, 4050 + 4 + 100);
    assert!((next.fraction() - 0.4154).abs() < 1e-9);
    // After a rewrite, the tracker falls back to a full estimate.
    t.invalidate();
    assert!(!t.estimate(None, &[], &messages).calibrated);
    assert_eq!(t.last().unwrap().window, 10_000);
}

#[test]
fn prefix_layers_are_ordered_and_frozen() {
    let mut memory = String::from("# Memory\n- fact one");
    let prefix = SessionPrefix::freeze("You are a coder.", Some(&memory), Some("Skills:\n- a"));
    let kinds: Vec<LayerKind> = prefix.layers().iter().map(|l| l.kind).collect();
    assert_eq!(
        kinds,
        [LayerKind::System, LayerKind::MemoryIndex, LayerKind::Skills]
    );
    assert_eq!(
        prefix.text(),
        "You are a coder.\n\n# Memory\n- fact one\n\nSkills:\n- a"
    );
    assert_eq!(prefix.layer(LayerKind::Skills), Some("Skills:\n- a"));
    // Later memory writes do not change the frozen prefix.
    let frozen = prefix.clone();
    memory.push_str("\n- fact two");
    assert_eq!(prefix, frozen);
    assert_eq!(prefix.sha256().len(), 64);
    // Empty layers are left out.
    let bare = SessionPrefix::freeze("s", Some("  "), None);
    assert_eq!(bare.layers().len(), 1);
    assert_eq!(bare.text(), "s");
}

#[test]
fn reasoning_is_kept_by_default_and_dropped_only_when_configured() {
    let thinking = ContentBlock::Reasoning {
        text: "think".into(),
        signature: Some("sig".into()),
    };
    let openai = ContentBlock::Opaque {
        provider: "openai".into(),
        data: json!({"type": "reasoning", "encrypted_content": "e"}),
    };
    let notice = ContentBlock::Opaque {
        provider: "harness".into(),
        data: json!({"type": "cost"}),
    };
    let assistant = |extra: Vec<ContentBlock>| Message {
        role: Role::Assistant,
        content: extra,
    };
    let original = vec![
        Message::user("first task"),
        assistant(vec![
            thinking.clone(),
            openai.clone(),
            ContentBlock::text("done"),
        ]),
        Message::user("second task"),
        assistant(vec![
            thinking.clone(),
            ContentBlock::tool_use("t1", "read", json!({})),
        ]),
        Message {
            role: Role::User,
            content: vec![ContentBlock::tool_result("t1", "out", false)],
        },
        assistant(vec![thinking.clone(), notice.clone()]),
    ];
    let mut keep = original.clone();
    assert_eq!(ReasoningRetention::default().apply(&mut keep), None);
    assert_eq!(keep, original);

    let mut drop = original.clone();
    assert_eq!(
        ReasoningRetention::DropCompletedTurns.apply(&mut drop),
        Some(1)
    );
    assert_eq!(drop[1].content, vec![ContentBlock::text("done")]);
    // The current tool loop (after the latest prompt) is untouched.
    assert_eq!(drop[3..], original[3..]);
    // Applying again changes nothing.
    assert_eq!(
        ReasoningRetention::DropCompletedTurns.apply(&mut drop),
        None
    );
}

#[test]
fn remaining_context_checker() {
    assert!(mentions_remaining_context(
        "You have 12,000 tokens remaining."
    ));
    assert!(mentions_remaining_context("Context window 85% full"));
    assert!(mentions_remaining_context(
        "context usage: 0.8 of the window"
    ));
    assert!(!mentions_remaining_context(CONTEXT_PROMPT_SECTION));
    assert!(!mentions_remaining_context(
        "Edit src/main.rs line 12 to fix the bug."
    ));
}

#[test]
fn initial_context_reports_layers_and_checks_the_threshold() {
    let prefix = SessionPrefix::freeze("x".repeat(4000), Some(&"m".repeat(400)), None);
    let init = InitialContext::measure(&prefix, &[tool()]);
    assert_eq!(init.system, 1000);
    assert_eq!(init.memory_index, 100);
    assert_eq!(init.skills, 0);
    assert!(init.check(DEFAULT_INITIAL_BUDGET).is_ok());
    let err = init.check(1000).unwrap_err();
    assert!(err.contains("threshold 1000"), "{err}");
    assert!(err.contains(&format!("initial context {} tokens", init.total())));
}
