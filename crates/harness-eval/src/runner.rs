//! Eval runner: runs suite tasks headless in isolated temporary workspaces,
//! in parallel, and records success, cost and hygiene together.
//!
//! Each attempt gets a fresh [`workspace::Workspace`] (fixture copy plus
//! setup), is run by an [`exec::Executor`], and is then checked. Metrics are
//! folded from the run's events ([`metrics_from_events`]), so they match
//! what the TUI and the run log show.
//!
//! [`run_suite`] returns [`Results`], which are written as JSON stamped with
//! the harness version and the effective config hash.

pub mod checks;
pub mod exec;
pub mod workspace;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use harness_agent::agent_loop::StopReason;
use harness_events::reducer::RunState;
use harness_events::schema::{EventKind, RunEvent};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;

use crate::suite::{Check, Suite, Task};
use checks::{CheckResult, HygieneFindings};
use exec::{Execution, Executor};
use workspace::Workspace;

/// Version of the results JSON format.
pub const RESULTS_SCHEMA: u32 = 1;

/// Stop code used when the run log has no final node status.
pub const STOP_UNKNOWN: &str = "unknown";

/// Options for [`run_suite`].
#[derive(Debug, Clone)]
pub struct RunOptions {
    /// Attempts run at the same time (at least 1).
    pub parallel: usize,
    /// Only tasks whose id contains this.
    pub filter: Option<String>,
    /// Overrides every task's `repeats`.
    pub repeats: Option<u32>,
    /// Keep the workspaces of failed attempts (their paths are recorded in
    /// the results).
    pub keep_failed: bool,
    /// Stamp written into the results.
    pub stamp: Stamp,
}

impl Default for RunOptions {
    fn default() -> Self {
        Self {
            parallel: 4,
            filter: None,
            repeats: None,
            keep_failed: false,
            stamp: Stamp::default(),
        }
    }
}

/// Identifies the build and configuration that produced results.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Stamp {
    /// `harness --version`.
    pub harness_version: String,
    /// `harness_config::effective::config_hash` of the effective config and
    /// loaded YAML.
    pub config_hash: String,
}

/// Token counts of an attempt.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tokens {
    /// Uncached input tokens.
    pub input: u64,
    /// Cached input tokens.
    pub cached_input: u64,
    /// Output tokens.
    pub output: u64,
    /// Reasoning tokens (part of output).
    pub reasoning: u64,
}

impl Tokens {
    /// Input + cached input + output.
    pub fn total(&self) -> u64 {
        self.input + self.cached_input + self.output
    }
}

/// Metrics folded from one attempt's events.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct EventMetrics {
    /// Stop code of the main agent (`end_turn`, `max_turns`, `budget`, ...).
    pub stop: String,
    /// Model calls made.
    pub model_calls: u32,
    /// Tool calls made.
    pub tool_calls: u32,
    /// Approvals requested.
    pub approvals_requested: u32,
    /// Tokens.
    pub tokens: Tokens,
    /// Cost in USD of the priced calls.
    pub cost_usd: f64,
    /// False when some model call had no price (cost is a lower bound).
    pub cost_known: bool,
}

/// Folds metrics from a run's events.
pub fn metrics_from_events(events: &[RunEvent]) -> EventMetrics {
    let state = RunState::from_events(events.iter());
    let mut m = EventMetrics {
        stop: STOP_UNKNOWN.into(),
        cost_known: true,
        ..Default::default()
    };
    let mut cost_events = 0u32;
    for e in events {
        match &e.kind {
            EventKind::ModelCallFinished { .. } => m.model_calls += 1,
            EventKind::ToolCall { .. } => m.tool_calls += 1,
            EventKind::ApprovalRequested { .. } => m.approvals_requested += 1,
            EventKind::Cost { usd } => {
                cost_events += 1;
                m.cost_usd += usd;
            }
            _ => {}
        }
    }
    m.cost_known = cost_events >= m.model_calls;
    m.tokens = Tokens {
        input: state.tokens.input,
        cached_input: state.tokens.cached_input,
        output: state.tokens.output,
        reasoning: state.tokens.reasoning,
    };
    if let Some(root) = state.roots().next()
        && root.status.is_terminal()
    {
        m.stop = stop_code(root.status, root.error.as_deref());
    }
    m
}

/// The stop code of a finished node: the agent loop's `<code>: detail`
/// prefix of the node error (`verify_failed` included), `end_turn` on
/// success.
pub fn stop_code(status: harness_events::schema::NodeStatus, error: Option<&str>) -> String {
    use harness_agent::agent_loop::codes;
    let prefix = error.map(|e| e.split_once(": ").map_or(e, |(c, _)| c));
    if let Some(p) = prefix
        && [
            codes::VERIFY_FAILED,
            codes::MAX_TURNS,
            codes::BUDGET,
            codes::CANCELLED,
            codes::ERROR,
        ]
        .contains(&p)
    {
        return p.to_owned();
    }
    StopReason::from_node_finished(status, error)
        .code()
        .to_owned()
}

/// One attempt of one task.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AttemptResult {
    /// Attempt number (1-based).
    pub attempt: u32,
    /// Every check passed (and the attempt ran).
    pub passed: bool,
    /// Check results.
    pub checks: Vec<CheckResult>,
    /// Folded event metrics.
    #[serde(flatten)]
    pub metrics: EventMetrics,
    /// The agent claimed to be done (`end_turn`) but checks failed.
    pub premature_stop: bool,
    /// Hygiene findings in the workspace.
    pub hygiene: HygieneFindings,
    /// Temporary files + out-of-scope changes.
    pub hygiene_violations: u32,
    /// Wall time of the agent run in milliseconds.
    pub wall_ms: u64,
    /// Exit code of the run.
    pub exit_code: Option<i32>,
    /// The attempt hit its timeout.
    pub timed_out: bool,
    /// Why the attempt could not run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Diagnostics printed by the run (stderr tail), for failed attempts.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub stderr: String,
    /// Kept workspace (failed attempts with `keep_failed`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<PathBuf>,
}

/// Aggregates over a set of attempts.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Summary {
    /// Attempts.
    pub attempts: u32,
    /// Passed attempts.
    pub passed: u32,
    /// `passed / attempts` (0 when empty).
    pub pass_rate: f64,
    /// Total cost in USD.
    pub cost_usd: f64,
    /// Mean cost per attempt.
    pub mean_cost_usd: f64,
    /// False if any attempt had unpriced calls.
    pub cost_known: bool,
    /// Total tokens.
    pub tokens: u64,
    /// Mean tokens per attempt.
    pub mean_tokens: f64,
    /// Total wall time in milliseconds.
    pub wall_ms: u64,
    /// Mean wall time per attempt.
    pub mean_wall_ms: f64,
    /// Total hygiene violations.
    pub hygiene_violations: u32,
    /// Total approvals requested.
    pub approvals_requested: u32,
    /// Attempts that stopped early (claimed done with failing checks).
    pub premature_stops: u32,
    /// Attempts that errored or timed out.
    pub errors: u32,
}

impl Summary {
    /// Aggregates `attempts`.
    pub fn of<'a>(attempts: impl IntoIterator<Item = &'a AttemptResult>) -> Summary {
        let mut s = Summary {
            cost_known: true,
            ..Default::default()
        };
        for a in attempts {
            s.attempts += 1;
            s.passed += u32::from(a.passed);
            s.cost_usd += a.metrics.cost_usd;
            s.cost_known &= a.metrics.cost_known;
            s.tokens += a.metrics.tokens.total();
            s.wall_ms += a.wall_ms;
            s.hygiene_violations += a.hygiene_violations;
            s.approvals_requested += a.metrics.approvals_requested;
            s.premature_stops += u32::from(a.premature_stop);
            s.errors += u32::from(a.error.is_some() || a.timed_out);
        }
        if s.attempts > 0 {
            let n = f64::from(s.attempts);
            s.pass_rate = f64::from(s.passed) / n;
            s.mean_cost_usd = s.cost_usd / n;
            s.mean_tokens = s.tokens as f64 / n;
            s.mean_wall_ms = s.wall_ms as f64 / n;
        }
        s
    }
}

/// Results of one task.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskResult {
    /// Task id.
    pub id: String,
    /// The problem it targets.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub problem: Option<String>,
    /// Aggregates.
    pub summary: Summary,
    /// Every attempt, in order.
    pub attempts: Vec<AttemptResult>,
}

/// Results of a suite run (the JSON written by `harness eval run`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Results {
    /// [`RESULTS_SCHEMA`].
    pub schema_version: u32,
    /// Suite name.
    pub suite: String,
    /// Build and config stamp.
    #[serde(flatten)]
    pub stamp: Stamp,
    /// Executor used (`process`, `in-process`).
    pub executor: String,
    /// Start time.
    #[serde(with = "time::serde::rfc3339")]
    pub started_at: OffsetDateTime,
    /// End time.
    #[serde(with = "time::serde::rfc3339")]
    pub finished_at: OffsetDateTime,
    /// Aggregates over every attempt.
    pub summary: Summary,
    /// Per task, in suite order.
    pub tasks: Vec<TaskResult>,
}

impl Results {
    /// Writes pretty JSON to `path` (parent directories are created).
    pub fn write(&self, path: &Path) -> std::io::Result<()> {
        if let Some(p) = path.parent() {
            std::fs::create_dir_all(p)?;
        }
        let text = serde_json::to_string_pretty(self).map_err(std::io::Error::other)?;
        std::fs::write(path, text + "\n")
    }

    /// Reads results JSON.
    pub fn load(path: &Path) -> Result<Results, String> {
        let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))
    }

    /// The task with `id`.
    pub fn task(&self, id: &str) -> Option<&TaskResult> {
        self.tasks.iter().find(|t| t.id == id)
    }

    /// Human-readable summary table.
    pub fn render(&self) -> String {
        use std::fmt::Write as _;
        let mut out = String::new();
        let _ = writeln!(
            out,
            "suite {} ({} via {}, config {})",
            self.suite,
            self.stamp.harness_version,
            self.executor,
            short(&self.stamp.config_hash)
        );
        for t in &self.tasks {
            let s = &t.summary;
            let _ = writeln!(
                out,
                "  {:<28} {}/{} passed  {}  {} tok  {:.1}s  hygiene {}  approvals {}  premature {}",
                t.id,
                s.passed,
                s.attempts,
                fmt_cost(s.cost_usd, s.cost_known),
                s.tokens,
                s.wall_ms as f64 / 1000.0,
                s.hygiene_violations,
                s.approvals_requested,
                s.premature_stops
            );
            for a in t.attempts.iter().filter(|a| !a.passed) {
                if let Some(e) = &a.error {
                    let _ = writeln!(out, "      #{}: error: {e}", a.attempt);
                }
                for c in a.checks.iter().filter(|c| !c.passed) {
                    let _ = writeln!(
                        out,
                        "      #{}: {} failed: {}",
                        a.attempt, c.check, c.detail
                    );
                }
            }
        }
        let s = &self.summary;
        let _ = writeln!(
            out,
            "total: {}/{} passed ({:.0}%), {}, {} tokens, hygiene violations {}, approvals {}, premature stops {}",
            s.passed,
            s.attempts,
            s.pass_rate * 100.0,
            fmt_cost(s.cost_usd, s.cost_known),
            s.tokens,
            s.hygiene_violations,
            s.approvals_requested,
            s.premature_stops
        );
        out
    }
}

/// `$1.2345`, with a `+` when some calls were unpriced.
pub fn fmt_cost(usd: f64, known: bool) -> String {
    if known {
        format!("${usd:.4}")
    } else {
        format!("${usd:.4}+ (partly unknown)")
    }
}

fn short(hash: &str) -> &str {
    hash.get(..12).unwrap_or(hash)
}

fn hygiene_scope(task: &Task) -> Vec<String> {
    task.checks
        .iter()
        .filter_map(|c| match c {
            Check::Hygiene { hygiene } => Some(hygiene.scope.clone()),
            _ => None,
        })
        .flatten()
        .collect()
}

async fn run_attempt(
    suite: &Suite,
    task: &Task,
    attempt: u32,
    executor: &dyn Executor,
    keep_failed: bool,
    cancel: &CancellationToken,
) -> AttemptResult {
    let resolved = suite.resolve(task);
    let failed = |error: String| AttemptResult {
        attempt,
        passed: false,
        checks: Vec::new(),
        metrics: EventMetrics {
            stop: STOP_UNKNOWN.into(),
            cost_known: true,
            ..Default::default()
        },
        premature_stop: false,
        hygiene: HygieneFindings::default(),
        hygiene_violations: 0,
        wall_ms: 0,
        exit_code: None,
        timed_out: false,
        error: Some(error),
        stderr: String::new(),
        workspace: None,
    };
    let mut ws = match Workspace::prepare(suite, task).await {
        Ok(ws) => ws,
        Err(e) => return failed(e),
    };
    let root = ws.path().to_owned();
    let before = checks::snapshot(&root);
    let exe: Execution = match executor.execute(&resolved, &root, cancel).await {
        Ok(x) => x,
        Err(e) => return failed(e),
    };
    let after = checks::snapshot(&root);
    let results = checks::run_checks(&task.checks, &root, &before, &after, &exe.events).await;
    let hygiene = checks::hygiene(&before, &after, &hygiene_scope(task));
    let metrics = metrics_from_events(&exe.events);
    let passed = !exe.timed_out && results.iter().all(|c| c.passed);
    // The agent reported success (or verification caught a false "done")
    // while the task's checks fail.
    let premature_stop = !passed
        && [
            harness_agent::agent_loop::codes::END_TURN,
            harness_agent::agent_loop::codes::VERIFY_FAILED,
        ]
        .contains(&metrics.stop.as_str());
    let workspace = (keep_failed && !passed).then(|| ws.keep());
    AttemptResult {
        attempt,
        passed,
        checks: results,
        premature_stop,
        hygiene_violations: hygiene.violations(),
        hygiene,
        metrics,
        wall_ms: exe.wall.as_millis() as u64,
        exit_code: exe.exit_code,
        timed_out: exe.timed_out,
        error: None,
        stderr: if passed { String::new() } else { exe.stderr },
        workspace,
    }
}

/// Runs every selected task of `suite` (`repeats` attempts each, up to
/// `opts.parallel` at a time) and collects the results.
pub async fn run_suite(
    suite: Arc<Suite>,
    executor: Arc<dyn Executor>,
    opts: &RunOptions,
    cancel: &CancellationToken,
) -> Results {
    let started_at = OffsetDateTime::now_utc();
    let sem = Arc::new(Semaphore::new(opts.parallel.max(1)));
    let tasks: Vec<usize> = suite
        .tasks
        .iter()
        .enumerate()
        .filter(|(_, t)| opts.filter.as_deref().is_none_or(|f| t.id.contains(f)))
        .map(|(i, _)| i)
        .collect();
    let mut set = tokio::task::JoinSet::new();
    for &ti in &tasks {
        let repeats = opts
            .repeats
            .unwrap_or_else(|| suite.resolve(&suite.tasks[ti]).repeats)
            .max(1);
        for attempt in 1..=repeats {
            let (suite, executor, sem, cancel) =
                (suite.clone(), executor.clone(), sem.clone(), cancel.clone());
            let keep = opts.keep_failed;
            set.spawn(async move {
                let _permit = sem.acquire_owned().await;
                let r = run_attempt(
                    &suite,
                    &suite.tasks[ti],
                    attempt,
                    executor.as_ref(),
                    keep,
                    &cancel,
                )
                .await;
                (ti, r)
            });
        }
    }
    let mut by_task: std::collections::BTreeMap<usize, Vec<AttemptResult>> = Default::default();
    while let Some(joined) = set.join_next().await {
        match joined {
            Ok((ti, r)) => by_task.entry(ti).or_default().push(r),
            Err(e) => tracing::error!(target: "harness::eval", "eval attempt panicked: {e}"),
        }
    }
    let mut out_tasks = Vec::new();
    for ti in tasks {
        let mut attempts = by_task.remove(&ti).unwrap_or_default();
        attempts.sort_by_key(|a| a.attempt);
        let t = &suite.tasks[ti];
        out_tasks.push(TaskResult {
            id: t.id.clone(),
            problem: t.problem.clone(),
            summary: Summary::of(&attempts),
            attempts,
        });
    }
    let summary = Summary::of(out_tasks.iter().flat_map(|t| &t.attempts));
    Results {
        schema_version: RESULTS_SCHEMA,
        suite: suite.name.clone(),
        stamp: opts.stamp.clone(),
        executor: executor.name().to_owned(),
        started_at,
        finished_at: OffsetDateTime::now_utc(),
        summary,
        tasks: out_tasks,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::suite::ResolvedTask;
    use async_trait::async_trait;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    /// Writes `out.txt` when the prompt says "messy", else `src/lib.rs`.
    struct Scripted {
        running: AtomicUsize,
        max_seen: AtomicUsize,
    }

    #[async_trait]
    impl Executor for Scripted {
        fn name(&self) -> &str {
            "scripted"
        }
        async fn execute(
            &self,
            task: &ResolvedTask,
            ws: &Path,
            _c: &CancellationToken,
        ) -> Result<Execution, String> {
            let n = self.running.fetch_add(1, Ordering::SeqCst) + 1;
            self.max_seen.fetch_max(n, Ordering::SeqCst);
            tokio::time::sleep(Duration::from_millis(30)).await;
            let prompt = task.prompt.clone().unwrap_or_default();
            std::fs::create_dir_all(ws.join("src")).unwrap();
            std::fs::write(ws.join("src/lib.rs"), "fn double() {}").unwrap();
            if prompt.contains("messy") {
                std::fs::write(ws.join("out.txt"), "x").unwrap();
            }
            self.running.fetch_sub(1, Ordering::SeqCst);
            Ok(Execution {
                exit_code: Some(0),
                events: Vec::new(),
                ..Default::default()
            })
        }
    }

    const SUITE: &str = r#"
name: demo
tasks:
  - id: clean
    prompt: do it
    repeats: 3
    checks: [ { file: { path: src/lib.rs, contains: double } }, { hygiene: { max_temp_files: 0 } } ]
  - id: messy
    problem: "problem-5"
    prompt: messy
    repeats: 2
    checks: [ { hygiene: { max_temp_files: 0 } } ]
"#;

    #[tokio::test]
    async fn runs_attempts_in_parallel_isolated_workspaces() {
        let suite = Arc::new(Suite::parse(SUITE, None, ".".into()).unwrap());
        let ex = Arc::new(Scripted {
            running: AtomicUsize::new(0),
            max_seen: AtomicUsize::new(0),
        });
        let opts = RunOptions {
            parallel: 3,
            stamp: Stamp {
                harness_version: "0.1.0".into(),
                config_hash: "abc".into(),
            },
            ..Default::default()
        };
        let r = run_suite(suite, ex.clone(), &opts, &CancellationToken::new()).await;
        assert!(ex.max_seen.load(Ordering::SeqCst) > 1, "ran in parallel");
        assert!(
            ex.max_seen.load(Ordering::SeqCst) <= 3,
            "respects the limit"
        );
        assert_eq!(r.tasks.len(), 2);
        let clean = r.task("clean").unwrap();
        assert_eq!(clean.summary.attempts, 3);
        assert_eq!(clean.summary.passed, 3);
        assert_eq!(
            clean.attempts.iter().map(|a| a.attempt).collect::<Vec<_>>(),
            [1, 2, 3]
        );
        let messy = r.task("messy").unwrap();
        assert_eq!(messy.summary.passed, 0);
        assert_eq!(messy.summary.hygiene_violations, 2);
        assert_eq!(messy.problem.as_deref(), Some("problem-5"));
        assert_eq!(r.summary.attempts, 5);
        assert!((r.summary.pass_rate - 0.6).abs() < 1e-9);
        assert_eq!(r.executor, "scripted");
        // JSON round trip with the stamp.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out/results.json");
        r.write(&path).unwrap();
        let json: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(json["config_hash"], "abc");
        assert_eq!(json["harness_version"], "0.1.0");
        assert_eq!(json["schema_version"], RESULTS_SCHEMA);
        assert_eq!(Results::load(&path).unwrap(), r);
        assert!(r.render().contains("messy"));
    }

    #[tokio::test]
    async fn filter_and_repeats_override() {
        let suite = Arc::new(Suite::parse(SUITE, None, ".".into()).unwrap());
        let ex = Arc::new(Scripted {
            running: AtomicUsize::new(0),
            max_seen: AtomicUsize::new(0),
        });
        let opts = RunOptions {
            filter: Some("cle".into()),
            repeats: Some(1),
            ..Default::default()
        };
        let r = run_suite(suite, ex, &opts, &CancellationToken::new()).await;
        assert_eq!(r.tasks.len(), 1);
        assert_eq!(r.summary.attempts, 1);
    }
}
