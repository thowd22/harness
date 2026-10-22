//! Run and node metrics shown on the map: live token and cost totals,
//! cache hit rate, blocked nodes and the critical path.

use std::collections::HashMap;

use harness_events::reducer::{EdgeKind, NodeState, RunState};
use harness_events::schema::{NodeId, NodeStatus};
use time::OffsetDateTime;

use crate::chat::text::cache_pct;

/// Prefix of `node_finished.error` when an agent stopped on its budget
/// (`harness_agent::agent_loop::codes::BUDGET`).
pub const BUDGET_PREFIX: &str = "budget";

/// Why a node needs the user.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Blocked {
    /// An approval prompt is open for the node.
    Approval,
    /// The node stopped because it hit its budget.
    Budget,
}

impl Blocked {
    /// One-word label for tight spaces: `approval` or `budget`.
    pub fn short(self) -> &'static str {
        match self {
            Blocked::Approval => "approval",
            Blocked::Budget => "budget",
        }
    }

    /// Short label, e.g. `approval`.
    pub fn label(self) -> &'static str {
        match self {
            Blocked::Approval => "waiting for approval",
            Blocked::Budget => "budget exceeded",
        }
    }
}

/// Whether `n` waits on the user, and why.
pub fn blocked(n: &NodeState) -> Option<Blocked> {
    if n.pending_approvals > 0 || n.status == NodeStatus::WaitingApproval {
        return Some(Blocked::Approval);
    }
    let budget = n
        .error
        .as_deref()
        .is_some_and(|e| e.starts_with(BUDGET_PREFIX));
    (budget && n.status.is_terminal()).then_some(Blocked::Budget)
}

/// Cache hit rate of a node's input tokens, in percent.
pub fn node_cache_pct(n: &NodeState) -> Option<u64> {
    cache_pct(n.tokens.input, n.tokens.cached_input)
}

/// Elapsed time of a node: its final duration, or the time since it
/// started while it runs.
pub fn elapsed_ms(n: &NodeState, now: OffsetDateTime) -> Option<u64> {
    if let Some(d) = n.duration_ms {
        return Some(d);
    }
    let s = n.started_at?;
    let end = n.finished_at.unwrap_or(now);
    Some(u64::try_from((end - s).whole_milliseconds()).unwrap_or(0))
}

/// Elapsed time of the whole run.
pub fn run_elapsed_ms(run: &RunState, now: OffsetDateTime) -> Option<u64> {
    let s = run
        .started_at
        .or_else(|| run.nodes.iter().filter_map(|n| n.started_at).min())?;
    let end = run.finished_at.unwrap_or(now);
    Some(u64::try_from((end - s).whole_milliseconds()).unwrap_or(0))
}

/// The longest chain of `needs` dependencies, weighted by node duration
/// (running nodes count their elapsed time). Returns the total and the
/// nodes on the path in order. Subagents are inside their parent's time.
pub fn critical_path(run: &RunState, now: OffsetDateTime) -> (u64, Vec<NodeId>) {
    let mut preds: HashMap<&NodeId, Vec<&NodeId>> = HashMap::new();
    for e in &run.edges {
        if e.kind == EdgeKind::Needs {
            preds.entry(&e.to).or_default().push(&e.from);
        }
    }
    let mut memo: HashMap<NodeId, (u64, Option<NodeId>)> = HashMap::new();
    // Memoized DFS; the visiting list stops cycles from recursing forever.
    fn best(
        id: &NodeId,
        run: &RunState,
        preds: &HashMap<&NodeId, Vec<&NodeId>>,
        memo: &mut HashMap<NodeId, (u64, Option<NodeId>)>,
        visiting: &mut Vec<NodeId>,
        now: OffsetDateTime,
    ) -> u64 {
        if let Some((v, _)) = memo.get(id) {
            return *v;
        }
        if visiting.contains(id) {
            return 0;
        }
        visiting.push(id.clone());
        let own = run.node(id).and_then(|n| elapsed_ms(n, now)).unwrap_or(0);
        let mut top = (0u64, None);
        for p in preds.get(id).map(Vec::as_slice).unwrap_or(&[]) {
            let v = best(p, run, preds, memo, visiting, now);
            if v > top.0 || top.1.is_none() {
                top = (v, Some((*p).clone()));
            }
        }
        visiting.pop();
        let total = own + top.0;
        memo.insert(id.clone(), (total, top.1));
        total
    }
    let mut end: Option<(u64, NodeId)> = None;
    let mut visiting = Vec::new();
    for n in &run.nodes {
        if n.parent.is_some() && !preds.contains_key(&n.id) {
            // Spawned children only count through their parent.
            if !run
                .edges
                .iter()
                .any(|e| e.kind == EdgeKind::Needs && e.from == n.id)
            {
                continue;
            }
        }
        let v = best(&n.id, run, &preds, &mut memo, &mut visiting, now);
        if end.as_ref().is_none_or(|(b, _)| v > *b) {
            end = Some((v, n.id.clone()));
        }
    }
    let Some((total, last)) = end else {
        return (0, Vec::new());
    };
    let mut path = vec![last.clone()];
    let mut cur = last;
    while let Some((_, Some(p))) = memo.get(&cur) {
        if path.contains(p) {
            break;
        }
        path.push(p.clone());
        cur = p.clone();
    }
    path.reverse();
    (total, path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use harness_events::schema::{EventKind, NodeKind, RunEvent, RunId, Usage};
    use time::macros::datetime;

    fn ev(seq: u64, secs: i64, node: &str, kind: EventKind) -> RunEvent {
        RunEvent {
            seq,
            ts: datetime!(2026-10-22 12:00:00 UTC) + time::Duration::seconds(secs),
            run_id: RunId(uuid::Uuid::nil()),
            node_id: Some(NodeId::from(node)),
            parent_node_id: None,
            kind,
        }
    }

    fn start(name: &str, needs: &[&str]) -> EventKind {
        EventKind::NodeStarted {
            kind: NodeKind::Agent,
            name: name.into(),
            agent: None,
            model: None,
            host: None,
            needs: needs.iter().map(|n| NodeId::from(*n)).collect(),
            queued: false,
        }
    }

    fn done() -> EventKind {
        EventKind::NodeFinished {
            status: NodeStatus::Succeeded,
            error: None,
        }
    }

    #[test]
    fn critical_path_takes_the_longest_chain() {
        let events = vec![
            ev(1, 0, "a", start("a", &[])),
            ev(2, 10, "a", done()),
            ev(3, 10, "fast", start("fast", &["a"])),
            ev(4, 12, "fast", done()),
            ev(5, 10, "slow", start("slow", &["a"])),
            ev(6, 40, "slow", done()),
            ev(7, 40, "end", start("end", &["fast", "slow"])),
            ev(8, 45, "end", done()),
        ];
        let run = RunState::from_events(&events);
        let (ms, path) = critical_path(&run, datetime!(2026-10-22 12:01:00 UTC));
        assert_eq!(ms, 45_000);
        let ids: Vec<&str> = path.iter().map(NodeId::as_str).collect();
        assert_eq!(ids, ["a", "slow", "end"]);
    }

    #[test]
    fn running_nodes_count_elapsed_time() {
        let events = vec![ev(1, 0, "a", start("a", &[]))];
        let run = RunState::from_events(&events);
        let (ms, _) = critical_path(&run, datetime!(2026-10-22 12:00:07 UTC));
        assert_eq!(ms, 7_000);
    }

    #[test]
    fn blocked_nodes_and_cache_rate() {
        let mut events = vec![
            ev(1, 0, "a", start("a", &[])),
            ev(
                2,
                1,
                "a",
                EventKind::ApprovalRequested {
                    approval_id: "x".into(),
                    action: "git push".into(),
                    preview: None,
                },
            ),
            ev(3, 0, "b", start("b", &[])),
            ev(
                4,
                2,
                "b",
                EventKind::ModelCallFinished {
                    call_id: "c".into(),
                    usage: Usage {
                        input: 100,
                        cached_input: 900,
                        output: 10,
                        reasoning: 0,
                    },
                    stop_reason: None,
                    response: None,
                    duration_ms: 1,
                },
            ),
            ev(
                5,
                3,
                "b",
                EventKind::NodeFinished {
                    status: NodeStatus::Failed,
                    error: Some("budget: run budget $1.00 reached".into()),
                },
            ),
        ];
        let run = RunState::from_events(&events);
        assert_eq!(blocked(&run.nodes[0]), Some(Blocked::Approval));
        assert_eq!(blocked(&run.nodes[1]), Some(Blocked::Budget));
        assert_eq!(node_cache_pct(&run.nodes[1]), Some(90));
        events.truncate(1);
        let run = RunState::from_events(&events);
        assert_eq!(blocked(&run.nodes[0]), None);
    }
}
