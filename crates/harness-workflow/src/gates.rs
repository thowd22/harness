//! Approval gates: human checkpoints inside workflows.
//!
//! An `approval` step pauses its branch of the workflow. The executor
//! records `approval_requested` on the step's node (the reducer then shows
//! the node as waiting for approval) and asks an [`ApprovalGate`]:
//!
//! - [`ChannelGates`]: the TUI answers prompts from a channel. Each
//!   [`GatePrompt`] carries the message, the artifacts to review and the
//!   step's node id, so the map can highlight it and jump to it
//!   ([`waiting_gates`]).
//! - [`HeadlessGates`]: `harness run` resolves gates by flag
//!   (`--approve-all`, `--approve <step>`, `--deny <step>`). A gate with no
//!   matching flag is denied, and the reason names the flag to pass.
//!
//! A denied gate fails the step. The step's `on_failure` decides what
//! happens next.

use std::collections::BTreeSet;

use async_trait::async_trait;
use harness_events::reducer::{NodeState, RunState};
use harness_events::schema::{NodeId, NodeKind, NodeStatus};
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use crate::artifacts::ArtifactInfo;

/// One gate waiting for a decision.
#[derive(Debug, Clone, PartialEq)]
pub struct GateRequest {
    /// Approval id (as in the event log).
    pub approval_id: String,
    /// The step's node.
    pub node: NodeId,
    /// Step id.
    pub step: String,
    /// Workflow name.
    pub workflow: String,
    /// The rendered question.
    pub message: String,
    /// Artifacts to review.
    pub artifacts: Vec<ArtifactInfo>,
}

/// The answer to a gate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GateDecision {
    /// True to continue.
    pub approved: bool,
    /// Who decided (`user`, `flag`, ...).
    pub by: String,
    /// Optional reason.
    pub reason: Option<String>,
}

impl GateDecision {
    /// Approved by `by`.
    pub fn approve(by: impl Into<String>) -> Self {
        Self {
            approved: true,
            by: by.into(),
            reason: None,
        }
    }

    /// Denied by `by` because `reason`.
    pub fn deny(by: impl Into<String>, reason: impl Into<String>) -> Self {
        Self {
            approved: false,
            by: by.into(),
            reason: Some(reason.into()),
        }
    }
}

/// Decides approval gates.
#[async_trait]
pub trait ApprovalGate: Send + Sync {
    /// Waits for a decision on `req`; a cancelled wait is a denial.
    async fn decide(&self, req: GateRequest, cancel: &CancellationToken) -> GateDecision;
}

/// Headless gate resolution from command-line flags.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HeadlessGates {
    /// `--approve-all`.
    pub approve_all: bool,
    /// `--approve <step>`.
    pub approve: BTreeSet<String>,
    /// `--deny <step>` (wins over approvals).
    pub deny: BTreeSet<String>,
}

impl HeadlessGates {
    /// Gates from flag values.
    pub fn from_flags(
        approve_all: bool,
        approve: impl IntoIterator<Item = String>,
        deny: impl IntoIterator<Item = String>,
    ) -> Self {
        Self {
            approve_all,
            approve: approve.into_iter().collect(),
            deny: deny.into_iter().collect(),
        }
    }

    /// The decision for step `step` (no waiting).
    pub fn decision(&self, step: &str) -> GateDecision {
        if self.deny.contains(step) {
            GateDecision::deny("flag", format!("denied by --deny {step}"))
        } else if self.approve_all || self.approve.contains(step) {
            GateDecision::approve("flag")
        } else {
            GateDecision::deny(
                "flag",
                format!("no approval in headless mode; pass --approve {step} or --approve-all"),
            )
        }
    }
}

#[async_trait]
impl ApprovalGate for HeadlessGates {
    async fn decide(&self, req: GateRequest, _cancel: &CancellationToken) -> GateDecision {
        self.decision(&req.step)
    }
}

/// A gate prompt for an interactive front end.
#[derive(Debug)]
pub struct GatePrompt {
    /// What to ask.
    pub request: GateRequest,
    /// Where the answer goes.
    pub reply: oneshot::Sender<GateDecision>,
}

/// Gates answered over a channel (the TUI).
#[derive(Debug, Clone)]
pub struct ChannelGates {
    tx: mpsc::Sender<GatePrompt>,
}

impl ChannelGates {
    /// Gates plus the receiver the front end reads prompts from.
    pub fn new(capacity: usize) -> (Self, mpsc::Receiver<GatePrompt>) {
        let (tx, rx) = mpsc::channel(capacity.max(1));
        (Self { tx }, rx)
    }
}

#[async_trait]
impl ApprovalGate for ChannelGates {
    async fn decide(&self, req: GateRequest, cancel: &CancellationToken) -> GateDecision {
        let (reply, answer) = oneshot::channel();
        if self
            .tx
            .send(GatePrompt {
                request: req,
                reply,
            })
            .await
            .is_err()
        {
            return GateDecision::deny("harness", "no front end is listening for approvals");
        }
        tokio::select! {
            a = answer => a.unwrap_or_else(|_| GateDecision::deny("user", "the prompt was dismissed")),
            _ = cancel.cancelled() => GateDecision::deny("harness", "cancelled"),
        }
    }
}

/// Approval-step nodes currently waiting for a decision, in node order
/// (for highlighting in the map and jumping straight to them).
pub fn waiting_gates(state: &RunState) -> Vec<&NodeState> {
    state
        .nodes
        .iter()
        .filter(|n| n.kind == Some(NodeKind::Approval) && n.status == NodeStatus::WaitingApproval)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(step: &str) -> GateRequest {
        GateRequest {
            approval_id: format!("gate-{step}"),
            node: NodeId::from(format!("wf/{step}")),
            step: step.into(),
            workflow: "wf".into(),
            message: "Ship?".into(),
            artifacts: Vec::new(),
        }
    }

    #[tokio::test]
    async fn headless_flags_resolve_gates() {
        let c = CancellationToken::new();
        let g = HeadlessGates::from_flags(false, ["ship".to_owned()], ["deploy".to_owned()]);
        assert!(g.decide(req("ship"), &c).await.approved);
        let d = g.decide(req("deploy"), &c).await;
        assert!(!d.approved);
        assert_eq!(d.reason.as_deref(), Some("denied by --deny deploy"));
        let d = g.decide(req("other"), &c).await;
        assert!(d.reason.unwrap().contains("--approve other"));
        let all = HeadlessGates::from_flags(true, [], ["deploy".to_owned()]);
        assert!(all.decide(req("other"), &c).await.approved);
        assert!(!all.decide(req("deploy"), &c).await.approved, "deny wins");
    }

    #[tokio::test]
    async fn channel_gates_wait_for_the_front_end() {
        let (g, mut rx) = ChannelGates::new(4);
        let c = CancellationToken::new();
        let task = tokio::spawn({
            let g = g.clone();
            let c = c.clone();
            async move { g.decide(req("ship"), &c).await }
        });
        let p = rx.recv().await.unwrap();
        assert_eq!(p.request.node, NodeId::from("wf/ship"));
        p.reply.send(GateDecision::approve("user")).unwrap();
        assert!(task.await.unwrap().approved);

        // Cancelling while waiting denies.
        let task = tokio::spawn({
            let g = g.clone();
            let c = c.clone();
            async move { g.decide(req("x"), &c).await }
        });
        let _p = rx.recv().await.unwrap();
        c.cancel();
        assert!(!task.await.unwrap().approved);
        drop(rx);
        let d = g.decide(req("y"), &CancellationToken::new()).await;
        assert!(d.reason.unwrap().contains("no front end"));
    }
}
