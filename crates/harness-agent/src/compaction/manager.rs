//! The [`ContextManager`] request hook.

use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::{Arc, Mutex, MutexGuard};
use std::task::Poll;

use async_trait::async_trait;
use harness_events::schema::{EventKind, NodeId};
use harness_events::store::{EventDraft, RunRecorder};
use harness_providers::model::{ContentBlock, Message};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use super::elide::{Elider, ElisionReport};
use super::handoff::{CompactionError, CompactionReport, Compactor, Summary};
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
    /// Background compaction (`[context] background_compaction`): start the
    /// handoff summary when the next request reaches this fraction of the
    /// window and swap it in at `compact_at`. `None` (the default)
    /// summarizes in-line at `compact_at`.
    pub background_at: Option<f64>,
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
            background_at: None,
        }
    }

    /// From `[context]` in `harness.ini`.
    pub fn from_config(cfg: &harness_config::ini::config::Context, context_window: u32) -> Self {
        Self {
            compact_at: cfg.compact_at,
            keep_recent_turns: cfg.keep_recent_turns,
            elide_tool_results_over: cfg.elide_tool_results_over,
            elide_after_turns: cfg.keep_recent_turns,
            background_at: cfg
                .background_compaction
                .then_some(cfg.background_compact_at),
            ..Self::new(context_window)
        }
    }

    /// The token count at which compaction starts.
    pub fn compact_threshold(&self) -> u64 {
        (f64::from(self.context_window) * self.compact_at.clamp(0.0, 1.0)) as u64
    }

    /// The token count at which a background summary starts, when
    /// background compaction is on (never above [`Self::compact_threshold`]).
    pub fn background_threshold(&self) -> Option<u64> {
        let at = self.background_at?.clamp(0.0, 1.0);
        Some(((f64::from(self.context_window) * at) as u64).min(self.compact_threshold()))
    }
}

/// A summary running (or finished) in the background.
#[derive(Debug)]
struct Background {
    /// Index where the verbatim tail starts.
    cut: usize,
    /// [`fingerprint`] of `messages[..cut]` when the summary started.
    fingerprint: u64,
    /// Cancels the call (a child of the run's token).
    cancel: CancellationToken,
    state: BackgroundState,
}

#[derive(Debug)]
enum BackgroundState {
    Running(JoinHandle<Result<Summary, CompactionError>>),
    Done(Box<Result<Summary, CompactionError>>),
}

impl Background {
    /// Waits for the result. The call runs under a child of the run's
    /// token, so cancelling the run ends it (recorded as cancelled).
    async fn wait(self) -> Result<Summary, CompactionError> {
        match self.state {
            BackgroundState::Done(r) => *r,
            BackgroundState::Running(h) => h
                .await
                .unwrap_or_else(|e| Err(CompactionError::Background(e.to_string()))),
        }
    }

    /// The finished summary, if it succeeded.
    fn summary(&self) -> Option<&Summary> {
        match &self.state {
            BackgroundState::Done(r) => r.as_ref().as_ref().ok(),
            BackgroundState::Running(_) => None,
        }
    }

    /// Moves a finished task's result into [`BackgroundState::Done`].
    fn settle(&mut self) {
        if let BackgroundState::Running(h) = &mut self.state
            && h.is_finished()
        {
            let r = futures::FutureExt::now_or_never(h)
                .map(|r| r.unwrap_or_else(|e| Err(CompactionError::Background(e.to_string()))));
            if let Some(r) = r {
                self.state = BackgroundState::Done(Box::new(r));
            }
        }
    }

    /// Cancels it without waiting (a running call records its node as
    /// cancelled when it next runs).
    fn discard(self) {
        self.cancel.cancel();
    }

    /// Cancels it and waits until a running call has recorded its end; the
    /// result, if any, is dropped.
    async fn stop(self) {
        self.cancel.cancel();
        if let BackgroundState::Running(h) = self.state {
            let _ = h.await;
        }
    }
}

/// Structural fingerprint of `messages`: roles, text, tool calls and
/// result ids. Tool-result content and reasoning are left out, because
/// elision and reasoning retention rewrite them without changing what the
/// summary covers.
fn fingerprint(messages: &[Message]) -> u64 {
    let mut h = DefaultHasher::new();
    messages.len().hash(&mut h);
    for m in messages {
        format!("{:?}", m.role).hash(&mut h);
        for b in &m.content {
            match b {
                ContentBlock::Text { text } => ("t", text).hash(&mut h),
                ContentBlock::Image { .. } => "i".hash(&mut h),
                ContentBlock::ToolUse { id, name, input } => {
                    ("u", id, name, input.to_string()).hash(&mut h);
                }
                ContentBlock::ToolResult {
                    tool_use_id,
                    is_error,
                    ..
                } => ("r", tool_use_id, is_error).hash(&mut h),
                _ => {}
            }
        }
    }
    h.finish()
}

#[derive(Debug)]
struct State {
    tracker: ContextTracker,
    requested: Option<Option<String>>,
    compactions: Vec<CompactionReport>,
    elisions: Vec<ElisionReport>,
    failed_at_messages: Option<usize>,
    background: Option<Background>,
    /// A background summary failed; compaction stays in-line until the
    /// next one succeeds.
    background_failed: bool,
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
                background: None,
                background_failed: false,
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
        self.discard_background(None).await;
        let report = compactor
            .compact(conv, recorder, node, instructions, cancel)
            .await?;
        let mut st = self.state();
        st.tracker.invalidate();
        st.requested = None;
        st.compactions.push(report.clone());
        Ok(report)
    }

    /// True while a background summary is running or waiting to be
    /// swapped in.
    pub fn background_pending(&self) -> bool {
        self.state().background.is_some()
    }

    /// Stops a pending background summary and waits until its node has
    /// recorded its end (cancelled, or finished with its cost). Call it
    /// before the run log closes, so no compaction node is left open.
    pub async fn stop_background(&self) {
        if let Some(bg) = self.take_background() {
            bg.stop().await;
        }
    }

    fn take_background(&self) -> Option<Background> {
        self.state().background.take()
    }

    /// Drops a pending background summary. A finished one is charged to
    /// the run when `ctx` is given (its tokens were spent).
    async fn discard_background(&self, ctx: Option<&mut RequestContext<'_>>) {
        let Some(mut bg) = self.take_background() else {
            return;
        };
        bg.settle();
        if let (Some(ctx), Some(summary)) = (ctx, bg.summary()) {
            ctx.charge(summary.usage(), summary.cost_usd());
        }
        bg.stop().await;
    }

    /// Below `compact_at`: drops a background summary that failed or no
    /// longer matches the history, and starts one at the background
    /// threshold.
    async fn background_step(
        &self,
        compactor: &Compactor,
        ctx: &mut RequestContext<'_>,
        tokens: u64,
    ) {
        let Some(at) = self.settings.background_threshold() else {
            return;
        };
        let pending = {
            let mut st = self.state();
            if let Some(bg) = &mut st.background {
                bg.settle();
            }
            st.background.take()
        };
        if let Some(bg) = pending {
            let msgs = &ctx.conversation.messages;
            let stale = msgs.len() <= bg.cut || fingerprint(&msgs[..bg.cut]) != bg.fingerprint;
            match &bg.state {
                _ if stale => self.drop_stale(bg, ctx).await,
                // Cancelled with an earlier run: it never finishes; start over.
                BackgroundState::Running(_) if bg.cancel.is_cancelled() => bg.stop().await,
                BackgroundState::Done(r) if r.is_err() => {
                    if let Err(e) = r.as_ref()
                        && !e.is_cancelled()
                    {
                        tracing::warn!(target: "harness::agent", error = %e, "background compaction failed; compacting in-line at compact_at");
                        self.state().background_failed = true;
                    }
                }
                _ => {
                    self.state().background = Some(bg);
                    return;
                }
            }
        }
        if self.settings.context_window == 0
            || tokens < at
            || self.state().background_failed
            || ctx.cancel.is_cancelled()
        {
            return;
        }
        let Ok(call) = compactor.start(&ctx.conversation.messages, ctx.recorder, ctx.node, None)
        else {
            return;
        };
        let cut = call.cut;
        let fingerprint = fingerprint(&ctx.conversation.messages[..cut]);
        let cancel = ctx.cancel.child_token();
        let mut fut = Box::pin(call.run(cancel.clone()));
        // Poll once here, so the request goes out before the agent's own
        // request (a deterministic order for scripted providers); the rest
        // runs in the background.
        let state = match futures::poll!(fut.as_mut()) {
            Poll::Ready(r) => BackgroundState::Done(Box::new(r)),
            Poll::Pending => BackgroundState::Running(tokio::spawn(fut)),
        };
        tracing::debug!(target: "harness::agent", cut, tokens, "background compaction started");
        self.state().background = Some(Background {
            cut,
            fingerprint,
            cancel,
            state,
        });
    }

    /// Drops `bg` (stale), charging it when it had finished.
    async fn drop_stale(&self, bg: Background, ctx: &mut RequestContext<'_>) {
        if let Some(summary) = bg.summary() {
            ctx.charge(summary.usage(), summary.cost_usd());
        }
        tracing::debug!(target: "harness::agent", "history changed; background summary dropped");
        bg.stop().await;
    }

    /// Waits for `bg` and swaps it in. False when it failed, was cancelled
    /// or no longer matches the history (the caller then compacts in-line).
    async fn swap_background(
        &self,
        compactor: &Compactor,
        ctx: &mut RequestContext<'_>,
        bg: Background,
    ) -> bool {
        let (cut, expected) = (bg.cut, bg.fingerprint);
        // The summary may have been started by an earlier turn, under that
        // turn's token: watch this turn's token too, so cancelling it while
        // waiting here stops the summary right away (as in-line does).
        let stopper = bg.cancel.clone();
        let mut wait = Box::pin(bg.wait());
        let result = tokio::select! {
            r = wait.as_mut() => r,
            () = ctx.cancel.cancelled() => {
                stopper.cancel();
                // Await its end, so the node is recorded as cancelled.
                wait.await
            }
        };
        let summary = match result {
            Ok(s) => s,
            Err(e) => {
                if !e.is_cancelled() {
                    tracing::warn!(target: "harness::agent", error = %e, "background compaction failed; compacting in-line");
                }
                return false;
            }
        };
        ctx.charge(summary.usage(), summary.cost_usd());
        let msgs = &ctx.conversation.messages;
        if msgs.len() <= cut || fingerprint(&msgs[..cut]) != expected {
            tracing::debug!(target: "harness::agent", "history changed; background summary dropped");
            return false;
        }
        let mut report = compactor.apply(ctx.conversation, cut, summary);
        report.background = true;
        let mut st = self.state();
        st.tracker.invalidate();
        st.failed_at_messages = None;
        st.background_failed = false;
        st.compactions.push(report);
        true
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

impl Drop for ContextManager {
    fn drop(&mut self) {
        let bg = self
            .state
            .get_mut()
            .unwrap_or_else(|e| e.into_inner())
            .background
            .take();
        if let Some(bg) = bg {
            bg.discard();
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
        if requested.is_none() && !over {
            self.background_step(compactor, ctx, usage.tokens).await;
            return HookDecision::Continue;
        }
        if requested.is_some() {
            // `/compact [focus]` summarizes the current history in-line.
            self.discard_background(Some(ctx)).await;
        } else if let Some(bg) = self.take_background() {
            // At `compact_at` with a summary started earlier: swap it in.
            if self.swap_background(compactor, ctx, bg).await {
                let usage = self.state().tracker.estimate(
                    ctx.conversation.system.as_deref(),
                    ctx.tools,
                    &ctx.conversation.messages,
                );
                if usage.tokens < self.settings.compact_threshold() || ctx.cancel.is_cancelled() {
                    return HookDecision::Continue;
                }
            } else if ctx.cancel.is_cancelled() {
                return HookDecision::Continue;
            }
        }
        // Do not retry a failed automatic compaction until the history grew.
        let retry_blocked = requested.is_none()
            && self.state().failed_at_messages == Some(ctx.conversation.messages.len());
        if retry_blocked {
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
                st.background_failed = false;
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

#[cfg(test)]
mod tests {
    use super::*;
    use harness_providers::model::{Role, ToolResultPart};
    use serde_json::json;

    #[test]
    fn background_settings_come_from_config_and_stay_below_compact_at() {
        let mut cfg = harness_config::ini::config::Config::defaults().context;
        assert_eq!(
            ContextSettings::from_config(&cfg, 10_000).background_at,
            None
        );
        assert_eq!(
            ContextSettings::from_config(&cfg, 10_000).background_threshold(),
            None
        );
        cfg.background_compaction = true;
        let s = ContextSettings::from_config(&cfg, 10_000);
        assert_eq!(s.background_at, Some(0.7));
        assert_eq!(s.background_threshold(), Some(7000));
        cfg.background_compact_at = 0.95;
        let s = ContextSettings::from_config(&cfg, 10_000);
        assert_eq!(s.background_threshold(), Some(8000), "capped at compact_at");
    }

    #[test]
    fn fingerprint_ignores_result_content_and_reasoning_but_not_the_conversation() {
        let call = Message {
            role: Role::Assistant,
            content: vec![
                ContentBlock::Reasoning {
                    text: "think".into(),
                    signature: None,
                },
                ContentBlock::ToolUse {
                    id: "c1".into(),
                    name: "read".into(),
                    input: json!({"path": "a"}),
                },
            ],
        };
        let result = |text: &str| Message {
            role: Role::User,
            content: vec![ContentBlock::ToolResult {
                tool_use_id: "c1".into(),
                content: vec![ToolResultPart::Text { text: text.into() }],
                is_error: false,
            }],
        };
        let base = vec![Message::user("goal"), call.clone(), result("full text")];
        let mut elided = base.clone();
        elided[2] = result("[elided]");
        if let ContentBlock::Reasoning { text, .. } = &mut elided[1].content[0] {
            text.clear();
        }
        assert_eq!(fingerprint(&base), fingerprint(&elided));
        let mut edited = base.clone();
        edited[0] = Message::user("another goal");
        assert_ne!(fingerprint(&base), fingerprint(&edited));
        assert_ne!(fingerprint(&base), fingerprint(&base[..2]));
    }
}
