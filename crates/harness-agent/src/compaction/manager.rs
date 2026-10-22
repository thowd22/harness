//! The [`ContextManager`] request hook.

use std::sync::{Arc, Mutex, MutexGuard};

use async_trait::async_trait;
use harness_events::schema::{EventKind, NodeId};
use harness_events::store::{EventDraft, RunRecorder};
use tokio_util::sync::CancellationToken;

use super::elide::{Elider, ElisionReport};
use super::handoff::{CompactionError, CompactionReport, Compactor};
use crate::agent_loop::{CallReport, Conversation, HookDecision, RequestContext, RequestHook};
use crate::context::{ContextTracker, ContextUsage, ReasoningRetention, cache_hit_rate_of};

/// Context settings of one agent (`[context]` plus the model's window).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ContextSettings {
    /// Compact when the next request is estimated at this fraction of the
    /// window. Default 0.8.
    pub compact_at: f64,
    /// Assistant turns kept verbatim on compaction. Default 6.
    pub keep_recent_turns: u32,
    /// Elide tool results larger than this many tokens. Default 4000.
    pub elide_tool_results_over: u64,
    /// Elide only results at least this many turns old. Default: same as
    /// `keep_recent_turns`.
    pub elide_after_turns: u32,
    /// Reasoning retention. Default keep everything.
    pub reasoning: ReasoningRetention,
    /// The model's context window in tokens.
    pub context_window: u32,
}

impl ContextSettings {
    /// Defaults for a model with `context_window` tokens.
    pub fn new(context_window: u32) -> Self {
        Self {
            compact_at: 0.8,
            keep_recent_turns: 6,
            elide_tool_results_over: 4000,
            elide_after_turns: 6,
            reasoning: ReasoningRetention::KeepAll,
            context_window,
        }
    }

    /// From `[context]` in `harness.ini`.
    pub fn from_config(cfg: &harness_config::ini::config::Context, context_window: u32) -> Self {
        Self {
            compact_at: cfg.compact_at,
            keep_recent_turns: cfg.keep_recent_turns,
            elide_tool_results_over: cfg.elide_tool_results_over,
            elide_after_turns: cfg.keep_recent_turns,
            ..Self::new(context_window)
        }
    }

    /// The token count at which compaction starts.
    pub fn compact_threshold(&self) -> u64 {
        (f64::from(self.context_window) * self.compact_at.clamp(0.0, 1.0)) as u64
    }
}

#[derive(Debug)]
struct State {
    tracker: ContextTracker,
    requested: Option<Option<String>>,
    compactions: Vec<CompactionReport>,
    elisions: Vec<ElisionReport>,
    failed_at_messages: Option<usize>,
}

/// Tracks context use and keeps the history within the window: reasoning
/// retention, batched tool-result elision and compaction, in that order,
/// before every request. One per agent loop (node).
pub struct ContextManager {
    settings: ContextSettings,
    elider: Option<Elider>,
    compactor: Option<Compactor>,
    state: Mutex<State>,
}

impl std::fmt::Debug for ContextManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ContextManager")
            .field("settings", &self.settings)
            .field("elider", &self.elider)
            .field("compactor", &self.compactor)
            .finish_non_exhaustive()
    }
}

impl ContextManager {
    /// A manager with `settings` and neither elision nor compaction.
    pub fn new(settings: ContextSettings) -> Self {
        Self {
            settings,
            elider: None,
            compactor: None,
            state: Mutex::new(State {
                tracker: ContextTracker::new(settings.context_window),
                requested: None,
                compactions: Vec::new(),
                elisions: Vec::new(),
                failed_at_messages: None,
            }),
        }
    }

    /// Enables elision, storing full results in `sink` (the read tool's
    /// spill sink, so `spill:<id>` resolves).
    pub fn with_elision(mut self, sink: Arc<dyn harness_tools::spill::BlobSink>) -> Self {
        self.elider = Some(Elider::new(
            self.settings.elide_tool_results_over,
            self.settings.elide_after_turns,
            sink,
        ));
        self
    }

    /// Enables compaction with `compactor`.
    pub fn with_compactor(mut self, compactor: Compactor) -> Self {
        self.compactor = Some(compactor);
        self
    }

    /// The settings.
    pub fn settings(&self) -> &ContextSettings {
        &self.settings
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Asks for compaction before the next request (`/compact [focus]`).
    pub fn request_compaction(&self, instructions: Option<String>) {
        self.state().requested = Some(instructions);
    }

    /// True when a compaction is pending.
    pub fn compaction_requested(&self) -> bool {
        self.state().requested.is_some()
    }

    /// The latest context estimate of this node.
    pub fn usage(&self) -> Option<ContextUsage> {
        self.state().tracker.last()
    }

    /// Compactions done so far.
    pub fn compactions(&self) -> Vec<CompactionReport> {
        self.state().compactions.clone()
    }

    /// Elision passes that changed something.
    pub fn elisions(&self) -> Vec<ElisionReport> {
        self.state().elisions.clone()
    }

    /// Compacts now, outside a loop run (`/compact` while idle).
    pub async fn compact_now(
        &self,
        conv: &mut Conversation,
        recorder: &RunRecorder,
        node: &NodeId,
        instructions: Option<&str>,
        cancel: &CancellationToken,
    ) -> Result<CompactionReport, CompactionError> {
        let compactor = self
            .compactor
            .as_ref()
            .ok_or(CompactionError::NothingToCompact)?;
        let report = compactor
            .compact(conv, recorder, node, instructions, cancel)
            .await?;
        let mut st = self.state();
        st.tracker.invalidate();
        st.requested = None;
        st.compactions.push(report.clone());
        Ok(report)
    }

    fn note(recorder: &RunRecorder, node: &NodeId, message: String) {
        let draft = EventDraft::node(
            node.clone(),
            EventKind::Error {
                message,
                recoverable: true,
                action: None,
            },
        );
        if let Err(e) = recorder.emit(draft) {
            tracing::warn!(target: "harness::agent", error = %e, "could not record context event");
        }
    }
}

#[async_trait]
impl RequestHook for ContextManager {
    fn name(&self) -> &str {
        "context"
    }

    async fn before_request(&self, ctx: &mut RequestContext<'_>) -> HookDecision {
        if self.settings.reasoning.apply_to(ctx).is_some() {
            self.state().tracker.invalidate();
        }
        if let Some(elider) = &self.elider {
            match elider.elide(&mut ctx.conversation.messages) {
                Ok(report) => {
                    if let Some(first) = report.first_changed {
                        ctx.conversation.invalidate_log_from(first);
                        let mut st = self.state();
                        st.tracker.invalidate();
                        st.elisions.push(report);
                    }
                }
                Err(e) => tracing::warn!(target: "harness::agent", error = %e, "elision failed"),
            }
        }
        let (usage, requested) = {
            let mut st = self.state();
            let usage = st.tracker.estimate(
                ctx.conversation.system.as_deref(),
                ctx.tools,
                &ctx.conversation.messages,
            );
            (usage, st.requested.clone())
        };
        let over =
            self.settings.context_window > 0 && usage.tokens >= self.settings.compact_threshold();
        let Some(compactor) = &self.compactor else {
            return HookDecision::Continue;
        };
        // Do not retry a failed automatic compaction until the history grew.
        let retry_blocked = requested.is_none()
            && self.state().failed_at_messages == Some(ctx.conversation.messages.len());
        if (!over && requested.is_none()) || retry_blocked {
            return HookDecision::Continue;
        }
        let instructions = requested.flatten();
        match compactor
            .compact(
                ctx.conversation,
                ctx.recorder,
                ctx.node,
                instructions.as_deref(),
                ctx.cancel,
            )
            .await
        {
            Ok(report) => {
                ctx.charge(report.usage, report.cost_usd);
                let mut st = self.state();
                st.tracker.invalidate();
                st.requested = None;
                st.failed_at_messages = None;
                st.compactions.push(report);
            }
            Err(CompactionError::NothingToCompact) => {
                self.state().requested = None;
            }
            Err(e) => {
                let mut st = self.state();
                st.requested = None;
                st.failed_at_messages = Some(ctx.conversation.messages.len());
                drop(st);
                Self::note(ctx.recorder, ctx.node, e.to_string());
            }
        }
        HookDecision::Continue
    }

    fn after_call(&self, report: &CallReport) {
        self.state().tracker.observe(report);
        tracing::debug!(
            target: "harness::agent",
            call = %report.call_id,
            input = report.usage.total_input(),
            cache_hit_rate = cache_hit_rate_of(&report.usage).unwrap_or(0.0),
            "context after call"
        );
    }
}
