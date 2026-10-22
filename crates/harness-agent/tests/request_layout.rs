//! Cache-stable request layout and reasoning preservation, checked against
//! the real request bodies of every provider.

mod support;

use std::sync::Arc;

use harness_agent::agent_loop::Conversation;
use harness_agent::compaction::{ContextManager, ContextSettings};
use harness_agent::context::{LayerKind, ReasoningRetention, SessionPrefix};
use harness_providers::anthropic::{AnthropicConfig, AnthropicProvider};
use harness_providers::fake::{FakeBlock, FakeTurn};
use harness_providers::model::{ChatRequest, ContentBlock, Role};
use harness_providers::openai::OpenAiProvider;
use harness_providers::openrouter::{OpenRouterOptions, OpenRouterProvider};
use serde_json::{Value, json};
use support::*;
use tokio_util::sync::CancellationToken;

const MEMORY: &str = "# Memory\n- The project uses cargo nextest.\n\n## Project notes (demo)\n- [[projects/demo/build]] Build (2 memories)";
const SKILLS: &str = "Skills (read the file to use one):\n- release-notes: Draft release notes (skills/release-notes.md)";

/// Builds every provider's body for `request`, with `model` per provider.
fn bodies(request: &ChatRequest) -> Vec<(&'static str, Value, &'static str)> {
    let anthropic = AnthropicProvider::new("test-key", AnthropicConfig::new()).unwrap();
    let openai = OpenAiProvider::with_key("test-key", None).unwrap();
    let openrouter =
        OpenRouterProvider::with_key("test-key", None, OpenRouterOptions::default()).unwrap();
    let with_model = |m: &str| ChatRequest {
        model: m.into(),
        ..request.clone()
    };
    vec![
        (
            "anthropic",
            anthropic
                .build_body(&with_model("claude-sonnet-4-6"))
                .unwrap(),
            "messages",
        ),
        (
            "openai",
            openai.request_body(&with_model("gpt-5")).unwrap(),
            "input",
        ),
        (
            "openrouter",
            openrouter.request_body(&with_model("anthropic/claude-sonnet-4-6")),
            "messages",
        ),
    ]
}

fn strip_cache_control(v: &mut Value) {
    match v {
        Value::Object(map) => {
            map.remove("cache_control");
            map.values_mut().for_each(strip_cache_control);
        }
        Value::Array(items) => items.iter_mut().for_each(strip_cache_control),
        _ => {}
    }
}

/// `(everything but the history, the history without cache markers)`.
fn split(body: &Value, key: &str) -> (String, Vec<String>) {
    let mut rest = body.clone();
    let mut history = rest
        .as_object_mut()
        .unwrap()
        .remove(key)
        .unwrap_or(Value::Array(vec![]));
    strip_cache_control(&mut history);
    let items = history
        .as_array()
        .unwrap()
        .iter()
        .map(|m| serde_json::to_string(m).unwrap())
        .collect();
    (serde_json::to_string(&rest).unwrap(), items)
}

#[tokio::test]
async fn serialized_prefix_is_byte_identical_across_turns_for_each_provider() {
    let h = Harness::new(vec![
        turn(vec![tool("echo", json!({"text": "a"}))]),
        turn(vec![tool("echo", json!({"text": "b"}))]),
        turn(vec![tool(
            "write",
            json!({"path": "x.rs", "content": "fn x() {}"}),
        )]),
        FakeTurn::text("first task done"),
        turn(vec![tool("echo", json!({"text": "c"}))]),
        FakeTurn::text("second task done"),
    ])
    .await;
    let prefix = SessionPrefix::freeze("You are a careful coder.", Some(MEMORY), Some(SKILLS));
    assert_eq!(
        prefix.layers().iter().map(|l| l.kind).collect::<Vec<_>>(),
        [LayerKind::System, LayerKind::MemoryIndex, LayerKind::Skills]
    );
    let manager = Arc::new(ContextManager::new(ContextSettings::new(200_000)));
    let mut conv = Conversation::new(Some(prefix.text().to_owned()));
    conv.push_user("first task");
    let mut agent = h.agent(config()).with_hook(manager);
    agent.run(&mut conv, &CancellationToken::new()).await;
    conv.push_user("second task");
    agent.run(&mut conv, &CancellationToken::new()).await;

    let requests = h.provider.requests();
    assert_eq!(requests.len(), 6);
    for r in &requests {
        // System text = frozen prefix: system, memory index, skill listing.
        assert_eq!(r.system.as_deref(), Some(prefix.text()));
    }
    for provider in ["anthropic", "openai", "openrouter"] {
        let split_all: Vec<(String, Vec<String>)> = requests
            .iter()
            .map(|r| {
                let (_, body, key) = bodies(r).into_iter().find(|b| b.0 == provider).unwrap();
                split(&body, key)
            })
            .collect();
        let (static0, _) = &split_all[0];
        for (i, pair) in split_all.windows(2).enumerate() {
            let (s_prev, h_prev) = &pair[0];
            let (s_next, h_next) = &pair[1];
            assert_eq!(
                s_prev, static0,
                "{provider}: static part changed at turn {i}"
            );
            assert_eq!(
                s_next,
                static0,
                "{provider}: static part changed at turn {}",
                i + 1
            );
            assert!(
                h_next.len() >= h_prev.len() && h_next[..h_prev.len()] == h_prev[..],
                "{provider}: history prefix changed between turns {i} and {}",
                i + 1
            );
        }
    }
    // The memory index and skill listing come after the system prompt.
    let anthropic = &bodies(&requests[0])[0].1;
    let system = serde_json::to_string(&anthropic["system"]).unwrap();
    let (sys, mem, skills) = (
        system.find("careful coder").unwrap(),
        system.find("cargo nextest").unwrap(),
        system.find("release-notes").unwrap(),
    );
    assert!(sys < mem && mem < skills);
    // Breakpoints at the layer boundaries Anthropic supports: end of tools,
    // end of the frozen system text, then the history.
    let tools = anthropic["tools"].as_array().unwrap();
    assert!(tools.last().unwrap().get("cache_control").is_some());
    assert!(system.contains("cache_control"));
}

fn reasoning_turns(block: FakeBlock) -> Vec<FakeTurn> {
    vec![
        turn(vec![block.clone(), tool("echo", json!({"text": "one"}))]),
        turn(vec![block, tool("echo", json!({"text": "two"}))]),
        FakeTurn::text("done"),
    ]
}

async fn run_loop(turns: Vec<FakeTurn>, retention: ReasoningRetention) -> Vec<ChatRequest> {
    let h = Harness::new(turns).await;
    let mut settings = ContextSettings::new(200_000);
    settings.reasoning = retention;
    let mut agent = h
        .agent(config())
        .with_hook(Arc::new(ContextManager::new(settings)));
    let mut conv = Conversation::new(Some("sys".into()));
    conv.push_user("task");
    agent.run(&mut conv, &CancellationToken::new()).await;
    h.provider.requests()
}

const SIGNATURE: &str = "EqQBCkgIARABGAIiQL3k9vX2Yw8pQ1rT7uJ0mZ4hN6cB5aD8eF2gH3iJ4kL5mN6oP7qR8sT9uV0wX1yZ2aB3cD4eF5gH6iJ7kL8mN9oP0qRqDE0b3JyZWQtYnktaGFybmVzcw==";

#[tokio::test]
async fn anthropic_signed_thinking_is_resent_unchanged_across_the_tool_loop() {
    let block = FakeBlock::Reasoning {
        text: "I should echo first.".into(),
        signature: Some(SIGNATURE.into()),
    };
    let requests = run_loop(reasoning_turns(block), ReasoningRetention::KeepAll).await;
    let last = requests.last().unwrap();
    let expected = ContentBlock::Reasoning {
        text: "I should echo first.".into(),
        signature: Some(SIGNATURE.into()),
    };
    for i in [1, 3] {
        assert_eq!(last.messages[i].role, Role::Assistant);
        assert_eq!(last.messages[i].content[0], expected, "message {i}");
    }
    let body = &bodies(last)[0].1;
    for i in [1, 3] {
        let first = &body["messages"][i]["content"][0];
        assert_eq!(first["type"], "thinking");
        assert_eq!(first["signature"], SIGNATURE);
        assert_eq!(first["thinking"], "I should echo first.");
    }
}

#[tokio::test]
async fn openai_encrypted_reasoning_items_are_resent_verbatim() {
    let item = json!({"type": "reasoning", "id": "rs_1", "encrypted_content": "gAAAAABpZ2VuY3J5cHRlZA", "summary": []});
    let block = FakeBlock::Opaque {
        provider: "openai".into(),
        data: item.clone(),
    };
    let requests = run_loop(reasoning_turns(block), ReasoningRetention::KeepAll).await;
    let last = requests.last().unwrap();
    let body = &bodies(last)[1].1;
    let resent: Vec<&Value> = body["input"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|i| i["type"] == "reasoning")
        .collect();
    assert_eq!(resent.len(), 2, "{body}");
    assert_eq!(resent[0]["encrypted_content"], item["encrypted_content"]);
}

#[tokio::test]
async fn openrouter_reasoning_details_are_resent_on_assistant_messages() {
    let details =
        json!([{"type": "reasoning.encrypted", "data": "c2VjcmV0LXJlYXNvbmluZw", "index": 0}]);
    let block = FakeBlock::Opaque {
        provider: "openrouter".into(),
        data: json!({"reasoning_details": details}),
    };
    let requests = run_loop(reasoning_turns(block), ReasoningRetention::KeepAll).await;
    let body = &bodies(requests.last().unwrap())[2].1;
    let assistants: Vec<&Value> = body["messages"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|m| m["role"] == "assistant")
        .collect();
    assert_eq!(assistants.len(), 2);
    for a in assistants {
        assert_eq!(a["reasoning_details"], details, "{a}");
    }
}

#[tokio::test]
async fn clearing_reasoning_is_explicit_and_only_touches_finished_turns() {
    let block = FakeBlock::Reasoning {
        text: "think".into(),
        signature: Some(SIGNATURE.into()),
    };
    let mut turns = reasoning_turns(block);
    turns.push(FakeTurn::text("second answer"));
    for retention in [
        ReasoningRetention::KeepAll,
        ReasoningRetention::DropCompletedTurns,
    ] {
        let h = Harness::new(turns.clone()).await;
        let mut settings = ContextSettings::new(200_000);
        settings.reasoning = retention;
        let mut agent = h
            .agent(config())
            .with_hook(Arc::new(ContextManager::new(settings)));
        let mut conv = Conversation::new(None);
        conv.push_user("task");
        agent.run(&mut conv, &CancellationToken::new()).await;
        // Within the tool loop reasoning is always kept.
        let in_loop = h.provider.requests()[2].clone();
        assert!(matches!(
            in_loop.messages[1].content[0],
            ContentBlock::Reasoning { .. }
        ));
        assert!(matches!(
            in_loop.messages[3].content[0],
            ContentBlock::Reasoning { .. }
        ));
        conv.push_user("next");
        agent.run(&mut conv, &CancellationToken::new()).await;
        let next = h.provider.requests().last().unwrap().clone();
        let has_reasoning = next
            .messages
            .iter()
            .flat_map(|m| &m.content)
            .any(|b| matches!(b, ContentBlock::Reasoning { .. }));
        assert_eq!(
            has_reasoning,
            retention == ReasoningRetention::KeepAll,
            "{retention:?}"
        );
    }
}

#[tokio::test]
async fn a_layered_prefix_puts_cache_breakpoints_at_layer_boundaries() {
    let h = Harness::new(vec![
        turn(vec![tool("echo", json!({"text": "a"}))]),
        FakeTurn::text("done"),
        FakeTurn::text("again"),
    ])
    .await;
    let prefix = SessionPrefix::freeze("You are a careful coder.", Some(MEMORY), Some(SKILLS));
    let mut conv = Conversation::from_prefix(&prefix);
    conv.push_user("first task");
    let mut agent = h.agent(config());
    agent.run(&mut conv, &CancellationToken::new()).await;
    conv.push_user("second task");
    agent.run(&mut conv, &CancellationToken::new()).await;
    let requests = h.provider.requests();
    assert_eq!(requests.len(), 3);
    let mut first_system = None;
    for r in &requests {
        assert_eq!(r.system.as_deref(), Some(prefix.text()));
        assert_eq!(
            r.system_layers.len(),
            2,
            "memory index and skills start later"
        );
        let body = &bodies(r)[0].1;
        let system = body["system"].as_array().unwrap();
        // System prompt, memory index, skill listing: one block each, in
        // order, and together exactly the frozen text.
        assert_eq!(system.len(), 3, "{system:?}");
        let joined: String = system.iter().map(|b| b["text"].as_str().unwrap()).collect();
        assert_eq!(joined, prefix.text());
        assert!(
            system[0]["text"]
                .as_str()
                .unwrap()
                .contains("careful coder")
        );
        assert!(
            system[1]["text"]
                .as_str()
                .unwrap()
                .contains("cargo nextest")
        );
        assert!(
            system[2]["text"]
                .as_str()
                .unwrap()
                .contains("release-notes")
        );
        // Breakpoints: end of tools, end of the base prompt, end of the
        // prefix; the history keeps one.
        assert!(
            body["tools"]
                .as_array()
                .unwrap()
                .last()
                .unwrap()
                .get("cache_control")
                .is_some()
        );
        assert!(system[0].get("cache_control").is_some());
        assert!(system[2].get("cache_control").is_some());
        let serialized = serde_json::to_string(system).unwrap();
        match &first_system {
            None => first_system = Some(serialized),
            Some(f) => assert_eq!(&serialized, f, "the layered prefix is byte-identical"),
        }
    }
}
