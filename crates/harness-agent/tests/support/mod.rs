//! Shared helpers for the context, cost and done-gating integration tests.
#![allow(dead_code)]

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use harness_agent::agent_loop::{AgentLoop, LoopConfig};
use harness_agent::dispatch::{AgentTool, AllowAll, Dispatcher, ToolSet};
use harness_events::redact::Redactor;
use harness_events::schema::{RunEvent, RunId};
use harness_events::store::{RecorderOptions, RunRecorder, read_run};
use harness_providers::fake::{FakeBlock, FakeProvider, FakeTurn};
use harness_providers::model::Usage;
use harness_tools::tool::{ToolContext, ToolError, ToolOutput};
use serde_json::{Value, json};

/// Echoes `text`, repeated `times` times (to make large results).
pub struct Echo;

#[async_trait]
impl AgentTool for Echo {
    fn name(&self) -> &str {
        "echo"
    }
    fn description(&self) -> &str {
        "echo text"
    }
    fn schema(&self) -> Value {
        json!({"type": "object", "properties": {"text": {"type": "string"}, "times": {"type": "integer"}}, "required": ["text"]})
    }
    fn read_only(&self) -> bool {
        true
    }
    async fn call(&self, _ctx: &ToolContext, args: Value) -> Result<ToolOutput, ToolError> {
        let text = args["text"].as_str().unwrap_or_default();
        let times = args["times"].as_u64().unwrap_or(1) as usize;
        Ok(ToolOutput::Text(text.repeat(times)))
    }
}

/// Records written paths; a mutating tool.
#[derive(Default)]
pub struct Write {
    pub written: Mutex<Vec<String>>,
}

#[async_trait]
impl AgentTool for Write {
    fn name(&self) -> &str {
        "write"
    }
    fn description(&self) -> &str {
        "write a file"
    }
    fn schema(&self) -> Value {
        json!({"type": "object", "properties": {"path": {"type": "string"}, "content": {"type": "string"}}, "required": ["path"]})
    }
    fn read_only(&self) -> bool {
        false
    }
    async fn call(&self, _ctx: &ToolContext, args: Value) -> Result<ToolOutput, ToolError> {
        let path = args["path"].as_str().unwrap_or_default().to_owned();
        self.written.lock().unwrap().push(path.clone());
        Ok(ToolOutput::Text(format!("wrote {path}")))
    }
}

pub struct Harness {
    pub dir: tempfile::TempDir,
    pub recorder: RunRecorder,
    pub provider: FakeProvider,
    pub write: Arc<Write>,
}

impl Harness {
    pub async fn new(turns: Vec<FakeTurn>) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let recorder = RunRecorder::create(
            dir.path(),
            RunId::new(),
            Arc::new(Redactor::default()),
            RecorderOptions::default(),
        )
        .await
        .unwrap();
        Self {
            dir,
            recorder,
            provider: FakeProvider::with_turns(turns),
            write: Arc::new(Write::default()),
        }
    }

    pub fn tools(&self) -> ToolSet {
        ToolSet::new()
            .with(Arc::new(Echo))
            .with(self.write.clone() as Arc<dyn AgentTool>)
    }

    pub fn agent(&self, config: LoopConfig) -> AgentLoop {
        AgentLoop::new(
            Arc::new(self.provider.clone()),
            Dispatcher::new(self.tools(), Arc::new(AllowAll)),
            self.recorder.clone(),
            config,
        )
    }

    pub async fn events(&self) -> Vec<RunEvent> {
        self.recorder.finish().await.unwrap();
        read_run(self.recorder.dir()).unwrap().events
    }
}

pub fn tool(name: &str, input: Value) -> FakeBlock {
    FakeBlock::ToolUse {
        id: None,
        name: name.into(),
        input,
    }
}

pub fn turn(blocks: Vec<FakeBlock>) -> FakeTurn {
    FakeTurn {
        content: blocks,
        ..FakeTurn::default()
    }
}

pub fn with_usage(mut t: FakeTurn, input: u64, cached: u64, output: u64) -> FakeTurn {
    t.usage = Usage {
        input_tokens: input,
        cache_read_tokens: cached,
        output_tokens: output,
        ..Usage::default()
    };
    t
}

pub fn config() -> LoopConfig {
    LoopConfig::new("coder", "fake-model")
}
