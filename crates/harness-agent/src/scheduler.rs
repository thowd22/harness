//! Concurrency scheduler shared by subagents and workflow steps.
//!
//! Parallel agents share two kinds of resources, and the [`Scheduler`]
//! limits both:
//!
//! - **Agent slots** (`[general] max_concurrent_agents`): one
//!   [`AgentSlot`] per running subagent or workflow step. Work that cannot
//!   get a slot waits; the caller is told through `on_queued` so it can
//!   record the node as queued (`node_started { queued: true }`).
//! - **Provider calls** ([`SchedulerConfig::provider_limits`]): at most
//!   N concurrent model calls per provider. [`ScheduledProvider`] wraps a
//!   [`Provider`] and takes a [`CallPermit`] for the lifetime of each
//!   stream.
//!
//! Rate limits slow the scheduler instead of failing runs. When a provider
//! reports HTTP 429 (a `retry` stream event or a rate-limited error),
//! [`Scheduler::note_rate_limited`] pauses new calls to that provider until
//! the `retry-after` deadline. [`ScheduledProvider`] also retries a call
//! whose retries were exhausted by rate limits before any output arrived,
//! waiting out the pause each time, up to
//! [`SchedulerConfig::max_rate_limit_waits`] times.
//!
//! A parent that waits for its children (spawn_agent) can lend its slot
//! with [`AgentSlot::suspend`] / [`AgentSlot::resume`], so nested agents
//! cannot deadlock the pool.

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use futures::StreamExt;
use harness_config::ini::Config;
use harness_providers::model::{Capabilities, ChatRequest};
use harness_providers::provider::{EventStream, Provider, ProviderError, StreamEvent};
use harness_providers::retry::{RetryEvent, RetryReason};
use tokio::sync::{OwnedSemaphorePermit, Semaphore, mpsc};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

/// Pause used when a rate-limit response carries no `retry-after`.
pub const DEFAULT_RATE_LIMIT_PAUSE: Duration = Duration::from_secs(5);
/// Upper bound on a single rate-limit pause.
pub const MAX_RATE_LIMIT_PAUSE: Duration = Duration::from_secs(300);
/// Default number of whole-call retries after exhausted rate limits.
pub const DEFAULT_MAX_RATE_LIMIT_WAITS: u32 = 8;

/// Scheduler limits.
#[derive(Debug, Clone, PartialEq)]
pub struct SchedulerConfig {
    /// Concurrent agent slots (subagents and workflow steps).
    pub max_concurrent_agents: usize,
    /// Concurrent model calls per provider name; providers not listed are
    /// unlimited (only the agent slots bound them).
    pub provider_limits: BTreeMap<String, usize>,
    /// Pause when a rate limit has no `retry-after`.
    pub default_rate_limit_pause: Duration,
    /// Longest pause honoured.
    pub max_rate_limit_pause: Duration,
    /// Whole-call retries after the transport gave up on rate limits.
    pub max_rate_limit_waits: u32,
}

impl Default for SchedulerConfig {
    fn default() -> Self {
        Self {
            max_concurrent_agents: 4,
            provider_limits: BTreeMap::new(),
            default_rate_limit_pause: DEFAULT_RATE_LIMIT_PAUSE,
            max_rate_limit_pause: MAX_RATE_LIMIT_PAUSE,
            max_rate_limit_waits: DEFAULT_MAX_RATE_LIMIT_WAITS,
        }
    }
}

impl SchedulerConfig {
    /// Limits from `[general] max_concurrent_agents` (at least 1).
    pub fn from_config(cfg: &Config) -> Self {
        Self {
            max_concurrent_agents: usize::try_from(cfg.general.max_concurrent_agents.max(1))
                .unwrap_or(usize::MAX),
            ..Self::default()
        }
    }

    /// Limits concurrent model calls to `provider` to `limit` (at least 1).
    pub fn with_provider_limit(mut self, provider: impl Into<String>, limit: usize) -> Self {
        self.provider_limits.insert(provider.into(), limit.max(1));
        self
    }
}

/// The wait was cancelled.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("cancelled while waiting for a scheduler slot")]
pub struct Cancelled;

#[derive(Default)]
struct ProviderGate {
    sem: Option<Arc<Semaphore>>,
    paused_until: Option<Instant>,
}

struct Inner {
    cfg: SchedulerConfig,
    agents: Arc<Semaphore>,
    providers: Mutex<HashMap<String, ProviderGate>>,
    queued: AtomicUsize,
    running: AtomicUsize,
    rate_limits: AtomicU64,
    calls: AtomicUsize,
}

/// Shared concurrency limits (cheap to clone).
#[derive(Clone)]
pub struct Scheduler {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for Scheduler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Scheduler")
            .field("config", &self.inner.cfg)
            .field("stats", &self.stats())
            .finish()
    }
}

/// A snapshot of the scheduler's counters.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SchedulerStats {
    /// Agents holding a slot.
    pub agents_running: usize,
    /// Agents waiting for a slot.
    pub agents_queued: usize,
    /// Model calls holding a provider permit.
    pub calls_running: usize,
    /// Rate-limit signals seen so far.
    pub rate_limits: u64,
}

/// One agent slot. Dropping it frees the slot.
pub struct AgentSlot {
    sched: Scheduler,
    permit: Mutex<Option<OwnedSemaphorePermit>>,
}

impl std::fmt::Debug for AgentSlot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentSlot")
            .field("held", &self.is_held())
            .finish()
    }
}

impl AgentSlot {
    /// True while the slot is held (not suspended).
    pub fn is_held(&self) -> bool {
        self.permit
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_some()
    }

    /// Frees the slot while the agent waits (e.g. for its children).
    pub fn suspend(&self) {
        let p = self.permit.lock().unwrap_or_else(|e| e.into_inner()).take();
        if p.is_some() {
            self.sched.inner.running.fetch_sub(1, Ordering::SeqCst);
        }
    }

    /// Takes the slot back after [`AgentSlot::suspend`] (waits if needed).
    pub async fn resume(&self) {
        if self.is_held() {
            return;
        }
        let sem = self.sched.inner.agents.clone();
        if let Ok(p) = sem.acquire_owned().await {
            *self.permit.lock().unwrap_or_else(|e| e.into_inner()) = Some(p);
            self.sched.inner.running.fetch_add(1, Ordering::SeqCst);
        }
    }
}

impl Drop for AgentSlot {
    fn drop(&mut self) {
        self.suspend();
    }
}

/// A provider-call permit. Dropping it frees the permit.
#[derive(Debug)]
pub struct CallPermit {
    _permit: Option<OwnedSemaphorePermit>,
    sched: Scheduler,
}

impl Drop for CallPermit {
    fn drop(&mut self) {
        self.sched.inner.calls.fetch_sub(1, Ordering::SeqCst);
    }
}

struct QueuedGuard<'a>(&'a AtomicUsize);

impl Drop for QueuedGuard<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

impl Scheduler {
    /// A scheduler with `cfg` limits.
    pub fn new(cfg: SchedulerConfig) -> Self {
        let slots = cfg.max_concurrent_agents.max(1);
        Self {
            inner: Arc::new(Inner {
                agents: Arc::new(Semaphore::new(slots)),
                cfg,
                providers: Mutex::new(HashMap::new()),
                queued: AtomicUsize::new(0),
                running: AtomicUsize::new(0),
                rate_limits: AtomicU64::new(0),
                calls: AtomicUsize::new(0),
            }),
        }
    }

    /// The limits.
    pub fn config(&self) -> &SchedulerConfig {
        &self.inner.cfg
    }

    /// Current counters.
    pub fn stats(&self) -> SchedulerStats {
        SchedulerStats {
            agents_running: self.inner.running.load(Ordering::SeqCst),
            agents_queued: self.inner.queued.load(Ordering::SeqCst),
            calls_running: self.inner.calls.load(Ordering::SeqCst),
            rate_limits: self.inner.rate_limits.load(Ordering::SeqCst),
        }
    }

    /// Takes an agent slot. When none is free, `on_queued` runs once before
    /// waiting (record the node as queued there).
    pub async fn acquire_agent(
        &self,
        cancel: &CancellationToken,
        on_queued: impl FnOnce(),
    ) -> Result<AgentSlot, Cancelled> {
        let sem = self.inner.agents.clone();
        let permit = match sem.clone().try_acquire_owned() {
            Ok(p) => p,
            Err(_) => {
                self.inner.queued.fetch_add(1, Ordering::SeqCst);
                let _q = QueuedGuard(&self.inner.queued);
                on_queued();
                tokio::select! {
                    p = sem.acquire_owned() => p.map_err(|_| Cancelled)?,
                    _ = cancel.cancelled() => return Err(Cancelled),
                }
            }
        };
        self.inner.running.fetch_add(1, Ordering::SeqCst);
        Ok(AgentSlot {
            sched: self.clone(),
            permit: Mutex::new(Some(permit)),
        })
    }

    /// How long new calls to `provider` are paused, if at all.
    pub fn paused_for(&self, provider: &str) -> Option<Duration> {
        let map = self
            .inner
            .providers
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let until = map.get(provider)?.paused_until?;
        let now = Instant::now();
        (until > now).then(|| until - now)
    }

    /// Pauses new calls to `provider` for `retry_after` (or the default
    /// pause), capped at [`SchedulerConfig::max_rate_limit_pause`]. A later
    /// deadline never shortens an earlier one. Returns the pause applied.
    pub fn note_rate_limited(&self, provider: &str, retry_after: Option<Duration>) -> Duration {
        let cfg = &self.inner.cfg;
        let pause = retry_after
            .unwrap_or(cfg.default_rate_limit_pause)
            .min(cfg.max_rate_limit_pause);
        self.inner.rate_limits.fetch_add(1, Ordering::SeqCst);
        let until = Instant::now() + pause;
        let mut map = self
            .inner
            .providers
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let gate = self.gate_entry(&mut map, provider);
        if gate.paused_until.is_none_or(|u| u < until) {
            gate.paused_until = Some(until);
        }
        tracing::info!(target: "harness::scheduler", provider, pause_ms = pause.as_millis() as u64, "rate limited; pausing new calls");
        pause
    }

    fn gate_entry<'m>(
        &self,
        map: &'m mut HashMap<String, ProviderGate>,
        provider: &str,
    ) -> &'m mut ProviderGate {
        map.entry(provider.to_owned())
            .or_insert_with(|| ProviderGate {
                sem: self
                    .inner
                    .cfg
                    .provider_limits
                    .get(provider)
                    .map(|&n| Arc::new(Semaphore::new(n.max(1)))),
                paused_until: None,
            })
    }

    /// Waits out any rate-limit pause, then takes a call permit for
    /// `provider`.
    pub async fn acquire_call(
        &self,
        provider: &str,
        cancel: &CancellationToken,
    ) -> Result<CallPermit, Cancelled> {
        loop {
            let (until, sem) = {
                let mut map = self
                    .inner
                    .providers
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                let gate = self.gate_entry(&mut map, provider);
                (gate.paused_until, gate.sem.clone())
            };
            if let Some(until) = until
                && until > Instant::now()
            {
                tokio::select! {
                    _ = tokio::time::sleep_until(until) => continue,
                    _ = cancel.cancelled() => return Err(Cancelled),
                }
            }
            let permit = match sem {
                Some(sem) => Some(tokio::select! {
                    p = sem.acquire_owned() => p.map_err(|_| Cancelled)?,
                    _ = cancel.cancelled() => return Err(Cancelled),
                }),
                None => None,
            };
            // A pause may have started while we waited for the permit.
            if self.paused_for(provider).is_some() {
                continue;
            }
            self.inner.calls.fetch_add(1, Ordering::SeqCst);
            return Ok(CallPermit {
                _permit: permit,
                sched: self.clone(),
            });
        }
    }
}

/// True when `e` (or the error retries gave up on) is a rate limit.
pub fn is_rate_limit(e: &ProviderError) -> bool {
    matches!(e.root(), ProviderError::RateLimited { .. })
}

/// A [`Provider`] whose calls go through a [`Scheduler`].
pub struct ScheduledProvider {
    inner: Arc<dyn Provider>,
    scheduler: Scheduler,
}

impl ScheduledProvider {
    /// Wraps `inner`; calls are limited and paused by `scheduler`.
    pub fn new(inner: Arc<dyn Provider>, scheduler: Scheduler) -> Self {
        Self { inner, scheduler }
    }
}

#[async_trait]
impl Provider for ScheduledProvider {
    fn name(&self) -> &str {
        self.inner.name()
    }

    fn request_body(&self, request: &ChatRequest) -> Option<serde_json::Value> {
        self.inner.request_body(request)
    }

    fn capabilities(&self, model: &str) -> Capabilities {
        self.inner.capabilities(model)
    }

    async fn stream(
        &self,
        request: ChatRequest,
        cancel: CancellationToken,
    ) -> Result<EventStream, ProviderError> {
        let (tx, rx) = mpsc::channel::<Result<StreamEvent, ProviderError>>(64);
        let inner = self.inner.clone();
        let sched = self.scheduler.clone();
        tokio::spawn(drive(inner, sched, request, cancel, tx));
        Ok(Box::pin(futures::stream::unfold(rx, |mut rx| async move {
            rx.recv().await.map(|item| (item, rx))
        })))
    }
}

async fn drive(
    inner: Arc<dyn Provider>,
    sched: Scheduler,
    request: ChatRequest,
    cancel: CancellationToken,
    tx: mpsc::Sender<Result<StreamEvent, ProviderError>>,
) {
    let provider = inner.name().to_owned();
    let max = sched.config().max_rate_limit_waits;
    let mut waits = 0u32;
    loop {
        let Ok(permit) = sched.acquire_call(&provider, &cancel).await else {
            let _ = tx.send(Err(ProviderError::Cancelled)).await;
            return;
        };
        let opened = tokio::select! {
            r = inner.stream(request.clone(), cancel.clone()) => r,
            _ = cancel.cancelled() => Err(ProviderError::Cancelled),
        };
        let mut content_seen = false;
        let failure = match opened {
            Err(e) => e,
            Ok(mut stream) => loop {
                let item = tokio::select! {
                    item = stream.next() => item,
                    _ = tx.closed() => return,
                };
                match item {
                    None => return,
                    Some(Ok(ev)) => {
                        if let StreamEvent::Retry(r) = &ev {
                            if r.reason == RetryReason::RateLimited {
                                sched.note_rate_limited(&provider, Some(r.delay));
                            }
                        } else {
                            content_seen = true;
                        }
                        if tx.send(Ok(ev)).await.is_err() {
                            return;
                        }
                    }
                    Some(Err(e)) => break e,
                }
            },
        };
        drop(permit);
        if is_rate_limit(&failure) && !content_seen && waits < max {
            waits += 1;
            let pause = sched.note_rate_limited(&provider, failure.root().retry_after());
            let note = StreamEvent::Retry(RetryEvent {
                attempt: waits,
                max_retries: max,
                delay: pause,
                reason: RetryReason::RateLimited,
                error: failure.to_string(),
                discard_partial: false,
            });
            if tx.send(Ok(note)).await.is_err() {
                return;
            }
            continue;
        }
        let _ = tx.send(Err(failure)).await;
        return;
    }
}

#[cfg(test)]
mod tests;
