//! Background compaction (`[context] background_compaction`, TASK-161.5):
//! the handoff summary starts at the background threshold, the agent keeps
//! working while it runs, and it is swapped in at `compact_at`. Fake
//! providers, real run store.

mod support;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use harness_agent::agent_loop::{AgentLoop, Conversation, StopReason};
use harness_agent::compaction::{COMPACTION_NODE, Compactor, ContextManager, ContextSettings};
use harness_agent::dispatch::{AllowAll, Dispatcher};
use harness_agent::session::rebuild_conversation;
use harness_events::schema::{EventKind, NodeStatus, RunEvent};
use harness_providers::fake::{FakeError, FakeErrorKind, FakeProvider, FakeTurn};
use harness_providers::model::{Capabilities, ChatRequest, Message, Role};
use harness_providers::provider::{EventStream, Provider, ProviderError};
use serde_json::json;
use support::*;
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

const WINDOW: u32 = 10_000;
const KEEP: u32 = 2;

/// The agent's replies: eleven `echo` calls whose reported input climbs
/// past 0.7 (before call 7) and 0.8 (before call 9) of a 10k window, then
/// drops after compaction, and a final answer.
fn agent_turns() -> Vec<FakeTurn> {
    let mut turns: Vec<FakeTurn> = (0..11u64)
        .map(|k| {
            let input = if k <= 8 { 4000 + 500 * k } else { 3000 };
            with_usage(
                turn(vec![tool("echo", json!({"text": format!("step {k}\n")}))]),
                input,
                0,
                10,
            )
        })
        .collect();
    turns.push(FakeTurn::text("All steps done."));
    turns
}

fn handoff_text() -> String {
    "## Goal\nRun every step.\n\n## Decisions\nOne step per call.\n\n\
     ## Current state\nSteps 0-6 ran.\n\n## Files touched\nnone\n\n\
     ## Next steps\nRun the remaining steps.\n\n## Open questions\nnone"
        .to_owned()
}

/// The summarizer: its first call waits for `gate`; records how many agent
/// requests were made before that call.
struct Gated {
    inner: FakeProvider,
    gate: Arc<Notify>,
    agent: FakeProvider,
    called_after: Arc<AtomicUsize>,
    calls: AtomicUsize,
}

#[async_trait]
impl Provider for Gated {
    fn name(&self) -> &str {
        "fake"
    }
    fn capabilities(&self, model: &str) -> Capabilities {
        self.inner.capabilities(model)
    }
    async fn stream(
        &self,
        request: ChatRequest,
        cancel: CancellationToken,
    ) -> Result<EventStream, ProviderError> {
        if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
            self.called_after
                .store(self.agent.requests().len(), Ordering::SeqCst);
            tokio::select! {
                _ = self.gate.notified() => {}
                _ = cancel.cancelled() => return Err(ProviderError::Cancelled),
            }
        }
        self.inner.stream(request, cancel).await
    }
}

/// The agent's provider: opens `gate` and/or cancels `cancel` when it is
/// about to serve request `at` (0-based).
struct Agent {
    inner: FakeProvider,
    gate: Arc<Notify>,
    release_at: Option<usize>,
    cancel_at: Option<(usize, CancellationToken)>,
}

#[async_trait]
impl Provider for Agent {
    fn name(&self) -> &str {
        "fake"
    }
    fn capabilities(&self, model: &str) -> Capabilities {
        self.inner.capabilities(model)
    }
    async fn stream(
        &self,
        request: ChatRequest,
        cancel: CancellationToken,
    ) -> Result<EventStream, ProviderError> {
        let n = self.inner.requests().len();
        if self.release_at == Some(n) {
            self.gate.notify_one();
        }
        if let Some((at, token)) = &self.cancel_at
            && *at == n
        {
            token.cancel();
            return Err(ProviderError::Cancelled);
        }
        self.inner.stream(request, cancel).await
    }
}

struct Setup {
    h: Harness,
    summarizer: FakeProvider,
    gate: Arc<Notify>,
    called_after: Arc<AtomicUsize>,
    manager: Arc<ContextManager>,
}

async fn setup(background: bool, summaries: Vec<FakeTurn>) -> Setup {
    let h = Harness::new(agent_turns()).await;
    let summarizer = FakeProvider::with_turns(summaries);
    let gate = Arc::new(Notify::new());
    let called_after = Arc::new(AtomicUsize::new(usize::MAX));
    let mut settings = ContextSettings::new(WINDOW);
    settings.keep_recent_turns = KEEP;
    settings.background_at = background.then_some(0.7);
    let summary_provider: Arc<dyn Provider> = if background {
        Arc::new(Gated {
            inner: summarizer.clone(),
            gate: gate.clone(),
            agent: h.provider.clone(),
            called_after: called_after.clone(),
            calls: AtomicUsize::new(0),
        })
    } else {
        Arc::new(summarizer.clone())
    };
    let manager = Arc::new(ContextManager::new(settings).with_compactor(Compactor::new(
        summary_provider,
        "fake-small",
        KEEP,
    )));
    Setup {
        h,
        summarizer,
        gate,
        called_after,
        manager,
    }
}

impl Setup {
    fn agent(
        &self,
        release_at: Option<usize>,
        cancel_at: Option<(usize, CancellationToken)>,
    ) -> AgentLoop {
        self.agent_with(config(), release_at, cancel_at)
    }

    fn agent_with(
        &self,
        config: harness_agent::agent_loop::LoopConfig,
        release_at: Option<usize>,
        cancel_at: Option<(usize, CancellationToken)>,
    ) -> AgentLoop {
        let provider = Agent {
            inner: self.h.provider.clone(),
            gate: self.gate.clone(),
            release_at,
            cancel_at,
        };
        AgentLoop::new(
            Arc::new(provider),
            Dispatcher::new(self.h.tools(), Arc::new(AllowAll)),
            self.h.recorder.clone(),
            config,
        )
        .with_hook(self.manager.clone())
    }
}

fn conversation() -> Conversation {
    let mut conv = Conversation::new(Some("You are a coder.".into()));
    conv.push_user("Run every step with echo.");
    conv
}

fn handoff_index(requests: &[ChatRequest]) -> usize {
    requests
        .iter()
        .position(|r| r.messages[0].text().contains("<handoff>"))
        .expect("a request after compaction")
}

fn compaction_nodes(events: &[RunEvent]) -> Vec<&RunEvent> {
    events
        .iter()
        .filter(
            |e| matches!(&e.kind, EventKind::NodeStarted { name, .. } if name == COMPACTION_NODE),
        )
        .collect()
}

fn finish_status(events: &[RunEvent], node: &RunEvent) -> Option<NodeStatus> {
    events.iter().find_map(|e| match &e.kind {
        EventKind::NodeFinished { status, .. } if e.node_id == node.node_id => Some(*status),
        _ => None,
    })
}

#[tokio::test]
async fn background_summary_runs_while_the_agent_works_and_matches_in_line_compaction() {
    // In-line reference run.
    let a = setup(false, vec![FakeTurn::text(handoff_text())]).await;
    let mut conv_a = conversation();
    let out_a = a
        .agent(None, None)
        .run(&mut conv_a, &CancellationToken::new())
        .await;
    assert_eq!(out_a.stop, StopReason::EndTurn);
    let req_a = a.h.provider.requests();
    let swap_a = handoff_index(&req_a);
    assert_eq!(swap_a, 9, "in-line compaction before call 9 (0.8)");
    let report_a = a.manager.compactions()[0].clone();
    assert!(!report_a.background);

    // Background run: the summary is gated until the agent is about to make
    // call 8, so the run can only finish if the agent kept working (calls 7
    // and 8) while the summary call was in flight.
    let b = setup(true, vec![FakeTurn::text(handoff_text())]).await;
    let mut conv_b = conversation();
    let out_b = b
        .agent(Some(8), None)
        .run(&mut conv_b, &CancellationToken::new())
        .await;
    assert_eq!(out_b.stop, StopReason::EndTurn);
    assert_eq!(out_b.final_text, "All steps done.");
    assert_eq!(
        b.called_after.load(Ordering::SeqCst),
        7,
        "the summary started at 0.7, before agent call 7"
    );
    let req_b = b.h.provider.requests();
    assert_eq!(req_b.len(), req_a.len(), "no extra agent calls");
    assert_eq!(handoff_index(&req_b), swap_a, "swapped in at 0.8");
    assert_eq!(b.summarizer.requests().len(), 1);
    let reports = b.manager.compactions();
    assert_eq!(reports.len(), 1);
    assert!(reports[0].background);
    assert!(!b.manager.background_pending());

    // Same history as in-line compaction up to the swap; the same handoff.
    assert_eq!(req_a[..swap_a], req_b[..swap_a]);
    let after_a = &req_a[swap_a].messages;
    let after_b = &req_b[swap_a].messages;
    assert_eq!(after_a[0].text(), after_b[0].text(), "same handoff");
    // Everything in-line compaction keeps verbatim is kept, plus the turns
    // added while the summary ran; nothing is lost or duplicated.
    assert!(after_b.len() > after_a.len());
    assert_eq!(
        after_b[after_b.len() - (after_a.len() - 1)..],
        after_a[1..],
        "the in-line tail is the end of the background tail"
    );
    let before = &req_b[swap_a - 1].messages;
    let cut = reports[0].summarized;
    assert_eq!(cut, report_a.summarized - 4, "two more turns kept verbatim");
    let kept = &after_b[1..];
    let added = kept.len() - (before.len() - cut);
    assert_eq!(kept[..kept.len() - added], before[cut..]);
    let assistants = kept.iter().filter(|m| m.role == Role::Assistant).count();
    assert!(assistants >= KEEP as usize, "keep_recent_turns still holds");
    // The final histories agree the same way.
    assert_eq!(
        conv_b.messages[conv_b.messages.len() - (conv_a.messages.len() - 1)..],
        conv_a.messages[1..]
    );

    // Recorded as a compaction child node; resume rebuilds the history.
    let events = b.h.events().await;
    let nodes = compaction_nodes(&events);
    assert_eq!(nodes.len(), 1);
    assert_eq!(nodes[0].parent_node_id.as_ref(), Some(&out_b.node));
    assert_eq!(
        finish_status(&events, nodes[0]),
        Some(NodeStatus::Succeeded)
    );
    let rebuilt = rebuild_conversation(&events, b.h.recorder.blobs()).unwrap();
    assert_eq!(rebuilt.messages, conv_b.messages);
}

fn server_error() -> FakeTurn {
    FakeTurn::error(FakeError::new(FakeErrorKind::Server, "summary failed"))
}

#[tokio::test]
async fn a_failed_background_summary_falls_back_to_in_line_compaction() {
    // The summary fails when it is released (call 8): the swap at 0.8 sees
    // the failure and compacts in-line with the next summary.
    let b = setup(true, vec![server_error(), FakeTurn::text(handoff_text())]).await;
    let mut conv = conversation();
    let out = b
        .agent(Some(8), None)
        .run(&mut conv, &CancellationToken::new())
        .await;
    assert_eq!(out.stop, StopReason::EndTurn);
    assert_eq!(b.summarizer.requests().len(), 2);
    let reports = b.manager.compactions();
    assert_eq!(reports.len(), 1);
    assert!(!reports[0].background, "in-line fallback");
    assert_eq!(handoff_index(&b.h.provider.requests()), 9);
    let events = b.h.events().await;
    let nodes = compaction_nodes(&events);
    assert_eq!(nodes.len(), 2);
    assert_eq!(finish_status(&events, nodes[0]), Some(NodeStatus::Failed));
    assert_eq!(
        finish_status(&events, nodes[1]),
        Some(NodeStatus::Succeeded)
    );
    let rebuilt = rebuild_conversation(&events, b.h.recorder.blobs()).unwrap();
    assert_eq!(rebuilt.messages, conv.messages);
}

#[tokio::test]
async fn a_background_summary_that_fails_early_is_not_retried_until_compact_at() {
    // Released at once: the summary fails inside the hook that starts it;
    // the agent works on without new background attempts and compacts
    // in-line at 0.8.
    let b = setup(true, vec![server_error(), FakeTurn::text(handoff_text())]).await;
    b.gate.notify_one();
    let mut conv = conversation();
    let out = b
        .agent(None, None)
        .run(&mut conv, &CancellationToken::new())
        .await;
    assert_eq!(out.stop, StopReason::EndTurn);
    assert_eq!(b.summarizer.requests().len(), 2, "one failure, one in-line");
    let reports = b.manager.compactions();
    assert_eq!(reports.len(), 1);
    assert!(!reports[0].background);
    assert_eq!(handoff_index(&b.h.provider.requests()), 9);
}

#[tokio::test]
async fn cancelling_the_run_cancels_the_summary_and_the_next_run_restarts_it() {
    let b = setup(true, vec![FakeTurn::text(handoff_text())]).await;
    let mut conv = conversation();
    let cancel = CancellationToken::new();
    // Cancelled as the agent is about to make call 8, with the summary in
    // flight (gated).
    let out = b
        .agent(None, Some((8, cancel.clone())))
        .run(&mut conv, &cancel)
        .await;
    assert_eq!(out.stop, StopReason::Cancelled);
    assert!(b.manager.compactions().is_empty());
    assert!(
        !conv.messages.iter().any(|m| m.text().contains("<handoff>")),
        "the history is untouched"
    );

    // A new run: the cancelled summary is dropped, a new one starts, and the
    // swap happens at 0.8.
    conv.push_user("continue");
    let out = b
        .agent(Some(8), None)
        .run(&mut conv, &CancellationToken::new())
        .await;
    assert_eq!(out.stop, StopReason::EndTurn);
    let reports = b.manager.compactions();
    assert_eq!(reports.len(), 1);
    assert!(reports[0].background);
    let events = b.h.events().await;
    let nodes = compaction_nodes(&events);
    assert_eq!(nodes.len(), 2);
    assert_eq!(
        finish_status(&events, nodes[0]),
        Some(NodeStatus::Cancelled)
    );
    assert_eq!(
        finish_status(&events, nodes[1]),
        Some(NodeStatus::Succeeded)
    );
    let rebuilt = rebuild_conversation(&events, b.h.recorder.blobs()).unwrap();
    assert_eq!(rebuilt.messages, conv.messages);
}

fn stop_after(calls: u32) -> harness_agent::agent_loop::LoopConfig {
    let mut c = config();
    c.limits.max_turns = Some(calls);
    c
}

#[tokio::test]
async fn a_summary_of_a_history_that_changed_is_dropped() {
    // The first run stops after 8 calls with the summary pending; the
    // history is then rewritten (the first prompt edited), so that summary
    // no longer describes it. The summary swapped in covers the new history.
    let b = setup(
        true,
        vec![
            FakeTurn::text(handoff_text()),
            FakeTurn::text(handoff_text()),
        ],
    )
    .await;
    let mut conv = conversation();
    b.agent_with(stop_after(8), None, None)
        .run(&mut conv, &CancellationToken::new())
        .await;
    assert!(b.manager.background_pending());
    b.gate.notify_one(); // let the stale summary finish
    for _ in 0..10 {
        tokio::task::yield_now().await;
    }
    conv.messages[0] = Message::user("Run every step with echo, twice.");
    conv.push_user("continue");
    let out = b
        .agent(None, None)
        .run(&mut conv, &CancellationToken::new())
        .await;
    assert_eq!(out.stop, StopReason::EndTurn);
    let reports = b.manager.compactions();
    assert_eq!(reports.len(), 1);
    let summaries = b.summarizer.requests();
    assert_eq!(summaries.len(), 2);
    assert!(
        summaries[1].messages[0].text().contains("twice"),
        "the summary swapped in covers the edited history"
    );
    assert!(conv.messages[0].text().contains("<handoff>"));
}

#[tokio::test]
async fn slash_compact_discards_a_pending_background_summary() {
    let b = setup(
        true,
        vec![
            FakeTurn::text(handoff_text()),
            FakeTurn::text(handoff_text()),
            FakeTurn::text(handoff_text()),
        ],
    )
    .await;
    let mut conv = conversation();
    b.agent_with(stop_after(8), None, None)
        .run(&mut conv, &CancellationToken::new())
        .await;
    assert!(b.manager.background_pending());
    b.manager.request_compaction(Some("focus on step 3".into()));
    conv.push_user("continue");
    let out = b
        .agent(None, None)
        .run(&mut conv, &CancellationToken::new())
        .await;
    assert_eq!(out.stop, StopReason::EndTurn);
    let reports = b.manager.compactions();
    assert!(!reports.is_empty());
    assert!(!reports[0].background, "/compact runs in-line");
    let summaries = b.summarizer.requests();
    assert!(
        summaries[0].messages[0].text().contains("focus on step 3"),
        "the pending summary never reached the provider"
    );
    let events = b.h.events().await;
    let rebuilt = rebuild_conversation(&events, b.h.recorder.blobs()).unwrap();
    assert_eq!(rebuilt.messages, conv.messages);
}

#[tokio::test(start_paused = true)]
async fn a_later_run_cancelled_while_it_waits_for_an_earlier_summary_stops_at_once() {
    // Run 1 starts the summary (gated, never released) and ends normally
    // with it pending: the summary runs under run 1's token. Run 2 reaches
    // compact_at and waits for it; cancelling run 2 there must stop the
    // summary and return, not wait for the summary call to end.
    let b = setup(true, vec![FakeTurn::text(handoff_text())]).await;
    let mut conv = conversation();
    let first = CancellationToken::new();
    b.agent_with(stop_after(8), None, None)
        .run(&mut conv, &first)
        .await;
    assert!(b.manager.background_pending());
    conv.push_user("continue");
    let before = conv.messages.clone();
    let second = CancellationToken::new();
    let mut agent = b.agent(None, None);
    let run = agent.run(&mut conv, &second);
    // The paused clock only advances once every task is idle: run 2 is then
    // blocked at compact_at on the gated summary.
    let cancel = async {
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        second.cancel();
    };
    let (out, ()) = tokio::time::timeout(std::time::Duration::from_secs(3600), async {
        tokio::join!(run, cancel)
    })
    .await
    .expect("run 2 returns once cancelled");
    assert_eq!(out.stop, StopReason::Cancelled);
    assert!(!first.is_cancelled(), "run 1's token is untouched");
    assert!(b.manager.compactions().is_empty());
    assert!(!b.manager.background_pending());
    assert!(b.summarizer.requests().is_empty(), "the summary never ran");
    assert_eq!(b.called_after.load(Ordering::SeqCst), 7);
    assert_eq!(
        conv.messages[..before.len()],
        before[..],
        "the history is untouched"
    );
    assert!(!conv.messages.iter().any(|m| m.text().contains("<handoff>")));
    let events = b.h.events().await;
    let nodes = compaction_nodes(&events);
    assert_eq!(nodes.len(), 1);
    assert_eq!(
        finish_status(&events, nodes[0]),
        Some(NodeStatus::Cancelled)
    );
    let rebuilt = rebuild_conversation(&events, b.h.recorder.blobs()).unwrap();
    assert_eq!(rebuilt.messages, conv.messages);
}

#[tokio::test]
async fn stop_background_records_the_end_of_a_pending_summary() {
    let b = setup(true, vec![FakeTurn::text(handoff_text())]).await;
    let mut conv = conversation();
    b.agent_with(stop_after(8), None, None)
        .run(&mut conv, &CancellationToken::new())
        .await;
    assert!(b.manager.background_pending());
    b.manager.stop_background().await;
    assert!(!b.manager.background_pending());
    let events = b.h.events().await;
    let nodes = compaction_nodes(&events);
    assert_eq!(nodes.len(), 1);
    assert_eq!(
        finish_status(&events, nodes[0]),
        Some(NodeStatus::Cancelled)
    );
}
