//! Bug-fix proof (fail-before / pass-after / revert).
//!
//! The `bugfix-proof` workflow step proves that a fix is what makes a test
//! pass. It runs in four phases:
//!
//! 1. **original**: the fix is taken out → the test must **fail**;
//! 2. **fixed**: the fix is put back → the test must **pass**;
//! 3. **reverted**: the fix is taken out again → the test must **fail**
//!    (the first result was not a fluke);
//! 4. **restore**: the fix is put back for good.
//!
//! Each phase is a child node of the proof node (`<proof>.original`, ...),
//! so it shows up on the map and in node detail with its own
//! `verify_started` / `verify_finished` log.
//!
//! The worktree is swapped through a [`ProofWorktree`].
//! [`CheckpointWorktree`] implements it with shadow-git checkpoints: the
//! "original" side is a checkpoint taken before the fix (normally the fix
//! step's first turn checkpoint), the "fix" side is a snapshot taken when
//! the proof starts. Only the files that differ are swapped, and paths
//! matching the `keep` globs (the new test itself) are never taken out.
//!
//! The fix is always restored: after the last phase, when the proof is
//! cancelled through its [`CancellationToken`], when a phase errors, and
//! (through a drop guard) when the proof future is dropped or panics.

use std::sync::Arc;

use harness_events::schema::{EventKind, NodeId, NodeKind, NodeStatus};
use serde::Serialize;
use tokio_util::sync::CancellationToken;

use crate::checkpoints::{CheckpointError, CheckpointKind, CheckpointStore};
use crate::events::{EventSink, draft};
use crate::scope::{Scope, ScopeError, ScopeSource};
use crate::verify::{VerifyRun, VerifyRunner};

/// Paths kept in place by default while the fix is swapped out (the
/// reproducing test lives there).
pub const DEFAULT_KEEP: &[&str] = &["tests/**"];

/// A proof phase.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    /// Test on the code without the fix.
    Original,
    /// Test with the fix.
    Fixed,
    /// Test with the fix reverted again.
    Reverted,
    /// Put the fix back.
    Restore,
}

impl Phase {
    /// All phases, in order.
    pub const ALL: [Phase; 4] = [
        Phase::Original,
        Phase::Fixed,
        Phase::Reverted,
        Phase::Restore,
    ];

    /// Id suffix of the phase's child node.
    pub fn id(self) -> &'static str {
        match self {
            Self::Original => "original",
            Self::Fixed => "fixed",
            Self::Reverted => "reverted",
            Self::Restore => "restore",
        }
    }

    /// Title shown on the map.
    pub fn title(self) -> &'static str {
        match self {
            Self::Original => "original code (expect fail)",
            Self::Fixed => "with fix (expect pass)",
            Self::Reverted => "fix reverted (expect fail)",
            Self::Restore => "restore fix",
        }
    }

    /// Whether the test should pass in this phase (`None`: no test run).
    pub fn expect_pass(self) -> Option<bool> {
        match self {
            Self::Original | Self::Reverted => Some(false),
            Self::Fixed => Some(true),
            Self::Restore => None,
        }
    }
}

/// Proof failures (as opposed to a fix that is not proven).
#[derive(Debug, thiserror::Error)]
pub enum ProofError {
    /// The fix and the original code are identical outside the kept paths.
    #[error("there is no fix to prove: no files differ from the original checkpoint")]
    NoFix,
    /// A checkpoint operation failed.
    #[error(transparent)]
    Checkpoint(#[from] CheckpointError),
    /// An invalid keep glob.
    #[error(transparent)]
    Scope(#[from] ScopeError),
    /// The test command could not be run.
    #[error("could not run the test: {0}")]
    Exec(#[from] harness_tools::exec::ExecError),
    /// The proof was cancelled (the fix has been restored).
    #[error("bug-fix proof cancelled; the fix was restored")]
    Cancelled,
}

/// Swaps the fix in and out of the worktree.
pub trait ProofWorktree: Send + Sync {
    /// Take the fix out.
    fn apply_original(&self) -> Result<(), ProofError>;
    /// Put the fix back.
    fn apply_fix(&self) -> Result<(), ProofError>;
}

/// A [`ProofWorktree`] over shadow-git checkpoints.
#[derive(Debug)]
pub struct CheckpointWorktree {
    store: Arc<CheckpointStore>,
    original: u64,
    fix: u64,
    paths: Vec<String>,
}

impl CheckpointWorktree {
    /// Snapshot the current worktree as the fix and work out which files
    /// the fix changed since checkpoint `original`, leaving out paths that
    /// match `keep` (for example the new test file; [`DEFAULT_KEEP`] is
    /// the usual choice).
    pub fn prepare(
        store: Arc<CheckpointStore>,
        original: u64,
        keep: &[String],
    ) -> Result<Self, ProofError> {
        let keep = Scope::new(ScopeSource::Session, keep.iter().cloned())?;
        let has_keep = !keep.globs().is_empty();
        let fix = store
            .snapshot(CheckpointKind::Manual, None, None, "bug-fix proof: fix")?
            .id;
        let paths: Vec<String> = store
            .changed_since(original)?
            .into_iter()
            .map(|c| c.path)
            .filter(|p| !(has_keep && keep.contains(p)))
            .collect();
        if paths.is_empty() {
            return Err(ProofError::NoFix);
        }
        Ok(Self {
            store,
            original,
            fix,
            paths,
        })
    }

    /// The files swapped in and out.
    pub fn paths(&self) -> &[String] {
        &self.paths
    }

    /// The checkpoint holding the fix.
    pub fn fix_checkpoint(&self) -> u64 {
        self.fix
    }

    fn restore(&self, id: u64) -> Result<(), ProofError> {
        let paths: Vec<&str> = self.paths.iter().map(String::as_str).collect();
        Ok(self.store.restore_paths(id, &paths)?)
    }
}

impl ProofWorktree for CheckpointWorktree {
    fn apply_original(&self) -> Result<(), ProofError> {
        self.restore(self.original)
    }

    fn apply_fix(&self) -> Result<(), ProofError> {
        self.restore(self.fix)
    }
}

/// The result of one phase.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PhaseResult {
    /// The phase.
    pub phase: Phase,
    /// The phase's child node.
    pub node_id: NodeId,
    /// The test run (`None` for [`Phase::Restore`]).
    pub run: Option<VerifyRun>,
    /// True when the phase did what it should.
    pub ok: bool,
}

/// The result of a whole proof.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProofReport {
    /// Every phase, in order.
    pub phases: Vec<PhaseResult>,
    /// True when every phase met its expectation.
    pub proven: bool,
}

impl ProofReport {
    /// Step outputs for workflow templating (`steps.proof.outputs.proven`).
    pub fn outputs(&self) -> serde_json::Value {
        serde_json::json!({
            "proven": self.proven,
            "phases": self.phases.iter().map(|p| serde_json::json!({
                "phase": p.phase,
                "ok": p.ok,
                "passed": p.run.as_ref().map(|r| r.passed),
                "exit_code": p.run.as_ref().and_then(|r| r.exit_code),
            })).collect::<Vec<_>>(),
        })
    }
}

/// Restores the fix when dropped while armed.
struct RestoreGuard<'a> {
    worktree: &'a dyn ProofWorktree,
    armed: bool,
}

impl Drop for RestoreGuard<'_> {
    fn drop(&mut self) {
        if self.armed
            && let Err(e) = self.worktree.apply_fix()
        {
            tracing::error!(target: "harness::hygiene", "bug-fix proof could not restore the fix: {e}");
        }
    }
}

/// Runs a bug-fix proof.
pub struct BugfixProof {
    runner: VerifyRunner,
    worktree: Arc<dyn ProofWorktree>,
    test_command: String,
    node: NodeId,
}

impl std::fmt::Debug for BugfixProof {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BugfixProof")
            .field("test_command", &self.test_command)
            .field("node", &self.node)
            .finish_non_exhaustive()
    }
}

impl BugfixProof {
    /// A proof of `test_command` on the proof node `node`. Events go to
    /// the runner's sink.
    pub fn new(
        runner: VerifyRunner,
        worktree: Arc<dyn ProofWorktree>,
        test_command: impl Into<String>,
        node: NodeId,
    ) -> Self {
        Self {
            runner,
            worktree,
            test_command: test_command.into(),
            node,
        }
    }

    fn sink(&self) -> &Arc<dyn EventSink> {
        self.runner.sink()
    }

    /// The child node id of `phase`.
    pub fn phase_node(&self, phase: Phase) -> NodeId {
        NodeId::from(format!("{}.{}", self.node.as_str(), phase.id()))
    }

    fn start_phase(&self, phase: Phase) -> NodeId {
        let id = self.phase_node(phase);
        self.sink().emit(
            draft(
                Some(&id),
                EventKind::NodeStarted {
                    kind: if phase == Phase::Restore {
                        NodeKind::Shell
                    } else {
                        NodeKind::Verify
                    },
                    name: phase.title().into(),
                    agent: None,
                    model: None,
                    host: None,
                    needs: Vec::new(),
                    queued: false,
                },
            )
            .with_parent(self.node.clone()),
        );
        id
    }

    fn finish_phase(&self, id: &NodeId, status: NodeStatus, error: Option<String>) {
        self.sink()
            .emit(draft(Some(id), EventKind::NodeFinished { status, error }));
    }

    /// Run all phases. The fix is in place when this returns, whatever
    /// the outcome.
    pub async fn run(&self, cancel: &CancellationToken) -> Result<ProofReport, ProofError> {
        let mut guard = RestoreGuard {
            worktree: self.worktree.as_ref(),
            armed: true,
        };
        let result = self.run_phases(cancel).await;
        // From here on restore explicitly, so errors are reported. On
        // success the restore phase already put the fix back.
        guard.armed = false;
        if result.is_err() {
            self.worktree.apply_fix()?;
        }
        result
    }

    async fn run_phases(&self, cancel: &CancellationToken) -> Result<ProofReport, ProofError> {
        let mut phases = Vec::new();
        for phase in Phase::ALL {
            let id = self.start_phase(phase);
            let step = async {
                match phase {
                    Phase::Original | Phase::Reverted => self.worktree.apply_original()?,
                    Phase::Fixed | Phase::Restore => self.worktree.apply_fix()?,
                }
                match phase.expect_pass() {
                    Some(_) => Ok::<_, ProofError>(Some(
                        self.runner.run(Some(&id), &self.test_command).await?,
                    )),
                    None => Ok(None),
                }
            };
            let outcome = tokio::select! {
                biased;
                _ = cancel.cancelled() => Err(ProofError::Cancelled),
                r = step => r,
            };
            match outcome {
                Ok(run) => {
                    let ok = match (phase.expect_pass(), &run) {
                        (Some(want), Some(r)) => r.passed == want && !r.timed_out,
                        _ => true,
                    };
                    let error = (!ok).then(|| match phase.expect_pass() {
                        Some(true) => "the test did not pass with the fix".to_string(),
                        _ => "the test passed without the fix".to_string(),
                    });
                    let status = if ok {
                        NodeStatus::Succeeded
                    } else {
                        NodeStatus::Failed
                    };
                    self.finish_phase(&id, status, error);
                    phases.push(PhaseResult {
                        phase,
                        node_id: id,
                        run,
                        ok,
                    });
                }
                Err(e) => {
                    let status = if matches!(e, ProofError::Cancelled) {
                        NodeStatus::Cancelled
                    } else {
                        NodeStatus::Failed
                    };
                    self.finish_phase(&id, status, Some(e.to_string()));
                    return Err(e);
                }
            }
        }
        let proven = phases.iter().all(|p| p.ok);
        Ok(ProofReport { phases, proven })
    }
}

#[cfg(test)]
mod tests;
