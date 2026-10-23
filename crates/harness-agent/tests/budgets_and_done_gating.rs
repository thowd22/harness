//! Budgets, done-gating and per-call accounting through the agent loop.

mod support;

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use harness_agent::agent_loop::{
    Conversation, StopReason, Verification, Verifier, VerifyResult, VerifyWhen, codes,
};
use harness_agent::budget::{
    BudgetApprover, BudgetExceeded, BudgetGuard, BudgetMeter, BudgetScope, Budgets, DenyOverruns,
};
use harness_agent::cost::{CostReport, UsageLedger};
use harness_config::ini::config::OnExceed;
use harness_events::schema::{EventKind, NodeStatus};
use harness_providers::fake::FakeTurn;
use harness_providers::model::Role;
use serde_json::json;
use support::*;
use tokio_util::sync::CancellationToken;

fn echo_turns(n: usize) -> Vec<FakeTurn> {
    let mut turns: Vec<FakeTurn> = (0..n)
        .map(|i| {
            with_usage(
                turn(vec![tool("echo", json!({"text": i.to_string()}))]),
                100,
                900,
                10,
            )
        })
        .collect();
    turns.push(FakeTurn::text("done"));
    turns
}

fn dollar_per_call() -> harness_agent::agent_loop::CostFn {
    Arc::new(|_u| Some(1.0))
}

fn warnings(events: &[harness_events::schema::RunEvent]) -> Vec<String> {
    events
        .iter()
        .filter_map(|e| match &e.kind {
            EventKind::Error {
                message,
                recoverable: true,
                ..
            } if message.starts_with("budget warning") => Some(message.clone()),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn session_budget_warns_at_80_percent_and_stops_at_the_limit() {
    let h = Harness::new(echo_turns(10)).await;
    let meter = BudgetMeter::new(
        Budgets::new()
            .with_limit(BudgetScope::Session, Some(5.0))
            .with_on_exceed(OnExceed::Stop),
    );
    let mut conv = Conversation::new(None);
    conv.push_user("go");
    let out = h
        .agent(config())
        .with_cost(dollar_per_call())
        .with_hook(Arc::new(BudgetGuard::new(
            meter.clone(),
            Arc::new(DenyOverruns),
        )))
        .run(&mut conv, &CancellationToken::new())
        .await;
    assert_eq!(out.turns, 5);
    assert!(
        matches!(&out.stop, StopReason::Budget { reason } if reason == "session budget of $5.00 reached ($5.0000 spent)"),
        "{:?}",
        out.stop
    );
    assert_eq!(meter.spent(BudgetScope::Session), 5.0);
    let events = h.events().await;
    assert_eq!(
        warnings(&events),
        ["budget warning: session spend $4.00 is 80% of the $5.00 budget"]
    );
}

struct ApproveOnce(Mutex<Vec<BudgetExceeded>>);

#[async_trait]
impl BudgetApprover for ApproveOnce {
    async fn approve(&self, exceeded: &BudgetExceeded) -> bool {
        let mut asked = self.0.lock().unwrap();
        asked.push(exceeded.clone());
        asked.len() == 1
    }
}

#[tokio::test]
async fn on_exceed_ask_continues_after_approval_and_stops_on_refusal() {
    let h = Harness::new(echo_turns(10)).await;
    let meter = BudgetMeter::new(Budgets::new().with_limit(BudgetScope::Run, Some(2.0)));
    let approver = Arc::new(ApproveOnce(Mutex::new(Vec::new())));
    let mut conv = Conversation::new(None);
    conv.push_user("go");
    let out = h
        .agent(config())
        .with_cost(dollar_per_call())
        .with_hook(Arc::new(BudgetGuard::new(meter.clone(), approver.clone())))
        .run(&mut conv, &CancellationToken::new())
        .await;
    // $2 reached -> asked, approved (limit 4) -> $4 reached -> refused.
    assert_eq!(out.turns, 4);
    assert!(matches!(out.stop, StopReason::Budget { .. }));
    let asked = approver.0.lock().unwrap();
    assert_eq!(asked.len(), 2);
    assert_eq!((asked[0].scope, asked[0].limit), (BudgetScope::Run, 2.0));
    assert_eq!(asked[1].limit, 4.0);
}

#[tokio::test]
async fn agent_budget_limits_one_loop_and_daily_baseline_counts() {
    let h = Harness::new(echo_turns(10)).await;
    let meter = BudgetMeter::new(
        Budgets::new()
            .with_limit(BudgetScope::Day, Some(30.0))
            .with_on_exceed(OnExceed::Stop),
    )
    .with_spent(BudgetScope::Day, 28.0);
    let guard = BudgetGuard::new(meter.clone(), Arc::new(DenyOverruns)).with_agent_limit(Some(5.0));
    let mut conv = Conversation::new(None);
    conv.push_user("go");
    let out = h
        .agent(config())
        .with_cost(dollar_per_call())
        .with_hook(Arc::new(guard))
        .run(&mut conv, &CancellationToken::new())
        .await;
    assert_eq!(out.turns, 2);
    assert!(
        matches!(&out.stop, StopReason::Budget { reason } if reason.starts_with("daily budget")),
        "{:?}",
        out.stop
    );

    let h = Harness::new(echo_turns(10)).await;
    let guard = BudgetGuard::new(BudgetMeter::default(), Arc::new(DenyOverruns))
        .with_agent_limit(Some(3.0));
    let mut conv = Conversation::new(None);
    conv.push_user("go");
    let out = h
        .agent(config())
        .with_cost(dollar_per_call())
        .with_hook(Arc::new(guard))
        .run(&mut conv, &CancellationToken::new())
        .await;
    assert_eq!(out.turns, 3);
    assert!(
        matches!(&out.stop, StopReason::Budget { reason } if reason.starts_with("agent budget"))
    );
}

#[tokio::test]
async fn per_call_usage_cost_and_cache_hit_rate_are_recorded() {
    let h = Harness::new(echo_turns(2)).await;
    let mut conv = Conversation::new(None);
    conv.push_user("go");
    h.agent(config())
        .with_cost(dollar_per_call())
        .run(&mut conv, &CancellationToken::new())
        .await;
    let events = h.events().await;
    let ledger = UsageLedger::from_events(&events);
    let calls = ledger.calls();
    assert_eq!(calls.len(), 3);
    assert_eq!(calls[0].usage.input, 100);
    assert_eq!(calls[0].usage.cached_input, 900);
    assert_eq!(calls[0].cost_usd, Some(1.0));
    assert_eq!(calls[0].cache_hit_rate(), Some(0.9));
    assert_eq!(calls[0].agent.as_deref(), Some("coder"));
    let report = CostReport::new(&ledger);
    assert_eq!(report.total.cost_usd, 3.0);
    assert_eq!(report.by_model[0].label, "fake/fake-model");
    assert!(
        report.lines(None)[0].contains("cache 90%"),
        "{:?}",
        report.lines(None)
    );
}

#[tokio::test]
async fn cache_writes_are_recorded_in_call_events_and_the_cost_report() {
    let mut first = with_usage(turn(vec![tool("echo", json!({"text": "a"}))]), 100, 0, 10);
    first.usage.cache_write_tokens = 2_000;
    let h = Harness::new(vec![first, FakeTurn::text("done")]).await;
    let mut conv = Conversation::new(None);
    conv.push_user("go");
    h.agent(config())
        .with_cost(dollar_per_call())
        .run(&mut conv, &CancellationToken::new())
        .await;
    let events = h.events().await;
    let ledger = UsageLedger::from_events(&events);
    let calls = ledger.calls();
    // Cache writes count as input and are also reported on their own.
    assert_eq!(calls[0].usage.input, 2_100);
    assert_eq!(calls[0].usage.cache_write, 2_000);
    assert_eq!(calls[1].usage.cache_write, 0);
    let lines = CostReport::new(&ledger).lines(None);
    assert!(lines[0].contains("2000 written to cache"), "{lines:?}");
}

/// Fails `failures` times, then passes.
struct Flaky {
    failures: Mutex<u32>,
    runs: Mutex<u32>,
}

impl Flaky {
    fn new(failures: u32) -> Self {
        Self {
            failures: Mutex::new(failures),
            runs: Mutex::new(0),
        }
    }
}

#[async_trait]
impl Verifier for Flaky {
    fn command(&self) -> String {
        "cargo test".into()
    }
    async fn verify(&self, _cancel: &CancellationToken) -> VerifyResult {
        *self.runs.lock().unwrap() += 1;
        let mut left = self.failures.lock().unwrap();
        if *left > 0 {
            *left -= 1;
            VerifyResult {
                passed: false,
                exit_code: Some(101),
                output: "test parser::empty ... FAILED".into(),
            }
        } else {
            VerifyResult {
                passed: true,
                exit_code: Some(0),
                output: "ok".into(),
            }
        }
    }
}

fn change_then_done(extra: usize) -> Vec<FakeTurn> {
    let mut turns = vec![
        turn(vec![tool("write", json!({"path": "src/parser.rs"}))]),
        FakeTurn::text("Fixed the parser."),
    ];
    for i in 0..extra {
        turns.push(turn(vec![tool(
            "write",
            json!({"path": format!("fix{i}.rs")}),
        )]));
        turns.push(FakeTurn::text(format!("Fix attempt {i} done.")));
    }
    turns
}

fn node_finished(events: &[harness_events::schema::RunEvent]) -> (NodeStatus, Option<String>) {
    events
        .iter()
        .rev()
        .find_map(|e| match &e.kind {
            EventKind::NodeFinished { status, error } => Some((*status, error.clone())),
            _ => None,
        })
        .unwrap()
}

#[tokio::test]
async fn verify_failures_are_fed_back_until_the_fix_passes() {
    let h = Harness::new(change_then_done(1)).await;
    let mut cfg = config();
    cfg.max_fix_attempts = 3;
    cfg.verify_when = VerifyWhen::OnChanges;
    let verifier = Arc::new(Flaky::new(1));
    let mut conv = Conversation::new(None);
    conv.push_user("fix the parser");
    let out = h
        .agent(cfg)
        .with_verifier(verifier.clone())
        .run(&mut conv, &CancellationToken::new())
        .await;
    assert_eq!(out.stop, StopReason::EndTurn);
    assert_eq!(out.verification, Verification::Verified);
    assert_eq!(out.verified, Some(true));
    assert_eq!(out.fix_attempts, 1);
    assert_eq!(*verifier.runs.lock().unwrap(), 2);
    // The failure was sent to the model as a user message.
    let feedback = h.provider.requests()[2].messages.last().unwrap().clone();
    assert_eq!(feedback.role, Role::User);
    let text = feedback.text();
    assert!(
        text.contains("Verification failed (fix attempt 1 of 3)"),
        "{text}"
    );
    assert!(text.contains("parser::empty ... FAILED"));
    let events = h.events().await;
    assert_eq!(node_finished(&events), (NodeStatus::Succeeded, None));
    let verifies = events
        .iter()
        .filter(|e| matches!(e.kind, EventKind::VerifyFinished { .. }))
        .count();
    assert_eq!(verifies, 2);
}

#[tokio::test]
async fn verification_that_keeps_failing_ends_failed_after_max_fix_attempts() {
    let h = Harness::new(change_then_done(2)).await;
    let mut cfg = config();
    cfg.max_fix_attempts = 2;
    let verifier = Arc::new(Flaky::new(10));
    let mut conv = Conversation::new(None);
    conv.push_user("fix the parser");
    let out = h
        .agent(cfg)
        .with_verifier(verifier.clone())
        .run(&mut conv, &CancellationToken::new())
        .await;
    assert_eq!(out.stop, StopReason::EndTurn);
    assert_eq!(out.verification, Verification::Failed);
    assert_eq!(out.fix_attempts, 2);
    assert_eq!(*verifier.runs.lock().unwrap(), 3);
    let events = h.events().await;
    let (status, error) = node_finished(&events);
    assert_eq!(status, NodeStatus::Failed);
    let error = error.unwrap();
    assert!(error.starts_with(codes::VERIFY_FAILED), "{error}");
    // The map's reducer sees the failed verification on the node.
    let state = harness_events::reducer::RunState::from_events(&events);
    let node = state.node(&out.node).unwrap();
    assert_eq!(node.status, NodeStatus::Failed);
    assert_eq!(node.last_verify_passed, Some(false));
    // The stop reason still round-trips as an end of turn.
    assert_eq!(
        StopReason::from_node_finished(status, Some(&error)),
        StopReason::EndTurn
    );
}

#[tokio::test]
async fn no_changes_means_unverified_when_verifying_only_on_changes() {
    let h = Harness::new(vec![FakeTurn::text("Nothing to change.")]).await;
    let mut cfg = config();
    cfg.verify_when = VerifyWhen::OnChanges;
    cfg.max_fix_attempts = 3;
    let verifier = Arc::new(Flaky::new(0));
    let mut conv = Conversation::new(None);
    conv.push_user("look around");
    let out = h
        .agent(cfg)
        .with_verifier(verifier.clone())
        .run(&mut conv, &CancellationToken::new())
        .await;
    assert_eq!(out.verification, Verification::Unverified);
    assert_eq!(out.verified, None);
    assert_eq!(*verifier.runs.lock().unwrap(), 0);
    assert_eq!(Verification::Unverified.to_string(), "unverified");

    // Without a verifier the result is unverified too.
    let h = Harness::new(change_then_done(0)).await;
    let mut conv = Conversation::new(None);
    conv.push_user("fix");
    let out = h
        .agent(config())
        .run(&mut conv, &CancellationToken::new())
        .await;
    assert_eq!(out.verification, Verification::Unverified);
}
