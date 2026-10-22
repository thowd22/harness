//! Verify runner with baseline/after failing-set diff.
//!
//! The verify command comes from, most specific first: the workflow step
//! (`verify:` on a step), the agent (`verify:` in agent YAML), `[verify]
//! command` in harness.ini, or auto-detection from the project's build
//! files ([`VerifySpec::resolve`]).
//!
//! [`VerifyGate`] runs it twice: optionally once before the first edit
//! (the **baseline**, `[verify] baseline = true`) and again when the agent
//! says it is done. Each run's exit status is recorded and, when the output
//! contains test results, the set of failing tests is extracted
//! ([`parse_test_output`]). Structured formats are preferred (libtest /
//! nextest JSON lines, `go test -json`); plain libtest and pytest output are
//! understood too. [`compare`] then separates **newly failing** tests from
//! **pre-existing** failures and fixed ones.
//!
//! Every run is recorded as `verify_started` / `verify_finished` events on
//! the node.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use harness_events::schema::{EventKind, NodeId, Payload};
use harness_tools::exec::{Exec, ExecError, ExecRequest};
use serde::Serialize;

use crate::events::{EventSink, NoEvents, draft};

/// Default verify timeout.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30 * 60);
/// Output kept per run (the tail is kept when longer).
pub const MAX_OUTPUT_BYTES: usize = 4 * 1024 * 1024;

/// Where the verify command came from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", content = "name", rename_all = "snake_case")]
pub enum VerifySource {
    /// A workflow step, by id.
    WorkflowStep(String),
    /// An agent, by name.
    Agent(String),
    /// `[verify] command` in harness.ini.
    Ini,
    /// Detected from this build file.
    Detected(String),
}

/// A resolved verify command.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct VerifySpec {
    /// The shell command line.
    pub command: String,
    /// Where it came from.
    pub source: VerifySource,
}

/// Build files and the test command they imply, in detection order.
pub const DETECTORS: &[(&str, &str)] = &[
    ("Cargo.toml", "cargo test --workspace"),
    ("go.mod", "go test ./..."),
    ("package.json", "npm test"),
    ("pyproject.toml", "pytest"),
    ("setup.py", "pytest"),
    ("Makefile", "make test"),
];

impl VerifySpec {
    /// Resolve the command: step, then agent, then `[verify]`, then
    /// detection in `root`. Blank commands count as unset.
    pub fn resolve(
        step: Option<(&str, &str)>,
        agent: Option<(&str, &str)>,
        ini: &harness_config::ini::config::Verify,
        root: &Path,
    ) -> Option<VerifySpec> {
        let set = |c: &str| !c.trim().is_empty();
        if let Some((id, c)) = step.filter(|(_, c)| set(c)) {
            return Some(Self::new(c, VerifySource::WorkflowStep(id.into())));
        }
        if let Some((name, c)) = agent.filter(|(_, c)| set(c)) {
            return Some(Self::new(c, VerifySource::Agent(name.into())));
        }
        if let Some(c) = ini.command.as_deref().filter(|c| set(c)) {
            return Some(Self::new(c, VerifySource::Ini));
        }
        detect_command(root).map(|(file, cmd)| Self::new(cmd, VerifySource::Detected(file.into())))
    }

    fn new(command: &str, source: VerifySource) -> Self {
        Self {
            command: command.trim().to_string(),
            source,
        }
    }
}

/// The test command implied by the first build file found in `root`.
pub fn detect_command(root: &Path) -> Option<(&'static str, &'static str)> {
    DETECTORS
        .iter()
        .copied()
        .find(|(file, _)| root.join(file).is_file())
}

/// The output format test results were read from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TestFormat {
    /// libtest / nextest JSON lines (`{"type":"test","event":"failed"}`).
    LibtestJson,
    /// `go test -json`.
    GoJson,
    /// Plain libtest (`test a::b ... FAILED`).
    Libtest,
    /// pytest summary (`FAILED tests/x.py::t`).
    Pytest,
}

impl TestFormat {
    /// True for machine-readable formats.
    pub fn is_structured(self) -> bool {
        matches!(self, Self::LibtestJson | Self::GoJson)
    }
}

/// Test results found in a run's output.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize)]
pub struct TestReport {
    /// Names of failing tests.
    pub failing: BTreeSet<String>,
    /// Number of passing tests seen.
    pub passed: usize,
    /// Formats recognised.
    pub formats: Vec<TestFormat>,
}

/// Extract test results from `output`, or `None` if it contains none.
pub fn parse_test_output(output: &str) -> Option<TestReport> {
    let structured = parse_json_lines(output);
    if !structured.formats.is_empty() {
        return Some(structured);
    }
    let mut r = TestReport::default();
    let mut libtest = false;
    let mut pytest = false;
    for line in output.lines() {
        let l = line.trim_end();
        if let Some(rest) = l.strip_prefix("test ") {
            if let Some(name) = rest.strip_suffix(" ... FAILED") {
                r.failing.insert(name.trim().to_string());
                libtest = true;
            } else if rest.ends_with(" ... ok") {
                r.passed += 1;
                libtest = true;
            }
        } else if l.starts_with("test result: ") {
            libtest = true;
        } else if let Some(rest) = l
            .strip_prefix("FAILED ")
            .or_else(|| l.strip_prefix("ERROR "))
            && rest.contains("::")
        {
            let name = rest.split(" - ").next().unwrap_or(rest).trim();
            r.failing.insert(name.to_string());
            pytest = true;
        } else if l.starts_with('=') && l.contains(" passed") && l.contains(" in ") {
            pytest = true;
            if let Some(n) = count_before(l, " passed") {
                r.passed = r.passed.max(n);
            }
        }
    }
    if libtest {
        r.formats.push(TestFormat::Libtest);
    }
    if pytest {
        r.formats.push(TestFormat::Pytest);
    }
    (!r.formats.is_empty()).then_some(r)
}

fn count_before(line: &str, word: &str) -> Option<usize> {
    let idx = line.find(word)?;
    line[..idx].split_whitespace().last()?.parse().ok()
}

fn parse_json_lines(output: &str) -> TestReport {
    let mut r = TestReport::default();
    for line in output.lines() {
        let l = line.trim();
        if !l.starts_with('{') {
            continue;
        }
        let Ok(v) = serde_json::from_str::<serde_json::Value>(l) else {
            continue;
        };
        let s = |k: &str| v.get(k).and_then(|x| x.as_str());
        if s("type") == Some("test")
            && let (Some(event), Some(name)) = (s("event"), s("name"))
        {
            match event {
                "failed" | "timeout" => {
                    r.failing.insert(name.to_string());
                }
                "ok" => r.passed += 1,
                _ => {}
            }
            if !r.formats.contains(&TestFormat::LibtestJson) {
                r.formats.push(TestFormat::LibtestJson);
            }
        } else if let (Some(action), Some(test)) = (s("Action"), s("Test")) {
            let name = match s("Package") {
                Some(p) => format!("{p}/{test}"),
                None => test.to_string(),
            };
            match action {
                "fail" => {
                    r.failing.insert(name);
                }
                "pass" => r.passed += 1,
                _ => {}
            }
            if !r.formats.contains(&TestFormat::GoJson) {
                r.formats.push(TestFormat::GoJson);
            }
        }
    }
    r
}

/// One verify run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct VerifyRun {
    /// The command line.
    pub command: String,
    /// Exit code, when the process exited.
    pub exit_code: Option<i32>,
    /// True when the command exited 0 within the timeout.
    pub passed: bool,
    /// True when the timeout fired.
    pub timed_out: bool,
    /// Wall time.
    pub duration: Duration,
    /// Combined stdout and stderr (tail kept when very long).
    pub output: String,
    /// Test results, when the output contained any.
    pub tests: Option<TestReport>,
}

impl VerifyRun {
    /// The failing tests, when known.
    pub fn failing(&self) -> Option<&BTreeSet<String>> {
        self.tests.as_ref().map(|t| &t.failing)
    }
}

/// The overall result of comparing a run against the baseline.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum VerifyStatus {
    /// The command passed.
    Passed,
    /// It failed, but every failing test was already failing in the
    /// baseline.
    PreExistingOnly,
    /// Tests fail that passed (or did not fail) in the baseline.
    NewFailures,
    /// It failed and the failures cannot be compared (no baseline, or no
    /// test results in the output).
    Failed,
}

/// Newly failing vs pre-existing failures.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Comparison {
    /// Overall status.
    pub status: VerifyStatus,
    /// Failing now, not failing in the baseline.
    pub newly_failing: BTreeSet<String>,
    /// Failing now and in the baseline.
    pub pre_existing: BTreeSet<String>,
    /// Failing in the baseline, not failing now.
    pub fixed: BTreeSet<String>,
    /// Every test failing now (when known), whether or not it could be
    /// compared.
    pub failing: BTreeSet<String>,
    /// True if a baseline existed.
    pub had_baseline: bool,
    /// True if the baseline run itself failed.
    pub baseline_failed: bool,
}

/// Compare `after` against `baseline`.
pub fn compare(baseline: Option<&VerifyRun>, after: &VerifyRun) -> Comparison {
    let empty = BTreeSet::new();
    let base_failing = baseline.and_then(VerifyRun::failing);
    let after_failing = after.failing();
    let fixed: BTreeSet<String> = match (base_failing, after_failing) {
        (Some(b), Some(a)) => b.difference(a).cloned().collect(),
        (Some(b), None) if after.passed => b.clone(),
        _ => BTreeSet::new(),
    };
    let mut c = Comparison {
        status: VerifyStatus::Passed,
        newly_failing: BTreeSet::new(),
        pre_existing: BTreeSet::new(),
        fixed,
        failing: after_failing.cloned().unwrap_or_default(),
        had_baseline: baseline.is_some(),
        baseline_failed: baseline.is_some_and(|b| !b.passed),
    };
    if after.passed {
        return c;
    }
    match (baseline, after_failing) {
        (Some(b), Some(a)) if !a.is_empty() && (b.tests.is_some() || b.passed) => {
            let b = b.failing().unwrap_or(&empty);
            c.newly_failing = a.difference(b).cloned().collect();
            c.pre_existing = a.intersection(b).cloned().collect();
            c.status = if c.newly_failing.is_empty() {
                VerifyStatus::PreExistingOnly
            } else {
                VerifyStatus::NewFailures
            };
        }
        _ => c.status = VerifyStatus::Failed,
    }
    c
}

impl Comparison {
    /// Human-readable summary lines (newly failing listed first).
    pub fn lines(&self) -> Vec<String> {
        let mut out = vec![match self.status {
            VerifyStatus::Passed => "verify passed".to_string(),
            VerifyStatus::PreExistingOnly => format!(
                "verify failed, but only with {} pre-existing failure(s)",
                self.pre_existing.len()
            ),
            VerifyStatus::NewFailures => format!(
                "verify failed: {} newly failing test(s), {} pre-existing",
                self.newly_failing.len(),
                self.pre_existing.len()
            ),
            VerifyStatus::Failed if !self.had_baseline => {
                "verify failed (no baseline to compare against)".to_string()
            }
            VerifyStatus::Failed => "verify failed (no test results to compare)".to_string(),
        }];
        let uncompared = if self.status == VerifyStatus::Failed {
            self.failing.clone()
        } else {
            BTreeSet::new()
        };
        for (label, set) in [
            ("newly failing", &self.newly_failing),
            ("pre-existing", &self.pre_existing),
            ("failing", &uncompared),
            ("fixed", &self.fixed),
        ] {
            for t in set {
                out.push(format!("  {label}: {t}"));
            }
        }
        out
    }
}

/// Runs verify commands through an [`Exec`].
#[derive(Clone)]
pub struct VerifyRunner {
    exec: Arc<dyn Exec>,
    cwd: PathBuf,
    timeout: Duration,
    env: Option<Vec<(String, String)>>,
    sink: Arc<dyn EventSink>,
}

impl std::fmt::Debug for VerifyRunner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VerifyRunner")
            .field("cwd", &self.cwd)
            .field("timeout", &self.timeout)
            .finish_non_exhaustive()
    }
}

impl VerifyRunner {
    /// A runner that starts commands in `cwd`.
    pub fn new(exec: Arc<dyn Exec>, cwd: impl Into<PathBuf>) -> Self {
        Self {
            exec,
            cwd: cwd.into(),
            timeout: DEFAULT_TIMEOUT,
            env: None,
            sink: Arc::new(NoEvents),
        }
    }

    /// Kill commands after `timeout`.
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Run with exactly this environment (for example the bash tool's
    /// scrubbed environment) instead of inheriting it.
    pub fn with_env(mut self, env: Vec<(String, String)>) -> Self {
        self.env = Some(env);
        self
    }

    /// Record runs as events.
    pub fn with_sink(mut self, sink: Arc<dyn EventSink>) -> Self {
        self.sink = sink;
        self
    }

    /// The sink runs are recorded to.
    pub fn sink(&self) -> &Arc<dyn EventSink> {
        &self.sink
    }

    /// Run `command` with `sh -c`, recording it on `node`. Dropping the
    /// future kills the command.
    pub async fn run(&self, node: Option<&NodeId>, command: &str) -> Result<VerifyRun, ExecError> {
        self.sink.emit(draft(
            node,
            EventKind::VerifyStarted {
                command: command.to_string(),
            },
        ));
        let mut req = ExecRequest::new(
            "sh",
            vec!["-c".into(), command.to_string()],
            self.cwd.clone(),
        )
        .with_timeout(self.timeout);
        if let Some(env) = &self.env {
            req = req.with_clean_env(env.clone());
        }
        let start = Instant::now();
        let result = async {
            let handle = self.exec.spawn(req).await?;
            let mut buf: Vec<u8> = Vec::new();
            let outcome = handle
                .drive(|chunk| {
                    buf.extend_from_slice(&chunk.data);
                    if buf.len() > 2 * MAX_OUTPUT_BYTES {
                        buf.drain(..buf.len() - MAX_OUTPUT_BYTES);
                    }
                })
                .await?;
            Ok::<_, ExecError>((outcome, buf))
        }
        .await;
        let duration = start.elapsed();
        let (outcome, buf) = match result {
            Ok(r) => r,
            Err(e) => {
                self.sink.emit(draft(
                    node,
                    EventKind::VerifyFinished {
                        command: command.to_string(),
                        passed: false,
                        exit_code: None,
                        duration_ms: duration.as_millis() as u64,
                        output: Payload::Text(e.to_string()),
                    },
                ));
                return Err(e);
            }
        };
        let start_at = buf.len().saturating_sub(MAX_OUTPUT_BYTES);
        let output = String::from_utf8_lossy(&buf[start_at..]).into_owned();
        let run = VerifyRun {
            command: command.to_string(),
            exit_code: outcome.code,
            passed: outcome.success(),
            timed_out: outcome.timed_out,
            duration,
            tests: parse_test_output(&output),
            output,
        };
        self.sink.emit(draft(
            node,
            EventKind::VerifyFinished {
                command: run.command.clone(),
                passed: run.passed,
                exit_code: run.exit_code,
                duration_ms: duration.as_millis() as u64,
                output: Payload::Text(run.output.clone()),
            },
        ));
        Ok(run)
    }
}

/// The result of [`VerifyGate::verify`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct VerifyOutcome {
    /// The run.
    pub run: VerifyRun,
    /// Against the baseline.
    pub comparison: Comparison,
}

/// Baseline-then-verify for one node.
#[derive(Debug)]
pub struct VerifyGate {
    runner: VerifyRunner,
    spec: Option<VerifySpec>,
    baseline_enabled: bool,
    baseline: Option<VerifyRun>,
    baseline_attempted: bool,
}

impl VerifyGate {
    /// A gate for `spec` (`None` = nothing to verify). `baseline` is
    /// `[verify] baseline`.
    pub fn new(runner: VerifyRunner, spec: Option<VerifySpec>, baseline: bool) -> Self {
        Self {
            runner,
            spec,
            baseline_enabled: baseline,
            baseline: None,
            baseline_attempted: false,
        }
    }

    /// The command, if any.
    pub fn spec(&self) -> Option<&VerifySpec> {
        self.spec.as_ref()
    }

    /// The baseline run, once taken.
    pub fn baseline(&self) -> Option<&VerifyRun> {
        self.baseline.as_ref()
    }

    /// Call before the first mutating tool call. Runs the baseline once
    /// when enabled; later calls do nothing.
    pub async fn before_first_edit(
        &mut self,
        node: Option<&NodeId>,
    ) -> Result<Option<&VerifyRun>, ExecError> {
        if self.baseline_attempted || !self.baseline_enabled {
            return Ok(self.baseline.as_ref());
        }
        self.baseline_attempted = true;
        let Some(spec) = &self.spec else {
            return Ok(None);
        };
        self.baseline = Some(self.runner.run(node, &spec.command).await?);
        Ok(self.baseline.as_ref())
    }

    /// Run the verify command and compare it with the baseline.
    pub async fn verify(&self, node: Option<&NodeId>) -> Result<Option<VerifyOutcome>, ExecError> {
        let Some(spec) = &self.spec else {
            return Ok(None);
        };
        let run = self.runner.run(node, &spec.command).await?;
        let comparison = compare(self.baseline.as_ref(), &run);
        Ok(Some(VerifyOutcome { run, comparison }))
    }
}

#[cfg(test)]
mod tests;
