//! Session persistence and resume against real run logs.

use std::sync::Arc;
use std::time::Duration;

use harness_agent::agent_loop::{AgentLoop, Conversation, LoopConfig, StopReason};
use harness_agent::dispatch::{AllowAll, Dispatcher, RunBlobSink, StandardTools, ToolSet};
use harness_agent::session::{SessionHandle, SessionMeta, SessionStore, rebuild_conversation};
use harness_events::blobs::{BLOBS_DIR, BlobStore};
use harness_events::redact::Redactor;
use harness_events::schema::{EventKind, RunId};
use harness_events::store::{RecorderOptions, RunRecorder, read_run};
use harness_providers::fake::{FakeBlock, FakeProvider, FakeTurn};
use harness_tools::bash::EnvScrubber;
use harness_tools::config::ToolsConfig;
use harness_tools::local::{LocalExec, LocalWorkspace};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

struct Env {
    data: tempfile::TempDir,
    project: tempfile::TempDir,
}

impl Env {
    fn new() -> Self {
        Self {
            data: tempfile::tempdir().unwrap(),
            project: tempfile::tempdir().unwrap(),
        }
    }

    async fn recorder(&self, threshold: usize) -> RunRecorder {
        RunRecorder::create(
            self.data.path(),
            RunId::new(),
            Arc::new(Redactor::default()),
            RecorderOptions {
                blob_threshold: threshold,
                ..RecorderOptions::default()
            },
        )
        .await
        .unwrap()
    }

    fn tools(&self, recorder: &RunRecorder) -> ToolSet {
        ToolSet::standard(StandardTools {
            workspace: Arc::new(LocalWorkspace::new(self.project.path())),
            exec: Arc::new(LocalExec::new()),
            cwd: self.project.path().to_path_buf(),
            config: ToolsConfig::default(),
            scrubber: EnvScrubber::new(Vec::new(), Vec::new()),
            spill: Arc::new(RunBlobSink(recorder.blobs().clone())),
        })
    }

    fn agent(&self, provider: &FakeProvider, recorder: &RunRecorder) -> AgentLoop {
        AgentLoop::new(
            Arc::new(provider.clone()),
            Dispatcher::new(self.tools(recorder), Arc::new(AllowAll)),
            recorder.clone(),
            LoopConfig::new("coder", "fake-model"),
        )
    }
}

fn thinking_turn() -> FakeTurn {
    FakeTurn {
        content: vec![
            FakeBlock::Reasoning {
                text: "I should write the file first.".into(),
                signature: Some("sig-EqQBCkYIBxgCKkD==".into()),
            },
            FakeBlock::Opaque {
                provider: "anthropic".into(),
                data: json!({"type": "redacted_thinking", "data": "opaque-bytes"}),
            },
            FakeBlock::Text("Writing it.".into()),
            FakeBlock::ToolUse {
                id: Some("toolu_1".into()),
                name: "write".into(),
                input: json!({"path": "notes.txt", "content": "hello\n"}),
            },
        ],
        ..FakeTurn::default()
    }
}

async fn rebuilt(recorder: &RunRecorder) -> Conversation {
    recorder.finish().await.unwrap();
    let replay = read_run(recorder.dir()).unwrap();
    rebuild_conversation(
        &replay.events,
        &BlobStore::new(recorder.dir().join(BLOBS_DIR)),
    )
    .unwrap()
}

#[tokio::test]
async fn rebuild_is_exact_including_reasoning_and_opaque_blocks() {
    let env = Env::new();
    let recorder = env.recorder(1 << 20).await;
    let provider = FakeProvider::with_turns([thinking_turn(), FakeTurn::text("Done.")]);
    let mut conv = Conversation::new(Some("You are a coder.".into()));
    conv.push_user("write notes");
    let out = env
        .agent(&provider, &recorder)
        .run(&mut conv, &CancellationToken::new())
        .await;
    assert_eq!(out.stop, StopReason::EndTurn);
    let back = rebuilt(&recorder).await;
    assert_eq!(back.system, conv.system);
    assert_eq!(back.messages, conv.messages);
    // And it is what the provider was sent (plus the final answer).
    let sent = provider.last_request().unwrap().messages;
    assert_eq!(back.messages[..sent.len()], sent[..]);
    let reasoning = serde_json::to_string(&back.messages[1]).unwrap();
    assert!(reasoning.contains("sig-EqQBCkYIBxgCKkD=="));
    assert!(reasoning.contains("redacted_thinking"));
}

#[tokio::test]
async fn rebuild_resolves_payloads_spilled_to_blobs() {
    let env = Env::new();
    let recorder = env.recorder(64).await;
    let provider =
        FakeProvider::with_turns([thinking_turn(), FakeTurn::text("long answer ".repeat(50))]);
    let mut conv = Conversation::new(Some("s".repeat(500)));
    conv.push_user("write notes");
    env.agent(&provider, &recorder)
        .run(&mut conv, &CancellationToken::new())
        .await;
    let replay_has_blobs = std::fs::read_dir(recorder.dir().join(BLOBS_DIR))
        .unwrap()
        .count()
        > 0;
    assert!(replay_has_blobs);
    let back = rebuilt(&recorder).await;
    assert_eq!(back.messages, conv.messages);
    assert_eq!(back.system, conv.system);
}

#[tokio::test]
async fn rebuild_after_cancel_mid_tool_keeps_trailing_results() {
    let env = Env::new();
    let recorder = env.recorder(1 << 20).await;
    let provider =
        FakeProvider::with_turns([FakeTurn::tool_use("bash", json!({"command": "sleep 30"}))]);
    let mut conv = Conversation::new(None);
    conv.push_user("wait");
    let cancel = CancellationToken::new();
    let c = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(200)).await;
        c.cancel();
    });
    let out = env
        .agent(&provider, &recorder)
        .run(&mut conv, &cancel)
        .await;
    assert_eq!(out.stop, StopReason::Cancelled);
    let back = rebuilt(&recorder).await;
    assert_eq!(back.messages, conv.messages);
    assert_eq!(back.messages.len(), 3);
}

#[tokio::test]
async fn sessions_are_saved_listed_and_resumed_with_the_exact_conversation() {
    let env = Env::new();
    let store = SessionStore::new(env.data.path().join("sessions"));
    let recorder = env.recorder(1 << 20).await;
    let provider = FakeProvider::with_turns([thinking_turn(), FakeTurn::text("Done.")]);
    let prompt = "write notes please";
    let mut handle = SessionHandle::start(
        store.clone(),
        SessionMeta::new(env.project.path(), "coder", "fake/fake-model", prompt),
    )
    .unwrap();
    handle.add_run(recorder.run_id()).unwrap();
    let mut conv = Conversation::new(Some("sys".into()));
    conv.push_user(prompt);
    let mut agent = env
        .agent(&provider, &recorder)
        .with_cost(Arc::new(|_: &harness_providers::model::Usage| Some(0.01)));
    let out = agent.run(&mut conv, &CancellationToken::new()).await;
    handle.record(&out, &conv).unwrap();
    recorder.finish().await.unwrap();

    // Listed with title, project, time and cost.
    let list = store.list().unwrap();
    assert_eq!(list.len(), 1);
    let meta = &list[0];
    assert_eq!(meta.title, prompt);
    assert_eq!(meta.project, env.project.path());
    assert!((meta.cost_usd - 0.02).abs() < 1e-9);
    assert_eq!(meta.last_stop.as_deref(), Some("end_turn"));
    assert!(meta.updated_at >= meta.created_at);
    assert!(meta.files.contains_key("notes.txt"));

    // Resume: nothing changed on disk, conversation is identical.
    let resumed = store.resume(&meta.id[..20], env.data.path()).unwrap();
    assert!(resumed.warnings.is_empty(), "{:?}", resumed.warnings);
    assert_eq!(resumed.conversation.messages, conv.messages);
    assert_eq!(resumed.conversation.system, conv.system);

    // Continue in a new run: the provider sees the old history verbatim.
    let recorder2 = env.recorder(1 << 20).await;
    let mut handle = SessionHandle::start(store.clone(), resumed.meta.clone()).unwrap();
    handle.add_run(recorder2.run_id()).unwrap();
    let provider2 = FakeProvider::with_turns([FakeTurn::text("Continuing.")]);
    let mut conv2 = resumed.conversation;
    conv2.push_user("and now?");
    let out2 = env
        .agent(&provider2, &recorder2)
        .run(&mut conv2, &CancellationToken::new())
        .await;
    assert_eq!(out2.stop, StopReason::EndTurn);
    let sent = provider2.last_request().unwrap();
    assert_eq!(sent.messages[..conv.messages.len()], conv.messages[..]);
    assert_eq!(sent.system.as_deref(), Some("sys"));
    handle.record(&out2, &conv2).unwrap();
    // The new run is self-contained: its first request logs everything.
    recorder2.finish().await.unwrap();
    let first: Value = read_run(recorder2.dir())
        .unwrap()
        .events
        .into_iter()
        .find_map(|e| match e.kind {
            EventKind::ModelCallStarted {
                request: harness_events::schema::Payload::Json(v),
                ..
            } => Some(v),
            _ => None,
        })
        .unwrap();
    assert_eq!(first["messages_from"], 0);
    assert_eq!(store.load(&meta.id).unwrap().runs.len(), 2);
    let again = store.resume(&meta.id, env.data.path()).unwrap();
    assert_eq!(again.conversation.messages, conv2.messages);
}

#[tokio::test]
async fn resuming_after_files_changed_on_disk_warns() {
    let env = Env::new();
    let store = SessionStore::new(env.data.path().join("sessions"));
    let recorder = env.recorder(1 << 20).await;
    let provider = FakeProvider::with_turns([thinking_turn(), FakeTurn::text("Done.")]);
    let mut handle = SessionHandle::start(
        store.clone(),
        SessionMeta::new(env.project.path(), "coder", "m", "notes"),
    )
    .unwrap();
    handle.add_run(recorder.run_id()).unwrap();
    let mut conv = Conversation::new(None);
    conv.push_user("notes");
    let out = env
        .agent(&provider, &recorder)
        .run(&mut conv, &CancellationToken::new())
        .await;
    handle.record(&out, &conv).unwrap();
    recorder.finish().await.unwrap();

    std::fs::write(env.project.path().join("notes.txt"), "edited by the user\n").unwrap();
    let resumed = store.resume("last", env.data.path()).unwrap();
    assert_eq!(resumed.drift.len(), 1);
    assert_eq!(resumed.warnings.len(), 1);
    assert!(
        resumed.warnings[0].contains("notes.txt was modified on disk"),
        "{:?}",
        resumed.warnings
    );
}
