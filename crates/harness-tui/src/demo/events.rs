//! A small builder for scripted run events.
//!
//! The UI gallery, the views' tests and the benchmarks all need realistic
//! event logs. [`EventBuilder`] keeps a clock and a sequence number and
//! emits [`RunEvent`]s with the same shapes the agent loop records.

use harness_events::schema::{
    ApprovalDecision, DeltaChannel, EventKind, FileChangeKind, NodeId, NodeKind, NodeStatus,
    Payload, RunEvent, RunId, RunStatus, Usage,
};
use serde_json::Value;
use time::OffsetDateTime;
use time::macros::datetime;

/// Start time of every scripted run.
pub const T0: OffsetDateTime = datetime!(2026-10-22 14:32:00 UTC);

/// How a node is started.
#[derive(Debug, Clone)]
pub struct NodeSpec {
    /// Node id.
    pub id: String,
    /// Display name.
    pub name: String,
    /// Kind.
    pub kind: NodeKind,
    /// Agent profile.
    pub agent: Option<String>,
    /// Model.
    pub model: Option<String>,
    /// Remote host.
    pub host: Option<String>,
    /// `needs`.
    pub needs: Vec<String>,
    /// Spawning parent.
    pub parent: Option<String>,
    /// Queued instead of running.
    pub queued: bool,
}

impl NodeSpec {
    /// A top-level agent step named after its id.
    pub fn step(id: &str) -> NodeSpec {
        NodeSpec {
            id: id.to_owned(),
            name: id.to_owned(),
            kind: NodeKind::Agent,
            agent: None,
            model: None,
            host: None,
            needs: Vec::new(),
            parent: None,
            queued: false,
        }
    }

    /// A subagent spawned by `parent`.
    pub fn subagent(id: &str, parent: &str) -> NodeSpec {
        NodeSpec {
            kind: NodeKind::Subagent,
            parent: Some(parent.to_owned()),
            ..NodeSpec::step(id)
        }
    }

    /// Sets the display name.
    pub fn name(mut self, name: &str) -> NodeSpec {
        self.name = name.to_owned();
        self
    }

    /// Sets the kind.
    pub fn kind(mut self, kind: NodeKind) -> NodeSpec {
        self.kind = kind;
        self
    }

    /// Sets the agent profile.
    pub fn agent(mut self, agent: &str) -> NodeSpec {
        self.agent = Some(agent.to_owned());
        self
    }

    /// Sets the model.
    pub fn model(mut self, model: &str) -> NodeSpec {
        self.model = Some(model.to_owned());
        self
    }

    /// Sets the remote host.
    pub fn host(mut self, host: &str) -> NodeSpec {
        self.host = Some(host.to_owned());
        self
    }

    /// Sets `needs`.
    pub fn needs(mut self, needs: &[&str]) -> NodeSpec {
        self.needs = needs.iter().map(|s| (*s).to_owned()).collect();
        self
    }

    /// Starts the node queued.
    pub fn queued(mut self) -> NodeSpec {
        self.queued = true;
        self
    }
}

/// Builds a run's event log with a running clock.
#[derive(Debug, Clone)]
pub struct EventBuilder {
    run_id: RunId,
    clock_ms: i64,
    events: Vec<RunEvent>,
}

impl Default for EventBuilder {
    fn default() -> Self {
        EventBuilder::new()
    }
}

impl EventBuilder {
    /// An empty log for a nil run id, starting at [`T0`].
    pub fn new() -> EventBuilder {
        EventBuilder::with_run(uuid_nil())
    }

    /// An empty log for `run_id`.
    pub fn with_run(run_id: RunId) -> EventBuilder {
        EventBuilder {
            run_id,
            clock_ms: 0,
            events: Vec::new(),
        }
    }

    /// Advances the clock.
    pub fn advance(&mut self, ms: i64) -> &mut Self {
        self.clock_ms += ms;
        self
    }

    /// Current time of the clock.
    pub fn now(&self) -> OffsetDateTime {
        T0 + time::Duration::milliseconds(self.clock_ms)
    }

    /// Emits one event.
    pub fn push(&mut self, node: Option<&str>, parent: Option<&str>, kind: EventKind) -> &mut Self {
        let seq = self.events.len() as u64 + 1;
        self.events.push(RunEvent {
            seq,
            ts: self.now(),
            run_id: self.run_id,
            node_id: node.map(NodeId::from),
            parent_node_id: parent.map(NodeId::from),
            kind,
        });
        self
    }

    /// `run_started`.
    pub fn run_started(&mut self, name: &str, workflow: Option<&str>) -> &mut Self {
        self.push(
            None,
            None,
            EventKind::RunStarted {
                name: name.to_owned(),
                workflow: workflow.map(str::to_owned),
                host: None,
            },
        )
    }

    /// `run_finished`.
    pub fn run_finished(&mut self, status: RunStatus) -> &mut Self {
        self.push(
            None,
            None,
            EventKind::RunFinished {
                status,
                error: None,
            },
        )
    }

    /// `node_started`.
    pub fn start(&mut self, spec: NodeSpec) -> &mut Self {
        let parent = spec.parent.clone();
        self.push(
            Some(&spec.id),
            parent.as_deref(),
            EventKind::NodeStarted {
                kind: spec.kind,
                name: spec.name,
                agent: spec.agent,
                model: spec.model,
                host: spec.host,
                needs: spec.needs.into_iter().map(NodeId::from).collect(),
                queued: spec.queued,
            },
        )
    }

    /// `node_running`.
    pub fn running(&mut self, node: &str) -> &mut Self {
        self.push(Some(node), None, EventKind::NodeRunning)
    }

    /// `node_retry`.
    pub fn retry(&mut self, node: &str, attempt: u32, reason: &str) -> &mut Self {
        self.push(
            Some(node),
            None,
            EventKind::NodeRetry {
                attempt,
                reason: reason.to_owned(),
            },
        )
    }

    /// `node_finished`.
    pub fn finish(&mut self, node: &str, status: NodeStatus, error: Option<&str>) -> &mut Self {
        self.push(
            Some(node),
            None,
            EventKind::NodeFinished {
                status,
                error: error.map(str::to_owned),
            },
        )
    }

    /// `model_call_started`.
    pub fn model_call(&mut self, node: &str, call: &str, model: &str) -> &mut Self {
        self.push(
            Some(node),
            None,
            EventKind::ModelCallStarted {
                call_id: call.to_owned(),
                provider: "fake".into(),
                model: model.to_owned(),
                request: Payload::Json(serde_json::json!({
                    "provider": "fake",
                    "model": model,
                    "messages_from": 0,
                    "messages": [],
                })),
            },
        )
    }

    /// `model_call_started` with an explicit request payload.
    pub fn model_call_with(
        &mut self,
        node: &str,
        call: &str,
        model: &str,
        request: Value,
    ) -> &mut Self {
        self.push(
            Some(node),
            None,
            EventKind::ModelCallStarted {
                call_id: call.to_owned(),
                provider: request["provider"].as_str().unwrap_or("fake").to_owned(),
                model: model.to_owned(),
                request: Payload::Json(request),
            },
        )
    }

    /// A `delta`.
    pub fn delta(
        &mut self,
        node: &str,
        call: &str,
        channel: DeltaChannel,
        text: &str,
    ) -> &mut Self {
        self.push(
            Some(node),
            None,
            EventKind::Delta {
                call_id: call.to_owned(),
                channel,
                text: text.to_owned(),
            },
        )
    }

    /// `model_call_finished` with `usage`.
    pub fn finish_call(&mut self, node: &str, call: &str, usage: Usage, ms: u64) -> &mut Self {
        self.finish_call_with(node, call, usage, ms, None)
    }

    /// `model_call_finished` with a response payload.
    pub fn finish_call_with(
        &mut self,
        node: &str,
        call: &str,
        usage: Usage,
        ms: u64,
        response: Option<Value>,
    ) -> &mut Self {
        self.push(
            Some(node),
            None,
            EventKind::ModelCallFinished {
                call_id: call.to_owned(),
                usage,
                stop_reason: Some("end_turn".into()),
                response: response.map(Payload::Json),
                duration_ms: ms,
            },
        )
    }

    /// `cost`.
    pub fn cost(&mut self, node: &str, usd: f64) -> &mut Self {
        self.push(Some(node), None, EventKind::Cost { usd })
    }

    /// `tool_call`.
    pub fn tool_call(&mut self, node: &str, call: &str, tool: &str, args: Value) -> &mut Self {
        self.push(
            Some(node),
            None,
            EventKind::ToolCall {
                call_id: call.to_owned(),
                tool: tool.to_owned(),
                args: Payload::Json(args),
            },
        )
    }

    /// `tool_result`.
    pub fn tool_result(
        &mut self,
        node: &str,
        call: &str,
        ok: bool,
        output: &str,
        ms: u64,
    ) -> &mut Self {
        self.push(
            Some(node),
            None,
            EventKind::ToolResult {
                call_id: call.to_owned(),
                ok,
                output: Payload::text(output),
                duration_ms: ms,
            },
        )
    }

    /// `file_changed`.
    pub fn file_changed(
        &mut self,
        node: &str,
        path: &str,
        change: FileChangeKind,
        diff: Option<&str>,
    ) -> &mut Self {
        self.push(
            Some(node),
            None,
            EventKind::FileChanged {
                path: path.to_owned(),
                change,
                diff: diff.map(Payload::text),
            },
        )
    }

    /// `approval_requested`.
    pub fn approval(&mut self, node: &str, id: &str, action: &str) -> &mut Self {
        self.push(
            Some(node),
            None,
            EventKind::ApprovalRequested {
                approval_id: id.to_owned(),
                action: action.to_owned(),
                preview: None,
            },
        )
    }

    /// `approval_resolved` (approved).
    pub fn approve(&mut self, node: &str, id: &str) -> &mut Self {
        self.push(
            Some(node),
            None,
            EventKind::ApprovalResolved {
                approval_id: id.to_owned(),
                decision: ApprovalDecision::Approved,
                by: "user".into(),
            },
        )
    }

    /// `verify_started` + `verify_finished`.
    pub fn verify(
        &mut self,
        node: &str,
        command: &str,
        passed: bool,
        output: &str,
        ms: u64,
    ) -> &mut Self {
        self.push(
            Some(node),
            None,
            EventKind::VerifyStarted {
                command: command.to_owned(),
            },
        );
        self.advance(ms as i64);
        self.push(
            Some(node),
            None,
            EventKind::VerifyFinished {
                command: command.to_owned(),
                passed,
                exit_code: Some(if passed { 0 } else { 101 }),
                duration_ms: ms,
                output: Payload::text(output),
            },
        )
    }

    /// The events so far.
    pub fn events(&self) -> &[RunEvent] {
        &self.events
    }

    /// Takes the events.
    pub fn into_events(self) -> Vec<RunEvent> {
        self.events
    }
}

/// Token usage shorthand: `usage(input, cached, output, reasoning)`.
pub fn usage(input: u64, cached_input: u64, output: u64, reasoning: u64) -> Usage {
    Usage {
        input,
        cached_input,
        output,
        reasoning,
    }
}

fn uuid_nil() -> harness_events::schema::RunId {
    // RunId wraps a uuid; the nil id keeps snapshots stable.
    serde_json::from_value(serde_json::json!("00000000-0000-0000-0000-000000000000"))
        .expect("nil run id parses")
}
