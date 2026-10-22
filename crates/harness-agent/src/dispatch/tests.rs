use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use harness_providers::model::Role;
use serde_json::json;
use tokio::sync::Barrier;

use super::*;

/// A test tool: optionally waits on a barrier, tracks concurrency and logs
/// call order.
struct Probe {
    name: &'static str,
    read_only: bool,
    barrier: Option<Arc<Barrier>>,
    in_flight: Arc<AtomicUsize>,
    max_in_flight: Arc<AtomicUsize>,
    log: Arc<Mutex<Vec<String>>>,
    fail: bool,
    sleep_ms: u64,
}

impl Probe {
    fn new(name: &'static str, read_only: bool, shared: &Shared) -> Self {
        Self {
            name,
            read_only,
            barrier: None,
            in_flight: shared.in_flight.clone(),
            max_in_flight: shared.max.clone(),
            log: shared.log.clone(),
            fail: false,
            sleep_ms: 5,
        }
    }
}

#[derive(Default)]
struct Shared {
    in_flight: Arc<AtomicUsize>,
    max: Arc<AtomicUsize>,
    log: Arc<Mutex<Vec<String>>>,
}

#[async_trait]
impl AgentTool for Probe {
    fn name(&self) -> &str {
        self.name
    }
    fn description(&self) -> &str {
        "probe"
    }
    fn schema(&self) -> Value {
        json!({"type": "object", "properties": {"n": {"type": "integer"}}, "required": ["n"], "additionalProperties": false})
    }
    fn read_only(&self) -> bool {
        self.read_only
    }
    async fn call(&self, _ctx: &ToolContext, args: Value) -> Result<ToolOutput, ToolError> {
        let now = self.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
        self.max_in_flight.fetch_max(now, Ordering::SeqCst);
        self.log
            .lock()
            .unwrap()
            .push(format!("{}:{}", self.name, args["n"]));
        if let Some(b) = &self.barrier {
            b.wait().await;
        }
        tokio::time::sleep(Duration::from_millis(self.sleep_ms)).await;
        self.in_flight.fetch_sub(1, Ordering::SeqCst);
        if self.fail {
            Err(ToolError::invalid_args("probe failed. Try n=1."))
        } else {
            Ok(ToolOutput::Text(format!("{} ok {}", self.name, args["n"])))
        }
    }
}

fn message(blocks: Vec<ContentBlock>) -> Message {
    Message {
        role: Role::Assistant,
        content: blocks,
    }
}

fn use_block(id: &str, name: &str, input: Value) -> ContentBlock {
    ContentBlock::tool_use(id, name, input)
}

fn nop(_: &PreparedCall, _: &CallOutcome) {}

#[tokio::test]
async fn read_only_calls_run_in_parallel() {
    let shared = Shared::default();
    let barrier = Arc::new(Barrier::new(3));
    let mut probe = Probe::new("look", true, &shared);
    probe.barrier = Some(barrier);
    let tools = ToolSet::new().with(Arc::new(probe));
    let mut msg = message(vec![
        use_block("a", "look", json!({"n": 1})),
        use_block("b", "look", json!({"n": 2})),
        use_block("c", "look", json!({"n": 3})),
    ]);
    let calls = prepare_calls(&mut msg, &tools);
    let d = Dispatcher::new(tools, Arc::new(AllowAll));
    // The barrier only opens when all three run at once.
    let out = tokio::time::timeout(
        Duration::from_secs(5),
        d.run(None, &calls, &CancellationToken::new(), &nop),
    )
    .await
    .expect("read-only calls did not run in parallel");
    assert_eq!(
        out.iter().map(|o| o.id.as_str()).collect::<Vec<_>>(),
        ["a", "b", "c"]
    );
    assert!(out.iter().all(|o| !o.is_error));
    assert_eq!(shared.max.load(Ordering::SeqCst), 3);
}

#[tokio::test]
async fn mutating_calls_run_serially_in_order() {
    let shared = Shared::default();
    let tools = ToolSet::new()
        .with(Arc::new(Probe::new("look", true, &shared)))
        .with(Arc::new(Probe::new("change", false, &shared)));
    let mut msg = message(vec![
        use_block("1", "change", json!({"n": 1})),
        use_block("2", "change", json!({"n": 2})),
        use_block("3", "look", json!({"n": 3})),
        use_block("4", "change", json!({"n": 4})),
    ]);
    let calls = prepare_calls(&mut msg, &tools);
    let d = Dispatcher::new(tools, Arc::new(AllowAll));
    let out = d.run(None, &calls, &CancellationToken::new(), &nop).await;
    assert_eq!(out.len(), 4);
    assert_eq!(shared.max.load(Ordering::SeqCst), 1);
    assert_eq!(
        *shared.log.lock().unwrap(),
        ["change:1", "change:2", "look:3", "change:4"]
    );
}

#[tokio::test]
async fn unknown_tools_bad_json_and_duplicate_ids_do_not_crash() {
    let shared = Shared::default();
    let tools = ToolSet::new().with(Arc::new(Probe::new("look", true, &shared)));
    let mut msg = message(vec![
        use_block("x", "lok", json!({"n": 1})),
        use_block("y", "look", json!("{\"n\": 2,")),
        use_block("y", "look", json!({"n": 3})),
        use_block("z", "look", json!("garbage")),
        use_block("w", "look", json!({"m": 1})),
    ]);
    let calls = prepare_calls(&mut msg, &tools);
    // The duplicate was renamed in the assistant message too.
    let ids: Vec<&str> = msg.tool_uses().map(|(id, _, _)| id).collect();
    assert_eq!(ids, ["x", "y", "y__2", "z", "w"]);
    assert_eq!(calls[2].duplicate_of.as_deref(), Some("y"));
    // Repaired args were written back to the message.
    assert_eq!(msg.tool_uses().nth(1).unwrap().2, &json!({"n": 2}));
    assert!(!calls[1].repairs.is_empty());
    assert_eq!(msg.tool_uses().nth(3).unwrap().2, &json!({}));

    let d = Dispatcher::new(tools, Arc::new(AllowAll));
    let out = d.run(None, &calls, &CancellationToken::new(), &nop).await;
    assert!(
        out[0].is_error && out[0].text().contains("Did you mean `look`?"),
        "{}",
        out[0].text()
    );
    assert!(!out[1].is_error);
    assert!(!out[2].is_error);
    assert!(
        out[3].is_error
            && out[3]
                .text()
                .contains("Fix: Send the arguments as one JSON object"),
        "{}",
        out[3].text()
    );
    assert!(
        out[4].is_error && out[4].text().contains("`n` is required"),
        "{}",
        out[4].text()
    );
    assert_eq!(shared.log.lock().unwrap().len(), 2);
}

struct DenyChange;

#[async_trait]
impl ToolGate for DenyChange {
    async fn review(&self, _node: Option<&NodeId>, calls: &[GateCall]) -> Vec<GateDecision> {
        calls
            .iter()
            .map(|c| {
                if c.tool == "change" {
                    GateDecision::Deny {
                        reason: "no changes today".into(),
                    }
                } else {
                    GateDecision::Allow
                }
            })
            .collect()
    }
}

#[tokio::test]
async fn gate_denials_are_returned_to_the_model() {
    let shared = Shared::default();
    let tools = ToolSet::new()
        .with(Arc::new(Probe::new("look", true, &shared)))
        .with(Arc::new(Probe::new("change", false, &shared)));
    let mut msg = message(vec![
        use_block("1", "change", json!({"n": 1})),
        use_block("2", "look", json!({"n": 2})),
    ]);
    let calls = prepare_calls(&mut msg, &tools);
    let d = Dispatcher::new(tools, Arc::new(DenyChange));
    let out = d.run(None, &calls, &CancellationToken::new(), &nop).await;
    assert!(out[0].is_error && out[0].text().contains("no changes today"));
    assert!(!out[1].is_error);
    assert_eq!(*shared.log.lock().unwrap(), ["look:2"]);
}

#[tokio::test]
async fn cancellation_stops_running_tools() {
    let shared = Shared::default();
    let mut slow = Probe::new("change", false, &shared);
    slow.sleep_ms = 60_000;
    let tools = ToolSet::new().with(Arc::new(slow));
    let mut msg = message(vec![
        use_block("1", "change", json!({"n": 1})),
        use_block("2", "change", json!({"n": 2})),
    ]);
    let calls = prepare_calls(&mut msg, &tools);
    let d = Dispatcher::new(tools, Arc::new(AllowAll));
    let cancel = CancellationToken::new();
    let c2 = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(50)).await;
        c2.cancel();
    });
    let out = tokio::time::timeout(Duration::from_secs(5), d.run(None, &calls, &cancel, &nop))
        .await
        .expect("cancel did not stop the tool");
    assert!(out.iter().all(|o| o.cancelled && o.is_error));
    assert_eq!(
        shared.log.lock().unwrap().len(),
        1,
        "second call never started"
    );
}

#[tokio::test]
async fn standard_tools_work_end_to_end() {
    let dir = tempfile::tempdir().unwrap();
    let blobs = tempfile::tempdir().unwrap();
    let tools = ToolSet::standard(StandardTools {
        workspace: Arc::new(harness_tools::local::LocalWorkspace::new(dir.path())),
        exec: Arc::new(harness_tools::local::LocalExec::new()),
        cwd: dir.path().to_path_buf(),
        config: ToolsConfig::default(),
        scrubber: EnvScrubber::new(Vec::new(), Vec::new()),
        spill: Arc::new(RunBlobSink(BlobStore::new(blobs.path()))),
    });
    assert_eq!(tools.names(), ["read", "write", "edit", "bash"]);
    let mut msg = message(vec![
        use_block("w", "write", json!({"path": "a.txt", "content": "hello\n"})),
        use_block("b", "bash", json!({"command": "cat a.txt"})),
    ]);
    let calls = prepare_calls(&mut msg, &tools);
    let events = Arc::new(harness_tools::tool::RecordingEvents::new());
    let d = Dispatcher::new(tools, Arc::new(AllowAll)).with_events(events.clone());
    let out = d.run(None, &calls, &CancellationToken::new(), &nop).await;
    assert!(!out[0].is_error, "{}", out[0].text());
    assert!(out[1].text().contains("hello"), "{}", out[1].text());
    assert!(matches!(events.events()[0].1, ToolEvent::FileChanged(_)));
}

#[test]
fn run_blob_sink_round_trips() {
    let dir = tempfile::tempdir().unwrap();
    let sink = RunBlobSink(BlobStore::new(dir.path()));
    let id = sink.put(b"big output").unwrap();
    assert_eq!(sink.get(&id).unwrap().unwrap(), b"big output");
    assert!(sink.get("nope").unwrap().is_none());
}

#[test]
fn base64_matches_reference_vectors() {
    assert_eq!(base64_encode(b""), "");
    assert_eq!(base64_encode(b"f"), "Zg==");
    assert_eq!(base64_encode(b"fo"), "Zm8=");
    assert_eq!(base64_encode(b"foo"), "Zm9v");
    assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
}

#[test]
fn tool_set_select_keeps_order_and_reports_missing() {
    let shared = Shared::default();
    let tools = ToolSet::new()
        .with(Arc::new(Probe::new("a", true, &shared)))
        .with(Arc::new(Probe::new("b", true, &shared)));
    let (picked, missing) = tools.select(&["b", "zz", "a"]);
    assert_eq!(picked.names(), ["b", "a"]);
    assert_eq!(missing, ["zz"]);
}

struct DenyUi;

#[async_trait]
impl harness_policy::approvals::ApprovalUi for DenyUi {
    async fn ask(
        &self,
        prompt: harness_policy::approvals::ApprovalPrompt,
    ) -> Vec<(String, harness_policy::approvals::ApprovalAnswer)> {
        prompt
            .requests
            .into_iter()
            .map(|r| {
                (
                    r.approval_id,
                    harness_policy::approvals::ApprovalAnswer::Deny { reason: None },
                )
            })
            .collect()
    }
}

#[tokio::test]
async fn live_output_and_policy_decisions_reach_the_run_log() {
    use harness_events::redact::Redactor;
    use harness_events::store::{RecorderOptions, read_run};
    use harness_policy::rules::PolicyConfig;

    let dir = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    let rec = RunRecorder::create(
        data.path(),
        harness_events::schema::RunId::new(),
        Arc::new(Redactor::default()),
        RecorderOptions::default(),
    )
    .await
    .unwrap();
    let tools = ToolSet::standard(StandardTools {
        workspace: Arc::new(harness_tools::local::LocalWorkspace::new(dir.path())),
        exec: Arc::new(harness_tools::local::LocalExec::new()),
        cwd: dir.path().to_path_buf(),
        config: ToolsConfig::default(),
        scrubber: EnvScrubber::new(Vec::new(), Vec::new()),
        spill: Arc::new(RunBlobSink(rec.blobs().clone())),
    });
    let engine = PolicyEngine::new(&PolicyConfig::default(), dir.path()).unwrap();
    let broker = ApprovalBroker::new(engine.grants().clone(), Arc::new(DenyUi), dir.path());
    let gate = PolicyGate::new(engine, Arc::new(broker), Mode::Auto).with_recorder(rec.clone());
    let node = NodeId::from("agent");
    let d = Dispatcher::new(tools.clone(), Arc::new(gate)).with_events(Arc::new(
        RecorderToolEvents::new(rec.clone(), node.clone(), dir.path()),
    ));
    let mut msg = message(vec![
        use_block(
            "b1",
            "bash",
            json!({"command": "printf 'h\\303\\251llo\\n'"}),
        ),
        use_block("b2", "bash", json!({"command": "git add -A"})),
    ]);
    let calls = prepare_calls(&mut msg, &tools);
    let out = d
        .run(Some(&node), &calls, &CancellationToken::new(), &nop)
        .await;
    assert!(!out[0].is_error, "{}", out[0].text());
    assert!(out[1].is_error, "git add -A must not run unapproved");
    rec.finish().await.unwrap();

    let events = read_run(rec.dir()).unwrap().events;
    let output: String = events
        .iter()
        .filter_map(|e| match &e.kind {
            EventKind::ToolOutput { call_id, text, .. } if call_id == "b1" => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(output, "héllo\n");
    let decisions: Vec<(&str, &str, &str)> = events
        .iter()
        .filter_map(|e| match &e.kind {
            EventKind::PolicyDecision {
                call_id,
                decision,
                origin,
                ..
            } => Some((call_id.as_str(), decision.as_str(), origin.as_str())),
            _ => None,
        })
        .collect();
    assert_eq!(decisions.len(), 2, "{decisions:?}");
    assert_eq!(decisions[0].0, "b1");
    assert_eq!(decisions[0].1, "allow");
    assert_eq!(
        decisions[1],
        ("b2", "ask", harness_policy::staging::STAGING_ORIGIN)
    );
    assert!(
        events
            .iter()
            .filter(|e| matches!(e.kind, EventKind::PolicyDecision { .. }))
            .all(|e| e.node_id.as_ref() == Some(&node))
    );
}

#[test]
fn split_utf8_sequences_are_held_until_complete() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let data = tempfile::tempdir().unwrap();
    let rec = rt.block_on(async {
        RunRecorder::create(
            data.path(),
            harness_events::schema::RunId::new(),
            Arc::new(harness_events::redact::Redactor::default()),
            Default::default(),
        )
        .await
        .unwrap()
    });
    let sink = RecorderToolEvents::new(rec, NodeId::from("n"), data.path());
    let bytes = "é".as_bytes();
    assert_eq!(sink.decode("c", OutputStream::Stdout, &bytes[..1]), "");
    assert_eq!(sink.decode("c", OutputStream::Stdout, &bytes[1..]), "é");
    // Invalid bytes are replaced, not held forever.
    assert_eq!(
        sink.decode("c", OutputStream::Stdout, b"\xffa"),
        "\u{fffd}a"
    );
}

#[tokio::test]
async fn capped_gates_never_get_looser_than_the_agent_mode() {
    use harness_policy::rules::PolicyConfig;
    let dir = tempfile::tempdir().unwrap();
    let engine = PolicyEngine::new(&PolicyConfig::default(), dir.path()).unwrap();
    let broker = ApprovalBroker::new(engine.grants().clone(), Arc::new(DenyUi), dir.path());
    let gate = Arc::new(PolicyGate::new(engine, Arc::new(broker), Mode::Auto));
    let calls = [GateCall {
        call_id: "c".into(),
        tool: "bash".into(),
        args: json!({"command": "touch x"}),
    }];
    assert_eq!(gate.review(None, &calls).await, vec![GateDecision::Allow]);
    let capped = CappedGate::new(gate.clone(), Mode::Ask);
    assert!(matches!(
        capped.review(None, &calls).await[0],
        GateDecision::Deny { .. }
    ));
    // A looser cap than the session changes nothing.
    *gate.mode_handle().lock().unwrap() = Mode::Ask;
    let loose = CappedGate::new(gate, Mode::Auto);
    assert!(matches!(
        loose.review(None, &calls).await[0],
        GateDecision::Deny { .. }
    ));
}
