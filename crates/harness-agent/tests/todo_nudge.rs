//! The todo nudge: agents that work without a task list get one short
//! reminder, recorded with the request that carries it.

use std::sync::Arc;

use async_trait::async_trait;
use harness_agent::agent_loop::{AgentLoop, Conversation, LoopConfig};
use harness_agent::dispatch::{AgentTool, AllowAll, Dispatcher, ToolSet};
use harness_agent::todo::{Actor, NUDGE_TEXT, TaskBoard, TodoNudge, TodoTool};
use harness_events::redact::Redactor;
use harness_events::schema::{EventKind, NodeId, Payload, RunId};
use harness_events::store::{RecorderOptions, RunRecorder, read_run};
use harness_providers::fake::{FakeProvider, FakeTurn};
use harness_providers::model::{ChatRequest, ContentBlock};
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
        json!({"type": "object"})
    }
    fn read_only(&self) -> bool {
        true
    }
    async fn call(&self, _ctx: &ToolContext, _args: Value) -> Result<ToolOutput, ToolError> {
        Ok(ToolOutput::Text("ok".into()))
    }
}

fn echo() -> FakeTurn {
    FakeTurn::tool_use("echo", json!({}))
}

struct Run {
    requests: Vec<ChatRequest>,
    logged: Vec<String>,
}

/// Runs `turns` with the nudge at `after`; `with_todo` gives the agent the
/// todo tool.
async fn run(turns: Vec<FakeTurn>, after: u32, with_todo: bool) -> Run {
    let dir = tempfile::tempdir().unwrap();
    let recorder = RunRecorder::create(
        dir.path(),
        RunId::new(),
        Arc::new(Redactor::new(Vec::<&str>::new())),
        RecorderOptions::default(),
    )
    .await
    .unwrap();
    let board = Arc::new(TaskBoard::new());
    let mut tools = ToolSet::new().with(Arc::new(Echo));
    if with_todo {
        tools = tools.with(Arc::new(TodoTool::new(
            board.clone(),
            Actor::new(NodeId::from("n"), "coder"),
        )));
    }
    let provider = FakeProvider::with_turns(turns);
    let mut agent = AgentLoop::new(
        Arc::new(provider.clone()),
        Dispatcher::new(tools, Arc::new(AllowAll)),
        recorder.clone(),
        LoopConfig::new("coder", "fake-model"),
    )
    .with_hook(Arc::new(TodoNudge::new(board, after)));
    let mut conv = Conversation::new(Some("system".into()));
    conv.push_user("index this codebase");
    agent.run(&mut conv, &CancellationToken::new()).await;
    recorder.finish().await.unwrap();
    let logged = read_run(recorder.dir())
        .unwrap()
        .events
        .into_iter()
        .filter_map(|e| match e.kind {
            EventKind::ModelCallStarted {
                request: Payload::Json(v),
                ..
            } => Some(v["messages"].to_string()),
            _ => None,
        })
        .collect();
    Run {
        requests: provider.requests(),
        logged,
    }
}

fn nudged(r: &ChatRequest) -> bool {
    r.messages.last().is_some_and(|m| {
        m.content
            .iter()
            .any(|b| matches!(b, ContentBlock::Text { text } if text == NUDGE_TEXT))
    })
}

fn pattern(r: &Run) -> Vec<bool> {
    r.requests.iter().map(nudged).collect()
}

#[tokio::test]
async fn nudges_after_n_calls_then_only_every_n_more() {
    let mut turns: Vec<FakeTurn> = (0..7).map(|_| echo()).collect();
    turns.push(FakeTurn::text("done"));
    let r = run(turns, 3, true).await;
    // Calls 4 and 7 (3 and 6 calls made before them) carry the reminder.
    assert_eq!(
        pattern(&r),
        [false, false, false, true, false, false, true, false]
    );
    // It is recorded with the request that carries it.
    assert!(
        r.logged[3].contains("Reminder from harness"),
        "{}",
        r.logged[3]
    );
    assert!(!r.logged[4].contains("Reminder from harness"));
}

#[tokio::test]
async fn earlier_messages_never_change_so_the_cache_prefix_holds() {
    let mut turns: Vec<FakeTurn> = (0..5).map(|_| echo()).collect();
    turns.push(FakeTurn::text("done"));
    let r = run(turns, 3, true).await;
    for pair in r.requests.windows(2) {
        let (a, b) = (&pair[0].messages, &pair[1].messages);
        assert_eq!(a[..], b[..a.len()], "a later request rewrote history");
    }
}

#[tokio::test]
async fn stops_once_a_task_exists() {
    let turns = vec![
        echo(),
        echo(),
        echo(),
        FakeTurn::tool_use("todo", json!({"action": "add", "title": "Map the crates"})),
        echo(),
        echo(),
        echo(),
        echo(),
        FakeTurn::text("done"),
    ];
    let r = run(turns, 3, true).await;
    assert_eq!(
        pattern(&r),
        [false, false, false, true, false, false, false, false, false]
    );
}

#[tokio::test]
async fn zero_disables_it_and_agents_without_todo_never_get_it() {
    let turns = || {
        let mut t: Vec<FakeTurn> = (0..5).map(|_| echo()).collect();
        t.push(FakeTurn::text("done"));
        t
    };
    assert!(pattern(&run(turns(), 0, true).await).iter().all(|n| !n));
    assert!(pattern(&run(turns(), 3, false).await).iter().all(|n| !n));
}

#[tokio::test]
async fn short_turns_and_text_replies_are_left_alone() {
    // Fewer calls than the threshold: no reminder.
    let r = run(vec![echo(), FakeTurn::text("done")], 3, true).await;
    assert!(pattern(&r).iter().all(|n| !n));
}
