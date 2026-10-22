//! The request inspector rebuilds exactly what the provider received, for
//! every call of a real agent loop (tool schemas included).

use std::sync::Arc;

use async_trait::async_trait;
use harness_agent::agent_loop::{AgentLoop, Conversation, LoopConfig};
use harness_agent::dispatch::{AgentTool, AllowAll, Dispatcher, ToolSet};
use harness_events::redact::Redactor;
use harness_events::schema::{EventKind, RunId};
use harness_events::store::{RecorderOptions, RunRecorder, read_run};
use harness_providers::fake::{FakeBlock, FakeProvider, FakeTurn};
use harness_providers::provider::Provider;
use harness_tools::tool::{ToolContext, ToolError, ToolOutput};
use harness_tui::inspector::Inspection;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

struct Echo;

#[async_trait]
impl AgentTool for Echo {
    fn name(&self) -> &str {
        "echo"
    }
    fn description(&self) -> &str {
        "Echoes its text back."
    }
    fn schema(&self) -> Value {
        json!({"type": "object", "properties": {"text": {"type": "string"}}, "required": ["text"]})
    }
    fn read_only(&self) -> bool {
        true
    }
    async fn call(&self, _ctx: &ToolContext, args: Value) -> Result<ToolOutput, ToolError> {
        Ok(ToolOutput::Text(
            args["text"].as_str().unwrap_or("").to_owned(),
        ))
    }
}

#[tokio::test]
async fn every_call_is_rebuilt_exactly() {
    let data = tempfile::tempdir().unwrap();
    let recorder = RunRecorder::create(
        data.path(),
        RunId::new(),
        Arc::new(Redactor::default()),
        RecorderOptions::default(),
    )
    .await
    .unwrap();
    let fake = FakeProvider::with_turns([
        FakeTurn {
            content: vec![FakeBlock::ToolUse {
                id: None,
                name: "echo".into(),
                input: json!({"text": "one"}),
            }],
            ..FakeTurn::default()
        },
        FakeTurn {
            content: vec![FakeBlock::ToolUse {
                id: None,
                name: "echo".into(),
                input: json!({"text": "two"}),
            }],
            ..FakeTurn::default()
        },
        FakeTurn::text("done"),
    ]);
    let tools = ToolSet::new().with(Arc::new(Echo));
    let mut agent = AgentLoop::new(
        Arc::new(fake.clone()),
        Dispatcher::new(tools, Arc::new(AllowAll)),
        recorder.clone(),
        LoopConfig::new("agent", "fake-model"),
    );
    let mut conv = Conversation::new(Some("You are terse.".into()));
    conv.push_user("echo twice");
    agent.run(&mut conv, &CancellationToken::new()).await;
    drop(agent);
    recorder.finish().await.unwrap();

    let events = read_run(recorder.dir()).unwrap().events;
    let calls: Vec<String> = events
        .iter()
        .filter_map(|e| match &e.kind {
            EventKind::ModelCallStarted { call_id, .. } => Some(call_id.clone()),
            _ => None,
        })
        .collect();
    let sent = fake.requests();
    assert_eq!(calls.len(), 3);
    assert_eq!(sent.len(), 3);
    for (call, sent) in calls.iter().zip(&sent) {
        let i = Inspection::from_events(&events, call, Some(recorder.blobs())).unwrap();
        let rebuilt = i.chat.clone().expect("rebuilt request");
        assert_eq!(&rebuilt, sent, "call {call}");
        assert_eq!(rebuilt.tools[0].input_schema["required"], json!(["text"]));
        // The live provider renders its own body from it.
        let mut i = i;
        i.set_provider_body(fake.request_body(&rebuilt));
        assert_eq!(
            i.provider_body.as_ref().unwrap()["messages"],
            serde_json::to_value(&sent.messages).unwrap()
        );
    }
}
