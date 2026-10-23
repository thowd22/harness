//! The identity suite (TASK-140): asked about a harness feature, the agent
//! reads the harness docs before answering. The bundled script reads
//! docs/guide/ and passes; the `scripts/bad` script answers from a guess and
//! is caught. Runs in-process with the fake provider and the built-in
//! defaults, whose prompts start with the identity layer.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use harness_eval::runner::exec::InProcessExecutor;
use harness_eval::runner::{Results, RunOptions, run_suite};
use harness_eval::suite::{Check, ProviderSpec, Suite};
use tokio_util::sync::CancellationToken;

fn suite_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("suites/identity/suite.yaml")
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

#[test]
fn identity_suite_asks_about_a_feature_and_checks_the_docs_were_read() {
    let suite = Suite::load(&suite_path()).unwrap();
    let ids: Vec<&str> = suite.tasks.iter().map(|t| t.id.as_str()).collect();
    assert_eq!(ids, ["reads-docs-before-answering"]);
    let t = &suite.tasks[0];
    assert!(t.problem.as_deref().unwrap().starts_with("problem-"));
    assert!(t.prompt.as_deref().unwrap().contains("workflow"));
    assert!(
        t.checks.iter().any(|c| matches!(
            c,
            Check::Consulted { consulted } if consulted.iter().any(|n| n == "docs/guide/")
        )),
        "{:?}",
        t.checks
    );
}

#[tokio::test]
async fn reading_the_docs_before_answering_passes() {
    let r = run(Suite::load(&suite_path()).unwrap()).await;
    let t = r.task("reads-docs-before-answering").unwrap();
    assert_eq!(t.summary.passed, 1, "{:#?}", t.attempts);
    let a = &t.attempts[0];
    assert_eq!(a.metrics.stop, "end_turn");
    assert_eq!(a.metrics.tool_calls, 1);
}

#[tokio::test]
async fn answering_without_reading_the_docs_is_caught() {
    let mut suite = Suite::load(&suite_path()).unwrap();
    for t in &mut suite.tasks {
        t.provider = Some(ProviderSpec {
            fake: Some(PathBuf::from(format!("scripts/bad/{}.yaml", t.id))),
            model: None,
        });
    }
    let r = run(suite).await;
    let t = r.task("reads-docs-before-answering").unwrap();
    assert_eq!(t.summary.passed, 0, "{:#?}", t.attempts);
    let a = &t.attempts[0];
    assert_eq!(a.error, None);
    assert_eq!(a.metrics.tool_calls, 0);
    let consulted = a
        .checks
        .iter()
        .find(|c| c.check.starts_with("consulted"))
        .unwrap();
    assert!(!consulted.passed);
    assert!(a.premature_stop, "answered while the check fails");
}
