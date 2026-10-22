//! The long-task evals: a task finishes after compaction (TASK-86) and
//! premature stops on long tasks are measured (TASK-87).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use harness_eval::runner::exec::{Executor, InProcessExecutor};
use harness_eval::runner::{Results, RunOptions, run_suite};
use harness_eval::suite::{ProviderSpec, Suite};
use harness_events::schema::EventKind;
use tokio_util::sync::CancellationToken;

fn suite() -> Suite {
    Suite::load(&Path::new(env!("CARGO_MANIFEST_DIR")).join("suites/long-tasks/suite.yaml"))
        .unwrap()
}

async fn run(suite: Suite) -> Results {
    run_suite(
        Arc::new(suite),
        Arc::new(InProcessExecutor::new()),
        &RunOptions::default(),
        &CancellationToken::new(),
    )
    .await
}

#[tokio::test]
async fn long_tasks_finish_and_survive_compaction() {
    let r = run(suite()).await;
    for t in &r.tasks {
        assert_eq!(t.summary.passed, 1, "{}: {:#?}", t.id, t.attempts);
        assert!(!t.attempts[0].premature_stop);
    }
    assert_eq!(r.summary.premature_stops, 0);
}

#[tokio::test]
async fn the_compaction_task_really_compacts() {
    let suite = suite();
    let task = suite
        .tasks
        .iter()
        .find(|t| t.id == "survives-compaction")
        .unwrap();
    let resolved = suite.resolve(task);
    let ws = tempfile::tempdir().unwrap();
    let exe = InProcessExecutor::new()
        .execute(&resolved, ws.path(), &CancellationToken::new())
        .await
        .unwrap();
    let compaction = exe.events.iter().any(|e| {
        matches!(&e.kind, EventKind::NodeStarted { name, .. }
            if name == harness_agent::compaction::COMPACTION_NODE)
    });
    assert!(compaction, "no compaction node in the run");
    let lines = std::fs::read_to_string(ws.path().join("steps.txt")).unwrap();
    assert_eq!(lines.lines().count(), 12, "{lines}");
}

#[tokio::test]
async fn premature_stops_on_long_tasks_are_measured() {
    let mut suite = suite();
    for t in &mut suite.tasks {
        t.provider = Some(ProviderSpec {
            fake: Some(PathBuf::from(format!("scripts/bad/{}.yaml", t.id))),
            model: None,
        });
    }
    let r = run(suite).await;
    for t in &r.tasks {
        assert_eq!(t.summary.passed, 0, "{} should fail", t.id);
        assert!(
            t.attempts[0].premature_stop,
            "{}: the agent said done, the checks disagree",
            t.id
        );
    }
    assert_eq!(r.summary.premature_stops, 2);
    assert!(r.render().contains("premature stops 2"), "{}", r.render());
}
