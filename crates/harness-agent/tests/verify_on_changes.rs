//! `verify_when = on_changes` follows what a change-tracking tool reports:
//! a bash run that only wrote generated artifacts is no change to verify.

mod support;

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use harness_agent::agent_loop::{
    AgentLoop, Conversation, Verification, Verifier, VerifyResult, VerifyWhen,
};
use harness_agent::dispatch::{AgentTool, AllowAll, Dispatcher, ToolSet};
use harness_providers::fake::FakeTurn;
use harness_tools::tool::{ToolContext, ToolError, ToolEvent, ToolOutput};
use serde_json::{Value, json};
use support::*;
use tokio_util::sync::CancellationToken;

/// A mutating `bash` stand-in that reports the changes its arguments name,
/// as the ledger's bash wrapper does.
struct TrackedBash;

#[async_trait]
impl AgentTool for TrackedBash {
    fn name(&self) -> &str {
        "bash"
    }
    fn description(&self) -> &str {
        "run a command"
    }
    fn schema(&self) -> Value {
        json!({"type": "object", "properties": {"files": {"type": "integer"}, "generated": {"type": "integer"}}})
    }
    fn read_only(&self) -> bool {
        false
    }
    async fn call(&self, ctx: &ToolContext, args: Value) -> Result<ToolOutput, ToolError> {
        ctx.emit(ToolEvent::WorktreeChanges {
            files: args["files"].as_u64().unwrap_or(0) as usize,
            generated: args["generated"].as_u64().unwrap_or(0) as usize,
        });
        Ok(ToolOutput::Text("ok".into()))
    }
}

#[derive(Default)]
struct Counting(Mutex<u32>);

#[async_trait]
impl Verifier for Counting {
    fn command(&self) -> String {
        "./run_tests.sh".into()
    }
    async fn verify(&self, _cancel: &CancellationToken) -> VerifyResult {
        *self.0.lock().unwrap() += 1;
        VerifyResult {
            passed: true,
            exit_code: Some(0),
            output: "OK".into(),
        }
    }
}

async fn run_with(calls: Vec<Value>) -> (Verification, u32) {
    let mut turns: Vec<FakeTurn> = calls
        .into_iter()
        .map(|args| turn(vec![tool("bash", args)]))
        .collect();
    turns.push(FakeTurn::text("All tests pass."));
    let h = Harness::new(turns).await;
    let mut cfg = config();
    cfg.verify_when = VerifyWhen::OnChanges;
    let verifier = Arc::new(Counting::default());
    let tools = ToolSet::new()
        .with(Arc::new(TrackedBash) as Arc<dyn AgentTool>)
        .with(h.write.clone() as Arc<dyn AgentTool>);
    let mut agent = AgentLoop::new(
        Arc::new(h.provider.clone()),
        Dispatcher::new(tools, Arc::new(AllowAll)),
        h.recorder.clone(),
        cfg,
    )
    .with_verifier(verifier.clone());
    let mut conv = Conversation::new(None);
    conv.push_user("run the tests");
    let out = agent.run(&mut conv, &CancellationToken::new()).await;
    let runs = *verifier.0.lock().unwrap();
    (out.verification, runs)
}

#[tokio::test]
async fn a_bash_run_that_only_writes_generated_artifacts_is_not_verified() {
    let (verification, runs) = run_with(vec![json!({"files": 0, "generated": 4})]).await;
    assert_eq!(runs, 0, "on_changes must not run the verifier");
    assert_eq!(verification, Verification::Unverified);
}

#[tokio::test]
async fn a_bash_run_that_changes_a_source_file_is_verified() {
    let (verification, runs) = run_with(vec![
        json!({"files": 0, "generated": 4}),
        json!({"files": 1, "generated": 1}),
    ])
    .await;
    assert_eq!(runs, 1);
    assert_eq!(verification, Verification::Verified);
}
