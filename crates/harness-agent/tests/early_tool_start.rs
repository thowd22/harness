//! `[experimental] early_tool_start`: read-only calls start while the reply
//! streams, with the same results and event log as without it.
//!
//! A scripted provider pauses before the end of the message until a tool
//! reports that it started (or a bound passes), so "started before the
//! message ended" is an ordering fact, not a timing measurement.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use harness_agent::agent_loop::{AgentLoop, Conversation, LoopConfig, LoopOutcome, StopReason};
use harness_agent::dispatch::{
    AgentTool, AllowAll, Dispatcher, GateCall, GateDecision, StandardTools, ToolGate, ToolSet,
};
use harness_events::redact::Redactor;
use harness_events::schema::{NodeId, RunId};
use harness_events::store::{RecorderOptions, RunRecorder, read_run};
use harness_providers::model::{Capabilities, ChatRequest, StopKind, StopReason as Stop, Usage};
use harness_providers::provider::{EventStream, Provider, ProviderError, StreamEvent};
use harness_providers::retry::{RetryEvent, RetryReason};
use harness_tools::bash::EnvScrubber;
use harness_tools::config::ToolsConfig;
use harness_tools::local::{LocalExec, LocalWorkspace};
use harness_tools::spill::MemoryBlobSink;
use harness_tools::tool::{ToolContext, ToolError, ToolErrorKind, ToolOutput};
use serde_json::{Value, json};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

/// What the tools and the provider share.
#[derive(Default)]
struct Ctx {
    /// Set by a `look` call when it starts.
    started: AtomicBool,
    started_notify: Notify,
    /// Whether a `look` call had started when each pause ended.
    seen_at_pause: Mutex<Vec<bool>>,
    /// `look` calls made (including discarded early ones).
    calls: AtomicUsize,
    /// A blocking `look` call was dropped (cancelled).
    dropped: AtomicBool,
    /// Order of tool starts (`look:<text>`, `write:<path>`).
    log: Mutex<Vec<String>>,
}

/// Read-only; `{"block": true}` never returns (until dropped).
struct Look(Arc<Ctx>);

struct DropFlag(Arc<Ctx>);

impl Drop for DropFlag {
    fn drop(&mut self) {
        self.0.dropped.store(true, Ordering::SeqCst);
    }
}

#[async_trait]
impl AgentTool for Look {
    fn name(&self) -> &str {
        "look"
    }
    fn description(&self) -> &str {
        "looks at text"
    }
    fn schema(&self) -> Value {
        json!({"type": "object", "properties": {"text": {"type": "string"}, "block": {"type": "boolean"}}})
    }
    fn read_only(&self) -> bool {
        true
    }
    async fn call(&self, _ctx: &ToolContext, args: Value) -> Result<ToolOutput, ToolError> {
        let ctx = &self.0;
        ctx.calls.fetch_add(1, Ordering::SeqCst);
        let text = args["text"].as_str().unwrap_or_default().to_owned();
        ctx.log.lock().unwrap().push(format!("look:{text}"));
        ctx.started.store(true, Ordering::SeqCst);
        ctx.started_notify.notify_one();
        if args["block"].as_bool() == Some(true) {
            let _guard = DropFlag(ctx.clone());
            std::future::pending::<()>().await;
        }
        Ok(ToolOutput::Text(format!("saw {text}")))
    }
}

/// Mutating.
struct Write(Arc<Ctx>);

#[async_trait]
impl AgentTool for Write {
    fn name(&self) -> &str {
        "write"
    }
    fn description(&self) -> &str {
        "writes"
    }
    fn schema(&self) -> Value {
        json!({"type": "object", "properties": {"path": {"type": "string"}}})
    }
    fn read_only(&self) -> bool {
        false
    }
    async fn call(&self, _ctx: &ToolContext, args: Value) -> Result<ToolOutput, ToolError> {
        let path = args["path"].as_str().unwrap_or_default().to_owned();
        self.0.log.lock().unwrap().push(format!("write:{path}"));
        Ok(ToolOutput::Text(format!("wrote {path}")))
    }
}

/// One step of a scripted reply.
#[derive(Clone)]
enum Step {
    Ev(StreamEvent),
    /// Wait until a `look` call started, at most this long; record whether
    /// one had.
    Pause(Duration),
    Fail(ProviderError),
    Cancel(CancellationToken),
}

struct Scripted {
    ctx: Arc<Ctx>,
    replies: Mutex<VecDeque<Vec<Step>>>,
}

#[async_trait]
impl Provider for Scripted {
    fn name(&self) -> &str {
        "scripted"
    }
    fn capabilities(&self, _model: &str) -> Capabilities {
        Capabilities::default()
    }
    async fn stream(
        &self,
        _request: ChatRequest,
        _cancel: CancellationToken,
    ) -> Result<EventStream, ProviderError> {
        let steps: VecDeque<Step> = self
            .replies
            .lock()
            .unwrap()
            .pop_front()
            .expect("a scripted reply per call")
            .into();
        let ctx = self.ctx.clone();
        Ok(Box::pin(futures::stream::unfold(
            (steps, ctx),
            |(mut steps, ctx)| async move {
                loop {
                    match steps.pop_front()? {
                        Step::Ev(e) => return Some((Ok(e), (steps, ctx))),
                        Step::Fail(e) => {
                            steps.clear();
                            return Some((Err(e), (steps, ctx)));
                        }
                        Step::Pause(max) => {
                            let _ = tokio::time::timeout(max, ctx.started_notify.notified()).await;
                            let seen = ctx.started.load(Ordering::SeqCst);
                            ctx.seen_at_pause.lock().unwrap().push(seen);
                        }
                        Step::Cancel(token) => {
                            token.cancel();
                            // Stay open; the loop sees the cancellation.
                            std::future::pending::<()>().await;
                        }
                    }
                }
            },
        )))
    }
}

fn start() -> Step {
    Step::Ev(StreamEvent::MessageStart {
        id: Some("m".into()),
        model: None,
    })
}

fn tool(index: usize, id: &str, name: &str, args: &str) -> Vec<Step> {
    let mut steps = vec![Step::Ev(StreamEvent::ToolUseStart {
        index,
        id: id.into(),
        name: name.into(),
    })];
    // Arguments in two fragments, as providers stream them.
    let mid = args.len() / 2;
    for part in [&args[..mid], &args[mid..]] {
        if !part.is_empty() {
            steps.push(Step::Ev(StreamEvent::ToolInputDelta {
                index,
                partial_json: part.into(),
            }));
        }
    }
    steps.push(Step::Ev(StreamEvent::BlockStop { index }));
    steps
}

fn text(index: usize, t: &str) -> Vec<Step> {
    vec![
        Step::Ev(StreamEvent::TextDelta {
            index,
            text: t.into(),
        }),
        Step::Ev(StreamEvent::BlockStop { index }),
    ]
}

fn stop(kind: StopKind) -> Step {
    Step::Ev(StreamEvent::MessageStop {
        stop: Stop::new(kind),
        usage: Usage::default(),
    })
}

fn done_reply() -> Vec<Step> {
    let mut r = vec![start()];
    r.extend(text(0, "done"));
    r.push(stop(StopKind::EndTurn));
    r
}

/// A gate that asks about `look` calls with `text = "secret"` (so they
/// never start early) and allows everything when reviewing.
struct AskSecrets {
    reviews: AtomicUsize,
}

#[async_trait]
impl ToolGate for AskSecrets {
    async fn review(&self, _node: Option<&NodeId>, calls: &[GateCall]) -> Vec<GateDecision> {
        self.reviews.fetch_add(1, Ordering::SeqCst);
        vec![GateDecision::Allow; calls.len()]
    }
    fn auto_allows(&self, _node: Option<&NodeId>, call: &GateCall) -> bool {
        call.args["text"] != "secret"
    }
}

struct Run {
    outcome: LoopOutcome,
    /// The event log without volatile fields (times, durations, model call
    /// ids).
    events: Vec<Value>,
    ctx: Arc<Ctx>,
    conversation: Conversation,
}

async fn run_with(
    early: bool,
    replies: impl FnOnce(&CancellationToken) -> Vec<Vec<Step>>,
    gate: Arc<dyn ToolGate>,
) -> Run {
    run_set(early, replies, gate, |ctx| {
        ToolSet::new()
            .with(Arc::new(Look(ctx.clone())))
            .with(Arc::new(Write(ctx.clone())))
    })
    .await
}

async fn run_set(
    early: bool,
    replies: impl FnOnce(&CancellationToken) -> Vec<Vec<Step>>,
    gate: Arc<dyn ToolGate>,
    make_tools: impl FnOnce(&Arc<Ctx>) -> ToolSet,
) -> Run {
    let dir = tempfile::tempdir().unwrap();
    let recorder = RunRecorder::create(
        dir.path(),
        RunId::new(),
        Arc::new(Redactor::default()),
        RecorderOptions::default(),
    )
    .await
    .unwrap();
    let ctx = Arc::new(Ctx::default());
    let cancel = CancellationToken::new();
    let provider = Scripted {
        ctx: ctx.clone(),
        replies: Mutex::new(replies(&cancel).into()),
    };
    let tools = make_tools(&ctx);
    let mut config = LoopConfig::new("coder", "scripted-model");
    config.early_tool_start = early;
    let mut agent = AgentLoop::new(
        Arc::new(provider),
        Dispatcher::new(tools, gate),
        recorder.clone(),
        config,
    )
    .with_node(NodeId::from("agent"));
    let mut conversation = Conversation::new(Some("system".into()));
    conversation.push_user("go");
    let outcome = agent.run(&mut conversation, &cancel).await;
    drop(agent);
    recorder.finish().await.unwrap();
    let events = read_run(recorder.dir())
        .unwrap()
        .events
        .into_iter()
        .map(|e| {
            let mut v = serde_json::to_value(&e.kind).unwrap();
            if let Some(o) = v.as_object_mut() {
                o.remove("duration_ms");
                if o.get("call_id")
                    .and_then(Value::as_str)
                    .is_some_and(|id| id.starts_with("call_"))
                {
                    o.remove("call_id");
                }
            }
            v
        })
        .collect();
    Run {
        outcome,
        events,
        ctx,
        conversation,
    }
}

async fn run(early: bool, replies: impl FnOnce(&CancellationToken) -> Vec<Vec<Step>>) -> Run {
    run_with(early, replies, Arc::new(AllowAll)).await
}

/// Waits (bounded) until a blocking early call was dropped.
async fn wait_dropped(ctx: &Ctx) -> bool {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while !ctx.dropped.load(Ordering::SeqCst) {
        if tokio::time::Instant::now() > deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    true
}

fn kinds(events: &[Value]) -> Vec<&str> {
    events
        .iter()
        .filter_map(|e| e["type"].as_str())
        .collect::<Vec<_>>()
}

/// Two reads, then a pause and more text before the message ends.
fn two_reads(pause: Duration) -> Vec<Vec<Step>> {
    let mut first = vec![start()];
    first.extend(tool(0, "t1", "look", r#"{"text":"a"}"#));
    first.extend(tool(1, "t2", "look", r#"{"text":"b"}"#));
    first.push(Step::Pause(pause));
    first.extend(text(2, "reading both"));
    first.push(stop(StopKind::ToolUse));
    vec![first, done_reply()]
}

#[tokio::test]
async fn leading_read_only_calls_start_before_the_message_ends() {
    let on = run(true, |_| two_reads(Duration::from_secs(10))).await;
    let off = run(false, |_| two_reads(Duration::from_millis(200))).await;

    assert_eq!(*on.ctx.seen_at_pause.lock().unwrap(), vec![true]);
    assert_eq!(*off.ctx.seen_at_pause.lock().unwrap(), vec![false]);
    assert_eq!(on.outcome.stop, StopReason::EndTurn);
    assert_eq!(on.outcome.stop, off.outcome.stop);
    // Each call ran once, and the model saw the same results.
    assert_eq!(on.ctx.calls.load(Ordering::SeqCst), 2);
    assert_eq!(on.conversation.messages, off.conversation.messages);
    // Same events in the same order.
    assert_eq!(on.events, off.events);
    let k = kinds(&on.events);
    let first_result = k.iter().position(|k| *k == "tool_result").unwrap();
    let finished = k.iter().position(|k| *k == "model_call_finished").unwrap();
    let call = k.iter().position(|k| *k == "tool_call").unwrap();
    assert!(finished < call && call < first_result, "{k:?}");
}

#[tokio::test]
async fn reads_after_a_mutating_call_wait_for_the_message() {
    let replies = |pause| {
        let mut first = vec![start()];
        first.extend(tool(0, "t1", "write", r#"{"path":"f"}"#));
        first.extend(tool(1, "t2", "look", r#"{"text":"f"}"#));
        first.push(Step::Pause(pause));
        first.push(stop(StopKind::ToolUse));
        vec![first, done_reply()]
    };
    let on = run(true, |_| replies(Duration::from_millis(300))).await;
    let off = run(false, |_| replies(Duration::from_millis(300))).await;
    assert_eq!(*on.ctx.seen_at_pause.lock().unwrap(), vec![false]);
    assert_eq!(*on.ctx.log.lock().unwrap(), vec!["write:f", "look:f"]);
    assert_eq!(on.events, off.events);
    assert_eq!(on.conversation.messages, off.conversation.messages);
}

#[tokio::test]
async fn calls_the_gate_would_ask_about_are_not_started_early() {
    let replies = |first_text: &str, pause| {
        let mut first = vec![start()];
        first.extend(tool(
            0,
            "t1",
            "look",
            &format!(r#"{{"text":"{first_text}"}}"#),
        ));
        first.extend(tool(1, "t2", "look", r#"{"text":"b"}"#));
        first.push(Step::Pause(pause));
        first.push(stop(StopKind::ToolUse));
        vec![first, done_reply()]
    };
    // A call that needs review blocks itself and everything after it.
    let gate = Arc::new(AskSecrets {
        reviews: AtomicUsize::new(0),
    });
    let on = run_with(
        true,
        |_| replies("secret", Duration::from_millis(300)),
        gate.clone(),
    )
    .await;
    assert_eq!(*on.ctx.seen_at_pause.lock().unwrap(), vec![false]);
    assert_eq!(
        gate.reviews.load(Ordering::SeqCst),
        1,
        "one review per batch"
    );
    let off = run_with(
        false,
        |_| replies("secret", Duration::from_millis(300)),
        Arc::new(AskSecrets {
            reviews: AtomicUsize::new(0),
        }),
    )
    .await;
    assert_eq!(on.events, off.events);
    assert_eq!(on.conversation.messages, off.conversation.messages);

    // An allowed first call starts; the asked-about second one does not.
    let gate = Arc::new(AskSecrets {
        reviews: AtomicUsize::new(0),
    });
    let mut first = vec![start()];
    first.extend(tool(0, "t1", "look", r#"{"text":"a"}"#));
    first.extend(tool(1, "t2", "look", r#"{"text":"secret"}"#));
    first.push(Step::Pause(Duration::from_secs(10)));
    first.push(stop(StopKind::ToolUse));
    let on = run_with(true, move |_| vec![first, done_reply()], gate.clone()).await;
    assert_eq!(*on.ctx.seen_at_pause.lock().unwrap(), vec![true]);
    assert_eq!(*on.ctx.log.lock().unwrap(), vec!["look:a", "look:secret"]);
    assert_eq!(gate.reviews.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn stream_errors_cancel_early_calls() {
    let on = run(true, |_| {
        let mut first = vec![start()];
        first.extend(tool(0, "t1", "look", r#"{"block":true}"#));
        first.push(Step::Pause(Duration::from_secs(10)));
        first.push(Step::Fail(ProviderError::InvalidRequest("boom".into())));
        vec![first]
    })
    .await;
    assert_eq!(*on.ctx.seen_at_pause.lock().unwrap(), vec![true]);
    assert!(matches!(on.outcome.stop, StopReason::Error { .. }));
    assert!(wait_dropped(&on.ctx).await, "the early call was cancelled");
    let k = kinds(&on.events);
    assert!(
        !k.contains(&"tool_call") && !k.contains(&"tool_result"),
        "{k:?}"
    );
}

#[tokio::test]
async fn retries_discard_early_calls() {
    let on = run(true, |_| {
        let mut first = vec![start()];
        first.extend(tool(0, "t1", "look", r#"{"block":true}"#));
        first.push(Step::Pause(Duration::from_secs(10)));
        first.push(Step::Ev(StreamEvent::Retry(RetryEvent {
            attempt: 1,
            max_retries: 3,
            delay: Duration::ZERO,
            reason: RetryReason::Connection,
            error: "reset".into(),
            discard_partial: true,
        })));
        first.extend(done_reply());
        vec![first]
    })
    .await;
    assert_eq!(on.outcome.stop, StopReason::EndTurn);
    assert_eq!(on.outcome.final_text, "done");
    assert!(wait_dropped(&on.ctx).await, "the early call was cancelled");
    let k = kinds(&on.events);
    assert!(
        !k.contains(&"tool_call") && !k.contains(&"tool_result"),
        "{k:?}"
    );
}

#[tokio::test]
async fn cancellation_cancels_early_calls() {
    let on = run(true, |cancel| {
        let mut first = vec![start()];
        first.extend(tool(0, "t1", "look", r#"{"block":true}"#));
        first.push(Step::Pause(Duration::from_secs(10)));
        first.push(Step::Cancel(cancel.clone()));
        vec![first]
    })
    .await;
    assert_eq!(on.outcome.stop, StopReason::Cancelled);
    assert!(wait_dropped(&on.ctx).await, "the early call was cancelled");
}

#[tokio::test]
async fn changed_arguments_run_the_call_again() {
    let replies = |pause| {
        let mut first = vec![start()];
        // The block stops with `{}`, then (against the protocol) more
        // arguments arrive: the final call differs from the early one.
        first.extend(tool(0, "t1", "look", ""));
        first.push(Step::Pause(pause));
        first.push(Step::Ev(StreamEvent::ToolInputDelta {
            index: 0,
            partial_json: r#"{"text":"late"}"#.into(),
        }));
        first.push(stop(StopKind::ToolUse));
        vec![first, done_reply()]
    };
    let on = run(true, |_| replies(Duration::from_secs(10))).await;
    let off = run(false, |_| replies(Duration::from_millis(100))).await;
    assert_eq!(*on.ctx.seen_at_pause.lock().unwrap(), vec![true]);
    assert_eq!(
        on.ctx.calls.load(Ordering::SeqCst),
        2,
        "early run discarded"
    );
    assert_eq!(*on.ctx.log.lock().unwrap(), vec!["look:", "look:late"]);
    assert_eq!(on.conversation.messages, off.conversation.messages);
    assert_eq!(on.events, off.events);
}

#[tokio::test]
async fn off_by_default() {
    assert!(!LoopConfig::new("a", "m").early_tool_start);
    let r = run(false, |_| two_reads(Duration::from_millis(100))).await;
    assert_eq!(*r.ctx.seen_at_pause.lock().unwrap(), vec![false]);
}

// The real read, write and edit tools: an early `read` that is discarded
// must not count as seen by write/edit (read-before-write and stale-read
// checks behave exactly as with the setting off).

/// The standard `read` tool; marks `started` once the read has finished
/// (so a pause proves the early read completed before the discard).
struct ReadDone(Arc<dyn AgentTool>, Arc<Ctx>);

#[async_trait]
impl AgentTool for ReadDone {
    fn name(&self) -> &str {
        self.0.name()
    }
    fn description(&self) -> &str {
        self.0.description()
    }
    fn schema(&self) -> Value {
        self.0.schema()
    }
    fn read_only(&self) -> bool {
        self.0.read_only()
    }
    async fn call(&self, ctx: &ToolContext, args: Value) -> Result<ToolOutput, ToolError> {
        let out = self.0.call(ctx, args).await;
        self.1.calls.fetch_add(1, Ordering::SeqCst);
        self.1.started.store(true, Ordering::SeqCst);
        self.1.started_notify.notify_one();
        out
    }
}

/// A temp project with `foo.txt`, and the standard tools over it with
/// `read` wrapped in [`ReadDone`].
struct Files {
    dir: tempfile::TempDir,
    standard: ToolSet,
}

fn files() -> Files {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("foo.txt"), "old\n").unwrap();
    let standard = ToolSet::standard(StandardTools {
        workspace: Arc::new(LocalWorkspace::new(dir.path())),
        exec: Arc::new(LocalExec::new()),
        cwd: dir.path().to_path_buf(),
        config: ToolsConfig::default(),
        scrubber: EnvScrubber::new(Vec::<String>::new(), Vec::<String>::new()),
        spill: Arc::new(MemoryBlobSink::default()),
    });
    Files { dir, standard }
}

impl Files {
    fn tools(&self, ctx: &Arc<Ctx>) -> ToolSet {
        let read = self.standard.get("read").unwrap().clone();
        ToolSet::new()
            .with(Arc::new(ReadDone(read, ctx.clone())))
            .with(self.standard.get("write").unwrap().clone())
            .with(self.standard.get("edit").unwrap().clone())
    }

    /// Calls the session's `write` on foo.txt directly (same tracker).
    async fn write_foo(&self) -> Result<ToolOutput, ToolError> {
        self.standard
            .get("write")
            .unwrap()
            .call(
                &ToolContext::new("direct"),
                json!({"path": "foo.txt", "content": "new\n"}),
            )
            .await
    }

    fn foo(&self) -> String {
        std::fs::read_to_string(self.dir.path().join("foo.txt")).unwrap()
    }
}

fn read_foo(index: usize, id: &str) -> Vec<Step> {
    tool(index, id, "read", r#"{"path":"foo.txt"}"#)
}

/// Text of every tool result the model was given, in order.
fn results(conversation: &Conversation) -> Vec<(String, bool)> {
    conversation
        .messages
        .iter()
        .flat_map(|m| m.content.iter())
        .filter_map(|b| match b {
            harness_providers::model::ContentBlock::ToolResult {
                content, is_error, ..
            } => Some((
                content
                    .iter()
                    .map(|p| match p {
                        harness_providers::model::ToolResultPart::Text { text } => text.clone(),
                        _ => String::new(),
                    })
                    .collect(),
                *is_error,
            )),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn a_read_discarded_by_a_retry_does_not_count_as_seen() {
    // Attempt 1 streams read(foo.txt), which finishes early; the stream is
    // then retried with its partial output discarded, and the new attempt
    // writes foo.txt without reading it.
    let replies = |pause| {
        let mut first = vec![start()];
        first.extend(read_foo(0, "t1"));
        first.push(Step::Pause(pause));
        first.push(Step::Ev(StreamEvent::Retry(RetryEvent {
            attempt: 1,
            max_retries: 3,
            delay: Duration::ZERO,
            reason: RetryReason::Connection,
            error: "reset".into(),
            discard_partial: true,
        })));
        first.push(start());
        first.extend(tool(
            0,
            "t2",
            "write",
            r#"{"path":"foo.txt","content":"new\n"}"#,
        ));
        first.push(stop(StopKind::ToolUse));
        vec![first, done_reply()]
    };
    let on_files = files();
    let on = run_set(
        true,
        |_| replies(Duration::from_secs(10)),
        Arc::new(AllowAll),
        |ctx| on_files.tools(ctx),
    )
    .await;
    let off_files = files();
    let off = run_set(
        false,
        |_| replies(Duration::from_millis(100)),
        Arc::new(AllowAll),
        |ctx| off_files.tools(ctx),
    )
    .await;
    assert_eq!(
        *on.ctx.seen_at_pause.lock().unwrap(),
        vec![true],
        "the early read finished before the retry"
    );
    assert_eq!(on.ctx.calls.load(Ordering::SeqCst), 1);
    assert_eq!(off.ctx.calls.load(Ordering::SeqCst), 0);
    let r = results(&on.conversation);
    assert_eq!(r.len(), 1, "{r:?}");
    assert!(r[0].1 && r[0].0.contains("you have not read it"), "{r:?}");
    assert_eq!(r, results(&off.conversation));
    assert_eq!(on.conversation.messages, off.conversation.messages);
    assert_eq!(on.events, off.events);
    assert_eq!(on_files.foo(), "old\n");
    assert_eq!(off_files.foo(), "old\n");
}

/// One reply that reads foo.txt early, then `end`s the stream.
fn read_then(end: Step) -> Vec<Vec<Step>> {
    let mut first = vec![start()];
    first.extend(read_foo(0, "t1"));
    first.push(Step::Pause(Duration::from_secs(10)));
    first.push(end);
    vec![first]
}

#[tokio::test]
async fn a_read_discarded_by_a_stream_error_does_not_count_as_seen() {
    let f = files();
    let on = run_set(
        true,
        |_| read_then(Step::Fail(ProviderError::InvalidRequest("boom".into()))),
        Arc::new(AllowAll),
        |ctx| f.tools(ctx),
    )
    .await;
    assert_eq!(*on.ctx.seen_at_pause.lock().unwrap(), vec![true]);
    assert!(matches!(on.outcome.stop, StopReason::Error { .. }));
    let err = f.write_foo().await.unwrap_err();
    assert_eq!(err.kind, ToolErrorKind::NotRead, "{err:?}");
    assert_eq!(f.foo(), "old\n");
}

#[tokio::test]
async fn a_read_discarded_by_cancellation_does_not_count_as_seen() {
    let f = files();
    let on = run_set(
        true,
        |cancel| read_then(Step::Cancel(cancel.clone())),
        Arc::new(AllowAll),
        |ctx| f.tools(ctx),
    )
    .await;
    assert_eq!(*on.ctx.seen_at_pause.lock().unwrap(), vec![true]);
    assert_eq!(on.outcome.stop, StopReason::Cancelled);
    let err = f.write_foo().await.unwrap_err();
    assert_eq!(err.kind, ToolErrorKind::NotRead, "{err:?}");
    assert_eq!(f.foo(), "old\n");
}

#[tokio::test]
async fn a_used_early_read_counts_as_seen() {
    // read(foo.txt) early and used, then write(foo.txt) next turn: allowed,
    // exactly as with the setting off.
    let replies = |pause| {
        let mut first = vec![start()];
        first.extend(read_foo(0, "t1"));
        first.push(Step::Pause(pause));
        first.push(stop(StopKind::ToolUse));
        let mut second = vec![start()];
        second.extend(tool(
            0,
            "t2",
            "write",
            r#"{"path":"foo.txt","content":"new\n"}"#,
        ));
        second.push(stop(StopKind::ToolUse));
        vec![first, second, done_reply()]
    };
    let on_files = files();
    let on = run_set(
        true,
        |_| replies(Duration::from_secs(10)),
        Arc::new(AllowAll),
        |ctx| on_files.tools(ctx),
    )
    .await;
    let off_files = files();
    let off = run_set(
        false,
        |_| replies(Duration::from_millis(100)),
        Arc::new(AllowAll),
        |ctx| off_files.tools(ctx),
    )
    .await;
    assert_eq!(*on.ctx.seen_at_pause.lock().unwrap(), vec![true]);
    let r = results(&on.conversation);
    assert_eq!(r.len(), 2, "{r:?}");
    assert!(!r[0].1 && !r[1].1, "{r:?}");
    assert_eq!(on.conversation.messages, off.conversation.messages);
    assert_eq!(on.events, off.events);
    assert_eq!(on_files.foo(), "new\n");
    assert_eq!(off_files.foo(), "new\n");
}
