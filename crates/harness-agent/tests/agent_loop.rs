//! Agent loop integration tests against the fake provider and a real run
//! store in a temp directory.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use harness_agent::agent_loop::{
    AgentLoop, Conversation, LoopConfig, LoopLimits, StopReason, Verifier, VerifyResult,
};
use harness_agent::dispatch::{AgentTool, AllowAll, Dispatcher, ToolSet};
use harness_events::redact::{MASK, Redactor};
use harness_events::schema::{EventKind, NodeStatus, RunEvent, RunId};
use harness_events::store::{RecorderOptions, RunRecorder, read_run};
use harness_providers::fake::{FakeBlock, FakeError, FakeErrorKind, FakeProvider, FakeTurn};
use harness_providers::model::{ContentBlock, Role, ToolResultPart, Usage};
use harness_providers::openai::notice::Notice;
use harness_tools::tool::{ToolContext, ToolError, ToolOutput};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

struct Echo;

#[async_trait]
impl AgentTool for Echo {
    fn name(&self) -> &str {
        "echo"
    }
    fn description(&self) -> &str {
        "echo text"
    }
    fn schema(&self) -> Value {
        json!({"type": "object", "properties": {"text": {"type": "string"}}, "required": ["text"], "additionalProperties": false})
    }
    fn read_only(&self) -> bool {
        true
    }
    async fn call(&self, _ctx: &ToolContext, args: Value) -> Result<ToolOutput, ToolError> {
        Ok(ToolOutput::Text(
            args["text"].as_str().unwrap_or_default().to_owned(),
        ))
    }
}

struct Fail;

#[async_trait]
impl AgentTool for Fail {
    fn name(&self) -> &str {
        "fail"
    }
    fn description(&self) -> &str {
        "always fails"
    }
    fn schema(&self) -> Value {
        json!({"type": "object"})
    }
    fn read_only(&self) -> bool {
        false
    }
    async fn call(&self, _ctx: &ToolContext, _args: Value) -> Result<ToolOutput, ToolError> {
        Err(ToolError::invalid_args("it broke. Try something else."))
    }
}

struct Slow;

#[async_trait]
impl AgentTool for Slow {
    fn name(&self) -> &str {
        "slow"
    }
    fn description(&self) -> &str {
        "sleeps"
    }
    fn schema(&self) -> Value {
        json!({"type": "object"})
    }
    fn read_only(&self) -> bool {
        false
    }
    async fn call(&self, _ctx: &ToolContext, _args: Value) -> Result<ToolOutput, ToolError> {
        tokio::time::sleep(Duration::from_secs(60)).await;
        Ok(ToolOutput::Text("done".into()))
    }
}

struct Harness {
    _dir: tempfile::TempDir,
    recorder: RunRecorder,
    provider: FakeProvider,
}

impl Harness {
    async fn new(turns: Vec<FakeTurn>) -> Self {
        Self::with_secrets(turns, Vec::new()).await
    }

    async fn with_secrets(turns: Vec<FakeTurn>, secrets: Vec<&str>) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let recorder = RunRecorder::create(
            dir.path(),
            RunId::new(),
            Arc::new(Redactor::new(secrets)),
            RecorderOptions::default(),
        )
        .await
        .unwrap();
        Self {
            _dir: dir,
            recorder,
            provider: FakeProvider::with_turns(turns),
        }
    }

    fn agent(&self, config: LoopConfig) -> AgentLoop {
        let tools = ToolSet::new()
            .with(Arc::new(Echo))
            .with(Arc::new(Fail))
            .with(Arc::new(Slow));
        AgentLoop::new(
            Arc::new(self.provider.clone()),
            Dispatcher::new(tools, Arc::new(AllowAll)),
            self.recorder.clone(),
            config,
        )
    }

    async fn events(&self) -> Vec<RunEvent> {
        self.recorder.finish().await.unwrap();
        read_run(self.recorder.dir()).unwrap().events
    }
}

fn config() -> LoopConfig {
    LoopConfig::new("coder", "fake-model")
}

fn types(events: &[RunEvent]) -> Vec<&'static str> {
    events.iter().map(|e| e.kind.type_name()).collect()
}

fn finished(events: &[RunEvent]) -> (NodeStatus, Option<String>) {
    events
        .iter()
        .find_map(|e| match &e.kind {
            EventKind::NodeFinished { status, error } => Some((*status, error.clone())),
            _ => None,
        })
        .expect("node_finished")
}

fn tool(name: &str, input: Value) -> FakeBlock {
    FakeBlock::ToolUse {
        id: None,
        name: name.into(),
        input,
    }
}

fn turn(blocks: Vec<FakeBlock>) -> FakeTurn {
    FakeTurn {
        content: blocks,
        ..FakeTurn::default()
    }
}

#[tokio::test]
async fn plain_text_turn_ends_with_end_turn_and_records_every_transition() {
    let h = Harness::new(vec![FakeTurn {
        chunk_size: 3,
        ..FakeTurn::text("Hello there")
    }])
    .await;
    let mut conv = Conversation::new(Some("system".into()));
    conv.push_user("hi");
    let out = h
        .agent(config())
        .run(&mut conv, &CancellationToken::new())
        .await;
    assert_eq!(out.stop, StopReason::EndTurn);
    assert_eq!(out.final_text, "Hello there");
    assert_eq!(out.turns, 1);
    let events = h.events().await;
    let t = types(&events);
    assert_eq!(t.first(), Some(&"node_started"));
    // The user's prompt is recorded on the node before the first request.
    assert_eq!(t[1], "user_message");
    assert!(
        matches!(&events[1].kind, EventKind::UserMessage { text } if text == "hi"),
        "{:?}",
        events[1].kind
    );
    assert_eq!(t[2], "model_call_started");
    assert!(t.contains(&"delta"));
    assert_eq!(&t[t.len() - 2..], ["model_call_finished", "node_finished"]);
    assert_eq!(finished(&events), (NodeStatus::Succeeded, None));
    let deltas: String = events
        .iter()
        .filter_map(|e| match &e.kind {
            EventKind::Delta { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(deltas, "Hello there");
}

#[tokio::test]
async fn tool_calls_are_dispatched_and_results_submitted_until_end_turn() {
    let h = Harness::new(vec![
        turn(vec![
            FakeBlock::Text("Let me echo.".into()),
            tool("echo", json!({"text": "one"})),
            tool("echo", json!({"text": "two"})),
        ]),
        FakeTurn::text("All done."),
    ])
    .await;
    let mut conv = Conversation::new(None);
    conv.push_user("go");
    let out = h
        .agent(config())
        .run(&mut conv, &CancellationToken::new())
        .await;
    assert_eq!(out.stop, StopReason::EndTurn);
    assert_eq!(out.turns, 2);
    let requests = h.provider.requests();
    assert_eq!(requests.len(), 2);
    let results = &requests[1].messages[2];
    assert_eq!(results.role, Role::User);
    let texts: Vec<String> = results
        .content
        .iter()
        .map(|b| match b {
            ContentBlock::ToolResult {
                content, is_error, ..
            } => {
                assert!(!is_error);
                match &content[0] {
                    ToolResultPart::Text { text } => text.clone(),
                    ToolResultPart::Image { .. } => unreachable!(),
                }
            }
            _ => panic!("expected tool results"),
        })
        .collect();
    assert_eq!(texts, ["one", "two"]);
    assert_eq!(requests[1].tools.len(), 3);
    let events = h.events().await;
    let t = types(&events);
    assert_eq!(t.iter().filter(|t| **t == "tool_call").count(), 2);
    assert_eq!(t.iter().filter(|t| **t == "tool_result").count(), 2);
    assert_eq!(t.iter().filter(|t| **t == "model_call_started").count(), 2);
    // The history is what was sent: user, assistant, tool results, assistant.
    assert_eq!(conv.messages.len(), 4);
}

#[tokio::test]
async fn max_turns_stops_with_its_own_status() {
    let h = Harness::new(
        (0..5)
            .map(|_| turn(vec![tool("echo", json!({"text": "x"}))]))
            .collect(),
    )
    .await;
    let mut cfg = config();
    cfg.limits = LoopLimits {
        max_turns: Some(2),
        ..LoopLimits::default()
    };
    let mut conv = Conversation::new(None);
    conv.push_user("loop forever");
    let out = h.agent(cfg).run(&mut conv, &CancellationToken::new()).await;
    assert_eq!(out.stop, StopReason::MaxTurns { limit: 2 });
    assert_eq!(h.provider.requests().len(), 2);
    let (status, error) = finished(&h.events().await);
    assert_eq!(status, NodeStatus::Failed);
    let error = error.unwrap();
    assert!(error.starts_with("max_turns: "), "{error}");
    assert_eq!(
        StopReason::from_node_finished(status, Some(&error)),
        StopReason::MaxTurns { limit: 2 }
    );
}

fn usage(input: u64, output: u64) -> Usage {
    Usage {
        input_tokens: input,
        output_tokens: output,
        ..Usage::default()
    }
}

#[tokio::test]
async fn token_budget_exhaustion_stops_without_running_more_tools() {
    let mut t = turn(vec![tool("echo", json!({"text": "x"}))]);
    t.usage = usage(900, 200);
    let h = Harness::new(vec![t.clone(), t.clone(), t]).await;
    let mut cfg = config();
    cfg.limits.max_tokens = Some(2000);
    let mut conv = Conversation::new(None);
    conv.push_user("go");
    let out = h.agent(cfg).run(&mut conv, &CancellationToken::new()).await;
    assert!(
        matches!(&out.stop, StopReason::Budget { reason } if reason.contains("token budget")),
        "{:?}",
        out.stop
    );
    assert_eq!(out.turns, 2);
    // The second call's tool was not run but still has a result.
    let last = conv.messages.last().unwrap();
    assert!(matches!(
        &last.content[0],
        ContentBlock::ToolResult { is_error: true, .. }
    ));
    let (status, error) = finished(&h.events().await);
    assert_eq!(status, NodeStatus::Failed);
    assert!(error.unwrap().starts_with("budget: "));
}

#[tokio::test]
async fn cost_budget_uses_cost_fn_and_reported_cost_wins() {
    // Call 1 reports its own cost (OpenRouter notice); call 2 is priced.
    let notice = Notice::Cost {
        provider: "openrouter".into(),
        usd: 0.5,
    };
    let ContentBlock::Opaque { provider, data } = notice.to_block() else {
        unreachable!()
    };
    let mut t1 = turn(vec![
        tool("echo", json!({"text": "x"})),
        FakeBlock::Opaque { provider, data },
    ]);
    t1.usage = usage(1_000_000, 0);
    let mut t2 = turn(vec![tool("echo", json!({"text": "y"}))]);
    t2.usage = usage(1_000_000, 0);
    let h = Harness::new(vec![t1, t2, FakeTurn::text("never")]).await;
    let mut cfg = config();
    cfg.limits.max_usd = Some(1.0);
    let mut conv = Conversation::new(None);
    conv.push_user("go");
    let out = h
        .agent(cfg)
        .with_cost(Arc::new(|u: &Usage| {
            Some(u.input_tokens as f64 / 1_000_000.0 * 3.0)
        }))
        .run(&mut conv, &CancellationToken::new())
        .await;
    assert!(
        matches!(out.stop, StopReason::Budget { .. }),
        "{:?}",
        out.stop
    );
    assert!((out.cost_usd - 3.5).abs() < 1e-9, "{}", out.cost_usd);
    let costs: Vec<f64> = h
        .events()
        .await
        .iter()
        .filter_map(|e| match e.kind {
            EventKind::Cost { usd } => Some(usd),
            _ => None,
        })
        .collect();
    assert_eq!(costs, [0.5, 3.0]);
}

#[tokio::test]
async fn cancel_during_streaming_stops_the_model_call() {
    let h = Harness::new(vec![FakeTurn {
        delay_ms: 30_000,
        ..FakeTurn::text("too late")
    }])
    .await;
    let cancel = CancellationToken::new();
    let c = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(50)).await;
        c.cancel();
    });
    let mut conv = Conversation::new(None);
    conv.push_user("hi");
    let mut agent = h.agent(config());
    let out = tokio::time::timeout(Duration::from_secs(5), agent.run(&mut conv, &cancel))
        .await
        .expect("cancel did not reach the model call");
    assert_eq!(out.stop, StopReason::Cancelled);
    let events = h.events().await;
    assert_eq!(finished(&events).0, NodeStatus::Cancelled);
    assert!(events.iter().any(|e| matches!(&e.kind,
        EventKind::ModelCallFinished { stop_reason: Some(r), .. } if r == "cancelled")));
}

#[tokio::test]
async fn cancel_during_tool_stops_the_tool_and_keeps_history_valid() {
    let h = Harness::new(vec![
        turn(vec![tool("slow", json!({}))]),
        FakeTurn::text("unused"),
    ])
    .await;
    let cancel = CancellationToken::new();
    let c = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(100)).await;
        c.cancel();
    });
    let mut conv = Conversation::new(None);
    conv.push_user("hi");
    let mut agent = h.agent(config());
    let out = tokio::time::timeout(Duration::from_secs(5), agent.run(&mut conv, &cancel))
        .await
        .expect("cancel did not reach the tool");
    assert_eq!(out.stop, StopReason::Cancelled);
    assert_eq!(h.provider.requests().len(), 1);
    let last = conv.messages.last().unwrap();
    assert!(matches!(
        &last.content[0],
        ContentBlock::ToolResult { is_error: true, .. }
    ));
    let events = h.events().await;
    assert!(
        events
            .iter()
            .any(|e| matches!(&e.kind, EventKind::ToolResult { ok: false, .. }))
    );
    assert_eq!(finished(&events).0, NodeStatus::Cancelled);
}

#[tokio::test]
async fn provider_errors_stop_with_error_status() {
    let h = Harness::new(vec![FakeTurn::error(FakeError::new(
        FakeErrorKind::Auth,
        "bad key",
    ))])
    .await;
    let mut conv = Conversation::new(None);
    conv.push_user("hi");
    let out = h
        .agent(config())
        .run(&mut conv, &CancellationToken::new())
        .await;
    assert!(
        matches!(&out.stop, StopReason::Error { message, .. } if message.contains("bad key")),
        "{:?}",
        out.stop
    );
    let events = h.events().await;
    let (status, error) = finished(&events);
    assert_eq!(status, NodeStatus::Failed);
    assert!(error.unwrap().starts_with("error: "));
    assert!(events.iter().any(|e| matches!(
        e.kind,
        EventKind::Error {
            recoverable: false,
            ..
        }
    )));
}

#[tokio::test]
async fn usage_limit_stops_with_the_manage_usage_action() {
    // TASK-133 AC6: the provider's primary action survives the agent loop,
    // both in the stop reason and in the recorded `error` event.
    let h = Harness::new(vec![FakeTurn::error(FakeError::new(
        FakeErrorKind::UsageLimit,
        "You have reached your plan's usage limit.",
    ))])
    .await;
    let mut conv = Conversation::new(None);
    conv.push_user("hi");
    let out = h
        .agent(config())
        .run(&mut conv, &CancellationToken::new())
        .await;
    let want = harness_events::schema::ErrorAction {
        label: "Manage usage".into(),
        url: harness_providers::chatgpt::MANAGE_USAGE_URL.into(),
    };
    match &out.stop {
        StopReason::Error { message, action } => {
            assert!(message.contains("usage limit"), "{message}");
            assert_eq!(action.as_ref(), Some(&want));
        }
        other => panic!("{other:?}"),
    }
    let events = h.events().await;
    assert!(
        events.iter().any(|e| matches!(
            &e.kind,
            EventKind::Error { recoverable: false, action: Some(a), .. } if *a == want
        )),
        "{events:?}"
    );
    // The usage limit is not retried and there is no fallback call.
    let calls = events
        .iter()
        .filter(|e| matches!(e.kind, EventKind::ModelCallStarted { .. }))
        .count();
    assert_eq!(calls, 1);
}

#[tokio::test]
async fn tool_output_is_redacted_before_it_reaches_the_model() {
    let secret = "hunter2-super-secret-value";
    let h = Harness::with_secrets(
        vec![
            turn(vec![tool(
                "echo",
                json!({"text": format!("the key is {secret}")}),
            )]),
            FakeTurn::text("ok"),
        ],
        vec![secret],
    )
    .await;
    let mut conv = Conversation::new(None);
    conv.push_user("show me");
    h.agent(config())
        .run(&mut conv, &CancellationToken::new())
        .await;
    let sent = serde_json::to_string(&h.provider.requests()[1].messages).unwrap();
    assert!(!sent.contains(secret), "secret reached the model: {sent}");
    assert!(sent.contains(MASK));
    let log = serde_json::to_string(&h.events().await).unwrap();
    assert!(!log.contains(secret));
}

#[tokio::test]
async fn repeated_identical_failures_trigger_loop_detection_and_a_nudge() {
    let failing = || turn(vec![tool("fail", json!({"same": true}))]);
    let h = Harness::new(vec![
        failing(),
        failing(),
        failing(),
        FakeTurn::text("giving up"),
    ])
    .await;
    let mut conv = Conversation::new(None);
    conv.push_user("try");
    let out = h
        .agent(config())
        .run(&mut conv, &CancellationToken::new())
        .await;
    assert_eq!(out.stop, StopReason::EndTurn);
    let last_results = serde_json::to_string(&h.provider.requests()[3].messages[6]).unwrap();
    assert!(last_results.contains("Loop detected"), "{last_results}");
    let second = serde_json::to_string(&h.provider.requests()[2].messages[4]).unwrap();
    assert!(!second.contains("Loop detected"));
    let events = h.events().await;
    assert!(events.iter().any(|e| matches!(&e.kind,
        EventKind::Error { message, recoverable: true, .. } if message.starts_with("loop detected"))));
}

#[tokio::test]
async fn malformed_and_unknown_calls_get_actionable_errors_and_the_loop_continues() {
    let h = Harness::new(vec![
        turn(vec![
            tool("echo", json!("{\"text\": \"fixed\",}")),
            tool("ecoh", json!({"text": "x"})),
            tool("echo", json!({"txt": "x"})),
        ]),
        FakeTurn::text("ok"),
    ])
    .await;
    let mut conv = Conversation::new(None);
    conv.push_user("go");
    let out = h
        .agent(config())
        .run(&mut conv, &CancellationToken::new())
        .await;
    assert_eq!(out.stop, StopReason::EndTurn);
    let req = &h.provider.requests()[1];
    // The repaired arguments replaced the raw string in the history.
    assert_eq!(
        req.messages[1].tool_uses().next().unwrap().2,
        &json!({"text": "fixed"})
    );
    let results = serde_json::to_string(&req.messages[2]).unwrap();
    assert!(results.contains("\"fixed\""));
    assert!(results.contains("Did you mean `echo`?"), "{results}");
    assert!(results.contains("did you mean `text`?"), "{results}");
    let events = h.events().await;
    assert!(events.iter().any(|e| matches!(&e.kind,
        EventKind::Error { message, .. } if message.contains("removed trailing comma"))));
}

#[tokio::test]
async fn pause_continues_and_context_limit_is_an_error() {
    let h = Harness::new(vec![
        FakeTurn {
            stop: Some(harness_providers::model::StopKind::Pause),
            ..FakeTurn::text("part one")
        },
        FakeTurn {
            stop: Some(harness_providers::model::StopKind::ContextLimit),
            ..FakeTurn::default()
        },
    ])
    .await;
    let mut conv = Conversation::new(None);
    conv.push_user("hi");
    let out = h
        .agent(config())
        .run(&mut conv, &CancellationToken::new())
        .await;
    assert_eq!(out.turns, 2);
    assert!(
        matches!(&out.stop, StopReason::Error { message, .. } if message.contains("context window"))
    );
}

struct FixedVerifier(bool);

#[async_trait]
impl Verifier for FixedVerifier {
    fn command(&self) -> String {
        "cargo test".into()
    }
    async fn verify(&self, _cancel: &CancellationToken) -> VerifyResult {
        VerifyResult {
            passed: self.0,
            exit_code: Some(if self.0 { 0 } else { 101 }),
            output: "test output".into(),
        }
    }
}

#[tokio::test]
async fn verifier_runs_after_end_turn_and_is_recorded() {
    let h = Harness::new(vec![FakeTurn::text("done")]).await;
    let mut conv = Conversation::new(None);
    conv.push_user("fix it");
    let out = h
        .agent(config())
        .with_verifier(Arc::new(FixedVerifier(false)))
        .run(&mut conv, &CancellationToken::new())
        .await;
    assert_eq!(out.stop, StopReason::EndTurn);
    assert_eq!(out.verified, Some(false));
    let events = h.events().await;
    assert!(events.iter().any(|e| matches!(
        &e.kind,
        EventKind::VerifyFinished {
            passed: false,
            exit_code: Some(101),
            ..
        }
    )));
}

#[tokio::test]
async fn second_run_on_the_same_conversation_logs_only_new_messages() {
    let h = Harness::new(vec![FakeTurn::text("first"), FakeTurn::text("second")]).await;
    let mut conv = Conversation::new(Some("sys".into()));
    conv.push_user("one");
    let mut agent = h.agent(config());
    agent.run(&mut conv, &CancellationToken::new()).await;
    conv.push_user("two");
    agent.run(&mut conv, &CancellationToken::new()).await;
    let requests: Vec<Value> = h
        .events()
        .await
        .into_iter()
        .filter_map(|e| match e.kind {
            EventKind::ModelCallStarted {
                request: harness_events::schema::Payload::Json(v),
                ..
            } => Some(v),
            _ => None,
        })
        .collect();
    assert_eq!(requests[0]["messages_from"], 0);
    assert_eq!(requests[0]["system"], "sys");
    assert_eq!(requests[1]["messages_from"], 2);
    assert_eq!(requests[1]["messages"].as_array().unwrap().len(), 1);
    assert!(
        requests[1].get("system").is_none(),
        "unchanged system prompt is not repeated"
    );
}
