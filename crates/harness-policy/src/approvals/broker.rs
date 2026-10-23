//! Batched approval prompts.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use harness_events::schema::{ApprovalDecision, EventKind, NodeId, Payload, RunEvent};
use harness_events::store::{EventDraft, RunRecorder};
use serde::Serialize;
use tokio::sync::{mpsc, oneshot};

use super::grants::SessionGrants;
use super::ini_edit::IniRuleProposal;
use crate::rules::{Action, Verdict};

/// How long an approval lasts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "scope", rename_all = "snake_case")]
pub enum ApprovalScope {
    /// This call only.
    Once,
    /// Every matching command (or path, for edits) for the rest of the session.
    Session {
        /// Command pattern (`cargo test*`) or path glob (`src/**`).
        pattern: String,
    },
    /// Session-wide now, and proposed as a harness.ini rule that is written
    /// only after the user confirms it.
    Always {
        /// Command pattern.
        pattern: String,
    },
}

/// The user's answer to one request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApprovalAnswer {
    /// Approve with a scope.
    Approve(ApprovalScope),
    /// Refuse, optionally telling the model why.
    Deny {
        /// Reason passed back to the model.
        reason: Option<String>,
    },
}

/// A tool call whose verdict was `ask`.
#[derive(Debug, Clone)]
pub struct PendingApproval {
    /// Tool call id.
    pub call_id: String,
    /// Node (agent) that made the call; approval events attach to it.
    pub node_id: Option<NodeId>,
    /// The action.
    pub action: Action,
    /// The verdict that asked.
    pub verdict: Verdict,
    /// Preview to show (diff, full command, ...).
    pub preview: Option<String>,
}

/// One request inside a prompt.
#[derive(Debug, Clone, Serialize)]
pub struct ApprovalRequest {
    /// Unique id (`approval-<batch>-<n>`).
    pub approval_id: String,
    /// Tool call id.
    pub call_id: String,
    /// Node that made the call.
    #[serde(skip)]
    pub node_id: Option<NodeId>,
    /// The action.
    pub action: Action,
    /// Why it asks.
    pub verdict: Verdict,
    /// Preview text.
    pub preview: Option<String>,
    /// Pattern offered for "this session" / "always".
    pub suggested_pattern: Option<String>,
}

/// One prompt shown to the user: every request of a batch of parallel calls.
#[derive(Debug, Clone, Serialize)]
pub struct ApprovalPrompt {
    /// Batch number (1-based, per broker).
    pub batch: u64,
    /// The requests, in call order.
    pub requests: Vec<ApprovalRequest>,
}

/// The outcome of one request.
#[derive(Debug, Clone, Serialize)]
pub struct ApprovalResolution {
    /// Approval id.
    pub approval_id: String,
    /// Tool call id.
    pub call_id: String,
    /// True if the call may run.
    pub approved: bool,
    /// Scope, when approved.
    pub scope: Option<ApprovalScope>,
    /// Reason for a denial.
    pub reason: Option<String>,
    /// Who decided (`user`, or `harness` when no answer came back).
    pub by: String,
    /// For "always": the harness.ini rule awaiting confirmation.
    pub ini_rule: Option<IniRuleProposal>,
}

impl ApprovalResolution {
    /// The event-log decision.
    pub fn decision(&self) -> ApprovalDecision {
        match (&self.approved, &self.scope) {
            (false, _) => ApprovalDecision::Denied,
            (true, Some(ApprovalScope::Always { .. })) => ApprovalDecision::ApprovedAlways,
            (true, _) => ApprovalDecision::Approved,
        }
    }
}

/// Shows prompts to the user.
#[async_trait]
pub trait ApprovalUi: Send + Sync {
    /// Show one prompt and return an answer per approval id. Missing answers
    /// count as denials.
    async fn ask(&self, prompt: ApprovalPrompt) -> Vec<(String, ApprovalAnswer)>;
}

/// A prompt plus the channel to answer it on.
#[derive(Debug)]
pub struct PromptEnvelope {
    /// The prompt.
    pub prompt: ApprovalPrompt,
    /// Send the answers here.
    pub reply: oneshot::Sender<Vec<(String, ApprovalAnswer)>>,
}

/// An [`ApprovalUi`] that forwards prompts over a channel (to the TUI).
/// If the receiver is gone or drops the reply, every request is denied.
#[derive(Debug, Clone)]
pub struct ChannelUi {
    tx: mpsc::Sender<PromptEnvelope>,
}

impl ChannelUi {
    /// A UI and the receiver the front end reads prompts from.
    pub fn new(capacity: usize) -> (Self, mpsc::Receiver<PromptEnvelope>) {
        let (tx, rx) = mpsc::channel(capacity.max(1));
        (Self { tx }, rx)
    }
}

#[async_trait]
impl ApprovalUi for ChannelUi {
    async fn ask(&self, prompt: ApprovalPrompt) -> Vec<(String, ApprovalAnswer)> {
        let (reply, rx) = oneshot::channel();
        if self
            .tx
            .send(PromptEnvelope { prompt, reply })
            .await
            .is_err()
        {
            return Vec::new();
        }
        rx.await.unwrap_or_default()
    }
}

/// Receives approval lifecycle events.
pub trait ApprovalEvents: Send + Sync {
    /// A request was raised.
    fn requested(&self, request: &ApprovalRequest);
    /// A request was answered.
    fn resolved(&self, resolution: &ApprovalResolution, node: Option<&NodeId>);
}

/// Discards approval events.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoApprovalEvents;

impl ApprovalEvents for NoApprovalEvents {
    fn requested(&self, _: &ApprovalRequest) {}
    fn resolved(&self, _: &ApprovalResolution, _: Option<&NodeId>) {}
}

/// Writes `approval_requested` / `approval_resolved` events to a run, so the
/// agent map shows the node as waiting for approval.
#[derive(Debug, Clone)]
pub struct RecorderApprovalEvents(pub RunRecorder);

impl RecorderApprovalEvents {
    fn emit(&self, node: Option<&NodeId>, kind: EventKind) {
        let draft = match node {
            Some(n) => EventDraft::node(n.clone(), kind),
            None => EventDraft::run(kind),
        };
        if let Err(e) = self.0.emit(draft) {
            tracing::warn!(error = %e, "failed to record approval event");
        }
    }
}

impl ApprovalEvents for RecorderApprovalEvents {
    fn requested(&self, r: &ApprovalRequest) {
        let preview = r
            .preview
            .clone()
            .or_else(|| r.verdict.command.clone())
            .map(Payload::Text);
        self.emit(
            r.node_id.as_ref(),
            EventKind::ApprovalRequested {
                approval_id: r.approval_id.clone(),
                action: r.action.summary(),
                preview,
            },
        );
    }

    fn resolved(&self, r: &ApprovalResolution, node: Option<&NodeId>) {
        self.emit(
            node,
            EventKind::ApprovalResolved {
                approval_id: r.approval_id.clone(),
                decision: r.decision(),
                by: r.by.clone(),
            },
        );
    }
}

/// Approval counters for one run.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct ApprovalMetrics {
    /// Prompts shown (one per batch).
    pub prompts: u64,
    /// Requests across all prompts.
    pub requests: u64,
    /// Requests approved.
    pub approved: u64,
    /// Requests denied.
    pub denied: u64,
    /// Session grants added.
    pub session_grants: u64,
    /// "Always" rules proposed.
    pub always_proposals: u64,
}

/// Number of approval prompts in a recorded run (from `approval_requested`
/// ids, which carry the batch number).
pub fn count_prompts<'a>(events: impl IntoIterator<Item = &'a RunEvent>) -> u64 {
    let mut batches = std::collections::BTreeSet::new();
    for e in events {
        if let EventKind::ApprovalRequested { approval_id, .. } = &e.kind
            && let Some(batch) = approval_id
                .strip_prefix("approval-")
                .and_then(|s| s.split('-').next())
        {
            batches.insert(batch.to_string());
        }
    }
    batches.len() as u64
}

/// Suggested session pattern for a command line: the program and its first
/// non-option argument, then `*` (`cargo test -p x` → `cargo test*`).
pub fn suggest_pattern(command: &str) -> String {
    let mut words = command.split_whitespace();
    match (words.next(), words.next()) {
        (Some(p), Some(a)) if !a.starts_with('-') => format!("{p} {a}*"),
        (Some(p), _) => format!("{p}*"),
        _ => "*".into(),
    }
}

/// Turns `ask` verdicts into prompts, applies the chosen scopes and records
/// events and metrics.
pub struct ApprovalBroker {
    grants: Arc<SessionGrants>,
    ui: Arc<dyn ApprovalUi>,
    events: Arc<dyn ApprovalEvents>,
    root: PathBuf,
    state: Mutex<(u64, ApprovalMetrics)>,
}

impl std::fmt::Debug for ApprovalBroker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ApprovalBroker")
            .field("root", &self.root)
            .field("metrics", &self.metrics())
            .finish_non_exhaustive()
    }
}

impl ApprovalBroker {
    /// A broker adding grants to `grants` (share them with the
    /// [`crate::rules::PolicyEngine`]); path patterns are relative to `root`.
    pub fn new(
        grants: Arc<SessionGrants>,
        ui: Arc<dyn ApprovalUi>,
        root: impl Into<PathBuf>,
    ) -> Self {
        Self {
            grants,
            ui,
            events: Arc::new(NoApprovalEvents),
            root: root.into(),
            state: Mutex::new((0, ApprovalMetrics::default())),
        }
    }

    /// Record approval events to `events`.
    pub fn with_events(mut self, events: Arc<dyn ApprovalEvents>) -> Self {
        self.events = events;
        self
    }

    /// Counters so far.
    pub fn metrics(&self) -> ApprovalMetrics {
        self.lock().1
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, (u64, ApprovalMetrics)> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Ask about every pending call in one prompt and return one resolution
    /// per call, in order. Pass all `ask` calls of one model turn together so
    /// parallel tool calls produce a single prompt.
    pub async fn request(&self, pending: Vec<PendingApproval>) -> Vec<ApprovalResolution> {
        if pending.is_empty() {
            return Vec::new();
        }
        let batch = {
            let mut s = self.lock();
            s.0 += 1;
            s.1.prompts += 1;
            s.1.requests += pending.len() as u64;
            s.0
        };
        let requests: Vec<ApprovalRequest> = pending
            .into_iter()
            .enumerate()
            .map(|(i, p)| ApprovalRequest {
                approval_id: format!("approval-{batch}-{}", i + 1),
                suggested_pattern: p.verdict.command.as_deref().map(suggest_pattern).or_else(
                    || match &p.action {
                        Action::Tool { name } if crate::mcp::parse_action_name(name).is_some() => {
                            Some(name.clone())
                        }
                        _ => None,
                    },
                ),
                call_id: p.call_id,
                node_id: p.node_id,
                action: p.action,
                verdict: p.verdict,
                preview: p.preview,
            })
            .collect();
        for r in &requests {
            self.events.requested(r);
        }
        let mut answers = self
            .ui
            .ask(ApprovalPrompt {
                batch,
                requests: requests.clone(),
            })
            .await;
        requests
            .into_iter()
            .map(|r| {
                let answer = answers
                    .iter()
                    .position(|(id, _)| *id == r.approval_id)
                    .map(|i| answers.swap_remove(i).1);
                let res = self.apply(&r, answer);
                self.events.resolved(&res, r.node_id.as_ref());
                res
            })
            .collect()
    }

    fn apply(&self, r: &ApprovalRequest, answer: Option<ApprovalAnswer>) -> ApprovalResolution {
        let mut res = ApprovalResolution {
            approval_id: r.approval_id.clone(),
            call_id: r.call_id.clone(),
            approved: false,
            scope: None,
            reason: None,
            by: "user".into(),
            ini_rule: None,
        };
        let mut m = self.lock();
        match answer {
            None => {
                res.by = "harness".into();
                res.reason = Some("no answer was given".into());
                m.1.denied += 1;
            }
            Some(ApprovalAnswer::Deny { reason }) => {
                res.reason = reason;
                m.1.denied += 1;
            }
            Some(ApprovalAnswer::Approve(scope)) => {
                res.approved = true;
                m.1.approved += 1;
                let pattern = match &scope {
                    ApprovalScope::Once => None,
                    ApprovalScope::Session { pattern } | ApprovalScope::Always { pattern } => {
                        Some(pattern.as_str())
                    }
                };
                if let Some(p) = pattern {
                    match &r.action {
                        Action::Bash { .. } => {
                            self.grants.allow_command(p);
                            m.1.session_grants += 1;
                        }
                        Action::Write { .. } | Action::Read { .. } => {
                            if self.grants.allow_path(p, &self.root).is_ok() {
                                m.1.session_grants += 1;
                            }
                        }
                        Action::Tool { .. } => {
                            if crate::mcp::parse_action_name(p).is_some() {
                                self.grants.allow_tool(p);
                                m.1.session_grants += 1;
                            }
                        }
                    }
                }
                let proposable = match &r.action {
                    Action::Bash { .. } => true,
                    Action::Tool { .. } => {
                        matches!(&scope, ApprovalScope::Always { pattern } if crate::mcp::parse_action_name(pattern).is_some())
                    }
                    _ => false,
                };
                if let (ApprovalScope::Always { pattern }, true) = (&scope, proposable) {
                    res.ini_rule = Some(IniRuleProposal {
                        pattern: pattern.clone(),
                    });
                    m.1.always_proposals += 1;
                }
                res.scope = Some(scope);
            }
        }
        res
    }
}

#[cfg(test)]
mod tests;
