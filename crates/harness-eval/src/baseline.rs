//! Baseline comparison and regression reports.
//!
//! A baseline is a stored [`Results`] file (for example committed as
//! `evals/baseline.json`). [`compare`] computes per-metric deltas between a
//! baseline and a new run, overall and per task, and decides whether the
//! change regresses: the run fails when the pass rate drops or the mean cost
//! rises beyond the [`Thresholds`]. The [`Comparison`] renders for the
//! terminal ([`Comparison::render`]) and serializes as JSON.

use std::fmt::Write as _;

use serde::{Deserialize, Serialize};

use crate::runner::{Results, Stamp, Summary};

/// Regression limits.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Thresholds {
    /// Largest allowed drop in pass rate, in percentage points (overall and
    /// per task). Default 0: any drop fails.
    pub max_pass_rate_drop_pts: f64,
    /// Largest allowed rise in mean cost per attempt, in percent. Default 10.
    pub max_cost_rise_pct: f64,
}

impl Default for Thresholds {
    fn default() -> Self {
        Self {
            max_pass_rate_drop_pts: 0.0,
            max_cost_rise_pct: 10.0,
        }
    }
}

/// Which way is better for a metric.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Better {
    /// Higher is better (pass rate).
    Higher,
    /// Lower is better (cost, tokens, time, violations).
    Lower,
}

/// One metric in both runs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MetricDelta {
    /// Metric name (`pass_rate`, `mean_cost_usd`, ...).
    pub metric: String,
    /// Baseline value.
    pub baseline: f64,
    /// Current value.
    pub current: f64,
    /// `current - baseline`.
    pub delta: f64,
    /// Relative change in percent (`None` when the baseline is 0).
    pub pct: Option<f64>,
    /// Which direction is better.
    pub better: Better,
    /// Whether this delta breaks a threshold.
    pub regressed: bool,
}

impl MetricDelta {
    fn new(metric: &str, baseline: f64, current: f64, better: Better) -> Self {
        let delta = current - baseline;
        MetricDelta {
            metric: metric.into(),
            baseline,
            current,
            delta,
            pct: (baseline != 0.0).then(|| delta / baseline * 100.0),
            better,
            regressed: false,
        }
    }

    /// `true` when the change goes in the worse direction.
    pub fn worse(&self) -> bool {
        match self.better {
            Better::Higher => self.delta < 0.0,
            Better::Lower => self.delta > 0.0,
        }
    }
}

/// How a task appears in the two runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    /// In both runs.
    Compared,
    /// Only in the current run.
    Added,
    /// Only in the baseline.
    Removed,
}

/// Deltas for one task.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskDelta {
    /// Task id.
    pub id: String,
    /// Presence.
    pub status: TaskStatus,
    /// Metric deltas (empty unless compared).
    pub metrics: Vec<MetricDelta>,
}

/// A baseline comparison.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Comparison {
    /// Baseline stamp.
    pub baseline: Stamp,
    /// Current stamp.
    pub current: Stamp,
    /// Whether the config hash differs.
    pub config_changed: bool,
    /// Thresholds applied.
    pub thresholds: Thresholds,
    /// Overall deltas.
    pub overall: Vec<MetricDelta>,
    /// Per-task deltas, baseline order then added tasks.
    pub tasks: Vec<TaskDelta>,
    /// Why the comparison fails (empty when it passes).
    pub failures: Vec<String>,
    /// No threshold was broken.
    pub passed: bool,
}

fn deltas(b: &Summary, c: &Summary) -> Vec<MetricDelta> {
    let per = |v: u32, s: &Summary| {
        if s.attempts == 0 {
            0.0
        } else {
            f64::from(v) / f64::from(s.attempts)
        }
    };
    vec![
        MetricDelta::new("pass_rate", b.pass_rate, c.pass_rate, Better::Higher),
        MetricDelta::new(
            "mean_cost_usd",
            b.mean_cost_usd,
            c.mean_cost_usd,
            Better::Lower,
        ),
        MetricDelta::new("mean_tokens", b.mean_tokens, c.mean_tokens, Better::Lower),
        MetricDelta::new(
            "mean_wall_ms",
            b.mean_wall_ms,
            c.mean_wall_ms,
            Better::Lower,
        ),
        MetricDelta::new(
            "hygiene_violations_per_attempt",
            per(b.hygiene_violations, b),
            per(c.hygiene_violations, c),
            Better::Lower,
        ),
        MetricDelta::new(
            "approvals_per_attempt",
            per(b.approvals_requested, b),
            per(c.approvals_requested, c),
            Better::Lower,
        ),
        MetricDelta::new(
            "premature_stops_per_attempt",
            per(b.premature_stops, b),
            per(c.premature_stops, c),
            Better::Lower,
        ),
    ]
}

/// Marks threshold breaches in `metrics` and returns their descriptions.
fn apply(scope: &str, metrics: &mut [MetricDelta], t: &Thresholds) -> Vec<String> {
    let mut out = Vec::new();
    for m in metrics.iter_mut() {
        match m.metric.as_str() {
            "pass_rate" => {
                let drop_pts = -m.delta * 100.0;
                if drop_pts > t.max_pass_rate_drop_pts + 1e-9 {
                    m.regressed = true;
                    out.push(format!(
                        "{scope}: pass rate dropped {:.0}% -> {:.0}% ({drop_pts:.1} pts, max {:.1})",
                        m.baseline * 100.0,
                        m.current * 100.0,
                        t.max_pass_rate_drop_pts
                    ));
                }
            }
            "mean_cost_usd" if scope == "overall" => {
                let breach = match m.pct {
                    Some(p) => p > t.max_cost_rise_pct + 1e-9,
                    None => m.current > 0.0,
                };
                if breach {
                    m.regressed = true;
                    out.push(format!(
                        "{scope}: mean cost rose ${:.4} -> ${:.4} ({}, max +{:.1}%)",
                        m.baseline,
                        m.current,
                        m.pct
                            .map_or_else(|| "from zero".to_owned(), |p| format!("{p:+.1}%")),
                        t.max_cost_rise_pct
                    ));
                }
            }
            _ => {}
        }
    }
    out
}

/// Compares `current` against `baseline`.
pub fn compare(baseline: &Results, current: &Results, t: &Thresholds) -> Comparison {
    let mut failures = Vec::new();
    let mut overall = deltas(&baseline.summary, &current.summary);
    failures.extend(apply("overall", &mut overall, t));
    let mut tasks = Vec::new();
    for b in &baseline.tasks {
        match current.task(&b.id) {
            Some(c) => {
                let mut metrics = deltas(&b.summary, &c.summary);
                failures.extend(apply(&format!("task {}", b.id), &mut metrics, t));
                tasks.push(TaskDelta {
                    id: b.id.clone(),
                    status: TaskStatus::Compared,
                    metrics,
                });
            }
            None => tasks.push(TaskDelta {
                id: b.id.clone(),
                status: TaskStatus::Removed,
                metrics: Vec::new(),
            }),
        }
    }
    for c in &current.tasks {
        if baseline.task(&c.id).is_none() {
            tasks.push(TaskDelta {
                id: c.id.clone(),
                status: TaskStatus::Added,
                metrics: Vec::new(),
            });
        }
    }
    Comparison {
        baseline: baseline.stamp.clone(),
        current: current.stamp.clone(),
        config_changed: baseline.stamp.config_hash != current.stamp.config_hash,
        thresholds: *t,
        overall,
        tasks,
        passed: failures.is_empty(),
        failures,
    }
}

fn fmt_value(metric: &str, v: f64) -> String {
    match metric {
        "pass_rate" => format!("{:.0}%", v * 100.0),
        "mean_cost_usd" => format!("${v:.4}"),
        "mean_wall_ms" => format!("{:.1}s", v / 1000.0),
        "mean_tokens" => format!("{v:.0}"),
        _ => format!("{v:.2}"),
    }
}

fn fmt_line(out: &mut String, indent: &str, m: &MetricDelta) {
    let arrow = if m.delta == 0.0 {
        "="
    } else if m.worse() {
        "worse"
    } else {
        "better"
    };
    let pct = m.pct.map(|p| format!(" ({p:+.1}%)")).unwrap_or_default();
    let flag = if m.regressed { "  REGRESSION" } else { "" };
    let _ = writeln!(
        out,
        "{indent}{:<32} {:>10} -> {:<10} {arrow}{pct}{flag}",
        m.metric,
        fmt_value(&m.metric, m.baseline),
        fmt_value(&m.metric, m.current),
    );
}

impl Comparison {
    /// Terminal report.
    pub fn render(&self) -> String {
        let mut out = String::new();
        let _ = writeln!(
            out,
            "baseline {} (config {}) -> current {} (config {}){}",
            self.baseline.harness_version,
            short(&self.baseline.config_hash),
            self.current.harness_version,
            short(&self.current.config_hash),
            if self.config_changed {
                "  [config changed]"
            } else {
                ""
            }
        );
        let _ = writeln!(out, "overall:");
        for m in &self.overall {
            fmt_line(&mut out, "  ", m);
        }
        for t in &self.tasks {
            match t.status {
                TaskStatus::Added => {
                    let _ = writeln!(out, "task {}: new (no baseline)", t.id);
                }
                TaskStatus::Removed => {
                    let _ = writeln!(out, "task {}: missing from the current run", t.id);
                }
                TaskStatus::Compared => {
                    let changed: Vec<&MetricDelta> =
                        t.metrics.iter().filter(|m| m.delta != 0.0).collect();
                    if changed.is_empty() {
                        continue;
                    }
                    let _ = writeln!(out, "task {}:", t.id);
                    for m in changed {
                        fmt_line(&mut out, "  ", m);
                    }
                }
            }
        }
        if self.passed {
            let _ = writeln!(out, "PASS: no regression beyond thresholds");
        } else {
            let _ = writeln!(out, "FAIL:");
            for f in &self.failures {
                let _ = writeln!(out, "  - {f}");
            }
        }
        out
    }

    /// JSON report.
    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_default()
    }
}

fn short(hash: &str) -> &str {
    hash.get(..12).unwrap_or(hash)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::{AttemptResult, EventMetrics, TaskResult};
    use time::OffsetDateTime;

    fn attempt(passed: bool, cost: f64) -> AttemptResult {
        AttemptResult {
            attempt: 1,
            passed,
            checks: vec![],
            metrics: EventMetrics {
                stop: "end_turn".into(),
                cost_usd: cost,
                cost_known: true,
                ..Default::default()
            },
            premature_stop: !passed,
            hygiene: Default::default(),
            hygiene_violations: 0,
            wall_ms: 1000,
            exit_code: Some(0),
            timed_out: false,
            error: None,
            stderr: String::new(),
            workspace: None,
        }
    }

    fn results(hash: &str, tasks: &[(&str, &[(bool, f64)])]) -> Results {
        let tasks: Vec<TaskResult> = tasks
            .iter()
            .map(|(id, atts)| {
                let attempts: Vec<AttemptResult> =
                    atts.iter().map(|(p, c)| attempt(*p, *c)).collect();
                TaskResult {
                    id: (*id).into(),
                    problem: None,
                    summary: Summary::of(&attempts),
                    attempts,
                }
            })
            .collect();
        Results {
            schema_version: 1,
            suite: "s".into(),
            stamp: Stamp {
                harness_version: "0.1.0".into(),
                config_hash: hash.into(),
            },
            executor: "x".into(),
            started_at: OffsetDateTime::UNIX_EPOCH,
            finished_at: OffsetDateTime::UNIX_EPOCH,
            summary: Summary::of(tasks.iter().flat_map(|t| &t.attempts)),
            tasks,
        }
    }

    #[test]
    fn equal_runs_pass_with_zero_deltas() {
        let b = results("h1", &[("a", &[(true, 1.0), (true, 1.0)])]);
        let c = compare(&b, &b, &Thresholds::default());
        assert!(c.passed);
        assert!(!c.config_changed);
        assert!(c.overall.iter().all(|m| m.delta == 0.0));
        assert!(c.render().contains("PASS"));
    }

    #[test]
    fn pass_rate_drop_fails_overall_and_per_task() {
        let b = results(
            "h1",
            &[("a", &[(true, 1.0), (true, 1.0)]), ("b", &[(true, 1.0)])],
        );
        let c = results(
            "h2",
            &[("a", &[(true, 1.0), (false, 1.0)]), ("b", &[(true, 1.0)])],
        );
        let cmp = compare(&b, &c, &Thresholds::default());
        assert!(!cmp.passed);
        assert!(cmp.config_changed);
        let pr = &cmp.overall[0];
        assert_eq!(pr.metric, "pass_rate");
        assert!(pr.regressed);
        assert!((pr.delta + 1.0 / 3.0).abs() < 1e-9);
        assert!(
            cmp.failures
                .iter()
                .any(|f| f.starts_with("task a: pass rate"))
        );
        assert!(!cmp.failures.iter().any(|f| f.starts_with("task b")));
        let text = cmp.render();
        assert!(text.contains("REGRESSION"));
        assert!(text.contains("[config changed]"));
        // A tolerant threshold accepts the drop.
        let lenient = Thresholds {
            max_pass_rate_drop_pts: 60.0,
            ..Default::default()
        };
        assert!(compare(&b, &c, &lenient).passed);
    }

    #[test]
    fn cost_rise_beyond_threshold_fails() {
        let b = results("h", &[("a", &[(true, 1.0)])]);
        let ok = results("h", &[("a", &[(true, 1.05)])]);
        let bad = results("h", &[("a", &[(true, 1.5)])]);
        assert!(compare(&b, &ok, &Thresholds::default()).passed);
        let cmp = compare(&b, &bad, &Thresholds::default());
        assert!(!cmp.passed);
        let m = cmp
            .overall
            .iter()
            .find(|m| m.metric == "mean_cost_usd")
            .unwrap();
        assert!(m.regressed);
        assert!((m.pct.unwrap() - 50.0).abs() < 1e-9);
        // Cheaper is never a regression.
        assert!(compare(&bad, &b, &Thresholds::default()).passed);
    }

    #[test]
    fn added_and_removed_tasks_are_reported_and_json_round_trips() {
        let b = results("h", &[("a", &[(true, 0.0)]), ("old", &[(true, 0.0)])]);
        let c = results("h", &[("a", &[(true, 0.0)]), ("new", &[(true, 0.0)])]);
        let cmp = compare(&b, &c, &Thresholds::default());
        let st: Vec<_> = cmp
            .tasks
            .iter()
            .map(|t| (t.id.as_str(), t.status))
            .collect();
        assert_eq!(
            st,
            [
                ("a", TaskStatus::Compared),
                ("old", TaskStatus::Removed),
                ("new", TaskStatus::Added)
            ]
        );
        let text = cmp.render();
        assert!(text.contains("task new: new"));
        assert!(text.contains("task old: missing"));
        let back: Comparison = serde_json::from_str(&cmp.to_json()).unwrap();
        assert_eq!(back, cmp);
    }
}
