//! End-to-end tests for dreaming (TASK-96) and memory undo (TASK-95):
//! a real git drive in a temp dir, the fake provider as the dream model and
//! run logs written with the real run recorder.

use std::path::Path;
use std::sync::Arc;

use harness_config::agents::MemoryAccess;
use harness_config::discovery::Catalog;
use harness_events::redact::Redactor;
use harness_events::schema::{EventKind, NodeId, Payload, RunId, Usage};
use harness_events::store::{EventDraft, RecorderOptions, RunRecorder};
use harness_memory::dream::history::{self, CommitKind};
use harness_memory::dream::schedule::DreamLog;
use harness_memory::dream::{DREAM_SUBJECT, DreamSettings, Dreamer, runs};
use harness_memory::drive::{Drive, Note, NotePath, read_note_at};
use harness_memory::extract::{ExtractConfig, Extractor};
use harness_memory::index::SessionIndex;
use harness_memory::sync::SessionCheckout;
use harness_memory::tool::{MemoryArgs, MemoryTool, MemoryToolConfig};
use harness_providers::fake::{FakeProvider, FakeTurn};
use harness_providers::model::Message;
use time::macros::{date, datetime};
use tokio_util::sync::CancellationToken;

fn drive() -> (tempfile::TempDir, Drive) {
    let tmp = tempfile::tempdir().unwrap();
    let d = Drive::open_or_init(tmp.path().join("memory")).unwrap();
    (tmp, d)
}

fn tool(d: &Drive, session: &str) -> MemoryTool {
    let co = Arc::new(SessionCheckout::open(d, session).unwrap());
    MemoryTool::new(
        co,
        MemoryToolConfig {
            access: MemoryAccess::ReadWrite,
            run_id: format!("run-{session}"),
            project_key: Some("app-1234abcd".into()),
            redactor: Arc::new(Redactor::default()),
            date: Some(date!(2026 - 10 - 01)),
        },
    )
}

fn write(t: &MemoryTool, path: &str, text: &str) -> harness_memory::tool::MemoryOutcome {
    t.execute(MemoryArgs::Write {
        path: path.into(),
        text: text.into(),
        title: None,
    })
    .unwrap()
}

fn put(d: &Drive, rel: &str, text: &str) {
    let co = SessionCheckout::open(d, "seed").unwrap();
    co.write("memory: write seed", |dir| {
        let p = dir.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
        Ok(())
    })
    .unwrap();
}

fn commits(d: &Drive) -> usize {
    d.git()
        .run(&["rev-list", "--count", "main"])
        .unwrap()
        .parse()
        .unwrap()
}

fn note(dir: &Path, p: &str) -> Option<Note> {
    read_note_at(dir, &NotePath::parse(p).unwrap())
        .unwrap()
        .map(|t| Note::parse(&t))
}

async fn record_run(data: &Path, user: &str, reply: &str) -> String {
    let run = RunId::new();
    let rec = RunRecorder::create(
        data,
        run,
        Arc::new(Redactor::default()),
        RecorderOptions::default(),
    )
    .await
    .unwrap();
    let node = NodeId::random();
    rec.emit(EventDraft::run(EventKind::RunStarted {
        name: "coder".into(),
        workflow: None,
        host: None,
        title: None,
        agent_names: Default::default(),
    }))
    .unwrap();
    rec.emit(EventDraft::node(
        node.clone(),
        EventKind::ModelCallStarted {
            call_id: "c1".into(),
            provider: "fake".into(),
            model: "fake".into(),
            request: Payload::Json(serde_json::json!({ "messages": [Message::user(user)] })),
        },
    ))
    .unwrap();
    rec.emit(EventDraft::node(
        node,
        EventKind::ModelCallFinished {
            call_id: "c1".into(),
            usage: Usage::default(),
            stop_reason: None,
            response: Some(Payload::Json(
                serde_json::to_value(Message::assistant(reply)).unwrap(),
            )),
            duration_ms: 1,
        },
    ))
    .unwrap();
    rec.finish().await.unwrap();
    run.to_string()
}

fn lesson_model(reply: &str) -> Extractor {
    let provider = Arc::new(FakeProvider::with_turns([FakeTurn::text(reply)]));
    Extractor::new(
        provider,
        ExtractConfig::new("fake-haiku"),
        Arc::new(Redactor::default()),
    )
}

#[tokio::test]
async fn first_pass_seeds_from_runs_and_is_one_revertable_commit() {
    let (tmp, d) = drive();
    let data = tmp.path().join("data");
    let run = record_run(
        &data,
        "Please use cargo nextest; cargo test hangs in CI here.",
        "Switched to cargo nextest.",
    )
    .await;
    let transcripts = runs::recent(&data, None, 10);
    assert_eq!(transcripts.len(), 1);
    assert_eq!(transcripts[0].messages.len(), 2);

    let dreamer = Dreamer::new(DreamSettings::default(), Arc::new(Redactor::default()))
        .with_lessons(lesson_model(
            r#"[{"path":"tools/cargo","text":"Use cargo nextest; plain cargo test hangs in CI.","reason":"user correction"}]"#,
        ));
    let log = DreamLog::new(&d.internal_dir());
    assert_eq!(
        dreamer.select_runs(&data, &log).len(),
        1,
        "first pass reviews recent runs"
    );

    let before = commits(&d);
    let co = SessionCheckout::open(&d, "dream").unwrap();
    let report = dreamer
        .dream(&co, &transcripts, true, &CancellationToken::new())
        .await
        .unwrap();
    assert!(report.first);
    assert_eq!(report.added.len(), 1, "{report:?}");
    assert_eq!(commits(&d), before + 1, "the result is one commit");

    let cargo = note(d.root(), "tools/cargo.md").unwrap();
    let b = cargo.bullets().next().unwrap();
    assert_eq!(
        b.source.as_deref(),
        Some(format!("harness://run/{run}").as_str())
    );
    let memory = std::fs::read_to_string(d.root().join("MEMORY.md")).unwrap();
    assert!(memory.contains("[[tools/cargo]]"), "{memory}");

    // The commit is in the dream history with a viewable diff.
    let dreams = history::dreams(&d, 5).unwrap();
    assert_eq!(dreams.len(), 1);
    assert_eq!(dreams[0].kind, CommitKind::Dream);
    assert!(dreams[0].subject.starts_with(DREAM_SUBJECT));
    assert!(dreams[0].body.contains("added: Use cargo nextest"));
    assert_eq!(
        Some(&dreams[0].commit),
        report.commit.as_ref().map(|c| &c.commit)
    );
    let diff = history::diff(&d, &dreams[0].commit).unwrap();
    assert!(diff.contains("+- Use cargo nextest"), "{diff}");

    // The log records the pass; the next pass only looks at newer runs.
    let entry = log.last().unwrap();
    assert!(entry.manual);
    assert_eq!(entry.runs_reviewed, 1);
    assert_eq!(dreamer.select_runs(&data, &log).len(), 0);

    // Undo reverts the whole pass in one commit.
    let undo = history::undo(&co, &dreams[0].commit).unwrap().unwrap();
    assert_eq!(d.head().unwrap(), undo.commit);
    assert!(note(d.root(), "tools/cargo.md").is_none());
    assert!(
        !std::fs::read_to_string(d.root().join("MEMORY.md"))
            .unwrap()
            .contains("[[tools/cargo]]")
    );
    assert_eq!(history::history(&d, 1).unwrap()[0].kind, CommitKind::Undo);
}

#[tokio::test]
async fn dreaming_merges_prunes_and_preserves_preferences_and_sources() {
    let (_tmp, d) = drive();
    put(
        &d,
        "MEMORY.md",
        "# Memory\n- The user prefers short answers. (src: harness://run/p1 · 2026-01-01)\n",
    );
    put(
        &d,
        "projects/app-1234abcd/testing.md",
        "# Testing\n- Run just test-int for integration tests (src: harness://run/a · 2026-10-02)\n- run just test-int for integration tests (src: harness://run/b · 2026-10-03)\n- PR #42 is waiting for review (src: harness://run/c · 2026-10-04)\n",
    );
    put(
        &d,
        "tools/legacy.md",
        "# Legacy\n- Flag --old was needed for v1 (src: harness://run/d · 2026-01-01)\n- Never run the legacy migrator twice (src: harness://run/e · 2026-01-01)\n",
    );
    let co = SessionCheckout::open(&d, "dream").unwrap();
    let report = Dreamer::new(DreamSettings::default(), Arc::new(Redactor::default()))
        .with_now(datetime!(2026-10-22 12:00 UTC))
        .dream(&co, &[], false, &CancellationToken::new())
        .await
        .unwrap();
    assert!(
        report.summary().contains("1 merged"),
        "{}",
        report.summary()
    );
    assert!(report.summary().contains("1 transient removed"));
    assert!(report.summary().contains("stale pruned"));

    let testing = note(d.root(), "projects/app-1234abcd/testing.md").unwrap();
    let bullets: Vec<_> = testing.bullets().collect();
    assert_eq!(bullets.len(), 1);
    assert_eq!(
        bullets[0].source.as_deref(),
        Some("harness://run/a,harness://run/b"),
        "merged bullets keep every source link"
    );
    let legacy = note(d.root(), "tools/legacy.md").unwrap();
    assert_eq!(
        legacy
            .bullets()
            .map(|b| b.text.as_str())
            .collect::<Vec<_>>(),
        ["Never run the legacy migrator twice"],
        "stale facts pruned, preferences kept"
    );
    let memory = note(d.root(), "MEMORY.md").unwrap();
    assert!(memory.contains_text("The user prefers short answers."));

    // Running again changes nothing: no new commit.
    let n = commits(&d);
    let again = Dreamer::new(DreamSettings::default(), Arc::new(Redactor::default()))
        .with_now(datetime!(2026-10-22 13:00 UTC))
        .dream(&co, &[], false, &CancellationToken::new())
        .await
        .unwrap();
    assert!(again.commit.is_none());
    assert_eq!(commits(&d), n);
    assert!(!again.first);
}

#[test]
fn settings_come_from_the_builtin_dream_workflow() {
    let catalog = Catalog::load(&harness_config::builtins::roots());
    let wf = catalog
        .workflows
        .get(harness_config::builtins::DREAM_WORKFLOW)
        .unwrap();
    let s = DreamSettings::from_workflow(&wf.document).unwrap();
    assert_eq!(s, DreamSettings::default());
    let custom =
        serde_json::json!({"steps": [{"id": "x", "type": "dream", "with": {"grace_days": 7}}]});
    assert_eq!(DreamSettings::from_workflow(&custom).unwrap().grace_days, 7);
    assert!(DreamSettings::from_workflow(&serde_json::json!({"steps": []})).is_err());
    let bad = serde_json::json!({"steps": [{"type": "dream", "with": {"bogus": 1}}]});
    assert!(DreamSettings::from_workflow(&bad).is_err());
}

#[test]
fn memory_write_undo_reverts_the_commit() {
    let (_tmp, d) = drive();
    let t = tool(&d, "s1");
    let out = write(
        &t,
        "tools/cargo",
        "Use cargo nextest; cargo test hangs in CI.",
    );
    let change = out.change.unwrap();
    assert!(note(d.root(), "tools/cargo.md").is_some());
    let writes = history::recent_writes(&d, 5).unwrap();
    assert_eq!(writes[0].commit, change.commit.commit);
    assert_eq!(writes[0].kind, CommitKind::Write);
    assert_eq!(writes[0].run.as_deref(), Some("harness://run/run-s1"));

    let co = SessionCheckout::open(&d, "undo").unwrap();
    let undo = history::undo(&co, &change.commit.commit).unwrap();
    assert!(undo.is_some());
    assert!(
        note(d.root(), "tools/cargo.md").is_none(),
        "the note is gone again"
    );
    assert!(history::undo(&co, "not-a-sha").is_err());
}

#[test]
fn new_memories_apply_to_future_sessions_only() {
    let (_tmp, d) = drive();
    let co1 = Arc::new(SessionCheckout::open(&d, "s1").unwrap());
    let index1 = SessionIndex::load(co1.path(), None, 1500).unwrap();
    let t = MemoryTool::new(
        co1.clone(),
        MemoryToolConfig {
            access: MemoryAccess::ReadWrite,
            run_id: "r1".into(),
            project_key: None,
            redactor: Arc::new(Redactor::default()),
            date: None,
        },
    );
    write(&t, "MEMORY.md", "The user prefers tabs over spaces.");
    // The running session keeps its frozen index (stable prompt prefix).
    assert!(!index1.text().contains("tabs over spaces"));
    // A new session sees the memory.
    let co2 = SessionCheckout::open(&d, "s2").unwrap();
    let index2 = SessionIndex::load(co2.path(), None, 1500).unwrap();
    assert!(index2.text().contains("tabs over spaces"));
}
