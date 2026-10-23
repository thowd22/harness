//! The starter trap suite (TASK-112): every task documents its problem, the
//! bundled "good behaviour" scripts pass, and each "bad behaviour" script is
//! caught by the task's checks. Runs in-process with the fake provider.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use harness_eval::runner::exec::InProcessExecutor;
use harness_eval::runner::{Results, RunOptions, run_suite};
use harness_eval::suite::{ProviderSpec, Suite};
use tokio_util::sync::CancellationToken;

const TRAPS: &[&str] = &[
    "no-test-tampering",
    "temp-file-hygiene",
    "regression-trap",
    "long-context",
    "false-done",
    "broad-git-add",
];

fn suite_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("suites/starter/suite.yaml")
}

async fn run(suite: Suite) -> Results {
    let opts = RunOptions {
        parallel: 6,
        ..Default::default()
    };
    run_suite(
        Arc::new(suite),
        Arc::new(InProcessExecutor::new()),
        &opts,
        &CancellationToken::new(),
    )
    .await
}

#[test]
fn starter_suite_has_every_trap_and_documents_its_problem() {
    let suite = Suite::load(&suite_path()).unwrap();
    let ids: Vec<&str> = suite.tasks.iter().map(|t| t.id.as_str()).collect();
    assert_eq!(ids, TRAPS);
    for t in &suite.tasks {
        let p = t.problem.as_deref().unwrap_or_default();
        assert!(
            p.starts_with("problem-"),
            "{}: problem must name a problem-N label",
            t.id
        );
        assert!(t.description.is_some(), "{} needs a description", t.id);
        assert!(!t.checks.is_empty(), "{} needs checks", t.id);
    }
}

#[tokio::test]
async fn good_behaviour_passes_every_trap() {
    let r = run(Suite::load(&suite_path()).unwrap()).await;
    for t in &r.tasks {
        assert_eq!(t.summary.passed, 1, "{}: {:#?}", t.id, t.attempts);
        let a = &t.attempts[0];
        assert_eq!(a.metrics.stop, "end_turn", "{}", t.id);
        assert!(!a.premature_stop);
        assert_eq!(a.hygiene_violations, 0, "{}: {:?}", t.id, a.hygiene);
        assert!(a.metrics.model_calls >= 1);
    }
    assert!((r.summary.pass_rate - 1.0).abs() < 1e-9);
    assert_eq!(r.executor, "in-process");
}

#[tokio::test]
async fn bad_behaviour_is_caught_by_every_trap() {
    let mut suite = Suite::load(&suite_path()).unwrap();
    for t in &mut suite.tasks {
        t.provider = Some(ProviderSpec {
            fake: Some(PathBuf::from(format!("scripts/bad/{}.yaml", t.id))),
            model: None,
        });
    }
    let r = run(suite).await;
    for t in &r.tasks {
        assert_eq!(
            t.summary.passed, 0,
            "{} should fail: {:#?}",
            t.id, t.attempts
        );
        assert_eq!(t.attempts[0].error, None, "{}", t.id);
    }
    let a = |id: &str| &r.task(id).unwrap().attempts[0];
    assert!(
        a("false-done").premature_stop,
        "claimed done with failing tests"
    );
    assert_eq!(a("false-done").metrics.tool_calls, 0);
    assert!(a("temp-file-hygiene").hygiene_violations >= 1);
    assert!(
        a("temp-file-hygiene").hygiene.temp_files[0].starts_with("test-output.log"),
        "{:?}",
        a("temp-file-hygiene").hygiene
    );
    let tampering = a("no-test-tampering");
    assert!(
        tampering
            .checks
            .iter()
            .any(|c| c.check.starts_with("unchanged") && !c.passed)
    );
    assert!(r.summary.premature_stops >= 1);
}

/// The same suite through the real binary (`harness run --json`). Build it
/// first, then run
/// `HARNESS_BIN=$PWD/target/debug/harness cargo test -p harness-eval --test starter -- --ignored`.
#[tokio::test]
#[ignore = "needs a built harness binary in HARNESS_BIN"]
async fn starter_suite_through_the_harness_binary() {
    let bin = std::env::var("HARNESS_BIN").expect("set HARNESS_BIN to the harness binary");
    let bin = std::fs::canonicalize(bin).unwrap();
    let suite = Suite::load(&suite_path()).unwrap();
    let opts = RunOptions {
        parallel: 6,
        ..Default::default()
    };
    // Hermetic: the binary never sees the user's config, home or data.
    let home = tempfile::tempdir().unwrap();
    let ini = home.path().join("harness.ini");
    std::fs::write(&ini, "").unwrap();
    let path = |rel: &str| home.path().join(rel).display().to_string();
    let env = vec![
        ("HARNESS_CONFIG".to_owned(), ini.display().to_string()),
        ("HOME".to_owned(), path("home")),
        ("XDG_CONFIG_HOME".to_owned(), path("config")),
        ("XDG_DATA_HOME".to_owned(), path("data")),
        ("XDG_CACHE_HOME".to_owned(), path("cache")),
    ];
    let r = run_suite(
        Arc::new(suite),
        Arc::new(harness_eval::runner::exec::ProcessExecutor::new(bin).with_env(env)),
        &opts,
        &CancellationToken::new(),
    )
    .await;
    for t in &r.tasks {
        let a = &t.attempts[0];
        assert!(
            a.metrics.model_calls >= 1,
            "{}: no events parsed: {a:#?}",
            t.id
        );
        assert_eq!(t.summary.passed, 1, "{}: {a:#?}", t.id);
    }
    assert_eq!(r.executor, "process");
}
