//! Staggered subagent fan-out (`[experimental] stagger_subagents`).
//!
//! Siblings started together (several `spawn_agent` calls in one tool
//! batch, parallel workflow steps) usually share a prompt prefix: the same
//! model, tools and system prompt. Started at once, every one of them
//! writes that prefix to the prompt cache, because a cache entry only
//! exists once the first response has started. [`StaggeredProvider`]
//! sends the first request for a prefix alone; requests with the same
//! prefix wait until its response begins (its first stream event), then
//! go and read the cache. With `cache_prewarm` on too, the first one
//! pre-warms the shared prefix ([`crate::prewarm`]) and releases the
//! others as soon as that returns, then sends its own request with them.
//!
//! The gate ([`FanOut`]) is keyed by everything that is cached before the
//! first message: provider, model, thinking settings, system prompt and
//! tool definitions. A prefix counts as warm for [`WARM_FOR`] after a
//! response began or a request passed (each read refreshes the entry);
//! later requests pass at once. Waiting is bounded by [`MAX_WAIT`], ends
//! early if the leader fails (another waiter then leads) and never holds
//! a scheduler call slot. Nothing about the requests changes, so answers
//! are the same with or without the gate.
//!
//! A leader's pre-warm is charged to the session's [`BudgetMeter`]
//! ([`FanOut::charge_to`]) like every other call, and is skipped while a
//! budget is exhausted.

use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use futures::StreamExt;
use harness_events::schema::NodeId;
use harness_events::store::RunRecorder;
use harness_providers::model::{Capabilities, ChatRequest, Usage};
use harness_providers::provider::{EventStream, Provider, ProviderError, StreamEvent};
use serde_json::Value;
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

use crate::agent_loop::CostFn;
use crate::budget::BudgetMeter;

/// How long a prefix stays warm after its last response start or pass
/// (under the five-minute cache lifetime, with room for the request).
pub const WARM_FOR: Duration = Duration::from_secs(4 * 60);

/// Longest a request waits for the leader of its prefix.
pub const MAX_WAIT: Duration = Duration::from_secs(30);

#[derive(Debug)]
enum Slot {
    /// A request for this prefix is in flight; `true` once it began.
    Leading(watch::Receiver<bool>),
    /// The prefix was cached at this instant.
    Warm(Instant),
}

/// The shared gate of one session (see the module docs). Clones share it.
#[derive(Debug, Clone, Default)]
pub struct FanOut {
    slots: Arc<Mutex<HashMap<u64, Slot>>>,
    prewarm: bool,
    /// The session's spend meter, charged for leaders' pre-warms.
    meter: Arc<OnceLock<BudgetMeter>>,
}

/// What the leader holds: opening it releases the waiters.
struct Lead {
    key: u64,
    slots: Arc<Mutex<HashMap<u64, Slot>>>,
    tx: Option<watch::Sender<bool>>,
}

impl Lead {
    fn open(&mut self) {
        if let Some(tx) = self.tx.take() {
            self.slots
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(self.key, Slot::Warm(Instant::now()));
            let _ = tx.send(true);
        }
    }
}

impl Drop for Lead {
    fn drop(&mut self) {
        if self.tx.is_some() {
            // Failed or cancelled before its response began: let a waiter
            // lead (dropping the sender wakes them).
            let mut slots = self.slots.lock().unwrap_or_else(|e| e.into_inner());
            if matches!(slots.get(&self.key), Some(Slot::Leading(_))) {
                slots.remove(&self.key);
            }
        }
    }
}

impl FanOut {
    /// A gate; with `prewarm`, leaders pre-warm the shared prefix first.
    pub fn new(prewarm: bool) -> Self {
        Self {
            slots: Arc::default(),
            prewarm,
            meter: Arc::default(),
        }
    }

    /// Charges leaders' pre-warms to `meter` (shared by every clone; the
    /// first meter set stays). With it, a pre-warm is skipped while a
    /// budget is exhausted.
    pub fn charge_to(&self, meter: BudgetMeter) {
        let _ = self.meter.set(meter);
    }

    /// Whether a budget the gate is charged to is exhausted.
    fn over_budget(&self) -> bool {
        self.meter
            .get()
            .is_some_and(|m| m.check().exceeded.is_some())
    }

    /// Whether leaders pre-warm.
    pub fn prewarms(&self) -> bool {
        self.prewarm
    }

    /// The cache key of `request` on `provider`: everything before the
    /// first message.
    pub fn key(provider: &str, request: &ChatRequest) -> u64 {
        let mut h = DefaultHasher::new();
        provider.hash(&mut h);
        request.model.hash(&mut h);
        request.system.hash(&mut h);
        request.system_layers.hash(&mut h);
        serde_json::to_string(&request.thinking)
            .unwrap_or_default()
            .hash(&mut h);
        serde_json::to_string(&request.tools)
            .unwrap_or_default()
            .hash(&mut h);
        h.finish()
    }

    /// Waits until a request for `key` may go. `Some(lead)` makes the
    /// caller the leader for a cold prefix.
    async fn enter(&self, key: u64, cancel: &CancellationToken) -> Option<Lead> {
        let deadline = tokio::time::Instant::now() + MAX_WAIT;
        loop {
            let mut rx = {
                let mut slots = self.slots.lock().unwrap_or_else(|e| e.into_inner());
                match slots.get(&key) {
                    Some(Slot::Warm(at)) if at.elapsed() < WARM_FOR => {
                        slots.insert(key, Slot::Warm(Instant::now()));
                        return None;
                    }
                    Some(Slot::Leading(rx)) => rx.clone(),
                    _ => {
                        let (tx, rx) = watch::channel(false);
                        slots.insert(key, Slot::Leading(rx));
                        return Some(Lead {
                            key,
                            slots: self.slots.clone(),
                            tx: Some(tx),
                        });
                    }
                }
            };
            tokio::select! {
                r = rx.wait_for(|began| *began) => {
                    if r.is_ok() {
                        return None;
                    }
                    // The leader failed: contend again.
                }
                _ = tokio::time::sleep_until(deadline) => return None,
                _ = cancel.cancelled() => return None,
            }
        }
    }
}

/// Where a leader records its pre-warm call.
#[derive(Clone)]
pub struct PrewarmSink {
    /// The run recorder.
    pub recorder: RunRecorder,
    /// Parent of the pre-warm node (the agent that leads).
    pub parent: NodeId,
    /// Pricing.
    pub cost: Option<CostFn>,
}

/// A provider whose calls pass through a [`FanOut`] gate.
pub struct StaggeredProvider {
    inner: Arc<dyn Provider>,
    gate: FanOut,
    sink: Option<PrewarmSink>,
}

impl StaggeredProvider {
    /// Wraps `inner`; leaders pre-warm into `sink` when the gate says so.
    pub fn new(inner: Arc<dyn Provider>, gate: FanOut, sink: Option<PrewarmSink>) -> Self {
        Self { inner, gate, sink }
    }
}

#[async_trait]
impl Provider for StaggeredProvider {
    fn name(&self) -> &str {
        self.inner.name()
    }

    fn capabilities(&self, model: &str) -> Capabilities {
        self.inner.capabilities(model)
    }

    fn request_body(&self, request: &ChatRequest) -> Option<Value> {
        self.inner.request_body(request)
    }

    async fn prewarm(&self) {
        self.inner.prewarm().await;
    }

    fn can_prewarm_cache(&self, request: &ChatRequest) -> bool {
        self.inner.can_prewarm_cache(request)
    }

    async fn prewarm_cache(
        &self,
        request: ChatRequest,
        cancel: CancellationToken,
    ) -> Result<Usage, ProviderError> {
        self.inner.prewarm_cache(request, cancel).await
    }

    async fn stream(
        &self,
        request: ChatRequest,
        cancel: CancellationToken,
    ) -> Result<EventStream, ProviderError> {
        let key = FanOut::key(self.inner.name(), &request);
        let Some(mut lead) = self.gate.enter(key, &cancel).await else {
            return self.inner.stream(request, cancel).await;
        };
        if self.gate.prewarm
            && !self.gate.over_budget()
            && let Some(sink) = &self.sink
        {
            let mut shared = request.clone();
            shared.messages.clear();
            let done = crate::prewarm::prewarm(
                self.inner.as_ref(),
                shared,
                &sink.recorder,
                &sink.parent,
                "fan_out",
                sink.cost.as_ref(),
                &cancel,
            )
            .await;
            if let Some(Ok(report)) = &done {
                if let (Some(meter), Some(usd)) = (self.gate.meter.get(), report.cost_usd) {
                    meter.charge(usd);
                }
                lead.open();
            }
        }
        let stream = self.inner.stream(request, cancel).await?;
        let mut lead = Some(lead);
        Ok(Box::pin(stream.map(move |item| {
            if matches!(&item, Ok(ev) if !matches!(ev, StreamEvent::Retry(_)))
                && let Some(mut l) = lead.take()
            {
                l.open();
            }
            item
        })))
    }
}

#[cfg(test)]
mod tests;
