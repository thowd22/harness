//! The long-wait hint (`[provider.anthropic] cache_ttl_auto_1h`): a
//! running background job and a long start-to-start gap inside one run
//! activate it; the pause before a new user prompt does not. Requests are
//! the same with or without the hook.

use std::sync::Arc;
use std::time::Duration;

use harness_agent::agent_loop::{AgentLoop, Conversation, LoopConfig};
use harness_agent::dispatch::{AllowAll, Dispatcher, StandardTools, ToolSet};
use harness_agent::long_wait::LongWaitHook;
use harness_events::redact::Redactor;
use harness_events::schema::{NodeId, RunId};
use harness_events::store::{RecorderOptions, RunRecorder};
use harness_providers::cache_hint::LongWait;
use harness_providers::fake::{FakeProvider, FakeTurn};
use harness_tools::bash::EnvScrubber;
use harness_tools::config::ToolsConfig;
use harness_tools::local::{LocalExec, LocalWorkspace};
use harness_tools::spill::MemoryBlobSink;
use serde_json::json;
use tokio_util::sync::CancellationToken;

async fn recorder(dir: &std::path::Path) -> RunRecorder {
    RunRecorder::create(
        &dir.join("run"),
        RunId::new(),
        Arc::new(Redactor::default()),
        RecorderOptions::default(),
    )
    .await
    .unwrap()
}

fn bash(dir: &std::path::Path) -> ToolSet {
    ToolSet::standard(StandardTools {
        workspace: Arc::new(LocalWorkspace::new(dir)),
        exec: Arc::new(LocalExec::new()),
        cwd: dir.to_path_buf(),
        config: ToolsConfig::default(),
        scrubber: EnvScrubber::new(Vec::<String>::new(), Vec::<String>::new()),
        spill: Arc::new(MemoryBlobSink::default()),
    })
    .select(&["bash"])
    .0
}

async fn run(
    provider: &FakeProvider,
    tools: &ToolSet,
    rec: &RunRecorder,
    hook: Option<Arc<LongWaitHook>>,
    conv: &mut Conversation,
    prompt: &str,
) {
    let mut lp = AgentLoop::new(
        Arc::new(provider.clone()),
        Dispatcher::new(tools.clone(), Arc::new(AllowAll)),
        rec.clone(),
        LoopConfig::new("coder", "fake-model"),
    )
    .with_node(NodeId::from("n1"));
    if let Some(h) = hook {
        lp = lp.with_hook(h);
    }
    conv.push_user(prompt);
    lp.run(conv, &CancellationToken::new()).await;
}

#[tokio::test]
async fn a_running_background_job_activates_the_hint_until_it_ends() {
    let dir = tempfile::tempdir().unwrap();
    let rec = recorder(dir.path()).await;
    let tools = bash(dir.path());
    let jobs = tools.jobs().unwrap();
    let hint = LongWait::new();
    let hook = Arc::new(LongWaitHook::with_hint(hint.clone(), Some(jobs.clone())));
    let provider = FakeProvider::with_turns([
        FakeTurn::tool_use(
            "bash",
            json!({"command": "sleep 30", "run_in_background": true}),
        ),
        FakeTurn::text("waiting for the job"),
    ]);
    let mut conv = Conversation::new(Some("sys".into()));
    run(
        &provider,
        &tools,
        &rec,
        Some(hook.clone()),
        &mut conv,
        "start it",
    )
    .await;
    assert_eq!(jobs.running(), 1);
    assert!(hint.active(), "the second request saw the running job");
    jobs.shutdown().await;
    assert_eq!(jobs.running(), 0);
    provider.push(FakeTurn::text("done"));
    run(&provider, &tools, &rec, Some(hook), &mut conv, "next").await;
    assert!(!hint.active(), "no job, no long gap");
}

#[tokio::test]
async fn long_gaps_count_only_inside_one_run() {
    let dir = tempfile::tempdir().unwrap();
    let rec = recorder(dir.path()).await;
    let tools = ToolSet::new();
    // Every gap counts as long here; only where it is measured matters.
    let hint = LongWait::with_long_gap(Duration::ZERO);
    let hook = Arc::new(LongWaitHook::with_hint(hint.clone(), None));
    let provider = FakeProvider::with_turns([FakeTurn::text("a"), FakeTurn::text("b")]);
    let mut conv = Conversation::new(Some("sys".into()));
    run(
        &provider,
        &tools,
        &rec,
        Some(hook.clone()),
        &mut conv,
        "one",
    )
    .await;
    run(
        &provider,
        &tools,
        &rec,
        Some(hook.clone()),
        &mut conv,
        "two",
    )
    .await;
    assert!(
        !hint.active(),
        "the gap between two user prompts is not a tool round"
    );
    provider.push(FakeTurn::tool_use("bash", json!({"command": "true"})));
    provider.push(FakeTurn::text("c"));
    run(&provider, &tools, &rec, Some(hook), &mut conv, "three").await;
    assert!(hint.active(), "a gap between two calls of one run counts");
}

#[tokio::test]
async fn requests_are_identical_with_and_without_the_hook() {
    let dir = tempfile::tempdir().unwrap();
    let rec = recorder(dir.path()).await;
    let tools = ToolSet::new();
    let mut seen = Vec::new();
    for hook in [
        None,
        Some(Arc::new(LongWaitHook::with_hint(
            LongWait::with_long_gap(Duration::ZERO),
            None,
        ))),
    ] {
        let provider = FakeProvider::with_turns([
            FakeTurn::tool_use("bash", json!({"command": "true"})),
            FakeTurn::text("ok"),
        ]);
        let mut conv = Conversation::new(Some("sys".into()));
        run(&provider, &tools, &rec, hook, &mut conv, "go").await;
        seen.push(provider.requests());
    }
    assert_eq!(seen[0].len(), 2);
    assert_eq!(seen[0], seen[1]);
}
