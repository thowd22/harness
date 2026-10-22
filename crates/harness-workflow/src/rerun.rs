//! Persisted workflow state, resume and re-run.
//!
//! The executor writes [`WorkflowState`] to `<run dir>/workflow.json`
//! (atomically) after every step change. A crashed or cancelled workflow
//! resumes from it, and a finished one can be re-run from a failed or
//! selected step. Either way a **new run** is created:
//!
//! - [`plan_resume`] reuses every step that succeeded or was skipped and
//!   runs the rest again.
//! - [`plan_rerun`] reuses everything except the selected steps and their
//!   downstream steps. Upstream outputs and artifacts are copied into the
//!   new run, so nothing successful is paid for twice.
//!
//! The new run's state records its [`Origin`] (previous run, kind, start
//! step). [`lineage`] walks a run's ancestry and [`reruns_of`] lists the
//! runs derived from one, for run history.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use harness_events::schema::{NodeId, RunId};
use harness_events::store::{list_runs, run_dir};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::schema::Workflow;

/// State file name inside the run directory.
pub const STATE_FILE: &str = "workflow.json";
/// Current state format version.
pub const STATE_VERSION: u32 = 1;

/// Status of a step (and the text templates see in `steps.<id>.status`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepStatus {
    /// Not started.
    #[default]
    Pending,
    /// Running.
    Running,
    /// Finished successfully.
    Succeeded,
    /// Failed.
    Failed,
    /// Not run (`if` false, upstream failed, workflow aborted).
    Skipped,
    /// Cancelled.
    Cancelled,
}

impl StepStatus {
    /// snake_case name.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Running => "running",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Skipped => "skipped",
            Self::Cancelled => "cancelled",
        }
    }

    /// True once the step will not change any more.
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Succeeded | Self::Failed | Self::Skipped | Self::Cancelled
        )
    }
}

/// Overall workflow status.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowStatus {
    /// Still running (or crashed while running).
    #[default]
    Running,
    /// Every step succeeded or was skipped.
    Succeeded,
    /// A step failed (or a budget stopped it).
    Failed,
    /// Cancelled.
    Cancelled,
}

/// The persisted record of one step.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct StepRecord {
    /// Status.
    pub status: StepStatus,
    /// The step's node.
    pub node: Option<NodeId>,
    /// Outputs.
    #[serde(default)]
    pub outputs: serde_json::Map<String, Value>,
    /// Artifact name to path.
    #[serde(default)]
    pub artifacts: BTreeMap<String, PathBuf>,
    /// Attempts made (retries + 1).
    #[serde(default)]
    pub attempts: u32,
    /// Loop iterations run.
    #[serde(default)]
    pub iterations: u32,
    /// Failure reason.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Known spend in USD.
    #[serde(default)]
    pub cost_usd: f64,
    /// Tokens used.
    #[serde(default)]
    pub tokens: u64,
    /// Checkpoint taken before the step changed files.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkpoint: Option<u64>,
    /// Run this result was reused from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reused_from: Option<RunId>,
}

/// How a run relates to an earlier one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OriginKind {
    /// Continue an unfinished run.
    Resume,
    /// Re-run from selected steps.
    Rerun,
}

/// The earlier run a run was derived from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Origin {
    /// The earlier run.
    pub run: RunId,
    /// Resume or re-run.
    pub kind: OriginKind,
    /// Steps the re-run started from.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub from_steps: Vec<String>,
}

/// Spend so far against the workflow budget.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct BudgetState {
    /// Known USD spent.
    pub spent_usd: f64,
    /// Tokens used.
    pub tokens: u64,
    /// Limit, if any.
    pub limit_usd: Option<f64>,
    /// Token limit, if any.
    pub limit_tokens: Option<u64>,
    /// True once the soft warning fired.
    pub warned: bool,
    /// True once the limit was reached.
    pub exceeded: bool,
}

impl BudgetState {
    /// A short badge for map nodes: `$1.20/$5.00 24%`, `!` appended once
    /// the warning fired, `exceeded` once the limit was hit. `None`
    /// without a limit.
    pub fn badge(&self) -> Option<String> {
        let mut parts = Vec::new();
        if let Some(l) = self.limit_usd {
            let pct = if l > 0.0 {
                self.spent_usd / l * 100.0
            } else {
                100.0
            };
            parts.push(format!("${:.2}/${l:.2} {pct:.0}%", self.spent_usd));
        }
        if let Some(l) = self.limit_tokens {
            let pct = if l > 0 {
                self.tokens as f64 / l as f64 * 100.0
            } else {
                100.0
            };
            parts.push(format!("{}/{l} tok {pct:.0}%", self.tokens));
        }
        if parts.is_empty() {
            return None;
        }
        let mut s = parts.join(" ");
        if self.exceeded {
            s.push_str(" exceeded");
        } else if self.warned {
            s.push_str(" !");
        }
        Some(s)
    }
}

/// Everything needed to resume a workflow.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct WorkflowState {
    /// Format version.
    pub version: u32,
    /// Workflow name.
    pub workflow: String,
    /// This run.
    pub run_id: Option<RunId>,
    /// The workflow node.
    pub node: Option<NodeId>,
    /// Resolved inputs.
    pub inputs: serde_json::Map<String, Value>,
    /// The run this one was derived from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<Origin>,
    /// Overall status.
    pub status: WorkflowStatus,
    /// Why it failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Per step id.
    pub steps: BTreeMap<String, StepRecord>,
    /// Budget.
    #[serde(default)]
    pub budget: BudgetState,
}

impl WorkflowState {
    /// Writes `<run_dir>/workflow.json` atomically.
    pub fn save(&self, run_dir: &Path) -> io::Result<()> {
        fs::create_dir_all(run_dir)?;
        let path = run_dir.join(STATE_FILE);
        let tmp = run_dir.join(format!(".{STATE_FILE}.tmp"));
        let data = serde_json::to_vec_pretty(self).map_err(io::Error::other)?;
        {
            let mut f = fs::File::create(&tmp)?;
            f.write_all(&data)?;
            f.sync_all()?;
        }
        fs::rename(tmp, path)
    }

    /// Reads `<run_dir>/workflow.json`.
    pub fn load(run_dir: &Path) -> io::Result<Self> {
        let data = fs::read(run_dir.join(STATE_FILE))?;
        let s: Self = serde_json::from_slice(&data)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        if s.version > STATE_VERSION {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "workflow state version {} is newer than this harness",
                    s.version
                ),
            ));
        }
        Ok(s)
    }

    /// Steps that failed, in workflow order.
    pub fn failed_steps(&self, wf: &Workflow) -> Vec<String> {
        wf.topo_order()
            .filter(|s| {
                self.steps
                    .get(&s.id)
                    .is_some_and(|r| matches!(r.status, StepStatus::Failed | StepStatus::Cancelled))
            })
            .map(|s| s.id.clone())
            .collect()
    }
}

/// Why a resume or re-run cannot be planned.
#[derive(Debug, thiserror::Error)]
pub enum RerunError {
    /// The state file could not be read.
    #[error("cannot read workflow state: {0}")]
    Io(#[from] io::Error),
    /// The run belongs to another workflow.
    #[error("run is workflow `{found}`, not `{expected}`")]
    WrongWorkflow {
        /// Workflow in the state.
        found: String,
        /// Workflow requested.
        expected: String,
    },
    /// A selected step does not exist.
    #[error("unknown step `{0}`")]
    UnknownStep(String),
    /// Nothing to re-run.
    #[error("no failed steps to re-run from; select a step instead")]
    NothingFailed,
    /// An upstream step did not succeed in the earlier run.
    #[error(
        "step `{0}` did not succeed in the earlier run, so it cannot be reused; re-run from it instead"
    )]
    NotReusable(String),
}

/// What a new run takes over from an earlier one.
#[derive(Debug, Clone, PartialEq)]
pub struct ReusePlan {
    /// The link recorded in the new run.
    pub origin: Origin,
    /// The earlier state.
    pub previous: WorkflowState,
    /// The earlier run directory (artifacts are copied from it).
    pub previous_dir: PathBuf,
    /// Steps taken over unchanged.
    pub reuse: BTreeSet<String>,
}

fn prev_run_id(state: &WorkflowState) -> RunId {
    state.run_id.unwrap_or_else(|| RunId(Default::default()))
}

/// Resume: reuse every step that succeeded or was skipped.
pub fn plan_resume(previous_dir: &Path, wf: &Workflow) -> Result<ReusePlan, RerunError> {
    let previous = WorkflowState::load(previous_dir)?;
    check_workflow(&previous, wf)?;
    let reuse =
        wf.steps
            .iter()
            .filter(|s| {
                previous.steps.get(&s.id).is_some_and(|r| {
                    matches!(r.status, StepStatus::Succeeded | StepStatus::Skipped)
                })
            })
            .map(|s| s.id.clone())
            .collect();
    Ok(ReusePlan {
        origin: Origin {
            run: prev_run_id(&previous),
            kind: OriginKind::Resume,
            from_steps: Vec::new(),
        },
        previous,
        previous_dir: previous_dir.to_path_buf(),
        reuse,
    })
}

fn check_workflow(previous: &WorkflowState, wf: &Workflow) -> Result<(), RerunError> {
    if previous.workflow != wf.name {
        return Err(RerunError::WrongWorkflow {
            found: previous.workflow.clone(),
            expected: wf.name.clone(),
        });
    }
    Ok(())
}

/// Re-run from `from` (empty = from the failed steps): those steps and
/// everything downstream run again; everything else is reused and must
/// have succeeded (or been skipped) in the earlier run.
pub fn plan_rerun(
    previous_dir: &Path,
    wf: &Workflow,
    from: &[String],
) -> Result<ReusePlan, RerunError> {
    let previous = WorkflowState::load(previous_dir)?;
    check_workflow(&previous, wf)?;
    let from: Vec<String> = if from.is_empty() {
        let failed = previous.failed_steps(wf);
        if failed.is_empty() {
            return Err(RerunError::NothingFailed);
        }
        failed
    } else {
        from.to_vec()
    };
    let mut again = BTreeSet::new();
    for f in &from {
        if wf.step(f).is_none() {
            return Err(RerunError::UnknownStep(f.clone()));
        }
        again.insert(f.clone());
        again.extend(wf.downstream(f));
    }
    let mut reuse = BTreeSet::new();
    for s in &wf.steps {
        if again.contains(&s.id) {
            continue;
        }
        let ok = previous
            .steps
            .get(&s.id)
            .is_some_and(|r| matches!(r.status, StepStatus::Succeeded | StepStatus::Skipped));
        if !ok {
            // Only upstream steps of the re-run must be reusable.
            if again.iter().any(|a| wf.upstream(a).contains(&s.id)) {
                return Err(RerunError::NotReusable(s.id.clone()));
            }
            continue;
        }
        reuse.insert(s.id.clone());
    }
    Ok(ReusePlan {
        origin: Origin {
            run: prev_run_id(&previous),
            kind: OriginKind::Rerun,
            from_steps: from,
        },
        previous,
        previous_dir: previous_dir.to_path_buf(),
        reuse,
    })
}

/// The run and its ancestors, newest first: `[(run, origin)]`.
pub fn lineage(data_dir: &Path, run: RunId) -> Vec<(RunId, Option<Origin>)> {
    let mut out = Vec::new();
    let mut seen = BTreeSet::new();
    let mut cur = Some(run);
    while let Some(r) = cur {
        if !seen.insert(r.0) {
            break;
        }
        let origin = WorkflowState::load(&run_dir(data_dir, r))
            .ok()
            .and_then(|s| s.origin);
        cur = origin.as_ref().map(|o| o.run);
        out.push((r, origin));
    }
    out
}

/// Runs whose origin is `run` (its resumes and re-runs).
pub fn reruns_of(data_dir: &Path, run: RunId) -> Vec<(RunId, Origin)> {
    let Ok(runs) = list_runs(data_dir) else {
        return Vec::new();
    };
    runs.into_iter()
        .filter_map(|r| {
            let o = WorkflowState::load(&run_dir(data_dir, r)).ok()?.origin?;
            (o.run == run).then_some((r, o))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::parse;

    const WF: &str = "name: w\nsteps:\n  - id: a\n    run: x\n  - id: b\n    needs: [a]\n    run: x\n  - id: c\n    needs: [b]\n    run: x\n  - id: d\n    run: x\n";

    fn state(statuses: &[(&str, StepStatus)]) -> WorkflowState {
        WorkflowState {
            version: STATE_VERSION,
            workflow: "w".into(),
            run_id: Some(RunId::new()),
            steps: statuses
                .iter()
                .map(|(s, st)| {
                    (
                        (*s).to_owned(),
                        StepRecord {
                            status: *st,
                            ..StepRecord::default()
                        },
                    )
                })
                .collect(),
            ..WorkflowState::default()
        }
    }

    #[test]
    fn budget_badge() {
        let mut b = BudgetState {
            spent_usd: 1.2,
            limit_usd: Some(5.0),
            ..BudgetState::default()
        };
        assert_eq!(b.badge().as_deref(), Some("$1.20/$5.00 24%"));
        b.warned = true;
        b.tokens = 10;
        b.limit_tokens = Some(100);
        assert_eq!(
            b.badge().as_deref(),
            Some("$1.20/$5.00 24% 10/100 tok 10% !")
        );
        b.exceeded = true;
        assert!(b.badge().unwrap().ends_with("exceeded"));
        assert_eq!(BudgetState::default().badge(), None);
    }

    #[test]
    fn state_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let s = state(&[("a", StepStatus::Succeeded)]);
        s.save(dir.path()).unwrap();
        assert_eq!(WorkflowState::load(dir.path()).unwrap(), s);
        let mut newer = s.clone();
        newer.version = 99;
        newer.save(dir.path()).unwrap();
        assert!(WorkflowState::load(dir.path()).is_err());
    }

    #[test]
    fn resume_reuses_finished_steps() {
        let wf = parse(WF, None).unwrap();
        let dir = tempfile::tempdir().unwrap();
        state(&[
            ("a", StepStatus::Succeeded),
            ("b", StepStatus::Failed),
            ("c", StepStatus::Pending),
            ("d", StepStatus::Skipped),
        ])
        .save(dir.path())
        .unwrap();
        let p = plan_resume(dir.path(), &wf).unwrap();
        assert_eq!(p.reuse, ["a", "d"].map(String::from).into());
        assert_eq!(p.origin.kind, OriginKind::Resume);
    }

    #[test]
    fn rerun_from_selected_or_failed_steps() {
        let wf = parse(WF, None).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let s = state(&[
            ("a", StepStatus::Succeeded),
            ("b", StepStatus::Succeeded),
            ("c", StepStatus::Failed),
            ("d", StepStatus::Succeeded),
        ]);
        s.save(dir.path()).unwrap();
        let p = plan_rerun(dir.path(), &wf, &["b".into()]).unwrap();
        assert_eq!(p.reuse, ["a", "d"].map(String::from).into());
        assert_eq!(p.origin.from_steps, ["b"]);
        assert_eq!(p.origin.run, s.run_id.unwrap());
        let p = plan_rerun(dir.path(), &wf, &[]).unwrap();
        assert_eq!(p.origin.from_steps, ["c"]);
        assert_eq!(p.reuse, ["a", "b", "d"].map(String::from).into());
        assert!(matches!(
            plan_rerun(dir.path(), &wf, &["zz".into()]),
            Err(RerunError::UnknownStep(_))
        ));
        // A failed upstream step cannot be reused.
        state(&[("a", StepStatus::Failed), ("b", StepStatus::Succeeded)])
            .save(dir.path())
            .unwrap();
        assert!(matches!(
            plan_rerun(dir.path(), &wf, &["b".into()]),
            Err(RerunError::NotReusable(s)) if s == "a"
        ));
        state(&[("a", StepStatus::Succeeded)])
            .save(dir.path())
            .unwrap();
        assert!(matches!(
            plan_rerun(dir.path(), &wf, &[]),
            Err(RerunError::NothingFailed)
        ));
    }

    #[test]
    fn lineage_and_reruns_follow_origins() {
        let data = tempfile::tempdir().unwrap();
        let first = RunId::new();
        let second = RunId::new();
        let mut s1 = state(&[]);
        s1.run_id = Some(first);
        let mut s2 = state(&[]);
        s2.run_id = Some(second);
        s2.origin = Some(Origin {
            run: first,
            kind: OriginKind::Rerun,
            from_steps: vec!["b".into()],
        });
        for (r, s) in [(first, &s1), (second, &s2)] {
            let d = run_dir(data.path(), r);
            fs::create_dir_all(&d).unwrap();
            // list_runs needs an events file.
            fs::write(d.join(harness_events::store::EVENTS_FILE), "").unwrap();
            s.save(&d).unwrap();
        }
        let l = lineage(data.path(), second);
        assert_eq!(
            l.iter().map(|(r, _)| *r).collect::<Vec<_>>(),
            [second, first]
        );
        let r = reruns_of(data.path(), first);
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].0, second);
        assert!(reruns_of(data.path(), second).is_empty());
    }
}
