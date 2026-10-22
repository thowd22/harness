//! Spend budgets: per session, run and day (`[budget]`), plus agent and
//! workflow budgets from YAML.
//!
//! A [`BudgetMeter`] holds the limits and what has been spent in each
//! [`BudgetScope`]. It is shared (cheap to clone) by every agent loop of a
//! session, so subagents spend from the same session and day budgets. Each
//! loop gets a [`BudgetGuard`] request hook that charges the loop's new
//! spend before every request and then:
//!
//! - emits a one-time warning when a scope reaches [`WARN_AT`] (80%) of its
//!   limit,
//! - at the limit, either stops the loop (`on_exceed = stop`, giving
//!   [`StopReason::Budget`]) or asks a [`BudgetApprover`] (`on_exceed =
//!   ask`). An approval raises that scope's limit by its original amount;
//!   a refusal stops the loop.
//!
//! Costs that are unknown (unpriced models) are not counted; the cost views
//! show them as unknown, never as zero.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex, MutexGuard};

use async_trait::async_trait;
use harness_config::ini::config::{Budget as IniBudget, OnExceed};
use harness_events::schema::{EventKind, NodeId};
use harness_events::store::EventDraft;

use crate::agent_loop::{HookDecision, RequestContext, RequestHook, StopReason};

/// Fraction of a limit at which the soft warning is emitted.
pub const WARN_AT: f64 = 0.8;

/// What a budget limits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum BudgetScope {
    /// One agent node (agent YAML `budget.usd`).
    Agent,
    /// One workflow run (workflow YAML `budget.usd`).
    Workflow,
    /// One run (`[budget] run_usd`).
    Run,
    /// One session (`[budget] session_usd`).
    Session,
    /// One UTC day across all sessions (`[budget] daily_usd`).
    Day,
}

impl BudgetScope {
    /// Every scope.
    pub const ALL: [BudgetScope; 5] = [
        BudgetScope::Agent,
        BudgetScope::Workflow,
        BudgetScope::Run,
        BudgetScope::Session,
        BudgetScope::Day,
    ];

    /// `agent`, `workflow`, `run`, `session` or `daily`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Agent => "agent",
            Self::Workflow => "workflow",
            Self::Run => "run",
            Self::Session => "session",
            Self::Day => "daily",
        }
    }
}

/// Limits per scope and the overrun behaviour.
#[derive(Debug, Clone, PartialEq)]
pub struct Budgets {
    limits: BTreeMap<BudgetScope, f64>,
    /// `ask` or `stop` at a limit.
    pub on_exceed: OnExceed,
}

impl Default for Budgets {
    fn default() -> Self {
        Self {
            limits: BTreeMap::new(),
            on_exceed: OnExceed::Ask,
        }
    }
}

impl Budgets {
    /// No limits, `on_exceed = ask`.
    pub fn new() -> Self {
        Self::default()
    }

    /// From `[budget]`: session, run and daily limits.
    pub fn from_config(cfg: &IniBudget) -> Self {
        Self {
            limits: BTreeMap::new(),
            on_exceed: cfg.on_exceed,
        }
        .with_limit(BudgetScope::Session, cfg.session_usd)
        .with_limit(BudgetScope::Run, cfg.run_usd)
        .with_limit(BudgetScope::Day, cfg.daily_usd)
    }

    /// Sets (or with `None`, clears) the limit of `scope`.
    pub fn with_limit(mut self, scope: BudgetScope, usd: Option<f64>) -> Self {
        match usd {
            Some(v) if v.is_finite() && v >= 0.0 => {
                self.limits.insert(scope, v);
            }
            _ => {
                self.limits.remove(&scope);
            }
        }
        self
    }

    /// Sets the overrun behaviour.
    pub fn with_on_exceed(mut self, on_exceed: OnExceed) -> Self {
        self.on_exceed = on_exceed;
        self
    }

    /// The limit of `scope`.
    pub fn limit(&self, scope: BudgetScope) -> Option<f64> {
        self.limits.get(&scope).copied()
    }
}

/// A scope at or past its soft threshold.
#[derive(Debug, Clone, PartialEq)]
pub struct BudgetWarning {
    /// Scope.
    pub scope: BudgetScope,
    /// Spent so far.
    pub spent: f64,
    /// Its limit.
    pub limit: f64,
}

impl std::fmt::Display for BudgetWarning {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "budget warning: {} spend ${:.2} is {:.0}% of the ${:.2} budget",
            self.scope.as_str(),
            self.spent,
            pct(self.spent, self.limit),
            self.limit
        )
    }
}

/// A scope at or past its limit.
#[derive(Debug, Clone, PartialEq)]
pub struct BudgetExceeded {
    /// Scope.
    pub scope: BudgetScope,
    /// Spent so far.
    pub spent: f64,
    /// The limit in force.
    pub limit: f64,
}

impl std::fmt::Display for BudgetExceeded {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} budget of ${:.2} reached (${:.4} spent)",
            self.scope.as_str(),
            self.limit,
            self.spent
        )
    }
}

fn pct(spent: f64, limit: f64) -> f64 {
    if limit > 0.0 {
        spent / limit * 100.0
    } else {
        100.0
    }
}

/// Result of [`BudgetMeter::check`].
#[derive(Debug, Clone, PartialEq)]
pub struct BudgetCheck {
    /// New soft warnings (each scope warns once per limit).
    pub warnings: Vec<BudgetWarning>,
    /// The first exceeded scope, if any.
    pub exceeded: Option<BudgetExceeded>,
}

#[derive(Debug, Default)]
struct Meter {
    budgets: Budgets,
    spent: BTreeMap<BudgetScope, f64>,
    raised: BTreeMap<BudgetScope, f64>,
    warned: BTreeSet<(BudgetScope, u64)>,
}

impl Meter {
    fn limit(&self, scope: BudgetScope) -> Option<f64> {
        self.budgets
            .limit(scope)
            .map(|l| l + self.raised.get(&scope).copied().unwrap_or(0.0))
    }
}

/// Shared spend meter (see the module docs).
#[derive(Debug, Clone, Default)]
pub struct BudgetMeter {
    inner: Arc<Mutex<Meter>>,
}

impl BudgetMeter {
    /// A meter for `budgets` with nothing spent.
    pub fn new(budgets: Budgets) -> Self {
        Self {
            inner: Arc::new(Mutex::new(Meter {
                budgets,
                ..Meter::default()
            })),
        }
    }

    fn lock(&self) -> MutexGuard<'_, Meter> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Sets what `scope` already spent (e.g. the day so far from
    /// [`crate::cost::daily_spend`], or a resumed session's cost).
    pub fn with_spent(self, scope: BudgetScope, usd: f64) -> Self {
        self.lock().spent.insert(scope, usd);
        self
    }

    /// Adds `usd` to every shared scope (all but `Agent`, which each
    /// [`BudgetGuard`] meters for its own loop).
    pub fn charge(&self, usd: f64) {
        if usd <= 0.0 || !usd.is_finite() {
            return;
        }
        let mut m = self.lock();
        for scope in BudgetScope::ALL {
            if scope != BudgetScope::Agent {
                *m.spent.entry(scope).or_insert(0.0) += usd;
            }
        }
    }

    /// Starts a new run: the `Run` scope restarts at zero.
    pub fn start_run(&self) {
        let mut m = self.lock();
        m.spent.insert(BudgetScope::Run, 0.0);
        m.raised.remove(&BudgetScope::Run);
        m.warned.retain(|(s, _)| *s != BudgetScope::Run);
    }

    /// Spent in `scope`.
    pub fn spent(&self, scope: BudgetScope) -> f64 {
        self.lock().spent.get(&scope).copied().unwrap_or(0.0)
    }

    /// The limit in force for `scope` (including approved raises).
    pub fn limit(&self, scope: BudgetScope) -> Option<f64> {
        self.lock().limit(scope)
    }

    /// The overrun behaviour.
    pub fn on_exceed(&self) -> OnExceed {
        self.lock().budgets.on_exceed
    }

    /// Raises `scope`'s limit by its original amount (after an approval).
    pub fn extend(&self, scope: BudgetScope) {
        let mut m = self.lock();
        if let Some(base) = m.budgets.limit(scope) {
            let step = if base > 0.0 { base } else { 1.0 };
            *m.raised.entry(scope).or_insert(0.0) += step;
        }
    }

    /// Checks every shared scope (all but `Agent`, which each
    /// [`BudgetGuard`] checks for its own loop).
    pub fn check(&self) -> BudgetCheck {
        let mut m = self.lock();
        let mut warnings = Vec::new();
        let mut exceeded = None;
        for scope in BudgetScope::ALL {
            if scope == BudgetScope::Agent {
                continue;
            }
            let Some(limit) = m.limit(scope) else {
                continue;
            };
            let spent = m.spent.get(&scope).copied().unwrap_or(0.0);
            if spent >= limit {
                exceeded.get_or_insert(BudgetExceeded {
                    scope,
                    spent,
                    limit,
                });
            } else if spent >= limit * WARN_AT && m.warned.insert((scope, limit.to_bits())) {
                warnings.push(BudgetWarning {
                    scope,
                    spent,
                    limit,
                });
            }
        }
        BudgetCheck { warnings, exceeded }
    }
}

/// Decides whether to continue past a limit when `on_exceed = ask`.
#[async_trait]
pub trait BudgetApprover: Send + Sync {
    /// True to raise the limit and continue.
    async fn approve(&self, exceeded: &BudgetExceeded) -> bool;
}

/// Refuses every overrun (headless runs).
#[derive(Debug, Clone, Copy, Default)]
pub struct DenyOverruns;

#[async_trait]
impl BudgetApprover for DenyOverruns {
    async fn approve(&self, _exceeded: &BudgetExceeded) -> bool {
        false
    }
}

#[derive(Debug, Default)]
struct Seen {
    node: Option<NodeId>,
    cost: f64,
    agent_raised: f64,
    agent_warned: bool,
}

/// Request hook enforcing a [`BudgetMeter`] for one agent loop.
pub struct BudgetGuard {
    meter: BudgetMeter,
    approver: Arc<dyn BudgetApprover>,
    agent_limit: Option<f64>,
    seen: Mutex<Seen>,
}

impl std::fmt::Debug for BudgetGuard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BudgetGuard")
            .field("meter", &self.meter)
            .finish_non_exhaustive()
    }
}

impl BudgetGuard {
    /// A guard charging `meter` and asking `approver` on overruns.
    pub fn new(meter: BudgetMeter, approver: Arc<dyn BudgetApprover>) -> Self {
        let agent_limit = meter.lock().budgets.limit(BudgetScope::Agent);
        Self {
            meter,
            approver,
            agent_limit,
            seen: Mutex::new(Seen::default()),
        }
    }

    /// Limits this loop's own spend (agent YAML `budget.usd`), overriding
    /// the meter's `Agent` limit.
    pub fn with_agent_limit(mut self, usd: Option<f64>) -> Self {
        if usd.is_some() {
            self.agent_limit = usd;
        }
        self
    }

    /// Checks the `Agent` scope for this loop.
    fn check_agent(&self, spent: f64) -> BudgetCheck {
        let mut out = BudgetCheck {
            warnings: Vec::new(),
            exceeded: None,
        };
        let Some(base) = self.agent_limit else {
            return out;
        };
        let mut seen = self.seen.lock().unwrap_or_else(|e| e.into_inner());
        let limit = base + seen.agent_raised;
        if spent >= limit {
            out.exceeded = Some(BudgetExceeded {
                scope: BudgetScope::Agent,
                spent,
                limit,
            });
        } else if spent >= limit * WARN_AT && !seen.agent_warned {
            seen.agent_warned = true;
            out.warnings.push(BudgetWarning {
                scope: BudgetScope::Agent,
                spent,
                limit,
            });
        }
        out
    }

    fn extend(&self, scope: BudgetScope) {
        if scope == BudgetScope::Agent {
            let mut seen = self.seen.lock().unwrap_or_else(|e| e.into_inner());
            let base = self.agent_limit.unwrap_or(0.0);
            seen.agent_raised += if base > 0.0 { base } else { 1.0 };
            seen.agent_warned = false;
        } else {
            self.meter.extend(scope);
        }
    }

    /// The shared meter.
    pub fn meter(&self) -> &BudgetMeter {
        &self.meter
    }

    /// Charges the loop's spend since the last call; returns the loop's
    /// own spend (for the `Agent` scope).
    fn charge(&self, node: &NodeId, run_cost: f64) -> f64 {
        let mut seen = self.seen.lock().unwrap_or_else(|e| e.into_inner());
        if seen.node.as_ref() != Some(node) {
            *seen = Seen {
                node: Some(node.clone()),
                ..Seen::default()
            };
        }
        let delta = run_cost - seen.cost;
        if delta > 0.0 {
            self.meter.charge(delta);
            seen.cost = run_cost;
        }
        run_cost
    }
}

fn note(ctx: &RequestContext<'_>, message: String) {
    let draft = EventDraft::node(
        ctx.node.clone(),
        EventKind::Error {
            message,
            recoverable: true,
            action: None,
        },
    );
    if let Err(e) = ctx.recorder.emit(draft) {
        tracing::warn!(target: "harness::agent", error = %e, "could not record budget event");
    }
}

#[async_trait]
impl RequestHook for BudgetGuard {
    fn name(&self) -> &str {
        "budget"
    }

    async fn before_request(&self, ctx: &mut RequestContext<'_>) -> HookDecision {
        let agent_spent = self.charge(ctx.node, ctx.run_cost);
        loop {
            let agent = self.check_agent(agent_spent);
            let shared = self.meter.check();
            for w in agent.warnings.iter().chain(&shared.warnings) {
                note(ctx, w.to_string());
            }
            let Some(exceeded) = agent.exceeded.or(shared.exceeded) else {
                return HookDecision::Continue;
            };
            let approved = match self.meter.on_exceed() {
                OnExceed::Stop => false,
                OnExceed::Ask => self.approver.approve(&exceeded).await,
            };
            if !approved {
                return HookDecision::Stop(StopReason::Budget {
                    reason: exceeded.to_string(),
                });
            }
            note(
                ctx,
                format!("{exceeded}; continuing after approval with a raised limit"),
            );
            self.extend(exceeded.scope);
        }
    }
}

#[cfg(test)]
mod tests;
