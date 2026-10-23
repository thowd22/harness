//! Folds events into [`RunState`].
//!
//! Live views, run history, replay and `harness runs show --json` all build
//! their picture of a run with this one reducer, so they cannot disagree.
//! [`RunState::apply`] is incremental and O(1) amortized per event (hash map
//! lookups and vector pushes only; `node_started` additionally costs one push
//! per `needs` entry). Output order depends only on event order, never on hash
//! iteration, so folding the same events always gives the same state.

use std::collections::{BTreeMap, HashMap};

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
    /// Short human-readable title: `run_started.title` (workflow inputs),
    /// else the first prompt of an agent run (see
    /// [`crate::names::title_from_text`]).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Agent display names recorded in `run_started.agent_names`.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub agent_names: BTreeMap<String, String>,
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
            title: None,
            agent_names: BTreeMap::new(),
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

    /// Whether the run is doing work: a node runs, is queued or waits for
    /// approval, or a workflow run has started but not created its first
    /// node yet. An interactive session's run stays open between turns, so
    /// an open run with no active node (even no node at all, before the
    /// first prompt) is idle, not working.
    pub fn is_working(&self) -> bool {
        if self.status != RunStatus::Running {
            return false;
        }
        let active = self.nodes.iter().any(|n| {
            matches!(
                n.status,
                NodeStatus::Running | NodeStatus::Queued | NodeStatus::WaitingApproval
            )
        });
        active || (self.nodes.is_empty() && self.workflow.is_some())
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
                    title,
                    agent_names,
                },
                _,
            ) => {
                self.name = Some(name.clone());
                self.workflow.clone_from(workflow);
                self.host.clone_from(host);
                if let Some(t) = title.as_deref().and_then(crate::names::title_from_text) {
                    self.title = Some(t);
                }
                self.agent_names.clone_from(agent_names);
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
            (EventKind::UserMessage { text }, _)
                if self.title.is_none() && self.workflow.is_none() =>
            {
                self.title = crate::names::title_from_text(text);
            }
            _ => {}
        }
    }

    /// The display name of `agent` in this run (its YAML `display_name`
    /// as recorded at the start, else the default).
    pub fn agent_display(&self, agent: &str) -> String {
        crate::names::agent_display_name(agent, &self.agent_names)
    }

    /// The display name of `n`'s agent, if it has one.
    pub fn node_agent(&self, n: &NodeState) -> Option<String> {
        n.agent.as_deref().map(|a| self.agent_display(a))
    }

    /// The name to show for `n`: the agent's display name when the node is
    /// named after its agent, a short tag when the name is a raw id (a
    /// node only referenced so far), else the node's own name (a step id).
    pub fn node_label(&self, n: &NodeState) -> String {
        if n.agent.as_deref() == Some(n.name.as_str()) {
            return self.agent_display(&n.name);
        }
        if crate::names::find_raw_id(&n.name).is_some() {
            return crate::names::scrub_ids(&n.name);
        }
        n.name.clone()
    }

    /// The agent detail to show next to [`RunState::node_label`]: the
    /// agent's display name when it differs from the label.
    pub fn node_agent_detail(&self, n: &NodeState) -> Option<String> {
        let label = self.node_label(n);
        self.node_agent(n).filter(|a| *a != label)
    }

    /// What ran: the workflow name, else the display name of the first
    /// top-level agent, else of the agent the run is named after.
    pub fn who(&self) -> String {
        if let Some(w) = &self.workflow {
            return w.clone();
        }
        if let Some(a) = self
            .roots()
            .find(|n| matches!(n.kind, Some(NodeKind::Agent)))
            .and_then(|n| n.agent.clone().or_else(|| Some(n.name.clone())))
        {
            return self.agent_display(&a);
        }
        match &self.name {
            Some(n) => self.agent_display(&crate::names::scrub_ids(n)),
            None => "run".into(),
        }
    }

    /// The header label: `who · title`, or just `who` before a title is
    /// known.
    pub fn label(&self) -> String {
        match &self.title {
            Some(t) => format!("{} · {t}", self.who()),
            None => self.who(),
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
    fn an_open_session_without_active_nodes_is_not_working() {
        let session = |workflow: Option<&str>| EventKind::RunStarted {
            name: "coder".into(),
            workflow: workflow.map(Into::into),
            host: None,
            title: None,
            agent_names: BTreeMap::new(),
        };
        // Interactive session before the first prompt: open, no node.
        let mut s = RunState::new();
        s.apply(&ev(0, 0, None, session(None)));
        assert_eq!(s.status, RunStatus::Running);
        assert!(!s.is_working(), "waiting for the first prompt");
        // A turn starts, then finishes: working, then idle again.
        s.apply(&ev(1, 1, Some("a"), started("a", &[])));
        assert!(s.is_working());
        s.apply(&ev(
            2,
            2,
            Some("a"),
            EventKind::NodeFinished {
                status: NodeStatus::Succeeded,
                error: None,
            },
        ));
        assert!(!s.is_working(), "between turns");
        // A workflow run is working from the start, before its first node.
        let mut w = RunState::new();
        w.apply(&ev(0, 0, None, session(Some("fix-bug"))));
        assert!(w.is_working());
        // A finished run is never working.
        w.apply(&ev(
            1,
            1,
            None,
            EventKind::RunFinished {
                status: RunStatus::Succeeded,
                error: None,
            },
        ));
        assert!(!w.is_working());
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

    fn run_started(workflow: Option<&str>, title: Option<&str>) -> EventKind {
        EventKind::RunStarted {
            name: "coder".into(),
            workflow: workflow.map(Into::into),
            host: None,
            title: title.map(Into::into),
            agent_names: BTreeMap::from([("coder".to_owned(), "Code Smith".to_owned())]),
        }
    }

    fn prompt(text: &str) -> EventKind {
        EventKind::UserMessage { text: text.into() }
    }

    #[test]
    fn agent_runs_are_titled_after_their_first_prompt() {
        let mut s = RunState::new();
        s.apply(&ev(0, 0, None, run_started(None, None)));
        assert_eq!(s.title, None);
        assert_eq!(s.label(), "Code Smith");
        s.apply(&ev(1, 1, Some("a"), started("coder", &[])));
        s.apply(&ev(
            2,
            1,
            Some("a"),
            prompt("Fix the header parser.\nDetails..."),
        ));
        s.apply(&ev(3, 2, Some("a"), prompt("and the tests")));
        assert_eq!(s.title.as_deref(), Some("Fix the header parser"));
        assert_eq!(s.label(), "Code Smith · Fix the header parser");
    }

    #[test]
    fn explicit_titles_win_and_workflow_briefs_are_not_titles() {
        let mut s = RunState::new();
        s.apply(&ev(
            0,
            0,
            None,
            run_started(Some("fix"), Some("header bug")),
        ));
        s.apply(&ev(1, 1, Some("a"), prompt("You are step one")));
        assert_eq!(s.title.as_deref(), Some("header bug"));
        assert_eq!(s.label(), "fix · header bug");
        let mut w = RunState::new();
        w.apply(&ev(0, 0, None, run_started(Some("fix"), None)));
        w.apply(&ev(1, 1, Some("a"), prompt("You are step one")));
        assert_eq!(w.title, None);
        assert_eq!(w.label(), "fix");
    }

    #[test]
    fn nodes_are_labelled_with_agent_display_names() {
        let mut s = RunState::new();
        s.apply(&ev(0, 0, None, run_started(None, None)));
        let agent_node = |name: &str, agent: &str| EventKind::NodeStarted {
            kind: NodeKind::Agent,
            name: name.into(),
            agent: Some(agent.into()),
            model: None,
            host: None,
            needs: Vec::new(),
            queued: false,
        };
        s.apply(&ev(1, 0, Some("a"), agent_node("coder", "coder")));
        s.apply(&ev(2, 0, Some("b"), agent_node("build", "reviewer")));
        let a = s.node(&"a".into()).unwrap();
        let b = s.node(&"b".into()).unwrap();
        assert_eq!(s.node_label(a), "Code Smith");
        assert_eq!(s.node_agent_detail(a), None);
        assert_eq!(s.node_label(b), "build");
        assert_eq!(s.node_agent_detail(b).as_deref(), Some("Reviewer"));
        let raw = "0199f1a2-3b4c-7d5e-8f60-123456abcdef";
        s.apply(&ev(3, 0, Some(raw), EventKind::NodeRunning));
        let r = s.node(&raw.into()).unwrap();
        assert_eq!(s.node_label(r), "#abcdef");
    }
}
