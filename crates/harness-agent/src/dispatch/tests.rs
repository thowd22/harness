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
    assert_eq!(
        tools.names(),
        ["read", "write", "edit", "bash", "bash_jobs", "grep", "glob"]
    );
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

#[tokio::test]
async fn search_tools_are_parallel_policy_checked_reads() {
    use harness_policy::rules::PolicyConfig;

    let dir = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    for (rel, text) in [
        ("src/lib.rs", "pub fn needle() {}\n"),
        ("secrets/key.rs", "needle = 1\n"),
        ("notes.md", "needle\n"),
    ] {
        std::fs::create_dir_all(root.join(rel).parent().unwrap()).unwrap();
        std::fs::write(root.join(rel), text).unwrap();
    }
    let tools = ToolSet::standard(StandardTools {
        workspace: Arc::new(harness_tools::local::LocalWorkspace::new(&root)),
        exec: Arc::new(harness_tools::local::LocalExec::new()),
        cwd: root.clone(),
        config: ToolsConfig::default(),
        scrubber: EnvScrubber::new(Vec::new(), Vec::new()),
        spill: Arc::new(harness_tools::spill::MemoryBlobSink::new()),
    });
    let config = PolicyConfig {
        paths_protected: vec!["secrets/**".into()],
        ..Default::default()
    };
    let engine = PolicyEngine::new(&config, &root).unwrap();
    let broker = ApprovalBroker::new(engine.grants().clone(), Arc::new(DenyUi), &root);
    let gate = PolicyGate::new(engine, Arc::new(broker), Mode::Ask);
    let mut msg = message(vec![
        use_block("g1", "grep", json!({"pattern": "needle"})),
        use_block("g2", "glob", json!({"pattern": "**/*.rs"})),
        use_block(
            "g3",
            "grep",
            json!({"pattern": "needle", "path": "secrets"}),
        ),
        use_block("r1", "read", json!({"path": "notes.md"})),
    ]);
    let calls = prepare_calls(&mut msg, &tools);
    assert!(
        calls[..2]
            .iter()
            .all(|c| c.plan == CallPlan::Run { read_only: true }),
        "{calls:?}"
    );
    let d = Dispatcher::new(tools, Arc::new(gate));
    let out = d.run(None, &calls, &CancellationToken::new(), &nop).await;
    // Ask mode still runs reads without approval; protected files are hidden.
    assert_eq!(out[0].text(), "notes.md\nsrc/lib.rs\n[2 files match.]");
    assert_eq!(out[1].text(), "src/lib.rs");
    assert!(out[2].is_error, "{}", out[2].text());
    assert!(out[2].text().contains("secrets/**"), "{}", out[2].text());
    assert!(!out[3].is_error, "{}", out[3].text());
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
    // The bash call reports its timeout as tool progress for the UI.
    let progress: Vec<Option<u64>> = events
        .iter()
        .filter_map(|e| match &e.kind {
            EventKind::ToolProgress {
                call_id,
                timeout_ms,
                ..
            } if call_id == "b1" => Some(*timeout_ms),
            _ => None,
        })
        .collect();
    assert_eq!(
        progress.first(),
        Some(&Some(
            ToolsConfig::default().bash_timeout().as_millis() as u64
        )),
        "{progress:?}"
    );
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

/// An approval UI that fails the test if it is ever asked.
struct NeverAskUi;

#[async_trait]
impl harness_policy::approvals::ApprovalUi for NeverAskUi {
    async fn ask(
        &self,
        prompt: harness_policy::approvals::ApprovalPrompt,
    ) -> Vec<(String, harness_policy::approvals::ApprovalAnswer)> {
        panic!("asked for approval in yolo mode: {:?}", prompt.requests);
    }
}

#[tokio::test]
async fn yolo_gates_never_ask() {
    use harness_policy::rules::PolicyConfig;
    let dir = tempfile::tempdir().unwrap();
    let config = PolicyConfig {
        bash_ask: vec!["make release*".into()],
        bash_deny: vec!["rm -rf /*".into()],
        ..Default::default()
    };
    let engine = PolicyEngine::new(&config, dir.path()).unwrap();
    let broker = ApprovalBroker::new(engine.grants().clone(), Arc::new(NeverAskUi), dir.path());
    let gate = Arc::new(PolicyGate::new(engine, Arc::new(broker), Mode::Yolo));
    let call = |id: &str, tool: &str, args: Value| GateCall {
        call_id: id.into(),
        tool: tool.into(),
        args,
    };
    let calls = [
        // An outward action, an ask rule, sweeping staging and an
        // unconfigured MCP tool: each asks in auto.
        call("push", "bash", json!({"command": "git push origin main"})),
        call("rule", "bash", json!({"command": "make release"})),
        call("stage", "bash", json!({"command": "git add -A"})),
        call("mcp", "mcp__gh__create_issue", json!({})),
        call("edit", "write", json!({"path": "x.txt", "content": "x"})),
    ];
    let allowed = vec![GateDecision::Allow; calls.len()];
    assert_eq!(gate.review(None, &calls).await, allowed);
    assert!(calls.iter().all(|c| gate.auto_allows(None, c)));
    // A subagent's gate (capped by its agent mode) is yolo too when the
    // agent sets no stricter mode.
    let sub = CappedGate::new(gate.clone(), Mode::Yolo);
    assert_eq!(sub.review(None, &calls).await, allowed);
    // Deny rules still refuse.
    let denied = gate
        .review(None, &[call("rm", "bash", json!({"command": "rm -rf /"}))])
        .await;
    assert!(matches!(denied[0], GateDecision::Deny { .. }), "{denied:?}");
}

/// A tool that forwards to an MCP server tool named by its `call` argument.
struct FakeMcp;

#[async_trait]
impl AgentTool for FakeMcp {
    fn name(&self) -> &str {
        "mcp__gh__x"
    }
    fn description(&self) -> &str {
        "fake mcp"
    }
    fn schema(&self) -> Value {
        json!({"type": "object", "properties": {"call": {"type": "string"}}})
    }
    fn read_only(&self) -> bool {
        false
    }
    async fn call(&self, _ctx: &ToolContext, _args: Value) -> Result<ToolOutput, ToolError> {
        Ok(ToolOutput::Text("done".into()))
    }
    fn mcp_target(&self, args: &Value) -> Option<McpTarget> {
        Some(McpTarget {
            server: "gh".into(),
            tool: args["call"].as_str().unwrap_or("x").into(),
        })
    }
}

#[tokio::test]
async fn mcp_calls_are_decided_as_server_and_tool() {
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
    let mut rules = harness_config::mcp_tools::McpPolicy::new();
    rules.insert(
        "gh".into(),
        harness_config::mcp_tools::McpToolRules {
            allow: vec!["get_*".into()],
            ..Default::default()
        },
    );
    let engine = PolicyEngine::new(&PolicyConfig::default(), dir.path())
        .unwrap()
        .with_mcp(harness_policy::mcp::McpRules::new(
            rules,
            Default::default(),
        ));
    let broker = ApprovalBroker::new(engine.grants().clone(), Arc::new(DenyUi), dir.path());
    // Auto mode: the outward default still asks, and DenyUi refuses.
    let gate = PolicyGate::new(engine, Arc::new(broker), Mode::Auto).with_recorder(rec.clone());
    let tools = ToolSet::new().with(Arc::new(FakeMcp));
    let mut msg = message(vec![
        use_block("m1", "mcp__gh__x", json!({"call": "get_issue"})),
        use_block("m2", "mcp__gh__x", json!({"call": "create_issue"})),
    ]);
    let calls = prepare_calls(&mut msg, &tools);
    let d = Dispatcher::new(tools, Arc::new(gate));
    let out = d.run(None, &calls, &CancellationToken::new(), &nop).await;
    assert!(!out[0].is_error, "{}", out[0].text());
    assert!(
        out[1].is_error && out[1].text().contains("denied"),
        "{}",
        out[1].text()
    );
    rec.finish().await.unwrap();
    let decisions: Vec<(String, String, String)> = read_run(rec.dir())
        .unwrap()
        .events
        .iter()
        .filter_map(|e| match &e.kind {
            EventKind::PolicyDecision {
                tool,
                decision,
                origin,
                ..
            } => Some((tool.clone(), decision.clone(), origin.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(
        decisions,
        [
            (
                "mcp:gh/get_issue".to_owned(),
                "allow".to_owned(),
                "harness.ini [policy] mcp.gh.allow".to_owned()
            ),
            (
                "mcp:gh/create_issue".to_owned(),
                "ask".to_owned(),
                "built-in outward guard".to_owned()
            ),
        ]
    );
}

/// Denies everything and counts the prompts it was shown.
#[derive(Default)]
struct CountingDenyUi(AtomicUsize);

#[async_trait]
impl harness_policy::approvals::ApprovalUi for CountingDenyUi {
    async fn ask(
        &self,
        prompt: harness_policy::approvals::ApprovalPrompt,
    ) -> Vec<(String, harness_policy::approvals::ApprovalAnswer)> {
        self.0.fetch_add(1, Ordering::SeqCst);
        DenyUi.ask(prompt).await
    }
}

/// Runs read / protected read / bash through a policy gate in `Ask` mode,
/// starting what may start early when `early`. Returns the outcomes, the
/// prompts shown, the `policy_decision` events and the ids started early.
async fn policy_batch(early: bool) -> (Vec<(bool, String)>, usize, Vec<Value>, Vec<String>) {
    use harness_events::redact::Redactor;
    use harness_events::store::{RecorderOptions, read_run};
    use harness_policy::rules::PolicyConfig;

    let dir = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    std::fs::create_dir_all(root.join("secrets")).unwrap();
    std::fs::write(root.join("notes.md"), "hello notes\n").unwrap();
    std::fs::write(root.join("secrets/key.rs"), "k = 1\n").unwrap();
    let rec = RunRecorder::create(
        data.path(),
        harness_events::schema::RunId::new(),
        Arc::new(Redactor::default()),
        RecorderOptions::default(),
    )
    .await
    .unwrap();
    let tools = ToolSet::standard(StandardTools {
        workspace: Arc::new(harness_tools::local::LocalWorkspace::new(&root)),
        exec: Arc::new(harness_tools::local::LocalExec::new()),
        cwd: root.clone(),
        config: ToolsConfig::default(),
        scrubber: EnvScrubber::new(Vec::new(), Vec::new()),
        spill: Arc::new(harness_tools::spill::MemoryBlobSink::new()),
    });
    let config = PolicyConfig {
        paths_protected: vec!["secrets/**".into()],
        ..Default::default()
    };
    let engine = PolicyEngine::new(&config, &root).unwrap();
    let ui = Arc::new(CountingDenyUi::default());
    let broker = ApprovalBroker::new(engine.grants().clone(), ui.clone(), &root);
    let gate =
        Arc::new(PolicyGate::new(engine, Arc::new(broker), Mode::Ask).with_recorder(rec.clone()));
    let node = NodeId::from("agent");
    let d = Dispatcher::new(tools.clone(), gate.clone());
    let blocks = vec![
        use_block("r1", "read", json!({"path": "notes.md"})),
        use_block("r2", "read", json!({"path": "secrets/key.rs"})),
        use_block("b1", "bash", json!({"command": "echo hi"})),
    ];
    // The pre-check neither records nor prompts.
    let gc = |tool: &str, args: Value| GateCall {
        call_id: "x".into(),
        tool: tool.into(),
        args,
    };
    assert!(gate.auto_allows(Some(&node), &gc("read", json!({"path": "notes.md"}))));
    assert!(!gate.auto_allows(Some(&node), &gc("read", json!({"path": "secrets/key.rs"}))));
    assert!(!gate.auto_allows(Some(&node), &gc("bash", json!({"command": "echo hi"}))));
    assert!(
        !CappedGate::new(gate.clone(), Mode::Ask)
            .auto_allows(Some(&node), &gc("bash", json!({"command": "echo hi"})))
    );
    assert_eq!(ui.0.load(Ordering::SeqCst), 0);

    let cancel = CancellationToken::new();
    let mut started = EarlyCalls::default();
    if early {
        for b in &blocks {
            if !d.start_early(Some(&node), b, &mut started, &cancel) {
                break;
            }
        }
    }
    let ids = started.ids().iter().map(|s| s.to_string()).collect();
    let mut msg = message(blocks);
    let calls = prepare_calls(&mut msg, &tools);
    let out = d
        .run_with_early(Some(&node), &calls, &cancel, &nop, started)
        .await;
    rec.finish().await.unwrap();
    let decisions = read_run(rec.dir())
        .unwrap()
        .events
        .into_iter()
        .map(|e| serde_json::to_value(&e.kind).unwrap())
        .filter(|v| v["type"] == "policy_decision")
        .collect();
    let out = out.iter().map(|o| (o.is_error, o.text())).collect();
    (out, ui.0.load(Ordering::SeqCst), decisions, ids)
}

#[tokio::test]
async fn early_start_keeps_policy_prompts_and_decisions() {
    let (off, off_prompts, off_decisions, off_ids) = policy_batch(false).await;
    let (on, on_prompts, on_decisions, on_ids) = policy_batch(true).await;
    assert!(off_ids.is_empty());
    // Only the allowed read starts early; the protected read and bash wait
    // for the review (and its prompt).
    assert_eq!(on_ids, ["r1"]);
    assert_eq!(on, off);
    assert!(!on[0].0 && on[0].1.contains("hello notes"), "{on:?}");
    assert!(on[1].0 && on[2].0, "denied by the user: {on:?}");
    assert_eq!(on_prompts, off_prompts);
    assert_eq!(on_prompts, 1, "one prompt for the batch");
    assert_eq!(on_decisions, off_decisions);
    assert_eq!(on_decisions.len(), 3);
}
