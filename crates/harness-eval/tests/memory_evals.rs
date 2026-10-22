//! Memory evals (TASK-98), run with the fake provider: corrections carry
//! over to the next session, retrieval finds the relevant notes for seeded
//! tasks, and dreaming keeps explicit preferences and source links.

use std::sync::Arc;

use harness_eval::memory_evals::{
    CorrectionCase, correction_eval, dream_preservation_eval, retrieval_cases, retrieval_eval,
    retrieval_seed, seed_drive,
};
use harness_providers::fake::{FakeProvider, FakeTurn};
use serde_json::json;
use time::macros::datetime;

fn case() -> CorrectionCase {
    CorrectionCase {
        correction:
            "Don't use npm in this repo: it uses pnpm, and npm install breaks the lockfile.".into(),
        task: "Add the lodash dependency.".into(),
        expect: vec!["pnpm add lodash".into()],
        forbid: vec!["npm install".into()],
    }
}

#[tokio::test]
async fn a_correction_in_session_one_is_applied_in_session_two() {
    let dir = tempfile::tempdir().unwrap();
    let first = FakeProvider::with_turns([
        FakeTurn::tool_use(
            "memory",
            json!({"action": "write", "path": "tools/pnpm", "text": "Use pnpm in this user's JavaScript repos; npm install breaks the lockfile."}),
        ),
        FakeTurn::text("Understood, I will use pnpm."),
    ]);
    // Session 2 looks memory up before acting.
    let second = FakeProvider::with_turns([
        FakeTurn::tool_use(
            "memory",
            json!({"action": "search", "query": "npm dependency install"}),
        ),
        FakeTurn::text("Run `pnpm add lodash`."),
    ]);
    let r = correction_eval(
        &case(),
        Arc::new(first),
        Arc::new(second),
        "fake",
        dir.path(),
    )
    .await
    .unwrap();
    assert!(r.memory_written, "{r:?}");
    assert!(r.injected, "session 2 found the memory: {r:?}");
    assert!(r.applied, "{}", r.output);
    assert!(r.passed());
}

#[tokio::test]
async fn general_preferences_reach_the_next_session_through_the_prefix() {
    let dir = tempfile::tempdir().unwrap();
    let first = FakeProvider::with_turns([
        FakeTurn::tool_use(
            "memory",
            json!({"action": "write", "path": "MEMORY.md", "text": "The user wants pnpm instead of npm for JavaScript dependencies."}),
        ),
        FakeTurn::text("Noted."),
    ]);
    let second = FakeProvider::with_turns([FakeTurn::text("Run `pnpm add lodash`.")]);
    let r = correction_eval(
        &case(),
        Arc::new(first),
        Arc::new(second.clone()),
        "fake",
        dir.path(),
    )
    .await
    .unwrap();
    assert!(r.passed(), "{r:?}");
    let system = second.requests()[0].system.clone().unwrap();
    assert!(system.contains("pnpm instead of npm"), "{system}");
}

#[tokio::test]
async fn the_eval_fails_when_nothing_is_remembered() {
    let dir = tempfile::tempdir().unwrap();
    let first = FakeProvider::with_turns([FakeTurn::text("OK.")]);
    let second = FakeProvider::with_turns([FakeTurn::text("Run `npm install lodash`.")]);
    let r = correction_eval(
        &case(),
        Arc::new(first),
        Arc::new(second),
        "fake",
        dir.path(),
    )
    .await
    .unwrap();
    assert!(!r.memory_written);
    assert!(!r.injected);
    assert!(!r.applied);
    assert!(!r.passed());
}

#[test]
fn retrieval_returns_relevant_notes_for_seeded_tasks() {
    let dir = tempfile::tempdir().unwrap();
    let drive = seed_drive(dir.path(), &retrieval_seed()).unwrap();
    let r = retrieval_eval(&drive, &retrieval_cases(), 3).unwrap();
    assert!(
        r.recall_at_k >= 0.9,
        "recall@3 {:.2}: {:#?}",
        r.recall_at_k,
        r.results
    );
    assert!(r.mrr >= 0.8, "mrr {:.2}: {:#?}", r.mrr, r.results);
}

#[tokio::test]
async fn dreaming_keeps_preferences_and_source_links() {
    let dir = tempfile::tempdir().unwrap();
    let p = dream_preservation_eval(dir.path(), datetime!(2026-10-22 12:00 UTC))
        .await
        .unwrap();
    assert!(p.passed(), "{p:?}");
    assert!(p.preferences >= 4);
    assert!(p.summary.contains("merged"), "{}", p.summary);
    assert!(p.summary.contains("stale pruned"), "{}", p.summary);
}

/// The same eval against a real model. Run with
/// `ANTHROPIC_API_KEY=... cargo test -p harness-eval --test memory_evals -- --ignored`.
#[tokio::test]
#[ignore = "live: calls the Anthropic API (needs ANTHROPIC_API_KEY)"]
async fn live_correction_eval() {
    let Ok(key) = std::env::var("ANTHROPIC_API_KEY") else {
        panic!("set ANTHROPIC_API_KEY to run the live memory eval");
    };
    let provider: Arc<dyn harness_providers::provider::Provider> = Arc::new(
        harness_providers::anthropic::AnthropicProvider::new(
            &key,
            harness_providers::anthropic::AnthropicConfig::new(),
        )
        .unwrap(),
    );
    let dir = tempfile::tempdir().unwrap();
    let r = correction_eval(
        &case(),
        provider.clone(),
        provider,
        "claude-haiku-4-5",
        dir.path(),
    )
    .await
    .unwrap();
    assert!(r.passed(), "{r:#?}");
}

/// Eval runs never get the memory tool (headless; `headless_access` is not
/// applied to evals), even for agents with `memory: read-write`.
#[tokio::test]
async fn eval_runs_do_not_access_memory() {
    use harness_eval::runner::exec::{Executor, InProcessExecutor, ProcessExecutor};
    use harness_events::schema::EventKind;
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("s.yaml");
    std::fs::write(
        &script,
        "responses:\n  - content: [ { tool_use: { name: memory, input: { action: search, query: x } } } ]\n  - content: [ { text: done } ]\n",
    )
    .unwrap();
    let suite = harness_eval::suite::Suite::parse(
        &format!(
            "name: s\ntasks: [{{id: t, prompt: p, provider: {{fake: {}}}}}]\n",
            script.display()
        ),
        None,
        dir.path().into(),
    )
    .unwrap();
    let task = suite.resolve(&suite.tasks[0]);
    let ws = tempfile::tempdir().unwrap();
    let exe = InProcessExecutor::new()
        .execute(
            &task,
            ws.path(),
            &tokio_util::sync::CancellationToken::new(),
        )
        .await
        .unwrap();
    let memory_ok = exe
        .events
        .iter()
        .any(|e| matches!(&e.kind, EventKind::ToolResult { ok: true, .. }));
    assert!(
        !memory_ok,
        "the memory call must fail: there is no memory tool"
    );
    // The process executor runs `harness run`, which is headless.
    assert_eq!(ProcessExecutor::args(&task, ws.path())[0], "run");
}
