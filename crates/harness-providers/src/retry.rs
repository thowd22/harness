//! Retry policy, `Retry-After` parsing and cancellable retry combinators.
//!
//! [`retry_call`] retries a single future; [`retry_stream`] retries a
//! streaming call, including failures that happen mid-stream (stalls,
//! dropped connections, overload events), and reports every retry as an
//! [`Attempt::Retry`] item so the UI can show "retrying in 4s (429 rate
//! limited, attempt 2/5)".

use std::collections::hash_map::RandomState;
use std::fmt;
use std::future::Future;
use std::hash::BuildHasher;
use std::pin::Pin;
use std::time::Duration;

use futures::stream::{BoxStream, Stream, StreamExt};
use reqwest::header::HeaderMap;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use tokio_util::sync::CancellationToken;

use crate::provider::ProviderError;

/// Jittered exponential backoff settings.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct RetryPolicy {
    /// Retries after the first attempt (0 disables retrying).
    pub max_retries: u32,
    /// Delay before the first retry, before jitter.
    pub base_delay: Duration,
    /// Upper bound for computed delays.
    pub max_delay: Duration,
    /// Fraction of the delay that is randomized, in `0.0..=1.0`. A computed
    /// delay `d` becomes a uniform value in `[d * (1 - jitter), d]`.
    pub jitter: f64,
    /// Longest server-requested `retry-after` that is honoured. A longer
    /// request is treated as permanent and the error is returned.
    pub max_retry_after: Duration,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_retries: 5,
            base_delay: Duration::from_secs(1),
            max_delay: Duration::from_secs(60),
            jitter: 0.5,
            max_retry_after: Duration::from_secs(300),
        }
    }
}

impl RetryPolicy {
    /// A policy that never retries.
    pub fn none() -> Self {
        Self {
            max_retries: 0,
            ..Self::default()
        }
    }

    /// Delay before retry number `retry` (1-based), using a random jitter
    /// sample. `retry_after` from the server wins when present.
    pub fn delay(&self, retry: u32, retry_after: Option<Duration>) -> Duration {
        self.delay_with_sample(retry, retry_after, random_unit())
    }

    /// Like [`RetryPolicy::delay`] with an explicit jitter sample in `[0, 1)`.
    pub fn delay_with_sample(
        &self,
        retry: u32,
        retry_after: Option<Duration>,
        sample: f64,
    ) -> Duration {
        if let Some(wait) = retry_after {
            return wait;
        }
        let exp = 2f64.powi(retry.saturating_sub(1).min(30) as i32);
        let base = self.base_delay.mul_f64(exp).min(self.max_delay);
        let jitter = self.jitter.clamp(0.0, 1.0) * sample.clamp(0.0, 1.0);
        base.mul_f64(1.0 - jitter)
    }
}

/// Uniform sample in `[0, 1)` from the std hasher's random keys.
fn random_unit() -> f64 {
    let bits = RandomState::new().hash_one(std::time::Instant::now());
    (bits >> 11) as f64 / (1u64 << 53) as f64
}

/// Why a call is being retried.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "status", rename_all = "snake_case")]
pub enum RetryReason {
    /// HTTP 429.
    RateLimited,
    /// Provider overloaded (HTTP 529 or overload error).
    Overloaded,
    /// Server error with this HTTP status.
    Server(u16),
    /// Transport-level failure.
    Connection,
    /// Request timeout.
    Timeout,
    /// No bytes for the stall interval.
    Stalled,
}

impl fmt::Display for RetryReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::RateLimited => f.write_str("429 rate limited"),
            Self::Overloaded => f.write_str("overloaded"),
            Self::Server(s) => write!(f, "{s} server error"),
            Self::Connection => f.write_str("connection error"),
            Self::Timeout => f.write_str("timeout"),
            Self::Stalled => f.write_str("stream stalled"),
        }
    }
}

/// Emitted before every retry.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RetryEvent {
    /// Retry number, 1-based.
    pub attempt: u32,
    /// Configured retry cap.
    pub max_retries: u32,
    /// How long the transport waits before retrying.
    pub delay: Duration,
    /// Why.
    pub reason: RetryReason,
    /// The error message that triggered the retry.
    pub error: String,
    /// True if the failed attempt already produced output that must be
    /// discarded.
    pub discard_partial: bool,
}

/// Parses `retry-after-ms` (milliseconds) or `retry-after` (seconds or an
/// HTTP date) from response headers.
pub fn parse_retry_after(headers: &HeaderMap) -> Option<Duration> {
    let get = |name: &str| headers.get(name).and_then(|v| v.to_str().ok());
    if let Some(ms) = get("retry-after-ms").and_then(|v| v.trim().parse::<f64>().ok())
        && ms.is_finite()
        && ms >= 0.0
    {
        return Some(Duration::from_secs_f64(ms / 1000.0));
    }
    get("retry-after").and_then(|v| parse_retry_after_value(v, OffsetDateTime::now_utc()))
}

/// Parses a `retry-after` value: delta seconds, or an HTTP date relative to
/// `now` (past dates yield zero).
pub fn parse_retry_after_value(value: &str, now: OffsetDateTime) -> Option<Duration> {
    let value = value.trim();
    if let Ok(secs) = value.parse::<f64>() {
        return (secs.is_finite() && secs >= 0.0).then(|| Duration::from_secs_f64(secs));
    }
    let when = OffsetDateTime::parse(value, &time::format_description::well_known::Rfc2822).ok()?;
    Some((when - now).try_into().unwrap_or(Duration::ZERO))
}

/// Decides whether `err` after `retries_done` retries should be retried,
/// returning the event to emit.
fn plan_retry(
    policy: &RetryPolicy,
    err: &ProviderError,
    retries_done: u32,
    discard_partial: bool,
) -> Option<RetryEvent> {
    let reason = err.retry_reason()?;
    if retries_done >= policy.max_retries {
        return None;
    }
    if err
        .retry_after()
        .is_some_and(|w| w > policy.max_retry_after)
    {
        return None;
    }
    let attempt = retries_done + 1;
    Some(RetryEvent {
        attempt,
        max_retries: policy.max_retries,
        delay: policy.delay(attempt, err.retry_after()),
        reason,
        error: err.to_string(),
        discard_partial,
    })
}

fn give_up(err: ProviderError, retries_done: u32) -> ProviderError {
    if retries_done > 0 && err.retry_reason().is_some() {
        ProviderError::RetriesExhausted {
            attempts: retries_done + 1,
            last: Box::new(err),
        }
    } else {
        err
    }
}

async fn sleep_or_cancel(delay: Duration, cancel: &CancellationToken) -> Result<(), ProviderError> {
    tokio::select! {
        biased;
        _ = cancel.cancelled() => Err(ProviderError::Cancelled),
        _ = tokio::time::sleep(delay) => Ok(()),
    }
}

/// Runs `op` until it succeeds, fails permanently, retries run out or
/// `cancel` fires. `on_retry` is called before every retry.
pub async fn retry_call<T, F, Fut>(
    policy: &RetryPolicy,
    cancel: &CancellationToken,
    mut on_retry: impl FnMut(&RetryEvent),
    mut op: F,
) -> Result<T, ProviderError>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T, ProviderError>>,
{
    let mut retries = 0;
    loop {
        let result = tokio::select! {
            biased;
            _ = cancel.cancelled() => return Err(ProviderError::Cancelled),
            r = op() => r,
        };
        let err = match result {
            Ok(v) => return Ok(v),
            Err(e) => e,
        };
        let Some(event) = plan_retry(policy, &err, retries, false) else {
            return Err(give_up(err, retries));
        };
        tracing::warn!(attempt = event.attempt, reason = %event.reason, delay_ms = event.delay.as_millis() as u64, "retrying provider call");
        on_retry(&event);
        sleep_or_cancel(event.delay, cancel).await?;
        retries += 1;
    }
}

/// An item from [`retry_stream`].
#[derive(Debug, Clone, PartialEq)]
pub enum Attempt<T> {
    /// An item from the current attempt.
    Item(T),
    /// The call is being retried. See [`RetryEvent::discard_partial`].
    Retry(RetryEvent),
}

type Opening<S> = Pin<Box<dyn Future<Output = Result<S, ProviderError>> + Send>>;

struct State<F, S> {
    policy: RetryPolicy,
    cancel: CancellationToken,
    make: F,
    current: Option<Pin<Box<S>>>,
    sleep: Option<Duration>,
    retries: u32,
    yielded: bool,
    done: bool,
}

/// Retries a streaming call.
///
/// `make` opens one attempt. Retryable errors from opening or from the
/// stream itself trigger a retry (up to the policy cap), signalled by an
/// [`Attempt::Retry`] item. After a permanent error, exhaustion
/// ([`ProviderError::RetriesExhausted`]) or cancellation
/// ([`ProviderError::Cancelled`]) the stream yields that error and ends.
pub fn retry_stream<T, S, F, Fut>(
    policy: RetryPolicy,
    cancel: CancellationToken,
    make: F,
) -> BoxStream<'static, Result<Attempt<T>, ProviderError>>
where
    T: Send + 'static,
    S: Stream<Item = Result<T, ProviderError>> + Send + 'static,
    F: FnMut() -> Fut + Send + 'static,
    Fut: Future<Output = Result<S, ProviderError>> + Send + 'static,
{
    let state = State {
        policy,
        cancel,
        make,
        current: None,
        sleep: None,
        retries: 0,
        yielded: false,
        done: false,
    };
    futures::stream::unfold(state, |mut st| async move {
        if st.done {
            return None;
        }
        loop {
            if let Some(delay) = st.sleep.take() {
                if let Err(e) = sleep_or_cancel(delay, &st.cancel).await {
                    st.done = true;
                    return Some((Err(e), st));
                }
                st.retries += 1;
            }
            let err: ProviderError = match st.current.as_mut() {
                None => {
                    let opening: Opening<S> = Box::pin((st.make)());
                    let opened = tokio::select! {
                        biased;
                        _ = st.cancel.cancelled() => Err(ProviderError::Cancelled),
                        r = opening => r,
                    };
                    match opened {
                        Ok(s) => {
                            st.current = Some(Box::pin(s));
                            st.yielded = false;
                            continue;
                        }
                        Err(e) => e,
                    }
                }
                Some(stream) => {
                    let next = tokio::select! {
                        biased;
                        _ = st.cancel.cancelled() => Some(Err(ProviderError::Cancelled)),
                        n = stream.next() => n,
                    };
                    match next {
                        None => return None,
                        Some(Ok(item)) => {
                            st.yielded = true;
                            return Some((Ok(Attempt::Item(item)), st));
                        }
                        Some(Err(e)) => e,
                    }
                }
            };
            st.current = None;
            if let Some(event) = plan_retry(&st.policy, &err, st.retries, st.yielded) {
                tracing::warn!(attempt = event.attempt, reason = %event.reason, delay_ms = event.delay.as_millis() as u64, "retrying provider stream");
                st.sleep = Some(event.delay);
                return Some((Ok(Attempt::Retry(event)), st));
            }
            st.done = true;
            let err = give_up(err, st.retries);
            return Some((Err(err), st));
        }
    })
    .boxed()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};

    fn quick() -> RetryPolicy {
        RetryPolicy {
            max_retries: 3,
            base_delay: Duration::from_millis(100),
            max_delay: Duration::from_millis(250),
            jitter: 0.5,
            max_retry_after: Duration::from_secs(60),
        }
    }

    #[test]
    fn backoff_is_exponential_capped_and_jittered() {
        let p = quick();
        assert_eq!(
            p.delay_with_sample(1, None, 0.0),
            Duration::from_millis(100)
        );
        assert_eq!(
            p.delay_with_sample(2, None, 0.0),
            Duration::from_millis(200)
        );
        assert_eq!(
            p.delay_with_sample(3, None, 0.0),
            Duration::from_millis(250)
        );
        assert_eq!(
            p.delay_with_sample(30, None, 0.0),
            Duration::from_millis(250)
        );
        assert_eq!(p.delay_with_sample(1, None, 1.0), Duration::from_millis(50));
        for _ in 0..100 {
            let d = p.delay(2, None);
            assert!(
                d >= Duration::from_millis(100) && d <= Duration::from_millis(200),
                "{d:?}"
            );
        }
        assert_eq!(
            p.delay(1, Some(Duration::from_secs(7))),
            Duration::from_secs(7)
        );
    }

    #[test]
    fn retry_after_parsing() {
        let now = OffsetDateTime::parse(
            "Wed, 21 Oct 2026 07:28:00 GMT",
            &time::format_description::well_known::Rfc2822,
        )
        .unwrap();
        assert_eq!(
            parse_retry_after_value("3", now),
            Some(Duration::from_secs(3))
        );
        assert_eq!(
            parse_retry_after_value("1.5", now),
            Some(Duration::from_millis(1500))
        );
        assert_eq!(
            parse_retry_after_value("Wed, 21 Oct 2026 07:28:10 GMT", now),
            Some(Duration::from_secs(10))
        );
        assert_eq!(
            parse_retry_after_value("Wed, 21 Oct 2026 07:27:00 GMT", now),
            Some(Duration::ZERO)
        );
        assert_eq!(parse_retry_after_value("soon", now), None);
        assert_eq!(parse_retry_after_value("-1", now), None);
        let mut h = HeaderMap::new();
        h.insert("retry-after", "2".parse().unwrap());
        assert_eq!(parse_retry_after(&h), Some(Duration::from_secs(2)));
        h.insert("retry-after-ms", "250".parse().unwrap());
        assert_eq!(parse_retry_after(&h), Some(Duration::from_millis(250)));
    }

    fn rate_limited() -> ProviderError {
        ProviderError::RateLimited {
            message: "slow".into(),
            retry_after: None,
        }
    }

    #[tokio::test(start_paused = true)]
    async fn retry_call_retries_then_succeeds_and_reports_each_retry() {
        let calls = AtomicU32::new(0);
        let mut events = Vec::new();
        let r = retry_call(
            &quick(),
            &CancellationToken::new(),
            |e| events.push(e.clone()),
            || {
                let n = calls.fetch_add(1, Ordering::SeqCst);
                async move { if n < 2 { Err(rate_limited()) } else { Ok(n) } }
            },
        )
        .await;
        assert_eq!(r, Ok(2));
        assert_eq!(
            events.iter().map(|e| e.attempt).collect::<Vec<_>>(),
            vec![1, 2]
        );
        assert!(
            events
                .iter()
                .all(|e| e.reason == RetryReason::RateLimited && e.max_retries == 3)
        );
    }

    #[tokio::test(start_paused = true)]
    async fn retry_call_caps_retries_and_skips_permanent_errors() {
        let calls = AtomicU32::new(0);
        let r: Result<(), _> = retry_call(
            &quick(),
            &CancellationToken::new(),
            |_| {},
            || {
                calls.fetch_add(1, Ordering::SeqCst);
                async { Err(ProviderError::from_status(503, "down", None)) }
            },
        )
        .await;
        assert_eq!(calls.load(Ordering::SeqCst), 4);
        match r.unwrap_err() {
            ProviderError::RetriesExhausted { attempts, last } => {
                assert_eq!(attempts, 4);
                assert!(matches!(*last, ProviderError::Http { status: 503, .. }));
            }
            e => panic!("{e:?}"),
        }
        calls.store(0, Ordering::SeqCst);
        let r: Result<(), _> = retry_call(
            &quick(),
            &CancellationToken::new(),
            |_| {},
            || {
                calls.fetch_add(1, Ordering::SeqCst);
                async { Err(ProviderError::InvalidRequest("bad".into())) }
            },
        )
        .await;
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(r.unwrap_err(), ProviderError::InvalidRequest("bad".into()));
    }

    #[tokio::test(start_paused = true)]
    async fn too_long_retry_after_is_not_honoured() {
        let err = ProviderError::RateLimited {
            message: "x".into(),
            retry_after: Some(Duration::from_secs(3600)),
        };
        let r: Result<(), _> = retry_call(
            &quick(),
            &CancellationToken::new(),
            |_| {},
            || {
                let e = err.clone();
                async move { Err(e) }
            },
        )
        .await;
        assert!(matches!(r.unwrap_err(), ProviderError::RateLimited { .. }));
    }

    #[tokio::test(start_paused = true)]
    async fn retry_call_cancel_during_backoff() {
        let cancel = CancellationToken::new();
        let c2 = cancel.clone();
        let r: Result<(), _> = retry_call(
            &quick(),
            &cancel,
            move |_| c2.cancel(),
            || async { Err(rate_limited()) },
        )
        .await;
        assert_eq!(r.unwrap_err(), ProviderError::Cancelled);
    }

    #[tokio::test(start_paused = true)]
    async fn retry_stream_restarts_after_mid_stream_error() {
        let calls = Arc::new(AtomicU32::new(0));
        let c = calls.clone();
        let s = retry_stream(quick(), CancellationToken::new(), move || {
            let n = c.fetch_add(1, Ordering::SeqCst);
            async move {
                let items: Vec<Result<u32, ProviderError>> = if n == 0 {
                    vec![Ok(1), Err(ProviderError::Stalled(Duration::from_secs(1)))]
                } else {
                    vec![Ok(1), Ok(2)]
                };
                Ok(futures::stream::iter(items))
            }
        });
        let out: Vec<_> = s.collect().await;
        assert_eq!(out.len(), 4);
        assert_eq!(out[0], Ok(Attempt::Item(1)));
        match &out[1] {
            Ok(Attempt::Retry(e)) => {
                assert!(e.discard_partial);
                assert_eq!(e.reason, RetryReason::Stalled);
            }
            o => panic!("{o:?}"),
        }
        assert_eq!(out[2..], [Ok(Attempt::Item(1)), Ok(Attempt::Item(2))]);
    }

    #[tokio::test(start_paused = true)]
    async fn retry_stream_open_failure_then_exhaustion() {
        let s = retry_stream(quick(), CancellationToken::new(), || async {
            Err::<futures::stream::Empty<Result<(), ProviderError>>, _>(ProviderError::Connection(
                "refused".into(),
            ))
        });
        let out: Vec<_> = s.collect().await;
        assert_eq!(out.len(), 4);
        for (i, item) in out[..3].iter().enumerate() {
            match item {
                Ok(Attempt::Retry(e)) => {
                    assert_eq!(e.attempt, i as u32 + 1);
                    assert!(!e.discard_partial);
                }
                o => panic!("{o:?}"),
            }
        }
        assert!(matches!(
            out[3],
            Err(ProviderError::RetriesExhausted { attempts: 4, .. })
        ));
    }

    #[tokio::test(start_paused = true)]
    async fn retry_stream_cancel_ends_stream() {
        let cancel = CancellationToken::new();
        let mut s = retry_stream(quick(), cancel.clone(), || async {
            Ok(futures::stream::pending::<Result<(), ProviderError>>())
        });
        cancel.cancel();
        assert_eq!(s.next().await, Some(Err(ProviderError::Cancelled)));
        assert_eq!(s.next().await, None);
    }
}
