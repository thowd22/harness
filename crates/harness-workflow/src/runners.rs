//! The standard [`StepRunner`]s.
//!
//! | Step | Runner | Success |
//! |---|---|---|
//! | agent | [`AgentStepRunner`] (a child agent via [`SubagentRuntime`]) | the agent ends its turn and returns the declared outputs |
//! | shell | [`ShellStepRunner`] (`sh -c` through an [`Exec`]) | exit code 0 |
//! | verify | [`VerifyStepRunner`] ([`harness_hygiene::verify::VerifyRunner`]) | passes (or fails with `expect: fail`) |
//! | bugfix-proof | [`ProofStepRunner`] ([`harness_hygiene::proof::BugfixProof`]) | the fix is proven |
//!
//! Outputs: agent steps return their declared `outputs` and `artifacts`
//! as a JSON object at the end of their reply (validated, with one
//! correction turn), plus `summary`. Shell steps return `stdout` and
//! `exit_code`, and set declared outputs with `name=value` lines on stdout.
//! Verify steps return `passed`, `exit_code` and `output`. Proofs return
//! `proven` and `phases`.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;
use harness_agent::agent_loop::{LoopLimits, StopReason};
use harness_agent::subagents::{ChildHost, ChildRequest, SubagentRuntime};
use harness_events::schema::{EventKind, NodeId, NodeKind, Payload};
use harness_events::store::{EventDraft, RunRecorder};
use harness_hygiene::checkpoints::{CheckpointKind, CheckpointStore};
use harness_hygiene::events::EventSink;
use harness_hygiene::proof::{BugfixProof, CheckpointWorktree, ProofWorktree};
use harness_hygiene::verify::VerifyRunner;
use harness_tools::exec::{Exec, ExecRequest, StreamKind};
use serde_json::{Value, json};

use crate::artifacts::ARTIFACTS_ENV;
use crate::executor::{Checkpointer, StepAction, StepFailure, StepOutput, StepRequest, StepRunner};
use crate::schema::Expect;

/// Longest stdout / output kept in step outputs.
pub const MAX_OUTPUT_CHARS: usize = 16 * 1024;

fn tail(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_owned();
    }
    let mut start = s.len() - max;
    while !s.is_char_boundary(start) {
        start += 1;
    }
    format!("[... {} bytes cut ...]\n{}", start, &s[start..])
}

/// The output schema an agent step must satisfy (declared outputs, plus
/// artifacts as strings). `None` when the step declares neither.
pub fn agent_output_schema(outputs: &[String], artifacts: &[String]) -> Option<Value> {
    if outputs.is_empty() && artifacts.is_empty() {
        return None;
    }
    let mut props = serde_json::Map::new();
    for o in outputs {
        props.insert(o.clone(), json!({}));
    }
    for a in artifacts {
        props.insert(
            a.clone(),
            json!({"type": "string", "description": format!("full content of artifact `{a}`")}),
        );
    }
    let required: Vec<&String> = outputs.iter().chain(artifacts).collect();
    Some(json!({"type": "object", "properties": props, "required": required}))
}

/// Runs agent steps as child agents of the step node.
#[derive(Debug, Clone)]
pub struct AgentStepRunner {
    runtime: SubagentRuntime,
    host: Option<ChildHost>,
}

impl AgentStepRunner {
    /// Runs agents through `runtime` (its builder resolves agent names).
    pub fn new(runtime: SubagentRuntime) -> Self {
        Self {
            runtime,
            host: None,
        }
    }

    /// Runs the agents' tools on `host` (see [`crate::remote`]).
    pub fn on_host(mut self, host: ChildHost) -> Self {
        self.host = Some(host);
        self
    }
}

#[async_trait]
impl StepRunner for AgentStepRunner {
    async fn run(&self, req: StepRequest) -> Result<StepOutput, StepFailure> {
        let StepAction::Agent {
            agent,
            model,
            brief,
        } = &req.action
        else {
            return Err(StepFailure::new("internal: not an agent step"));
        };
        let mut brief = brief.clone();
        if !req.scope.is_empty() {
            brief.push_str(&format!(
                "\n\nEdit scope: you may change files matching {}.",
                req.scope.join(", ")
            ));
        }
        let mut child = ChildRequest::new(
            agent.clone(),
            brief,
            req.node.clone(),
            0,
            req.cancel.clone(),
        );
        child.node = Some(NodeId::from(format!("{}.agent", req.node.as_str())));
        child.node_kind = NodeKind::Agent;
        child.model.clone_from(model);
        child.limits = Some(LoopLimits {
            max_turns: None,
            max_tokens: req.limits.tokens,
            max_usd: req.limits.usd,
        });
        child.output_schema = agent_output_schema(&req.outputs, &req.artifacts);
        child.slot = req.slot.clone();
        child.host.clone_from(&self.host);
        let res = self.runtime.run_child(child).await;
        let (cost_usd, tokens) = res
            .outcome
            .as_ref()
            .map_or((0.0, 0), |o| (o.cost_usd, o.usage.total()));
        if !res.succeeded() {
            let cancelled = res
                .outcome
                .as_ref()
                .is_some_and(|o| o.stop == StopReason::Cancelled);
            let message = res.error.clone().unwrap_or_else(|| match &res.outcome {
                Some(o) => o
                    .stop
                    .error_text()
                    .unwrap_or_else(|| o.stop.code().to_owned()),
                None => "the agent did not start".into(),
            });
            return Err(StepFailure {
                message: format!("agent `{agent}` failed: {message}"),
                cancelled,
                cost_usd,
                tokens,
            });
        }
        let mut out = StepOutput {
            cost_usd,
            tokens,
            ..StepOutput::default()
        };
        out.outputs.insert("summary".into(), json!(res.summary()));
        if let Some(Value::Object(m)) = &res.structured {
            for o in &req.outputs {
                if let Some(v) = m.get(o) {
                    out.outputs.insert(o.clone(), v.clone());
                }
            }
            for a in &req.artifacts {
                if let Some(v) = m.get(a) {
                    let text = v.as_str().map_or_else(|| v.to_string(), str::to_owned);
                    out.artifacts.insert(a.clone(), text.into_bytes());
                }
            }
        }
        Ok(out)
    }
}

/// Runs shell steps with `sh -c`.
#[derive(Clone)]
pub struct ShellStepRunner {
    exec: Arc<dyn Exec>,
    cwd: PathBuf,
    env: Vec<(String, String)>,
    recorder: Option<RunRecorder>,
}

impl std::fmt::Debug for ShellStepRunner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ShellStepRunner")
            .field("cwd", &self.cwd)
            .finish_non_exhaustive()
    }
}

impl ShellStepRunner {
    /// Runs commands through `exec` in `cwd`.
    pub fn new(exec: Arc<dyn Exec>, cwd: impl Into<PathBuf>) -> Self {
        Self {
            exec,
            cwd: cwd.into(),
            env: Vec::new(),
            recorder: None,
        }
    }

    /// Extra environment (use the bash tool's scrubbed environment).
    pub fn with_env(mut self, env: Vec<(String, String)>) -> Self {
        self.env = env;
        self
    }

    /// Records each command as a `tool_call` / `tool_result` on the step.
    pub fn with_recorder(mut self, recorder: RunRecorder) -> Self {
        self.recorder = Some(recorder);
        self
    }
}

/// Parses `name=value` lines for the declared outputs (last one wins).
pub fn parse_output_lines(stdout: &str, names: &[String]) -> serde_json::Map<String, Value> {
    let mut out = serde_json::Map::new();
    for line in stdout.lines() {
        if let Some((k, v)) = line.split_once('=')
            && names.iter().any(|n| n == k.trim())
        {
            out.insert(k.trim().to_owned(), json!(v.trim()));
        }
    }
    out
}

#[async_trait]
impl StepRunner for ShellStepRunner {
    async fn run(&self, req: StepRequest) -> Result<StepOutput, StepFailure> {
        let StepAction::Shell { command } = &req.action else {
            return Err(StepFailure::new("internal: not a shell step"));
        };
        let call_id = format!("{}#{}", req.node.as_str(), req.attempt);
        if let Some(r) = &self.recorder {
            let _ = r.emit(EventDraft::node(
                req.node.clone(),
                EventKind::ToolCall {
                    call_id: call_id.clone(),
                    tool: "shell".into(),
                    args: Payload::Json(json!({"command": command})),
                },
            ));
        }
        let mut env = self.env.clone();
        env.push((
            ARTIFACTS_ENV.into(),
            req.artifacts_dir.display().to_string(),
        ));
        let mut er = ExecRequest::new("sh", vec!["-c".into(), command.clone()], self.cwd.clone());
        er.env = env;
        let start = Instant::now();
        let handle = self
            .exec
            .spawn(er)
            .await
            .map_err(|e| StepFailure::new(format!("could not start `{command}`: {e}")))?;
        let mut stdout = Vec::new();
        let mut both = Vec::new();
        let exit = tokio::select! {
            r = handle.drive(|c| {
                if c.stream == StreamKind::Stdout {
                    stdout.extend_from_slice(&c.data);
                }
                both.extend_from_slice(&c.data);
            }) => r,
            _ = req.cancel.cancelled() => return Err(StepFailure::cancelled()),
        };
        let exit = exit.map_err(|e| StepFailure::new(format!("`{command}` was lost: {e}")))?;
        let stdout = String::from_utf8_lossy(&stdout).into_owned();
        let combined = String::from_utf8_lossy(&both).into_owned();
        if let Some(r) = &self.recorder {
            let _ = r.emit(EventDraft::node(
                req.node.clone(),
                EventKind::ToolResult {
                    call_id,
                    ok: exit.success(),
                    output: Payload::Text(tail(&combined, MAX_OUTPUT_CHARS)),
                    duration_ms: u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX),
                },
            ));
        }
        if !exit.success() {
            let why = match (exit.code, exit.signal, exit.timed_out) {
                (_, _, true) => "timed out".to_owned(),
                (Some(c), _, _) => format!("exit code {c}"),
                (None, Some(s), _) => format!("killed by signal {s}"),
                _ => "failed".to_owned(),
            };
            return Err(StepFailure::new(format!(
                "`{command}` failed ({why}):\n{}",
                tail(combined.trim_end(), 2000)
            )));
        }
        let mut outputs = parse_output_lines(&stdout, &req.outputs);
        outputs.insert(
            "stdout".into(),
            json!(tail(stdout.trim_end(), MAX_OUTPUT_CHARS)),
        );
        outputs.insert("exit_code".into(), json!(exit.code));
        Ok(StepOutput {
            outputs,
            ..StepOutput::default()
        })
    }
}

/// Runs verify steps.
#[derive(Clone)]
pub struct VerifyStepRunner {
    exec: Arc<dyn Exec>,
    cwd: PathBuf,
    env: Vec<(String, String)>,
    recorder: RunRecorder,
}

impl std::fmt::Debug for VerifyStepRunner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VerifyStepRunner")
            .field("cwd", &self.cwd)
            .finish_non_exhaustive()
    }
}

impl VerifyStepRunner {
    /// Runs verify commands through `exec` in `cwd`, recording
    /// `verify_started` / `verify_finished`.
    pub fn new(exec: Arc<dyn Exec>, cwd: impl Into<PathBuf>, recorder: RunRecorder) -> Self {
        Self {
            exec,
            cwd: cwd.into(),
            env: Vec::new(),
            recorder,
        }
    }

    /// Extra environment.
    pub fn with_env(mut self, env: Vec<(String, String)>) -> Self {
        self.env = env;
        self
    }

    fn runner(&self) -> VerifyRunner {
        VerifyRunner::new(self.exec.clone(), self.cwd.clone())
            .with_env(self.env.clone())
            .with_sink(Arc::new(self.recorder.clone()) as Arc<dyn EventSink>)
    }
}

#[async_trait]
impl StepRunner for VerifyStepRunner {
    async fn run(&self, req: StepRequest) -> Result<StepOutput, StepFailure> {
        let StepAction::Verify { command, expect } = &req.action else {
            return Err(StepFailure::new("internal: not a verify step"));
        };
        let runner = self.runner();
        let run = tokio::select! {
            r = runner.run(Some(&req.node), command) => r,
            _ = req.cancel.cancelled() => return Err(StepFailure::cancelled()),
        };
        let run = run.map_err(|e| StepFailure::new(format!("could not run `{command}`: {e}")))?;
        let ok = match expect {
            Expect::Pass => run.passed,
            Expect::Fail => !run.passed && !run.timed_out,
        };
        if !ok {
            let want = if *expect == Expect::Pass {
                "pass"
            } else {
                "fail"
            };
            return Err(StepFailure::new(format!(
                "`{command}` was expected to {want} but {} (exit {:?}):\n{}",
                if run.passed { "passed" } else { "failed" },
                run.exit_code,
                tail(run.output.trim_end(), 2000)
            )));
        }
        let mut out = StepOutput::default();
        out.outputs.insert("passed".into(), json!(run.passed));
        out.outputs.insert("exit_code".into(), json!(run.exit_code));
        out.outputs
            .insert("output".into(), json!(tail(&run.output, MAX_OUTPUT_CHARS)));
        for a in &req.artifacts {
            out.artifacts
                .insert(a.clone(), run.output.clone().into_bytes());
        }
        Ok(out)
    }
}

/// Builds the worktree swapper of a proof from the original checkpoint and
/// the keep globs.
pub type WorktreeFactory =
    Arc<dyn Fn(u64, &[String]) -> Result<Arc<dyn ProofWorktree>, String> + Send + Sync>;

/// Runs bug-fix proofs.
#[derive(Clone)]
pub struct ProofStepRunner {
    verify: VerifyStepRunner,
    worktree: WorktreeFactory,
}

impl std::fmt::Debug for ProofStepRunner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProofStepRunner").finish_non_exhaustive()
    }
}

impl ProofStepRunner {
    /// Proofs over shadow-git checkpoints in `store`.
    pub fn new(verify: VerifyStepRunner, store: Arc<CheckpointStore>) -> Self {
        Self::with_worktree(
            verify,
            Arc::new(move |original, keep| {
                CheckpointWorktree::prepare(store.clone(), original, keep)
                    .map(|w| Arc::new(w) as Arc<dyn ProofWorktree>)
                    .map_err(|e| e.to_string())
            }),
        )
    }

    /// Proofs with a custom worktree swapper (tests, remote worktrees).
    pub fn with_worktree(verify: VerifyStepRunner, worktree: WorktreeFactory) -> Self {
        Self { verify, worktree }
    }
}

#[async_trait]
impl StepRunner for ProofStepRunner {
    async fn run(&self, req: StepRequest) -> Result<StepOutput, StepFailure> {
        let StepAction::Proof { test, keep } = &req.action else {
            return Err(StepFailure::new("internal: not a bugfix-proof step"));
        };
        let Some(original) = req.checkpoint else {
            return Err(StepFailure::new(
                "bugfix-proof needs a checkpoint from an upstream agent step; configure workflow checkpoints",
            ));
        };
        let worktree = (self.worktree)(original, keep).map_err(StepFailure::new)?;
        let proof = BugfixProof::new(
            self.verify.runner(),
            worktree,
            test.clone(),
            req.node.clone(),
        );
        let report = proof.run(&req.cancel).await.map_err(|e| {
            if matches!(e, harness_hygiene::proof::ProofError::Cancelled) {
                StepFailure::cancelled()
            } else {
                StepFailure::new(e.to_string())
            }
        })?;
        let outputs = report.outputs();
        if !report.proven {
            return Err(StepFailure::new(format!(
                "the fix is not proven: {outputs}"
            )));
        }
        let mut out = StepOutput::default();
        if let Value::Object(m) = &outputs {
            out.outputs = m.clone();
        }
        let text = serde_json::to_string_pretty(&outputs).unwrap_or_default();
        for a in &req.artifacts {
            out.artifacts.insert(a.clone(), text.clone().into_bytes());
        }
        Ok(out)
    }
}

/// [`Checkpointer`] over a shadow-git [`CheckpointStore`].
#[derive(Debug, Clone)]
pub struct StoreCheckpointer(pub Arc<CheckpointStore>);

impl Checkpointer for StoreCheckpointer {
    fn checkpoint(&self, node: &NodeId, label: &str) -> Result<u64, String> {
        self.0
            .snapshot(CheckpointKind::Manual, Some(node), None, label)
            .map(|c| c.id)
            .map_err(|e| e.to_string())
    }
}

/// Artifacts per name, for callers that build [`StepOutput`]s.
pub type Artifacts = BTreeMap<String, Vec<u8>>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_lines_and_schema() {
        let m = parse_output_lines(
            "noise\nversion=1.2\nversion = 1.3\nother=x\n",
            &["version".into()],
        );
        assert_eq!(m.get("version"), Some(&json!("1.3")));
        assert!(m.get("other").is_none());
        assert!(agent_output_schema(&[], &[]).is_none());
        let s = agent_output_schema(&["test_name".into()], &["plan".into()]).unwrap();
        assert_eq!(s["required"], json!(["test_name", "plan"]));
        assert_eq!(s["properties"]["plan"]["type"], "string");
        assert_eq!(tail("abcdef", 3), "[... 3 bytes cut ...]\ndef");
    }
}
