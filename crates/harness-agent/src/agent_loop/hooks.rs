//! Extension points of the agent loop.
//!
//! A [`RequestHook`] runs in the `Request` state, after the history has been
//! repaired and before the [`ChatRequest`](harness_providers::model::ChatRequest)
//! is built. Hooks may rewrite the [`Conversation`] (tool-result elision,
//! compaction), charge extra usage (a summarization call) or stop the loop
//! (budgets). After every successful model call the loop reports the
//! provider's usage through [`RequestHook::after_call`], which is how the
//! context tracker learns the real request size.
//!
//! Hooks run in registration order. A hook that rewrites earlier messages
//! must call [`Conversation::invalidate_log_from`] so the next
//! `model_call_started` re-records them and session resume stays exact.

use async_trait::async_trait;
use harness_events::schema::NodeId;
use harness_events::store::RunRecorder;
use harness_providers::model::{ToolSpec, Usage};
use tokio_util::sync::CancellationToken;

use super::{Conversation, StopReason};

/// What a hook decided.
#[derive(Debug, Clone, PartialEq)]
pub enum HookDecision {
    /// Send the request.
    Continue,
    /// End the loop with this reason (no request is sent).
    Stop(StopReason),
}

/// The state a [`RequestHook`] sees before a request.
pub struct RequestContext<'a> {
    /// The history about to be sent. Hooks may rewrite it.
    pub conversation: &'a mut Conversation,
    /// The agent node.
    pub node: &'a NodeId,
    /// The run recorder (for events such as a compaction child node).
    pub recorder: &'a RunRecorder,
    /// Provider name.
    pub provider: &'a str,
    /// Model id the request goes to.
    pub model: &'a str,
    /// Tool specs the request will carry.
    pub tools: &'a [ToolSpec],
    /// Model calls made so far in this run.
    pub turn: u32,
    /// Usage summed over this run so far.
    pub run_usage: Usage,
    /// Known cost of this run so far in USD.
    pub run_cost: f64,
    /// Cancellation of the run.
    pub cancel: &'a CancellationToken,
    pub(super) charged_usage: Usage,
    pub(super) charged_cost: f64,
    pub(super) charged_unknown: bool,
}

impl std::fmt::Debug for RequestContext<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RequestContext")
            .field("node", self.node)
            .field("model", &self.model)
            .field("turn", &self.turn)
            .field("messages", &self.conversation.messages.len())
            .finish_non_exhaustive()
    }
}

impl RequestContext<'_> {
    /// Adds usage and cost spent by the hook itself (e.g. a summarization
    /// call) to the run's totals, so budgets see it. `cost = None` marks the
    /// run's cost as partly unknown.
    pub fn charge(&mut self, usage: Usage, cost: Option<f64>) {
        self.charged_usage += usage;
        self.run_usage += usage;
        match cost {
            Some(usd) => {
                self.charged_cost += usd;
                self.run_cost += usd;
            }
            None => self.charged_unknown = true,
        }
    }
}

/// A finished model call, reported to [`RequestHook::after_call`].
#[derive(Debug, Clone, PartialEq)]
pub struct CallReport {
    /// The call id used in the event log.
    pub call_id: String,
    /// Provider-reported usage of the call.
    pub usage: Usage,
    /// Number of conversation messages the request carried.
    pub sent_messages: usize,
    /// Number of conversation messages after the response was appended.
    pub messages_after: usize,
}

/// A hook run before every model request (see the module docs).
#[async_trait]
pub trait RequestHook: Send + Sync {
    /// Short name for logs.
    fn name(&self) -> &str;

    /// Runs before the request is built.
    async fn before_request(&self, ctx: &mut RequestContext<'_>) -> HookDecision;

    /// Runs after a successful model call. The default does nothing.
    fn after_call(&self, _report: &CallReport) {}
}
