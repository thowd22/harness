//! Shared HTTP transport for all providers: reqwest over rustls, per-provider
//! base URL and timeouts, status mapping, stall detection and retries.
//!
//! Providers usually build a stream with [`crate::retry::retry_stream`] whose
//! `make` closure calls [`HttpClient::open`] and decodes the bytes with
//! [`crate::sse::sse_stream`]; a fresh decoder per attempt keeps retried
//! attempts independent. [`HttpClient::post_sse`] does exactly that for raw
//! SSE events.
//!
//! Connections are shared: every [`HttpClient`] for the same base URL (and
//! connect timeout) uses one process-wide reqwest client, so a subagent,
//! session summary or dreaming provider built after the main one reuses its
//! warm (DNS, TCP, TLS, HTTP/2) connection. The shared client carries no
//! credentials: each [`HttpClient`] sends its own
//! [`HttpConfig::default_headers`] (auth, versions) with every request. See
//! [`HttpClient::new`] for the pool settings.
//!
//! When [`HttpConfig::compression`] is set, large JSON bodies for the listed
//! paths are gzip-encoded (see [`compression`]). A server that rejects the
//! encoding gets the raw body once more and compression is switched off for
//! the rest of the process.

pub mod compression;

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};
use std::time::Duration;

use bytes::Bytes;
use futures::stream::{BoxStream, Stream, StreamExt};
use reqwest::header::{CONTENT_ENCODING, CONTENT_TYPE, HeaderMap, HeaderValue};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::provider::ProviderError;
use crate::retry::{Attempt, RetryEvent, RetryPolicy, parse_retry_after, retry_call, retry_stream};
use crate::sse::{SseEvent, sse_stream};

pub use compression::{BodyStats, GzipLatch, RequestCompression};

/// Error bodies are read up to this size to build the error message.
const ERROR_BODY_LIMIT: usize = 64 * 1024;

/// Once part of an error body has arrived, a gap this long ends the read.
const ERROR_BODY_IDLE: Duration = Duration::from_secs(1);

/// A boxed stream of response body chunks.
pub type ByteStream = BoxStream<'static, Result<Bytes, ProviderError>>;

/// Transport settings for one provider.
#[derive(Debug, Clone, PartialEq)]
pub struct HttpConfig {
    /// Base URL, e.g. `https://api.anthropic.com`. Request paths are appended.
    pub base_url: String,
    /// TCP + TLS connect timeout.
    pub connect_timeout: Duration,
    /// Whole-request timeout for non-streaming calls ([`HttpClient::post_json`]).
    pub request_timeout: Duration,
    /// A call that receives no bytes (headers or body) for this long is
    /// aborted with [`ProviderError::Stalled`] and retried.
    pub stall_timeout: Duration,
    /// Retry policy.
    pub retry: RetryPolicy,
    /// Headers sent with every request (auth, versions).
    pub default_headers: HeaderMap,
    /// gzip for request bodies; `None` (the default) sends every body raw.
    pub compression: Option<RequestCompression>,
}

impl HttpConfig {
    /// Defaults (connect 10s, request 600s, stall 90s, default retries) for
    /// `base_url`.
    pub fn new(base_url: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into(),
            connect_timeout: Duration::from_secs(10),
            request_timeout: Duration::from_secs(600),
            stall_timeout: Duration::from_secs(90),
            retry: RetryPolicy::default(),
            default_headers: HeaderMap::new(),
            compression: None,
        }
    }
}

/// How long an idle pooled connection is kept open.
pub const POOL_IDLE_TIMEOUT: Duration = Duration::from_secs(300);

/// TCP keepalive interval on pooled connections.
pub const TCP_KEEPALIVE: Duration = Duration::from_secs(30);

/// HTTP/2 ping interval, also while a connection is idle, so a connection
/// a NAT or load balancer silently dropped is noticed before the next
/// request instead of by it.
pub const HTTP2_KEEPALIVE_INTERVAL: Duration = Duration::from_secs(30);

/// How long an HTTP/2 keepalive ping may go unanswered before the
/// connection is closed.
pub const HTTP2_KEEPALIVE_TIMEOUT: Duration = Duration::from_secs(10);

/// Key of a shared reqwest client: base URL, connect timeout and the tokio
/// runtime the connections live on (a pooled connection's driver task runs
/// on the runtime that opened it, so runtimes never share one).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct PoolKey {
    base_url: String,
    connect_timeout: Duration,
    runtime: Option<tokio::runtime::Id>,
}

/// The process-wide shared clients.
static POOLS: LazyLock<Mutex<HashMap<PoolKey, reqwest::Client>>> = LazyLock::new(Mutex::default);

/// A reqwest client (rustls, HTTP/2 when offered) with the pool and
/// keepalive settings above and no default headers.
fn build_client(connect_timeout: Duration) -> Result<reqwest::Client, ProviderError> {
    reqwest::Client::builder()
        .connect_timeout(connect_timeout)
        .pool_idle_timeout(POOL_IDLE_TIMEOUT)
        .tcp_keepalive(TCP_KEEPALIVE)
        .http2_keep_alive_interval(HTTP2_KEEPALIVE_INTERVAL)
        .http2_keep_alive_timeout(HTTP2_KEEPALIVE_TIMEOUT)
        .http2_keep_alive_while_idle(true)
        .user_agent(concat!("harness/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|e| ProviderError::Other(format!("building HTTP client: {e}")))
}

/// Where an [`HttpClient`] gets its reqwest client.
#[derive(Debug, Clone)]
enum Pool {
    /// The process-wide client for this config's base URL.
    Shared,
    /// A private client (its own connections).
    Isolated(reqwest::Client),
}

/// Cheaply clonable HTTP client bound to one provider's [`HttpConfig`].
#[derive(Debug, Clone)]
pub struct HttpClient {
    pool: Pool,
    config: HttpConfig,
}

impl HttpClient {
    /// A client on the process-wide connection pool for `config.base_url`
    /// (rustls, HTTP/2 when offered; idle connections kept
    /// [`POOL_IDLE_TIMEOUT`], TCP keepalive [`TCP_KEEPALIVE`], HTTP/2 pings
    /// every [`HTTP2_KEEPALIVE_INTERVAL`] while idle). Clients for the same
    /// base URL share connections; `config.default_headers` stay with this
    /// client and are sent with each of its requests only.
    pub fn new(config: HttpConfig) -> Result<Self, ProviderError> {
        let client = Self {
            pool: Pool::Shared,
            config,
        };
        client.reqwest()?;
        Ok(client)
    }

    /// A client with its own connection pool, shared with no other client.
    /// For measuring connection setup (`harness bench --net`).
    pub fn isolated(config: HttpConfig) -> Result<Self, ProviderError> {
        Ok(Self {
            pool: Pool::Isolated(build_client(config.connect_timeout)?),
            config,
        })
    }

    /// True when `self` and `other` send their requests over the same
    /// connection pool (in the current tokio runtime).
    pub fn shares_connections_with(&self, other: &HttpClient) -> bool {
        match (&self.pool, &other.pool) {
            (Pool::Shared, Pool::Shared) => self.pool_key() == other.pool_key(),
            _ => false,
        }
    }

    fn pool_key(&self) -> PoolKey {
        PoolKey {
            base_url: self.config.base_url.trim_end_matches('/').to_owned(),
            connect_timeout: self.config.connect_timeout,
            runtime: tokio::runtime::Handle::try_current().ok().map(|h| h.id()),
        }
    }

    /// The reqwest client to send on: the shared one for this base URL and
    /// runtime (built on first use), or the private one.
    fn reqwest(&self) -> Result<reqwest::Client, ProviderError> {
        if let Pool::Isolated(c) = &self.pool {
            return Ok(c.clone());
        }
        let key = self.pool_key();
        let mut pools = POOLS.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(c) = pools.get(&key) {
            return Ok(c.clone());
        }
        let c = build_client(key.connect_timeout)?;
        pools.insert(key, c.clone());
        Ok(c)
    }

    /// A request to `url` carrying this client's default headers.
    fn request(
        &self,
        method: reqwest::Method,
        url: String,
    ) -> Result<reqwest::RequestBuilder, ProviderError> {
        Ok(self
            .reqwest()?
            .request(method, url)
            .headers(self.config.default_headers.clone()))
    }

    /// The client's configuration.
    pub fn config(&self) -> &HttpConfig {
        &self.config
    }

    /// `base_url` joined with `path` (exactly one `/` between them).
    pub fn url(&self, path: &str) -> String {
        format!(
            "{}/{}",
            self.config.base_url.trim_end_matches('/'),
            path.trim_start_matches('/')
        )
    }

    /// One POST attempt with a JSON body, returning the body as a
    /// stall-guarded byte stream. Non-success statuses become typed errors
    /// with `retry-after` parsed. No retries.
    pub async fn open(
        &self,
        path: &str,
        headers: &HeaderMap,
        body: &Value,
    ) -> Result<ByteStream, ProviderError> {
        Ok(self.open_measured(path, headers, body).await?.0)
    }

    /// Like [`HttpClient::open`], also reporting the raw and on-the-wire
    /// size of the body that was accepted.
    pub async fn open_measured(
        &self,
        path: &str,
        headers: &HeaderMap,
        body: &Value,
    ) -> Result<(ByteStream, BodyStats), ProviderError> {
        let (response, stats) = self.send(path, headers, body, None).await?;
        let limit = self.config.connect_timeout;
        let bytes = response
            .bytes_stream()
            .map(move |r| r.map_err(|e| map_reqwest(e, limit)));
        Ok((stall_guard(bytes, self.config.stall_timeout), stats))
    }

    /// Serializes `body` and sends it, gzipped when
    /// [`HttpConfig::compression`] applies. A rejected encoding trips the
    /// latch and the raw body is sent once more; whatever that second
    /// attempt returns is the result.
    async fn send(
        &self,
        path: &str,
        headers: &HeaderMap,
        body: &Value,
        timeout: Option<Duration>,
    ) -> Result<(reqwest::Response, BodyStats), ProviderError> {
        // The same bytes reqwest's `.json()` would send.
        let raw = Bytes::from(
            serde_json::to_vec(body)
                .map_err(|e| ProviderError::InvalidRequest(format!("request JSON: {e}")))?,
        );
        if let Some(c) = self
            .config
            .compression
            .as_ref()
            .filter(|c| c.applies(path, raw.len()))
        {
            let wire = Bytes::from(compression::gzip(&raw, c.level));
            let stats = BodyStats {
                raw_bytes: raw.len() as u64,
                wire_bytes: wire.len() as u64,
                encoding: Some("gzip".into()),
            };
            match self.attempt(path, headers, wire, true, timeout).await {
                Ok(response) => return Ok((response, stats)),
                Err(Failure::Rejected(e)) => {
                    if c.latch.trip() {
                        tracing::warn!(
                            path,
                            error = %e,
                            "server rejected a gzip request body; resending it uncompressed \
                             and sending raw bodies for the rest of this process"
                        );
                    }
                }
                Err(Failure::Error(e)) => return Err(e),
            }
        }
        let stats = BodyStats::raw(raw.len());
        match self.attempt(path, headers, raw, false, timeout).await {
            Ok(response) => Ok((response, stats)),
            Err(Failure::Rejected(e) | Failure::Error(e)) => Err(e),
        }
    }

    /// One POST of `wire` (gzip-encoded when `gzipped`). A failed response
    /// is always dropped before this returns, so its connection is closed
    /// or back in the pool before any resend.
    async fn attempt(
        &self,
        path: &str,
        headers: &HeaderMap,
        wire: Bytes,
        gzipped: bool,
        timeout: Option<Duration>,
    ) -> Result<reqwest::Response, Failure> {
        let mut req = self
            .request(reqwest::Method::POST, self.url(path))
            .map_err(Failure::Error)?
            .headers(headers.clone())
            .header(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        if gzipped {
            req = req.header(CONTENT_ENCODING, HeaderValue::from_static("gzip"));
        }
        req = req.body(wire);
        if let Some(t) = timeout {
            req = req.timeout(t);
        }
        let stall = self.config.stall_timeout;
        let limit = timeout.unwrap_or(self.config.connect_timeout);
        let response = tokio::time::timeout(stall, req.send())
            .await
            .map_err(|_| Failure::Error(ProviderError::Stalled(stall)))?
            .map_err(|e| Failure::Error(map_reqwest(e, limit)))?;
        if response.status().is_success() {
            return Ok(response);
        }
        let status = response.status().as_u16();
        let retry_after = parse_retry_after(response.headers());
        let resp_headers = response.headers().clone();
        let text = if gzipped && status == 415 {
            drop(response);
            String::new()
        } else {
            read_error_body(response, stall).await
        };
        let err = error_from_response(status, &text, retry_after);
        if gzipped && compression::is_encoding_rejection(status, &resp_headers, &text) {
            Err(Failure::Rejected(err))
        } else {
            Err(Failure::Error(err))
        }
    }

    /// Sends `GET path` on this client's connection pool and waits for the
    /// response headers only (the body is dropped). Returns the negotiated
    /// HTTP version and the time to headers. Used to open (pre-warm) a
    /// connection and by `harness bench` to measure connection setup.
    pub async fn warm(&self, path: &str) -> Result<(String, Duration), ProviderError> {
        let start = std::time::Instant::now();
        let response = tokio::time::timeout(
            self.config.connect_timeout * 2,
            self.request(reqwest::Method::GET, self.url(path))?.send(),
        )
        .await
        .map_err(|_| ProviderError::Timeout(self.config.connect_timeout * 2))?
        .map_err(|e| map_reqwest(e, self.config.connect_timeout))?;
        let took = start.elapsed();
        Ok((format!("{:?}", response.version()), took))
    }

    /// `GET path` with this client's default headers and the whole-request
    /// timeout. Returns the status and the body text, whatever the status.
    pub async fn get_text(&self, path: &str) -> Result<(u16, String), ProviderError> {
        let limit = self.config.request_timeout;
        let response = self
            .request(reqwest::Method::GET, self.url(path))?
            .timeout(limit)
            .send()
            .await
            .map_err(|e| map_reqwest(e, limit))?;
        let status = response.status().as_u16();
        let text = response.text().await.map_err(|e| map_reqwest(e, limit))?;
        Ok((status, text))
    }

    /// Streams SSE events from a POST, with retries, stall detection and
    /// cancellation. On retry, an [`Attempt::Retry`] item is yielded and the
    /// request is sent again from scratch.
    pub fn post_sse(
        &self,
        path: &str,
        headers: HeaderMap,
        body: Value,
        cancel: CancellationToken,
    ) -> BoxStream<'static, Result<Attempt<SseEvent>, ProviderError>> {
        let client = self.clone();
        let path = path.to_owned();
        retry_stream(self.config.retry, cancel, move || {
            let (client, path, headers, body) =
                (client.clone(), path.clone(), headers.clone(), body.clone());
            async move { Ok(sse_stream(client.open(&path, &headers, &body).await?)) }
        })
    }

    /// A non-streaming JSON POST with retries and the whole-request timeout.
    /// `on_retry` is called before every retry.
    pub async fn post_json(
        &self,
        path: &str,
        headers: &HeaderMap,
        body: &Value,
        cancel: &CancellationToken,
        on_retry: impl FnMut(&RetryEvent),
    ) -> Result<Value, ProviderError> {
        let limit = self.config.request_timeout;
        retry_call(&self.config.retry, cancel, on_retry, || async move {
            let (response, _) = self.send(path, headers, body, Some(limit)).await?;
            let bytes = response.bytes().await.map_err(|e| map_reqwest(e, limit))?;
            serde_json::from_slice(&bytes)
                .map_err(|e| ProviderError::Decode(format!("response JSON: {e}")))
        })
        .await
    }
}

/// Why an attempt failed.
enum Failure {
    /// The server rejected the request's `Content-Encoding`.
    Rejected(ProviderError),
    /// Any other failure.
    Error(ProviderError),
}

/// Reads an error body (for its message) and drops the response. Reading
/// stops at the end of the body, at [`ERROR_BODY_LIMIT`] bytes, once the
/// bytes so far form a complete JSON document, after a gap of
/// [`ERROR_BODY_IDLE`] once some bytes arrived, or after `stall`, whichever
/// comes first; a body that never ends cannot hold the call open.
async fn read_error_body(mut response: reqwest::Response, stall: Duration) -> String {
    let mut buf: Vec<u8> = Vec::new();
    let deadline = tokio::time::Instant::now() + stall;
    while buf.len() < ERROR_BODY_LIMIT {
        let until = if buf.is_empty() {
            deadline
        } else {
            deadline.min(tokio::time::Instant::now() + ERROR_BODY_IDLE)
        };
        match tokio::time::timeout_at(until, response.chunk()).await {
            Ok(Ok(Some(chunk))) => {
                buf.extend_from_slice(&chunk);
                if serde_json::from_slice::<serde::de::IgnoredAny>(&buf).is_ok() {
                    break;
                }
            }
            _ => break,
        }
    }
    drop(response);
    String::from_utf8_lossy(&buf).into_owned()
}

/// Wraps `stream` so that a gap of more than `timeout` between chunks
/// yields [`ProviderError::Stalled`] and ends the stream.
pub fn stall_guard<S>(stream: S, timeout: Duration) -> ByteStream
where
    S: Stream<Item = Result<Bytes, ProviderError>> + Send + 'static,
{
    futures::stream::unfold(Some(Box::pin(stream)), move |state| async move {
        let mut s = state?;
        match tokio::time::timeout(timeout, s.next()).await {
            Ok(Some(item)) => Some((item, Some(s))),
            Ok(None) => None,
            Err(_) => Some((Err(ProviderError::Stalled(timeout)), None)),
        }
    })
    .boxed()
}

/// Maps a reqwest error; `limit` is the timeout that applied to the call.
fn map_reqwest(e: reqwest::Error, limit: Duration) -> ProviderError {
    if e.is_timeout() {
        ProviderError::Timeout(limit)
    } else if e.is_builder() {
        ProviderError::InvalidRequest(e.to_string())
    } else if let Some(status) = e.status() {
        ProviderError::from_status(status.as_u16(), e.to_string(), None)
    } else {
        ProviderError::Connection(error_chain(&e))
    }
}

fn error_chain(e: &dyn std::error::Error) -> String {
    let mut out = e.to_string();
    let mut src = e.source();
    while let Some(s) = src {
        out.push_str(": ");
        out.push_str(&s.to_string());
        src = s.source();
    }
    out
}

/// The typed error for a non-success response: a ChatGPT plan usage limit
/// (`subscription_sharing_usage_limit_exceeded`) becomes
/// [`ProviderError::UsageLimit`], which is never retried; everything else
/// goes through [`ProviderError::from_status`].
pub fn error_from_response(
    status: u16,
    body: &str,
    retry_after: Option<Duration>,
) -> ProviderError {
    let code = serde_json::from_str::<Value>(body).ok().and_then(|v| {
        v.pointer("/error/code")
            .or_else(|| v.get("code"))
            .and_then(Value::as_str)
            .map(str::to_owned)
    });
    if let Some(e) = code
        .as_deref()
        .and_then(|c| crate::chatgpt::usage_limit_error(c, &error_message(body)))
    {
        return e;
    }
    ProviderError::from_status(status, error_message(body), retry_after)
}

/// Extracts a human-readable message from an error body: `error.message`,
/// `message` or `error` (string) from JSON, otherwise the text truncated to
/// 500 characters.
pub fn error_message(body: &str) -> String {
    if let Ok(v) = serde_json::from_str::<Value>(body) {
        let found = v
            .pointer("/error/message")
            .or_else(|| v.get("message"))
            .or_else(|| v.get("error").filter(|e| e.is_string()));
        if let Some(Value::String(s)) = found {
            return s.clone();
        }
    }
    body.chars().take(500).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_joining() {
        let c = HttpClient::new(HttpConfig::new("http://h:1/v1/")).unwrap();
        assert_eq!(c.url("/messages"), "http://h:1/v1/messages");
        assert_eq!(c.url("chat"), "http://h:1/v1/chat");
    }

    #[test]
    fn usage_limit_bodies_become_a_non_retried_usage_limit_error() {
        let body = r#"{"error":{"code":"subscription_sharing_usage_limit_exceeded","message":"Usage limit reached"}}"#;
        let e = error_from_response(429, body, None);
        assert!(matches!(e, ProviderError::UsageLimit { .. }), "{e:?}");
        assert!(e.retry_reason().is_none());
        assert_eq!(e.action().unwrap().label, "Manage usage");
        assert_eq!(e.action().unwrap().url, crate::chatgpt::MANAGE_USAGE_URL);
        let plain = error_from_response(429, r#"{"error":{"message":"slow"}}"#, None);
        assert!(matches!(plain, ProviderError::RateLimited { .. }));
        assert!(plain.action().is_none());
    }

    #[test]
    fn error_message_extraction() {
        assert_eq!(
            error_message(
                r#"{"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}"#
            ),
            "Overloaded"
        );
        assert_eq!(error_message(r#"{"message":"m"}"#), "m");
        assert_eq!(error_message(r#"{"error":"e"}"#), "e");
        assert_eq!(error_message("plain"), "plain");
        assert_eq!(error_message(&"x".repeat(900)).len(), 500);
    }

    #[tokio::test(start_paused = true)]
    async fn stall_guard_fires_on_gap() {
        let s = futures::stream::iter(vec![Ok(Bytes::from_static(b"a"))])
            .chain(futures::stream::pending());
        let out: Vec<_> = stall_guard(s, Duration::from_secs(5)).collect().await;
        assert_eq!(
            out,
            vec![
                Ok(Bytes::from_static(b"a")),
                Err(ProviderError::Stalled(Duration::from_secs(5)))
            ]
        );
    }
}
