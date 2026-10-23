//! The search suite (TASK-152): a multi-file lookup that the built-in grep
//! tool answers in fewer model calls than the bash-only pattern recorded in
//! run 01a1691a.
//!
//! `grep_answers_a_multi_file_lookup_in_fewer_model_calls_than_bash` is a
//! regression check of the suite, not a measurement: it runs in-process with
//! the fake provider, so its model-call counts (3 vs 6) are the lengths of
//! the two hand-written scripts. It proves the task, both scripts and the
//! checks stay consistent and that the grep path passes the same checks.
//! Whether the tools and prompt make a real model use fewer calls is
//! measured only by `live_model_uses_search_tools`, which runs the task
//! against a real model when `HARNESS_EVAL_LIVE_MODEL` names one (ignored by
//! default because it spends provider credit; see docs/evals.md).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use harness_eval::runner::exec::{InProcessExecutor, ProcessExecutor};
use harness_eval::runner::{Results, RunOptions, run_suite};
use harness_eval::suite::{ProviderSpec, Suite};
use tokio_util::sync::CancellationToken;

const TASK: &str = "find-session-spawns";

fn suite_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("suites/search/suite.yaml")
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

fn with_script(mut suite: Suite, dir: &str) -> Suite {
    for t in &mut suite.tasks {
        t.provider = Some(ProviderSpec {
            fake: Some(PathBuf::from(format!("scripts/{dir}/{}.yaml", t.id))),
            model: None,
        });
    }
    suite
}

#[tokio::test]
async fn grep_answers_a_multi_file_lookup_in_fewer_model_calls_than_bash() {
    let with_grep = run(Suite::load(&suite_path()).unwrap()).await;
    let bash_only = run(with_script(
        Suite::load(&suite_path()).unwrap(),
        "bash-only",
    ))
    .await;
    let g = &with_grep.task(TASK).unwrap();
    let b = &bash_only.task(TASK).unwrap();
    assert_eq!(g.summary.passed, 1, "{:#?}", g.attempts);
    assert_eq!(b.summary.passed, 1, "{:#?}", b.attempts);
    let (g, b) = (&g.attempts[0].metrics, &b.attempts[0].metrics);
    assert_eq!((g.model_calls, g.tool_calls), (3, 2), "{g:#?}");
    assert_eq!((b.model_calls, b.tool_calls), (6, 5), "{b:#?}");
    assert!(g.model_calls < b.model_calls);
}

/// Measures a real model on the task through the `harness` binary:
/// `HARNESS_BIN=$PWD/target/debug/harness HARNESS_EVAL_LIVE_MODEL=<alias>
/// cargo test -p harness-eval --test search -- --ignored`. Credentials come
/// from the provider's API key variable or from the ini named by
/// `HARNESS_EVAL_LIVE_CONFIG`; home and data directories are temporary.
/// Passes when the checks pass and the model needed fewer model calls than
/// the bash-only replay.
#[tokio::test]
#[ignore = "needs a real model: set HARNESS_BIN and HARNESS_EVAL_LIVE_MODEL"]
async fn live_model_uses_search_tools() {
    let (Ok(bin), Ok(model)) = (
        std::env::var("HARNESS_BIN"),
        std::env::var("HARNESS_EVAL_LIVE_MODEL"),
    ) else {
        panic!("set HARNESS_BIN and HARNESS_EVAL_LIVE_MODEL");
    };
    let bin = std::fs::canonicalize(bin).unwrap();
    let bash_only = run(with_script(
        Suite::load(&suite_path()).unwrap(),
        "bash-only",
    ))
    .await;
    let baseline = bash_only.task(TASK).unwrap().attempts[0]
        .metrics
        .model_calls;
    let mut suite = Suite::load(&suite_path()).unwrap();
    for t in &mut suite.tasks {
        t.provider = Some(ProviderSpec {
            fake: None,
            model: Some(model.clone()),
        });
    }
    let home = tempfile::tempdir().unwrap();
    let ini = match std::env::var("HARNESS_EVAL_LIVE_CONFIG") {
        Ok(p) => PathBuf::from(p),
        Err(_) => {
            let p = home.path().join("harness.ini");
            std::fs::write(&p, "").unwrap();
            p
        }
    };
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
        Arc::new(ProcessExecutor::new(bin).with_env(env)),
        &RunOptions::default(),
        &CancellationToken::new(),
    )
    .await;
    let t = r.task(TASK).unwrap();
    let a = &t.attempts[0];
    eprintln!(
        "live {model}: passed {}, model calls {} (bash-only replay {baseline}), tool calls {}",
        t.summary.passed, a.metrics.model_calls, a.metrics.tool_calls
    );
    assert_eq!(t.summary.passed, 1, "{:#?}", t.attempts);
    assert!(a.metrics.model_calls < baseline, "{:#?}", a.metrics);
}
