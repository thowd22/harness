//! Anthropic Messages API provider.
//!
//! [`AnthropicProvider`] streams `POST /v1/messages` with text, parallel tool
//! use, adaptive or budgeted thinking and prompt caching. Requests are built
//! by [`request::build_body`] and SSE events are decoded by
//! [`stream::decode_stream`]; both are public so golden tests can check them
//! without a network.
//!
//! The API key is only placed in a sensitive `x-api-key` header and is never
//! logged or echoed in errors.
//!
//! Request bodies of [`compression::MIN_GZIP_BYTES`] or more for
//! `/v1/messages` and `/v1/messages/count_tokens` are gzip-encoded when the
//! provider talks to `https://api.anthropic.com` directly (see
//! [`GzipRequests`] and [`gzip_enabled`]). Each response carries a
//! [`Notice::RequestBody`] block with the raw and wire size.
//!
//! Request options from `[provider.anthropic]` (see docs/providers-anthropic.md):
//! `thinking_display` ([`request::ThinkingDisplay`]; `updates` adds the
//! [`request::THINKING_UPDATES_BETA`] beta), `cache_ttl`
//! ([`request::CacheTtl`]) and `cache_ttl_auto_1h`
//! ([`AnthropicConfig::long_wait`]: one-hour breakpoints while the
//! [`LongWait`] hint is active). [`Provider::prewarm_cache`] sends the
//! [`request::build_prewarm_body`] call.

pub mod request;
pub mod stream;

use std::sync::Arc;

use async_trait::async_trait;
use futures::StreamExt;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::http::compression::{self, DEFAULT_GZIP_LEVEL};
use crate::http::{GzipLatch, HttpClient, HttpConfig, RequestCompression};
use crate::model::{Capabilities, ChatRequest, Usage};
use crate::openai::notice::{Notice, REQUEST_BODY_INDEX};
use crate::provider::{EventStream, Provider, ProviderError, StreamEvent};
use crate::registry::{ModelRef, Registry, ThinkingStyle};
use crate::retry::{Attempt, retry_stream};
use crate::sse::sse_stream;

pub use crate::cache_hint::LongWait;
pub use request::{CacheMode, CacheTtl, ModelProfile, RequestOptions, ThinkingDisplay};

/// Provider name used in `harness.ini` and model references.
pub const NAME: &str = "anthropic";

/// Default API base URL.
pub const DEFAULT_BASE_URL: &str = "https://api.anthropic.com";

/// Messages endpoint path.
pub const MESSAGES_PATH: &str = "/v1/messages";

/// Token counting endpoint path.
pub const COUNT_TOKENS_PATH: &str = "/v1/messages/count_tokens";

/// Environment variable that disables request gzip when set to `0`
/// (also `false`, `off` or `no`).
pub const GZIP_ENV: &str = "HARNESS_GZIP_REQUEST_BODIES";

/// Request-body gzip mode (`[provider.anthropic] gzip_requests`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum GzipRequests {
    /// gzip large bodies only when talking to `https://api.anthropic.com`
    /// on the default port, with no proxy and no custom CA configured.
    #[default]
    Auto,
    /// Never compress.
    Off,
    /// gzip large bodies to any base URL. Not settable from `harness.ini`;
    /// tests and benchmarks use it against local servers.
    Always,
}

/// `anthropic-version` header value sent by default.
pub const API_VERSION: &str = "2026-10-21";

/// Configuration for [`AnthropicProvider`].
#[derive(Debug, Clone, PartialEq)]
pub struct AnthropicConfig {
    /// Transport settings (base URL, timeouts, retries).
    pub http: HttpConfig,
    /// `anthropic-version` header value.
    pub api_version: String,
    /// Values for the `anthropic-beta` header, comma-joined; empty sends none.
    pub betas: Vec<String>,
    /// Request builder options (cache placement, default `max_tokens`).
    pub request: RequestOptions,
    /// Request-body gzip mode.
    pub gzip: GzipRequests,
    /// gzip level, 1 to 9.
    pub gzip_level: u32,
    /// Switch that a rejected encoding trips; the process-wide one by
    /// default.
    pub gzip_latch: GzipLatch,
    /// `cache_ttl_auto_1h`: while this hint is active, every breakpoint
    /// gets the one-hour lifetime regardless of `request.cache_ttl`.
    /// `None` (the default) never upgrades.
    pub long_wait: Option<LongWait>,
}

impl Default for AnthropicConfig {
    fn default() -> Self {
        Self::new()
    }
}

impl AnthropicConfig {
    /// Defaults for the public API.
    pub fn new() -> Self {
        Self {
            http: HttpConfig::new(DEFAULT_BASE_URL),
            api_version: API_VERSION.to_owned(),
            betas: Vec::new(),
            request: RequestOptions::default(),
            gzip: GzipRequests::Auto,
            gzip_level: DEFAULT_GZIP_LEVEL,
            gzip_latch: GzipLatch::process(),
            long_wait: None,
        }
    }

    /// Uses `base_url` instead of [`DEFAULT_BASE_URL`] (from
    /// `[provider.anthropic] base_url`).
    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.http.base_url = base_url.into();
        self
    }
}

/// Environment variables that make reqwest use a proxy for HTTPS.
const PROXY_ENV: &[&str] = &["HTTPS_PROXY", "https_proxy", "ALL_PROXY", "all_proxy"];

/// Environment variables that point TLS at a custom CA bundle.
const CUSTOM_CA_ENV: &[&str] = &["SSL_CERT_FILE", "SSL_CERT_DIR"];

/// Whether `base_url` is the public API reached directly: `https`, host
/// `api.anthropic.com`, the default port and no path, with no proxy and no
/// custom CA set in the environment (read through `env`). Anything in
/// between could be a gateway that mangles or rejects encoded bodies.
pub fn gzip_eligible(base_url: &str, env: &dyn Fn(&str) -> Option<String>) -> bool {
    let Ok(url) = reqwest::Url::parse(base_url) else {
        return false;
    };
    let set = |k: &&str| env(k).is_some_and(|v| !v.trim().is_empty());
    url.scheme() == "https"
        && url.host_str() == Some("api.anthropic.com")
        && url.port().is_none()
        && matches!(url.path(), "" | "/")
        && url.username().is_empty()
        && !PROXY_ENV.iter().any(set)
        && !CUSTOM_CA_ENV.iter().any(set)
}

/// Whether request gzip is on for `config`: [`GzipRequests::Off`] and
/// `HARNESS_GZIP_REQUEST_BODIES=0` always win, [`GzipRequests::Auto`]
/// requires [`gzip_eligible`].
pub fn gzip_enabled(config: &AnthropicConfig, env: &dyn Fn(&str) -> Option<String>) -> bool {
    let env_off = env(GZIP_ENV).is_some_and(|v| {
        matches!(
            v.trim().to_ascii_lowercase().as_str(),
            "0" | "false" | "off" | "no"
        )
    });
    !env_off
        && match config.gzip {
            GzipRequests::Off => false,
            GzipRequests::Always => true,
            GzipRequests::Auto => gzip_eligible(&config.http.base_url, env),
        }
}

/// The Anthropic provider.
#[derive(Debug, Clone)]
pub struct AnthropicProvider {
    client: HttpClient,
    options: RequestOptions,
    long_wait: Option<LongWait>,
    registry: Arc<Registry>,
}

fn header(
    name: &'static str,
    value: &str,
    sensitive: bool,
) -> Result<(HeaderName, HeaderValue), ProviderError> {
    let mut v = HeaderValue::from_str(value).map_err(|_| {
        ProviderError::InvalidRequest(format!(
            "{name} contains characters not allowed in a header"
        ))
    })?;
    v.set_sensitive(sensitive);
    Ok((HeaderName::from_static(name), v))
}

impl AnthropicProvider {
    /// Creates a provider authenticated with `api_key`. Capabilities come
    /// from [`Registry::shared`] unless [`AnthropicProvider::with_registry`]
    /// is used.
    pub fn new(api_key: &str, config: AnthropicConfig) -> Result<Self, ProviderError> {
        if api_key.trim().is_empty() {
            return Err(ProviderError::Auth(
                "no Anthropic API key: set api_key, api_key_cmd or api_key_env in [provider.anthropic]".into(),
            ));
        }
        let gzip = gzip_enabled(&config, &|k| std::env::var(k).ok());
        let mut http = config.http;
        http.compression = gzip.then(|| {
            RequestCompression::gzip(config.gzip_level, [MESSAGES_PATH, COUNT_TOKENS_PATH])
                .with_latch(config.gzip_latch.clone())
        });
        for (name, value, sensitive) in [
            ("x-api-key", api_key.trim(), true),
            ("anthropic-version", config.api_version.as_str(), false),
        ] {
            let (k, v) = header(name, value, sensitive)?;
            http.default_headers.insert(k, v);
        }
        let mut betas = config.betas.clone();
        if config.request.thinking_display == ThinkingDisplay::Updates
            && !betas.iter().any(|b| b == request::THINKING_UPDATES_BETA)
        {
            betas.push(request::THINKING_UPDATES_BETA.to_owned());
        }
        if !betas.is_empty() {
            let (k, v) = header("anthropic-beta", &betas.join(","), false)?;
            http.default_headers.insert(k, v);
        }
        Ok(Self {
            client: HttpClient::new(http)?,
            options: config.request,
            long_wait: config.long_wait,
            registry: Arc::new(Registry::builtin()),
        })
    }

    /// Uses `registry` for model capabilities and thinking style.
    pub fn with_registry(mut self, registry: Arc<Registry>) -> Self {
        self.registry = registry;
        self
    }

    /// The transport configuration in use.
    pub fn http_config(&self) -> &HttpConfig {
        self.client.config()
    }

    /// What the request builder assumes about `model`. Unknown models get
    /// conservative capabilities but adaptive thinking, which every current
    /// Claude model accepts.
    pub fn profile(&self, model: &str) -> ModelProfile {
        match self.registry.lookup(&ModelRef::new(NAME, model)) {
            Some(info) => ModelProfile {
                capabilities: info.capabilities,
                thinking: info.thinking,
            },
            None => ModelProfile {
                capabilities: crate::registry::CONSERVATIVE_CAPABILITIES,
                thinking: ThinkingStyle::Adaptive,
            },
        }
    }

    /// The request options for the next call: the configured ones, with
    /// one-hour breakpoints while the [`LongWait`] hint is active.
    pub fn options(&self) -> RequestOptions {
        let mut o = self.options;
        if self.long_wait.as_ref().is_some_and(LongWait::active) {
            o.cache_ttl = CacheTtl::OneHour;
        }
        o
    }

    /// The JSON body that [`Provider::stream`] would send for `request`.
    pub fn build_body(&self, request: &ChatRequest) -> Result<Value, ProviderError> {
        request::build_body(request, &self.profile(&request.model), &self.options())
    }

    /// The cache pre-warm body for `request` (see
    /// [`request::build_prewarm_body`]); `None` when it cannot help.
    pub fn prewarm_body(&self, request: &ChatRequest) -> Result<Option<Value>, ProviderError> {
        request::build_prewarm_body(request, &self.profile(&request.model), &self.options())
    }

    /// The body [`AnthropicProvider::count_tokens`] sends: the messages body
    /// without the generation-only fields the endpoint does not take.
    pub fn count_tokens_body(&self, request: &ChatRequest) -> Result<Value, ProviderError> {
        let mut body = self.build_body(request)?;
        if let Some(o) = body.as_object_mut() {
            for k in [
                "max_tokens",
                "stream",
                "temperature",
                "top_p",
                "top_k",
                "stop_sequences",
                "metadata",
                "service_tier",
            ] {
                o.remove(k);
            }
        }
        Ok(body)
    }

    /// Input tokens `request` would use (`POST /v1/messages/count_tokens`),
    /// with retries.
    pub async fn count_tokens(
        &self,
        request: &ChatRequest,
        cancel: &CancellationToken,
    ) -> Result<u64, ProviderError> {
        let body = self.count_tokens_body(request)?;
        let v = self
            .client
            .post_json(COUNT_TOKENS_PATH, &HeaderMap::new(), &body, cancel, |_| {})
            .await?;
        v.get("input_tokens")
            .and_then(Value::as_u64)
            .ok_or_else(|| ProviderError::Decode("count_tokens: no input_tokens".into()))
    }
}

/// `stream` with a [`Notice::RequestBody`] block for `stats` inserted just
/// before `MessageStop`, so it lands in the response without counting as
/// partial output if the attempt fails earlier.
fn with_body_notice<S>(
    stream: S,
    stats: compression::BodyStats,
) -> impl futures::Stream<Item = Result<StreamEvent, ProviderError>> + Send
where
    S: futures::Stream<Item = Result<StreamEvent, ProviderError>> + Send,
{
    let notice = Notice::RequestBody {
        provider: NAME.into(),
        raw_bytes: stats.raw_bytes,
        wire_bytes: stats.wire_bytes,
        encoding: stats.encoding,
    }
    .to_event(REQUEST_BODY_INDEX);
    let mut notice = Some(notice);
    stream.flat_map(move |item| {
        let items = match (&item, notice.take()) {
            (Ok(StreamEvent::MessageStop { .. }), Some(n)) => vec![Ok(n), item],
            (_, n) => {
                notice = n;
                vec![item]
            }
        };
        futures::stream::iter(items)
    })
}

/// The usage of a non-streaming Messages response.
fn prewarm_usage(response: &Value) -> Usage {
    let u = &response["usage"];
    let n = |k: &str| u.get(k).and_then(Value::as_u64).unwrap_or(0);
    Usage {
        input_tokens: n("input_tokens"),
        output_tokens: n("output_tokens"),
        cache_read_tokens: n("cache_read_input_tokens"),
        cache_write_tokens: n("cache_creation_input_tokens"),
        reasoning_tokens: 0,
    }
}

#[async_trait]
impl Provider for AnthropicProvider {
    fn name(&self) -> &str {
        NAME
    }

    fn request_body(&self, request: &ChatRequest) -> Option<Value> {
        self.build_body(request).ok()
    }

    async fn prewarm(&self) {
        if let Err(e) = self.client.warm("/v1/models").await {
            tracing::debug!(error = %e, "anthropic connection pre-warm failed");
        }
    }

    fn capabilities(&self, model: &str) -> Capabilities {
        self.registry.capabilities(NAME, model)
    }

    fn can_prewarm_cache(&self, request: &ChatRequest) -> bool {
        matches!(self.prewarm_body(request), Ok(Some(_)))
    }

    async fn prewarm_cache(
        &self,
        request: ChatRequest,
        cancel: CancellationToken,
    ) -> Result<Usage, ProviderError> {
        let body = self.prewarm_body(&request)?.ok_or_else(|| {
            ProviderError::InvalidRequest(
                "cache pre-warm needs prompt caching and adaptive (or no) thinking".into(),
            )
        })?;
        let v = self
            .client
            .post_json(MESSAGES_PATH, &HeaderMap::new(), &body, &cancel, |_| {})
            .await?;
        Ok(prewarm_usage(&v))
    }

    async fn stream(
        &self,
        request: ChatRequest,
        cancel: CancellationToken,
    ) -> Result<EventStream, ProviderError> {
        let body = Arc::new(self.build_body(&request)?);
        let client = self.client.clone();
        let attempts = retry_stream(client.config().retry, cancel, move || {
            let (client, body) = (client.clone(), body.clone());
            async move {
                let (bytes, stats) = client
                    .open_measured(MESSAGES_PATH, &HeaderMap::new(), &body)
                    .await?;
                Ok(with_body_notice(
                    stream::decode_stream(sse_stream(bytes)),
                    stats,
                ))
            }
        });
        Ok(Box::pin(attempts.map(|item| {
            item.map(|attempt| match attempt {
                Attempt::Item(event) => event,
                Attempt::Retry(retry) => StreamEvent::Retry(retry),
            })
        })))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_key_is_an_auth_error_and_headers_are_set() {
        assert!(matches!(
            AnthropicProvider::new("  ", AnthropicConfig::new()),
            Err(ProviderError::Auth(_))
        ));
        let mut cfg = AnthropicConfig::new().with_base_url("http://localhost:1");
        cfg.betas = vec!["a-1".into(), "b-2".into()];
        let p = AnthropicProvider::new("sk-test", cfg).unwrap();
        let h = &p.http_config().default_headers;
        assert!(h["x-api-key"].is_sensitive());
        assert_eq!(h["anthropic-version"], API_VERSION);
        assert_eq!(h["anthropic-beta"], "a-1,b-2");
        assert_eq!(p.http_config().base_url, "http://localhost:1");
        assert!(!format!("{p:?}").contains("sk-test"));
    }

    fn env(pairs: &'static [(&'static str, &'static str)]) -> impl Fn(&str) -> Option<String> {
        move |k| {
            pairs
                .iter()
                .find(|(n, _)| *n == k)
                .map(|(_, v)| (*v).to_owned())
        }
    }

    #[test]
    fn gzip_is_eligible_only_for_the_public_api_reached_directly() {
        let clean = env(&[]);
        for ok in [
            "https://api.anthropic.com",
            "https://api.anthropic.com/",
            "https://api.anthropic.com:443",
            "https://API.anthropic.com",
        ] {
            assert!(gzip_eligible(ok, &clean), "{ok}");
        }
        for no in [
            "http://api.anthropic.com",
            "https://api.anthropic.com:8443",
            "https://api.anthropic.com/proxy",
            "https://gateway.example.com",
            "https://api.anthropic.com.evil.example",
            "http://localhost:8080",
            "https://user@api.anthropic.com",
            "not a url",
        ] {
            assert!(!gzip_eligible(no, &clean), "{no}");
        }
        for var in [
            "HTTPS_PROXY",
            "https_proxy",
            "ALL_PROXY",
            "all_proxy",
            "SSL_CERT_FILE",
            "SSL_CERT_DIR",
        ] {
            let e = move |k: &str| (k == var).then(|| "/x".to_owned());
            assert!(!gzip_eligible(DEFAULT_BASE_URL, &e), "{var}");
        }
        let blank = env(&[("HTTPS_PROXY", " ")]);
        assert!(gzip_eligible(DEFAULT_BASE_URL, &blank));
    }

    #[test]
    fn gzip_mode_and_env_switch() {
        let clean = env(&[]);
        let mut cfg = AnthropicConfig::new();
        assert_eq!(cfg.gzip, GzipRequests::Auto);
        assert_eq!(cfg.gzip_level, 3);
        assert!(gzip_enabled(&cfg, &clean));
        for v in ["0", "false", "off", "no", " OFF "] {
            let e = move |k: &str| (k == GZIP_ENV).then(|| v.to_owned());
            assert!(!gzip_enabled(&cfg, &e), "{v}");
        }
        assert!(gzip_enabled(
            &cfg,
            &env(&[("HARNESS_GZIP_REQUEST_BODIES", "1")])
        ));
        cfg.gzip = GzipRequests::Off;
        assert!(!gzip_enabled(&cfg, &clean));
        cfg = cfg.with_base_url("http://localhost:1");
        cfg.gzip = GzipRequests::Auto;
        assert!(!gzip_enabled(&cfg, &clean));
        cfg.gzip = GzipRequests::Always;
        assert!(gzip_enabled(&cfg, &clean));
        assert!(!gzip_enabled(
            &cfg,
            &env(&[("HARNESS_GZIP_REQUEST_BODIES", "0")])
        ));
    }

    #[test]
    fn compression_covers_messages_and_count_tokens() {
        let mut cfg = AnthropicConfig::new().with_base_url("http://localhost:1");
        cfg.gzip = GzipRequests::Always;
        cfg.gzip_level = 6;
        let p = AnthropicProvider::new("k", cfg).unwrap();
        let c = p.http_config().compression.as_ref().unwrap();
        assert_eq!(c.level, 6);
        assert_eq!(c.min_bytes, 4096);
        assert_eq!(c.paths, [MESSAGES_PATH, COUNT_TOKENS_PATH]);
        assert_eq!(c.latch, GzipLatch::process());
    }

    #[test]
    fn updates_display_adds_its_beta_once() {
        let mut cfg = AnthropicConfig::new().with_base_url("http://localhost:1");
        cfg.request.thinking_display = ThinkingDisplay::Updates;
        let p = AnthropicProvider::new("k", cfg.clone()).unwrap();
        assert_eq!(
            p.http_config().default_headers["anthropic-beta"],
            request::THINKING_UPDATES_BETA
        );
        cfg.betas = vec!["a-1".into(), request::THINKING_UPDATES_BETA.into()];
        let p = AnthropicProvider::new("k", cfg.clone()).unwrap();
        assert_eq!(
            p.http_config().default_headers["anthropic-beta"],
            format!("a-1,{}", request::THINKING_UPDATES_BETA)
        );
        cfg.betas.clear();
        cfg.request.thinking_display = ThinkingDisplay::Omitted;
        let p = AnthropicProvider::new("k", cfg).unwrap();
        assert!(
            p.http_config()
                .default_headers
                .get("anthropic-beta")
                .is_none()
        );
    }

    #[test]
    fn long_wait_upgrades_every_breakpoint_to_one_hour() {
        let hint = LongWait::new();
        let mut cfg = AnthropicConfig::new().with_base_url("http://localhost:1");
        cfg.request.cache_ttl = CacheTtl::Mixed;
        cfg.long_wait = Some(hint.clone());
        let p = AnthropicProvider::new("k", cfg.clone()).unwrap();
        assert_eq!(p.options().cache_ttl, CacheTtl::Mixed);
        hint.set_jobs_running(true);
        assert_eq!(p.options().cache_ttl, CacheTtl::OneHour);
        let mut req = ChatRequest::new("claude-opus-5-5");
        req.system = Some("s".into());
        req.messages.push(crate::model::Message::user("hi"));
        let body = p.build_body(&req).unwrap();
        assert_eq!(
            body["messages"][0]["content"][0]["cache_control"]["ttl"],
            "1h"
        );
        hint.set_jobs_running(false);
        let body = p.build_body(&req).unwrap();
        assert!(
            body["messages"][0]["content"][0]["cache_control"]
                .get("ttl")
                .is_none()
        );
        // Off by default: an active hint changes nothing.
        cfg.long_wait = None;
        hint.set_jobs_running(true);
        let p = AnthropicProvider::new("k", cfg).unwrap();
        assert_eq!(p.options().cache_ttl, CacheTtl::Mixed);
    }

    #[test]
    fn prewarm_usage_reads_the_cache_counters() {
        let u = prewarm_usage(
            &serde_json::json!({"content": [], "stop_reason": "max_tokens",
            "usage": {"input_tokens": 3, "output_tokens": 0,
                      "cache_creation_input_tokens": 5000, "cache_read_input_tokens": 0}}),
        );
        assert_eq!(
            (
                u.input_tokens,
                u.output_tokens,
                u.cache_write_tokens,
                u.cache_read_tokens
            ),
            (3, 0, 5000, 0)
        );
    }

    #[test]
    fn bad_header_characters_do_not_echo_the_key() {
        let err = AnthropicProvider::new("sk\nsecret", AnthropicConfig::new()).unwrap_err();
        assert!(!err.to_string().contains("secret"));
    }

    #[test]
    fn profile_uses_registry_and_falls_back_for_unknown_models() {
        let p = AnthropicProvider::new("k", AnthropicConfig::new()).unwrap();
        assert_eq!(
            p.profile("claude-haiku-4-5").thinking,
            ThinkingStyle::Budget
        );
        assert_eq!(
            p.profile("claude-opus-5-5").thinking,
            ThinkingStyle::Adaptive
        );
        let unknown = p.profile("claude-next");
        assert_eq!(unknown.thinking, ThinkingStyle::Adaptive);
        assert_eq!(
            unknown.capabilities,
            crate::registry::CONSERVATIVE_CAPABILITIES
        );
        assert!(p.capabilities("claude-opus-5-5").caching);
    }
}
