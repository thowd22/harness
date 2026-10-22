//! Folds events into [`RunState`].
//!
//! Live views, run history, replay and `harness runs show --json` all build
//! their picture of a run with this one reducer, so they cannot disagree.
//! [`RunState::apply`] is incremental and O(1) amortized per event (hash map
//! lookups and vector pushes only; `node_started` additionally costs one push
//! per `needs` entry). Output order depends only on event order, never on hash
//! iteration, so folding the same events always gives the same state.

use std::collections::HashMap;

use serde::Serialize;
use time::OffsetDateTime;

use crate::schema::{EventKind, NodeId, NodeKind, NodeStatus, RunEvent, RunId, RunStatus, Usage};

/// Why two nodes are connected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EdgeKind {
    /// Workflow dependency: `from` must finish before `to` (`needs`).
    Needs,
    /// `from` spawned `to` (subagent or nested step).
    Spawn,
}

/// A directed edge in the run graph.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Edge {
    /// Source node.
    pub from: NodeId,
    /// Target node.
    pub to: NodeId,
    /// Edge kind.
    pub kind: EdgeKind,
}

/// Everything the map and detail views show about one node.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct NodeState {
    /// Node id.
    pub id: NodeId,
    /// Parent node (spawner), if any.
    pub parent: Option<NodeId>,
    /// Node kind; `None` while only referenced (e.g. by `needs`) and not yet
    /// started.
    pub kind: Option<NodeKind>,
    /// Display name.
    pub name: String,
    /// Agent profile.
    pub agent: Option<String>,
    /// Model.
    pub model: Option<String>,
    /// Host badge (`None` = local).
    pub host: Option<String>,
    /// Current status.
    pub status: NodeStatus,
    /// When execution began.
    #[serde(with = "time::serde::rfc3339::option")]
    pub started_at: Option<OffsetDateTime>,
    /// When the node finished.
    #[serde(with = "time::serde::rfc3339::option")]
    pub finished_at: Option<OffsetDateTime>,
    /// `finished_at - started_at` in milliseconds once finished.
    pub duration_ms: Option<u64>,
    /// Tokens used by this node's model calls.
    pub tokens: Usage,
    /// Cost of this node in USD.
    pub cost_usd: f64,
    /// Number of retries / loop iterations after the first attempt.
    pub retries: u32,
    /// Model calls started.
    pub model_calls: u32,
    /// Tool calls requested.
    pub tool_calls: u32,
    /// Files changed.
    pub files_changed: u32,
    /// Approvals currently awaiting an answer.
    pub pending_approvals: u32,
    /// Result of the most recent verify run.
    pub last_verify_passed: Option<bool>,
    /// Error from `node_finished` or the last unrecoverable error event.
    pub error: Option<String>,
    /// Child node ids in spawn order.
    pub children: Vec<NodeId>,
    /// Sequence number of the last event applied to this node.
    pub last_seq: u64,
}

impl NodeState {
    fn placeholder(id: NodeId, seq: u64) -> Self {
        Self {
            name: id.0.clone(),
            id,
            parent: None,
            kind: None,
            agent: None,
            model: None,
            host: None,
            status: NodeStatus::Queued,
            started_at: None,
            finished_at: None,
            duration_ms: None,
            tokens: Usage::default(),
            cost_usd: 0.0,
            retries: 0,
            model_calls: 0,
            tool_calls: 0,
            files_changed: 0,
            pending_approvals: 0,
            last_verify_passed: None,
            error: None,
            children: Vec::new(),
            last_seq: seq,
        }
    }
}

/// The folded state of a run.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RunState {
    /// Run id (from the first event).
    pub run_id: Option<RunId>,
    /// Run name.
    pub name: Option<String>,
    /// Workflow name.
    pub workflow: Option<String>,
    /// Run host (`None` = local).
    pub host: Option<String>,
    /// Run status.
    pub status: RunStatus,
    /// When the run started.
    #[serde(with = "time::serde::rfc3339::option")]
    pub started_at: Option<OffsetDateTime>,
    /// When the run finished.
    #[serde(with = "time::serde::rfc3339::option")]
    pub finished_at: Option<OffsetDateTime>,
    /// Run error message.
    pub error: Option<String>,
    /// Nodes in order of first appearance.
    pub nodes: Vec<NodeState>,
    /// Edges in order of creation.
    pub edges: Vec<Edge>,
    /// Total tokens across all nodes.
    pub tokens: Usage,
    /// Total cost across the run in USD.
    pub cost_usd: f64,
    /// Number of events applied.
    pub event_count: u64,
    /// Sequence number of the last applied event.
    pub last_seq: Option<u64>,
    /// Incremented whenever a node or edge is added, so views can recompute
    /// layout only when the topology changes.
    pub topology_version: u64,
    #[serde(skip)]
    index: HashMap<NodeId, usize>,
}

impl Default for RunState {
    fn default() -> Self {
        Self {
            run_id: None,
            name: None,
            workflow: None,
            host: None,
            status: RunStatus::Running,
            started_at: None,
            finished_at: None,
            error: None,
            nodes: Vec::new(),
            edges: Vec::new(),
            tokens: Usage::default(),
            cost_usd: 0.0,
            event_count: 0,
            last_seq: None,
            topology_version: 0,
            index: HashMap::new(),
        }
    }
}

impl RunState {
    /// An empty state.
    pub fn new() -> Self {
        Self::default()
    }

    /// Folds a sequence of events into a fresh state.
    pub fn from_events<'a>(events: impl IntoIterator<Item = &'a RunEvent>) -> Self {
        let mut state = Self::new();
        for e in events {
            state.apply(e);
        }
        state
    }

    /// Looks up a node.
    pub fn node(&self, id: &NodeId) -> Option<&NodeState> {
        self.index.get(id).map(|&i| &self.nodes[i])
    }

    /// Top-level nodes (no parent), in order of appearance.
    pub fn roots(&self) -> impl Iterator<Item = &NodeState> {
        self.nodes.iter().filter(|n| n.parent.is_none())
    }

    fn node_index(&mut self, id: &NodeId, seq: u64) -> usize {
        if let Some(&i) = self.index.get(id) {
            return i;
        }
        let i = self.nodes.len();
        self.nodes.push(NodeState::placeholder(id.clone(), seq));
        self.index.insert(id.clone(), i);
        self.topology_version += 1;
        i
    }

    fn link_parent(&mut self, child: usize, parent: &NodeId, seq: u64) {
        if self.nodes[child].parent.is_some() {
            return;
        }
        let child_id = self.nodes[child].id.clone();
        let p = self.node_index(parent, seq);
        self.nodes[child].parent = Some(parent.clone());
        self.nodes[p].children.push(child_id.clone());
        self.edges.push(Edge {
            from: parent.clone(),
            to: child_id,
            kind: EdgeKind::Spawn,
        });
        self.topology_version += 1;
    }

    /// Applies one event. O(1) amortized.
    pub fn apply(&mut self, e: &RunEvent) {
        self.event_count += 1;
        self.last_seq = Some(e.seq);
        if self.run_id.is_none() {
            self.run_id = Some(e.run_id);
        }

        let idx = e.node_id.as_ref().map(|id| self.node_index(id, e.seq));
        if let (Some(i), Some(parent)) = (idx, &e.parent_node_id) {
            self.link_parent(i, parent, e.seq);
        }
        if let Some(i) = idx {
            self.nodes[i].last_seq = e.seq;
        }

        match (&e.kind, idx) {
            (
                EventKind::RunStarted {
                    name,
                    workflow,
                    host,
                },
                _,
            ) => {
                self.name = Some(name.clone());
                self.workflow.clone_from(workflow);
                self.host.clone_from(host);
                self.status = RunStatus::Running;
                self.started_at = Some(e.ts);
            }
            (EventKind::RunFinished { status, error }, _) => {
                self.status = *status;
                self.error.clone_from(error);
                self.finished_at = Some(e.ts);
            }
            (EventKind::Cost { usd }, i) => {
                self.cost_usd += usd;
                if let Some(i) = i {
                    self.nodes[i].cost_usd += usd;
                }
            }
            (EventKind::ModelCallFinished { usage, .. }, i) => {
                self.tokens.add(usage);
                if let Some(i) = i {
                    self.nodes[i].tokens.add(usage);
                }
            }
            (
                EventKind::NodeStarted {
                    kind,
                    name,
                    agent,
                    model,
                    host,
                    needs,
                    queued,
                },
                Some(i),
            ) => {
                let first_start = self.nodes[i].kind.is_none();
                let n = &mut self.nodes[i];
                n.kind = Some(*kind);
                n.name.clone_from(name);
                n.agent.clone_from(agent);
                n.model.clone_from(model);
                n.host.clone_from(host);
                if !n.status.is_terminal() {
                    if *queued {
                        n.status = NodeStatus::Queued;
                    } else {
                        n.status = NodeStatus::Running;
                        n.started_at.get_or_insert(e.ts);
                    }
                }
                if first_start {
                    let to = n.id.clone();
                    for need in needs {
                        self.node_index(need, e.seq);
                        self.edges.push(Edge {
                            from: need.clone(),
                            to: to.clone(),
                            kind: EdgeKind::Needs,
                        });
                        self.topology_version += 1;
                    }
                }
            }
            (EventKind::NodeRunning, Some(i)) => {
                let n = &mut self.nodes[i];
                if !n.status.is_terminal() {
                    n.status = NodeStatus::Running;
                }
                n.started_at.get_or_insert(e.ts);
            }
            (EventKind::NodeRetry { .. }, Some(i)) => {
                let n = &mut self.nodes[i];
                n.retries += 1;
                n.status = NodeStatus::Running;
                n.finished_at = None;
                n.duration_ms = None;
                n.error = None;
            }
            (EventKind::NodeFinished { status, error }, Some(i)) => {
                let n = &mut self.nodes[i];
                n.status = *status;
                n.error.clone_from(error);
                n.finished_at = Some(e.ts);
                n.duration_ms = n
                    .started_at
                    .map(|s| u64::try_from((e.ts - s).whole_milliseconds()).unwrap_or(0));
            }
            (EventKind::ModelCallStarted { model, .. }, Some(i)) => {
                let n = &mut self.nodes[i];
                n.model_calls += 1;
                if n.model.is_none() {
                    n.model = Some(model.clone());
                }
            }
            (EventKind::ToolCall { .. }, Some(i)) => self.nodes[i].tool_calls += 1,
            (EventKind::FileChanged { .. }, Some(i)) => self.nodes[i].files_changed += 1,
            (EventKind::VerifyFinished { passed, .. }, Some(i)) => {
                self.nodes[i].last_verify_passed = Some(*passed);
            }
            (EventKind::ApprovalRequested { .. }, Some(i)) => {
                let n = &mut self.nodes[i];
                n.pending_approvals += 1;
                if !n.status.is_terminal() {
                    n.status = NodeStatus::WaitingApproval;
                }
            }
            (EventKind::ApprovalResolved { .. }, Some(i)) => {
                let n = &mut self.nodes[i];
                n.pending_approvals = n.pending_approvals.saturating_sub(1);
                if n.pending_approvals == 0 && n.status == NodeStatus::WaitingApproval {
                    n.status = NodeStatus::Running;
                }
            }
            (
                EventKind::Error {
                    message,
                    recoverable: false,
                    ..
                },
                Some(i),
            ) => self.nodes[i].error = Some(message.clone()),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::{ApprovalDecision, Payload};

    fn ev(seq: u64, secs: i64, node: Option<&str>, kind: EventKind) -> RunEvent {
        RunEvent {
            seq,
            ts: time::macros::datetime!(2026-10-22 12:00:00 UTC) + time::Duration::seconds(secs),
            run_id: RunId(uuid::Uuid::nil()),
            node_id: node.map(NodeId::from),
            parent_node_id: None,
            kind,
        }
    }

    fn started(name: &str, needs: &[&str]) -> EventKind {
        EventKind::NodeStarted {
            kind: NodeKind::Workflow,
            name: name.into(),
            agent: None,
            model: None,
            host: None,
            needs: needs.iter().map(|&s| s.into()).collect(),
            queued: false,
        }
    }

    #[test]
    fn needs_edges_and_timings() {
        let events = [
            ev(0, 0, Some("a"), started("a", &[])),
            ev(1, 0, Some("b"), started("b", &["a"])),
            ev(
                2,
                5,
                Some("a"),
                EventKind::NodeFinished {
                    status: NodeStatus::Succeeded,
                    error: None,
                },
            ),
        ];
        let s = RunState::from_events(&events);
        assert_eq!(
            s.edges,
            vec![Edge {
                from: "a".into(),
                to: "b".into(),
                kind: EdgeKind::Needs
            }]
        );
        let a = s.node(&"a".into()).unwrap();
        assert_eq!(a.duration_ms, Some(5000));
        assert_eq!(a.status, NodeStatus::Succeeded);
        assert_eq!(s.node(&"b".into()).unwrap().status, NodeStatus::Running);
    }

    #[test]
    fn approvals_toggle_waiting_status() {
        let mut s = RunState::new();
        s.apply(&ev(0, 0, Some("a"), started("a", &[])));
        s.apply(&ev(
            1,
            1,
            Some("a"),
            EventKind::ApprovalRequested {
                approval_id: "x".into(),
                action: "rm".into(),
                preview: Some(Payload::text("rm -rf build")),
            },
        ));
        assert_eq!(s.nodes[0].status, NodeStatus::WaitingApproval);
        s.apply(&ev(
            2,
            2,
            Some("a"),
            EventKind::ApprovalResolved {
                approval_id: "x".into(),
                decision: ApprovalDecision::Approved,
                by: "user".into(),
            },
        ));
        assert_eq!(s.nodes[0].status, NodeStatus::Running);
    }

    #[test]
    fn retry_clears_finish_and_counts() {
        let mut s = RunState::new();
        s.apply(&ev(0, 0, Some("a"), started("a", &[])));
        s.apply(&ev(
            1,
            1,
            Some("a"),
            EventKind::NodeFinished {
                status: NodeStatus::Failed,
                error: Some("boom".into()),
            },
        ));
        s.apply(&ev(
            2,
            2,
            Some("a"),
            EventKind::NodeRetry {
                attempt: 2,
                reason: "boom".into(),
            },
        ));
        let a = &s.nodes[0];
        assert_eq!(
            (a.retries, a.status, a.error.clone(), a.finished_at),
            (1, NodeStatus::Running, None, None)
        );
    }

    #[test]
    fn topology_version_changes_only_on_new_nodes_or_edges() {
        let mut s = RunState::new();
        s.apply(&ev(0, 0, Some("a"), started("a", &[])));
        let v = s.topology_version;
        s.apply(&ev(1, 0, Some("a"), EventKind::Cost { usd: 0.1 }));
        s.apply(&ev(2, 0, Some("a"), EventKind::NodeRunning));
        assert_eq!(s.topology_version, v);
        s.apply(&ev(3, 0, Some("b"), started("b", &["a"])));
        assert!(s.topology_version > v);
    }
}
