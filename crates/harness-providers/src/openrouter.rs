//! OpenRouter provider, using the OpenAI-compatible Chat Completions API
//! (`POST /chat/completions`) with streaming and OpenRouter's extensions.
//!
//! - Tools are offered as `function` tools; tool calls stream as tool-use
//!   blocks.
//! - Reasoning: [`ThinkingConfig`](crate::model::ThinkingConfig) maps to
//!   `reasoning.effort` or `reasoning.max_tokens`. `delta.reasoning` text
//!   streams live as reasoning deltas. `reasoning_details` (including
//!   signatures and encrypted blobs) are merged into one opaque block
//!   (provider `openrouter`) and sent back on the next turn. When reasoning
//!   was requested or billed but none was streamed, a
//!   [`Notice::ReasoningHidden`](crate::openai::notice::Notice) block flags
//!   its absence.
//! - Routing: `provider_order` and `allow_fallbacks` become the request's
//!   `provider` object.
//! - Prompt caching: for Anthropic models (`anthropic/...`), which cache
//!   only with explicit markers, the body carries up to four
//!   `cache_control` breakpoints placed like the Anthropic provider's
//!   (see [`request::uses_cache_breakpoints`]). Other models cache
//!   automatically and get no markers.
//! - Usage accounting is always requested (`usage.include`). The reported
//!   cost arrives as a [`Notice::Cost`](crate::openai::notice::Notice)
//!   block; read it with [`crate::openai::notice::reported_cost`].
//! - App attribution: `app_name` is sent as `X-Title` (and as
//!   `HTTP-Referer` when it is an `http(s)://` URL).
//! - [`OpenRouterProvider::check_agent_model`] consults the model catalog
//!   and rejects models without tool support at agent load. With
//!   [`OpenRouterProvider::with_model_cache`] it reads the on-disk model
//!   list first, so a session start with a fresh cached list makes no
//!   catalog request (see the method for the rules).

pub mod models;
pub mod request;
mod stream;

use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue, REFERER};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::catalog::live::ListKind;
use crate::catalog::{LiveList, ModelListCache};
use crate::http::{HttpClient, HttpConfig, error_message};
use crate::model::{Capabilities, ChatRequest};
use crate::openai::transport;
use crate::provider::{EventStream, Provider, ProviderError};

pub use crate::openai::notice;
pub use models::ModelInfo;

/// Provider name, as used in `harness.ini` (`[provider.openrouter]`) and
/// on opaque blocks this provider produces.
pub const PROVIDER_NAME: &str = "openrouter";

/// Default API base URL.
pub const DEFAULT_BASE_URL: &str = "https://openrouter.ai/api/v1";

/// Streaming request path.
pub const CHAT_PATH: &str = "chat/completions";

/// Model catalog path.
pub const MODELS_PATH: &str = "models";

/// Header carrying the app name for OpenRouter attribution.
pub const TITLE_HEADER: &str = "x-title";

/// OpenRouter-specific settings from `[provider.openrouter]`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OpenRouterOptions {
    /// `app_name`: sent as attribution headers.
    pub app_name: Option<String>,
    /// `provider_order`: upstream providers to try, in order.
    pub provider_order: Vec<String>,
    /// `allow_fallbacks`: whether OpenRouter may use providers outside
    /// `provider_order`. `None` leaves OpenRouter's default.
    pub allow_fallbacks: Option<bool>,
    /// `cache_ttl`: lifetime of the prompt-cache entries written for
    /// Anthropic models. `None` means the 5-minute default.
    pub cache_ttl: Option<CacheTtl>,
    /// `session_id` sent with every request (`[experimental]
    /// openrouter_session_id`): OpenRouter routes requests with the same
    /// id to the same upstream (sticky routing) from the first request on.
    /// `None` (the default) sends none.
    pub session_id: Option<String>,
    /// `reasoning_exclude`: ask OpenRouter to leave reasoning out of the
    /// response (`reasoning.exclude: true`). The model still reasons (and
    /// is billed for it); only the reasoning text and details are not
    /// returned. Default false.
    pub reasoning_exclude: bool,
    /// `service_tier`: sent as the top-level `service_tier` field.
    /// `None` (the default) sends none, so OpenRouter routes to standard
    /// endpoints (or, for a `:nitro` model, to the fastest endpoint,
    /// priority tiers included).
    pub service_tier: Option<ServiceTier>,
}

/// OpenRouter service tier (`service_tier` in the request body).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceTier {
    /// `"default"`: standard endpoints only.
    Default,
    /// `"fast"`: the priority tier (Anthropic fast mode on Claude Opus) at
    /// its higher price where the model has one; standard endpoints at the
    /// standard price otherwise.
    Fast,
}

impl ServiceTier {
    /// The request-body value.
    pub fn as_str(self) -> &'static str {
        match self {
            ServiceTier::Default => "default",
            ServiceTier::Fast => "fast",
        }
    }
}

/// Model-id suffixes that only change how OpenRouter routes the request,
/// not the model: `:nitro` sorts the endpoints by throughput (admitting
/// priority tiers), `:floor` by price (admitting flex tiers).
pub const ROUTING_VARIANTS: [&str; 2] = [":nitro", ":floor"];

/// The base model id of a routing variant (`anthropic/claude-opus-5.5`
/// for `anthropic/claude-opus-5.5:nitro`), or `None` for any other id.
pub fn routing_base(model: &str) -> Option<&str> {
    ROUTING_VARIANTS
        .iter()
        .find_map(|v| model.strip_suffix(v))
        .filter(|base| !base.is_empty())
}

/// Prompt-cache lifetime requested on `cache_control` breakpoints.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum CacheTtl {
    /// Five minutes (the default; no `ttl` field is sent).
    #[default]
    FiveMinutes,
    /// One hour (`"ttl": "1h"`; writes cost more than 5-minute writes).
    OneHour,
}

/// Attribution headers for `app_name`: `X-Title` always, `HTTP-Referer`
/// when the name is an `http(s)://` URL. Names that are not valid header
/// values are skipped with a warning.
pub fn attribution_headers(app_name: Option<&str>) -> HeaderMap {
    let mut headers = HeaderMap::new();
    let Some(name) = app_name.map(str::trim).filter(|n| !n.is_empty()) else {
        return headers;
    };
    match HeaderValue::from_str(name) {
        Ok(value) => {
            if name.starts_with("http://") || name.starts_with("https://") {
                headers.insert(REFERER, value.clone());
            }
            headers.insert(HeaderName::from_static(TITLE_HEADER), value);
        }
        Err(_) => tracing::warn!("openrouter app_name is not a valid header value; not sent"),
    }
    headers
}

/// Capabilities assumed for a model missing from the catalog.
pub const UNKNOWN_MODEL_CAPABILITIES: Capabilities = Capabilities {
    tools: true,
    parallel_tool_calls: true,
    thinking: false,
    caching: false,
    images: false,
    context_window: 128_000,
    max_output_tokens: 0,
};

/// Where the loaded model catalog came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Source {
    /// `GET /models` in this process (or [`OpenRouterProvider::set_models`]).
    Live,
    /// The on-disk model list; `fresh` while younger than the TTL.
    Cached { fresh: bool },
}

/// The loaded model catalog.
#[derive(Debug)]
struct Loaded {
    models: HashMap<String, ModelInfo>,
    source: Source,
}

/// The on-disk model-list cache and its TTL.
#[derive(Debug, Clone)]
struct DiskCache {
    cache: ModelListCache,
    ttl: Duration,
}

/// The OpenRouter provider.
#[derive(Debug, Clone)]
pub struct OpenRouterProvider {
    client: HttpClient,
    options: OpenRouterOptions,
    catalog: Arc<RwLock<Option<Loaded>>>,
    disk: Option<DiskCache>,
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

impl OpenRouterProvider {
    /// A provider using `api_key`, the transport settings in `http` (whose
    /// `base_url` is usually [`DEFAULT_BASE_URL`]) and `options`. The key is
    /// a sensitive `Authorization: Bearer` header; attribution headers come
    /// from `options.app_name`.
    pub fn new(
        api_key: &str,
        mut http: HttpConfig,
        options: OpenRouterOptions,
    ) -> Result<Self, ProviderError> {
        http.default_headers
            .extend(transport::bearer_headers(api_key)?);
        http.default_headers
            .extend(attribution_headers(options.app_name.as_deref()));
        Ok(Self {
            client: HttpClient::new(http)?,
            options,
            catalog: Arc::default(),
            disk: None,
        })
    }

    /// Uses `cache` (usually `<data_dir>/model-lists`, shared with the
    /// `/model` catalog) for [`OpenRouterProvider::check_agent_model`]: a
    /// cached list is used while younger than `ttl` (`[general]
    /// model_list_ttl`), and every catalog download is written back to it.
    pub fn with_model_cache(mut self, cache: ModelListCache, ttl: Duration) -> Self {
        self.disk = Some(DiskCache { cache, ttl });
        self
    }

    /// A provider with default transport settings. `base_url` defaults to
    /// [`DEFAULT_BASE_URL`].
    pub fn with_key(
        api_key: &str,
        base_url: Option<&str>,
        options: OpenRouterOptions,
    ) -> Result<Self, ProviderError> {
        Self::new(
            api_key,
            HttpConfig::new(base_url.unwrap_or(DEFAULT_BASE_URL)),
            options,
        )
    }

    /// The configured options.
    pub fn options(&self) -> &OpenRouterOptions {
        &self.options
    }

    /// The JSON body that [`Provider::stream`] would send for `request`.
    pub fn request_body(&self, request: &ChatRequest) -> Value {
        request::build_body(request, &self.options)
    }

    /// Replaces the cached model catalog (for tests or offline use).
    pub fn set_models(&self, models: impl IntoIterator<Item = ModelInfo>) {
        self.install(models, Source::Live);
    }

    fn install(&self, models: impl IntoIterator<Item = ModelInfo>, source: Source) {
        let models = models.into_iter().map(|m| (m.id.clone(), m)).collect();
        *self.catalog.write().unwrap_or_else(|e| e.into_inner()) = Some(Loaded { models, source });
    }

    fn source(&self) -> Option<Source> {
        self.catalog
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .map(|l| l.source)
    }

    /// Catalog entry for `model`, if the catalog is loaded and lists it.
    /// A routing variant the catalog does not list itself (`:nitro`,
    /// `:floor`, see [`routing_base`]) resolves to its base model's entry.
    pub fn model_info(&self, model: &str) -> Option<ModelInfo> {
        let guard = self.catalog.read().unwrap_or_else(|e| e.into_inner());
        let models = &guard.as_ref()?.models;
        models
            .get(model)
            .or_else(|| routing_base(model).and_then(|base| models.get(base)))
            .cloned()
    }

    /// Loads the on-disk list into the catalog when one exists for this
    /// base URL. Returns whether it did.
    fn load_from_disk(&self) -> bool {
        let Some(disk) = &self.disk else {
            return false;
        };
        let base = self.client.config().base_url.trim_end_matches('/');
        let Some(list) = disk
            .cache
            .load(PROVIDER_NAME)
            .filter(|l| l.base_url.trim_end_matches('/') == base)
        else {
            return false;
        };
        let fresh = list.is_fresh(now_secs(), disk.ttl, &list.base_url);
        self.install(
            list.models.iter().map(ModelInfo::from_live),
            Source::Cached { fresh },
        );
        true
    }

    /// Writes a downloaded catalog body to the on-disk cache, if any.
    fn store_to_disk(&self, body: &Value) {
        let Some(disk) = &self.disk else {
            return;
        };
        let models = match crate::catalog::live::parse(ListKind::OpenRouter, body) {
            Ok(page) => page.models,
            Err(e) => {
                tracing::debug!(error = %e, "openrouter model list not cached");
                return;
            }
        };
        let base = self.client.config().base_url.clone();
        let list = LiveList::new(PROVIDER_NAME, base, now_secs(), models);
        if let Err(e) = disk.cache.store(&list) {
            tracing::debug!(error = %e, "writing the openrouter model list cache failed");
        }
    }

    /// Refreshes the catalog in the background (when a tokio runtime is
    /// running); the provider and its clones see the new list when it lands.
    fn spawn_refresh(&self) {
        let Ok(rt) = tokio::runtime::Handle::try_current() else {
            return;
        };
        let me = self.clone();
        rt.spawn(async move {
            if let Err(e) = me.refresh_models().await {
                tracing::debug!(error = %e, "background openrouter catalog refresh failed");
            }
        });
    }

    /// Fetches `GET /models` and caches it (in memory, and on disk with
    /// [`OpenRouterProvider::with_model_cache`]). Returns the number of
    /// models.
    pub async fn refresh_models(&self) -> Result<usize, ProviderError> {
        let (status, text) = self.client.get_text(MODELS_PATH).await?;
        if !(200..300).contains(&status) {
            return Err(ProviderError::from_status(
                status,
                error_message(&text),
                None,
            ));
        }
        let body: Value = serde_json::from_str(&text)
            .map_err(|e| ProviderError::Decode(format!("OpenRouter /models: {e}")))?;
        let models = models::parse_catalog(&body)?;
        let n = models.len();
        self.set_models(models);
        self.store_to_disk(&body);
        Ok(n)
    }

    /// Validates `model` when an agent is loaded: fails for unknown models,
    /// and fails with a clear message when `needs_tools` is set but the
    /// model cannot call tools.
    ///
    /// The catalog is loaded once per provider. With
    /// [`OpenRouterProvider::with_model_cache`], the on-disk list is tried
    /// first: when it lists `model` as usable, the check passes without a
    /// request (a stale list is then refreshed in the background). When
    /// the list lacks the model or says it cannot call tools, the live
    /// catalog decides; if that download fails, the cached answer stands.
    /// Without a usable cached list the catalog is downloaded first.
    pub async fn check_agent_model(
        &self,
        model: &str,
        needs_tools: bool,
    ) -> Result<Capabilities, ProviderError> {
        if self.source().is_none() && !self.load_from_disk() {
            self.refresh_models().await?;
        }
        if let Some(Source::Cached { fresh }) = self.source() {
            let usable = self
                .model_info(model)
                .filter(|info| models::check_tools(info, needs_tools).is_ok());
            if let Some(info) = usable {
                if !fresh {
                    self.spawn_refresh();
                }
                return Ok(info.capabilities());
            }
            if let Err(e) = self.refresh_models().await
                && self.model_info(model).is_none()
            {
                return Err(e);
            }
        }
        let info = self.model_info(model).ok_or_else(|| {
            ProviderError::InvalidRequest(format!(
                "unknown OpenRouter model `{model}`; check the id on openrouter.ai/models"
            ))
        })?;
        models::check_tools(&info, needs_tools)?;
        Ok(info.capabilities())
    }
}

#[async_trait]
impl Provider for OpenRouterProvider {
    fn name(&self) -> &str {
        PROVIDER_NAME
    }

    fn request_body(&self, request: &ChatRequest) -> Option<Value> {
        Some(OpenRouterProvider::request_body(self, request))
    }

    async fn prewarm(&self) {
        // `GET /key` is authenticated, small and free.
        if let Err(e) = self.client.warm("key").await {
            tracing::debug!(error = %e, "openrouter connection pre-warm failed");
        }
    }

    fn capabilities(&self, model: &str) -> Capabilities {
        self.model_info(model)
            .map_or(UNKNOWN_MODEL_CAPABILITIES, |m| m.capabilities())
    }

    async fn stream(
        &self,
        request: ChatRequest,
        cancel: CancellationToken,
    ) -> Result<EventStream, ProviderError> {
        if let Some(info) = self.model_info(&request.model) {
            models::check_tools(&info, !request.tools.is_empty())?;
        }
        let body = self.request_body(&request);
        let reasoning_requested = request.thinking.is_some();
        Ok(transport::open_stream(
            self.client.clone(),
            CHAT_PATH,
            HeaderMap::new(),
            body,
            cancel,
            move || stream::ChatMapper::new(reasoning_requested),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::notice::{HiddenReason, reasoning_hidden, reported_cost};
    use super::*;
    use crate::model::{
        ContentBlock, Message, ReasoningEffort, Role, StopKind, ThinkingConfig, ToolSpec,
    };
    use crate::openai::mock::{Canned, MockServer, sse_body};
    use crate::provider::StreamEvent;
    use crate::retry::RetryPolicy;
    use futures::StreamExt;
    use serde_json::json;
    use std::time::Duration;

    fn options() -> OpenRouterOptions {
        OpenRouterOptions {
            app_name: Some("harness".into()),
            provider_order: vec!["anthropic".into(), "openai".into()],
            allow_fallbacks: Some(true),
            cache_ttl: None,
            session_id: None,
            reasoning_exclude: false,
            service_tier: None,
        }
    }

    fn provider(base: &str) -> OpenRouterProvider {
        let mut http = HttpConfig::new(base);
        http.retry = RetryPolicy {
            base_delay: Duration::from_millis(1),
            max_delay: Duration::from_millis(5),
            ..RetryPolicy::default()
        };
        OpenRouterProvider::new("or-key", http, options()).unwrap()
    }

    fn request() -> ChatRequest {
        let mut r = ChatRequest::new("anthropic/claude-sonnet-4");
        r.messages = vec![Message::user("go")];
        r.tools = vec![ToolSpec {
            name: "read".into(),
            description: "read".into(),
            input_schema: json!({"type": "object"}),
        }];
        r.thinking = Some(ThinkingConfig {
            effort: Some(ReasoningEffort::High),
            budget_tokens: None,
        });
        r
    }

    fn tool_turn() -> String {
        let chunk = |delta: serde_json::Value, finish: serde_json::Value| {
            json!({"id": "gen-1", "model": "anthropic/claude-sonnet-4",
                   "choices": [{"index": 0, "delta": delta, "finish_reason": finish}]})
        };
        let mut body = sse_body(&[
            (
                "",
                chunk(
                    json!({"role": "assistant", "reasoning": "Let me ",
                "reasoning_details": [{"type": "reasoning.text", "text": "Let me ", "index": 0, "format": "anthropic-claude-v1"}]}),
                    json!(null),
                ),
            ),
            (
                "",
                chunk(
                    json!({"reasoning": "read.",
                "reasoning_details": [{"type": "reasoning.text", "text": "read.", "signature": "SIG", "index": 0}]}),
                    json!(null),
                ),
            ),
            ("", chunk(json!({"content": "Reading."}), json!(null))),
            (
                "",
                chunk(
                    json!({"tool_calls": [{"index": 0, "id": "toolu_1", "type": "function", "function": {"name": "read", "arguments": ""}}]}),
                    json!(null),
                ),
            ),
            (
                "",
                chunk(
                    json!({"tool_calls": [{"index": 0, "function": {"arguments": "{\"path\":"}}]}),
                    json!(null),
                ),
            ),
            (
                "",
                chunk(
                    json!({"tool_calls": [{"index": 0, "function": {"arguments": "\"a.rs\"}"}}]}),
                    json!(null),
                ),
            ),
            ("", chunk(json!({}), json!("tool_calls"))),
            (
                "",
                json!({"id": "gen-1", "choices": [], "usage": {"prompt_tokens": 300, "completion_tokens": 50,
                "prompt_tokens_details": {"cached_tokens": 100}, "completion_tokens_details": {"reasoning_tokens": 20},
                "cost": 0.0042}}),
            ),
        ]);
        body.insert_str(0, ": OPENROUTER PROCESSING\n\n");
        body.push_str("data: [DONE]\n\n");
        body
    }

    #[tokio::test]
    async fn streams_tools_reasoning_cost_and_sends_routing_and_headers() {
        let server = MockServer::start(vec![Canned::sse(tool_turn())]).await;
        let p = provider(&server.base_url);
        let events: Vec<StreamEvent> = p
            .stream(request(), CancellationToken::new())
            .await
            .unwrap()
            .map(Result::unwrap)
            .collect()
            .await;
        assert_eq!(
            events[1],
            StreamEvent::ReasoningDelta {
                index: 0,
                text: "Let me ".into()
            }
        );
        let mut b = crate::provider::ResponseBuilder::new();
        events.into_iter().for_each(|e| b.apply(e).unwrap());
        let resp = b.finish().unwrap();
        assert_eq!(resp.stop.kind, StopKind::ToolUse);
        assert_eq!(resp.usage.input_tokens, 200);
        assert_eq!(resp.usage.cache_read_tokens, 100);
        assert_eq!(resp.usage.reasoning_tokens, 20);
        assert_eq!(reported_cost(&resp), Some(0.0042));
        assert_eq!(reasoning_hidden(&resp), None);
        assert_eq!(
            resp.message.tool_uses().collect::<Vec<_>>(),
            vec![("toolu_1", "read", &json!({"path": "a.rs"}))]
        );
        assert_eq!(
            resp.message.content[0],
            ContentBlock::Reasoning {
                text: "Let me read.".into(),
                signature: None
            }
        );
        assert_eq!(resp.message.text(), "Reading.");

        let req = &server.requests()[0];
        assert_eq!(req.path, "/v1/chat/completions");
        assert_eq!(req.header("authorization"), Some("Bearer or-key"));
        assert_eq!(req.header("x-title"), Some("harness"));
        let body = req.json();
        assert_eq!(
            body["provider"],
            json!({"order": ["anthropic", "openai"], "allow_fallbacks": true})
        );
        assert_eq!(body["usage"], json!({"include": true}));
        assert_eq!(body["reasoning"], json!({"effort": "high"}));
        assert_eq!(body["stream"], json!(true));
    }

    #[tokio::test]
    async fn reasoning_details_round_trip_on_the_next_turn() {
        let done = sse_body(&[
            (
                "",
                json!({"id": "gen-2", "choices": [{"delta": {"content": "ok"}, "finish_reason": "stop"}]}),
            ),
            (
                "",
                json!({"id": "gen-2", "choices": [], "usage": {"prompt_tokens": 10, "completion_tokens": 1}}),
            ),
        ]) + "data: [DONE]\n\n";
        let server = MockServer::start(vec![Canned::sse(tool_turn()), Canned::sse(done)]).await;
        let p = provider(&server.base_url);
        let mut req = request();
        let first = p
            .complete(req.clone(), CancellationToken::new())
            .await
            .unwrap();
        req.messages.push(first.message);
        req.messages.push(Message {
            role: Role::User,
            content: vec![ContentBlock::tool_result("toolu_1", "fn main(){}", false)],
        });
        let second = p.complete(req, CancellationToken::new()).await.unwrap();
        assert_eq!(second.message.text(), "ok");
        // Reasoning was requested but the model sent none: flagged.
        assert_eq!(reasoning_hidden(&second), Some(HiddenReason::NotProvided));
        assert_eq!(reported_cost(&second), None);

        let body = server.requests()[1].json();
        let msgs = body["messages"].as_array().unwrap();
        assert_eq!(msgs[1]["role"], json!("assistant"));
        assert_eq!(msgs[1]["content"], json!("Reading."));
        assert_eq!(
            msgs[1]["reasoning_details"],
            json!([{"type": "reasoning.text", "text": "Let me read.", "index": 0,
                    "format": "anthropic-claude-v1", "signature": "SIG"}])
        );
        assert_eq!(msgs[1]["tool_calls"][0]["id"], json!("toolu_1"));
        // An Anthropic model: tool output is a content part, and as the
        // final message it carries a cache breakpoint.
        assert_eq!(
            msgs[2],
            json!({"role": "tool", "tool_call_id": "toolu_1", "content": [
                {"type": "text", "text": "fn main(){}", "cache_control": {"type": "ephemeral"}}]})
        );
    }

    fn text_turn(text: &str, cached: u64, written: u64) -> String {
        sse_body(&[
            (
                "",
                json!({"id": "gen-t", "choices": [{"delta": {"content": text}, "finish_reason": "stop"}]}),
            ),
            (
                "",
                json!({"id": "gen-t", "choices": [], "usage": {"prompt_tokens": 1000 + cached + written,
                    "completion_tokens": 1, "cost": 0.001,
                    "prompt_tokens_details": {"cached_tokens": cached, "cache_write_tokens": written}}}),
            ),
        ]) + "data: [DONE]\n\n"
    }

    /// `v` without `cache_control` keys.
    fn unmarked(v: &serde_json::Value) -> serde_json::Value {
        let mut v = v.clone();
        if let Some(parts) = v["content"].as_array_mut() {
            for p in parts {
                if let Some(o) = p.as_object_mut() {
                    o.remove("cache_control");
                }
            }
        }
        v
    }

    /// Runs a three-call session (tool call, tool result, follow-up) and
    /// returns the request bodies.
    async fn session_bodies(layers: bool) -> Vec<serde_json::Value> {
        let server = MockServer::start(vec![
            Canned::sse(tool_turn()),
            Canned::sse(text_turn("done", 0, 900)),
            Canned::sse(text_turn("ok", 900, 50)),
        ])
        .await;
        let p = provider(&server.base_url);
        let mut req = request();
        req.system = Some("You are terse.\n\n## Memory\n- prefer nextest".into());
        if layers {
            req.system_layers = vec!["You are terse.\n\n".len()];
        }
        let first = p
            .complete(req.clone(), CancellationToken::new())
            .await
            .unwrap();
        req.messages.push(first.message);
        req.messages.push(Message {
            role: Role::User,
            content: vec![ContentBlock::tool_result("toolu_1", "fn main(){}", false)],
        });
        let second = p
            .complete(req.clone(), CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(second.usage.cache_write_tokens, 900);
        req.messages.push(second.message);
        req.messages.push(Message::user("next"));
        let third = p.complete(req, CancellationToken::new()).await.unwrap();
        assert_eq!(
            (
                third.usage.cache_read_tokens,
                third.usage.cache_write_tokens,
                third.usage.input_tokens
            ),
            (900, 50, 1000)
        );
        server.requests().iter().map(|r| r.json()).collect()
    }

    #[tokio::test]
    async fn later_requests_reuse_the_cached_prefix() {
        for layers in [false, true] {
            let bodies = session_bodies(layers).await;
            for pair in bodies.windows(2) {
                let (a, b) = (&pair[0], &pair[1]);
                // Tools and system (with their breakpoints) are byte-identical.
                assert_eq!(a["tools"].to_string(), b["tools"].to_string());
                assert_eq!(a["messages"][0].to_string(), b["messages"][0].to_string());
                assert!(a["tools"][0].get("cache_control").is_some());
                // Every earlier message is byte-identical apart from moved
                // markers, so the next call's prefix matches what this call
                // wrote up to its last breakpoint.
                let (am, bm) = (
                    a["messages"].as_array().unwrap(),
                    b["messages"].as_array().unwrap(),
                );
                assert!(bm.len() > am.len());
                for (x, y) in am.iter().zip(bm) {
                    assert_eq!(unmarked(x).to_string(), unmarked(y).to_string());
                }
                let last = am.last().unwrap();
                assert!(last.to_string().contains("cache_control"), "{last}");
                if !layers {
                    // With room for two message breakpoints, the previous
                    // final message keeps its marker, byte for byte.
                    assert_eq!(last.to_string(), bm[am.len() - 1].to_string());
                }
            }
            for b in &bodies {
                let n = b.to_string().matches("cache_control").count();
                assert!(n <= request::MAX_CACHE_BREAKPOINTS, "{n} breakpoints");
            }
        }
    }

    #[tokio::test]
    async fn catalog_rejects_models_without_tools() {
        let catalog = json!({"data": [
            {"id": "a/tools", "context_length": 100000, "supported_parameters": ["tools"]},
            {"id": "b/no-tools", "context_length": 8000, "supported_parameters": ["temperature"]}
        ]});
        let server = MockServer::start(vec![Canned::json(200, catalog.to_string())]).await;
        let p = provider(&server.base_url);
        let caps = p.check_agent_model("a/tools", true).await.unwrap();
        assert!(caps.tools);
        assert_eq!(caps.context_window, 100_000);
        let err = p.check_agent_model("b/no-tools", true).await.unwrap_err();
        assert!(matches!(err, ProviderError::InvalidRequest(_)));
        assert!(err.to_string().contains("does not support tool calling"));
        assert!(p.check_agent_model("b/no-tools", false).await.is_ok());
        assert!(p.check_agent_model("c/unknown", false).await.is_err());
        // The catalog is fetched once and cached.
        let reqs = server.requests();
        assert_eq!(reqs.len(), 1);
        assert_eq!(
            (reqs[0].method.as_str(), reqs[0].path.as_str()),
            ("GET", "/v1/models")
        );
        assert_eq!(reqs[0].header("authorization"), Some("Bearer or-key"));
        // stream() also refuses to offer tools to such a model, without a call.
        let mut r = request();
        r.model = "b/no-tools".into();
        let err = p.stream(r, CancellationToken::new()).await.err().unwrap();
        assert!(err.to_string().contains("does not support tool calling"));
        assert!(!p.capabilities("b/no-tools").tools);
        assert_eq!(p.capabilities("zzz"), UNKNOWN_MODEL_CAPABILITIES);
    }

    #[tokio::test]
    async fn routing_variants_resolve_to_their_base_model() {
        let catalog = json!({"data": [
            {"id": "a/tools", "context_length": 100000, "supported_parameters": ["tools"]},
            {"id": "a/tools:batch", "context_length": 5000, "supported_parameters": ["tools"]},
            {"id": "b/no-tools", "context_length": 8000, "supported_parameters": ["temperature"]}
        ]});
        let server = MockServer::start(vec![Canned::json(200, catalog.to_string())]).await;
        let p = provider(&server.base_url);
        for id in ["a/tools:nitro", "a/tools:floor"] {
            let caps = p.check_agent_model(id, true).await.unwrap();
            assert_eq!(caps.context_window, 100_000, "{id}");
        }
        // The base model's limits still apply to the variant.
        let err = p
            .check_agent_model("b/no-tools:nitro", true)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("does not support tool calling"));
        // A variant the catalog lists itself keeps its own entry; other
        // suffixes are not stripped.
        assert_eq!(p.capabilities("a/tools:batch").context_window, 5000);
        assert!(p.check_agent_model("a/tools:online", true).await.is_err());
        assert!(p.check_agent_model(":nitro", true).await.is_err());
        assert_eq!(routing_base("x/y:nitro"), Some("x/y"));
        assert_eq!(routing_base("x/y"), None);
    }

    fn catalog_body() -> serde_json::Value {
        json!({"data": [
            {"id": "a/tools", "context_length": 100000, "supported_parameters": ["tools"]},
            {"id": "b/no-tools", "context_length": 8000, "supported_parameters": ["temperature"]}
        ]})
    }

    /// A cache in `dir` holding `catalog_body()` as fetched `age` seconds
    /// ago from `base`.
    fn seeded_cache(dir: &std::path::Path, base: &str, age: u64) -> ModelListCache {
        let cache = ModelListCache::new(dir);
        let page = crate::catalog::live::parse(ListKind::OpenRouter, &catalog_body()).unwrap();
        let list = LiveList::new(PROVIDER_NAME, base, now_secs() - age, page.models);
        cache.store(&list).unwrap();
        cache
    }

    const TTL: Duration = Duration::from_secs(3600);

    #[test]
    fn cached_entries_match_the_live_catalog() {
        let page = crate::catalog::live::parse(ListKind::OpenRouter, &catalog_body()).unwrap();
        let live = models::parse_catalog(&catalog_body()).unwrap();
        let cached: Vec<ModelInfo> = page.models.iter().map(ModelInfo::from_live).collect();
        assert_eq!(cached, live);
    }

    #[tokio::test]
    async fn fresh_cached_catalog_skips_the_download_and_still_rejects_tool_less_models() {
        // The server is never reached for a usable model.
        let server = MockServer::start(vec![Canned::json(200, catalog_body().to_string())]).await;
        let tmp = tempfile::tempdir().unwrap();
        let cache = seeded_cache(tmp.path(), &server.base_url, 10);
        let p = provider(&server.base_url).with_model_cache(cache.clone(), TTL);
        let caps = p.check_agent_model("a/tools", true).await.unwrap();
        assert!(caps.tools);
        assert_eq!(caps.context_window, 100_000);
        assert!(p.check_agent_model("b/no-tools", false).await.is_ok());
        assert!(server.requests().is_empty(), "no catalog request");
        // A cached "no tools" is confirmed by the live catalog: rejected.
        let err = p.check_agent_model("b/no-tools", true).await.unwrap_err();
        assert!(err.to_string().contains("does not support tool calling"));
        assert_eq!(server.requests().len(), 1);
        // stream() refuses tools for it too, from the loaded catalog.
        let mut r = request();
        r.model = "b/no-tools".into();
        let err = p.stream(r, CancellationToken::new()).await.err().unwrap();
        assert!(err.to_string().contains("does not support tool calling"));
    }

    #[tokio::test]
    async fn cached_rejection_stands_offline_and_unknown_models_are_fetched() {
        // Nothing listens here: every download fails.
        let dead = {
            let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            format!("http://{}/v1", l.local_addr().unwrap())
        };
        let tmp = tempfile::tempdir().unwrap();
        let cache = seeded_cache(tmp.path(), &dead, 10);
        let p = provider(&dead).with_model_cache(cache, TTL);
        let err = p.check_agent_model("b/no-tools", true).await.unwrap_err();
        assert!(
            err.to_string().contains("does not support tool calling"),
            "{err}"
        );
        let err = p.check_agent_model("c/unknown", false).await.unwrap_err();
        assert!(matches!(err, ProviderError::Connection(_)), "{err:?}");

        // A model the cached list lacks is looked up live.
        let mut newer = catalog_body();
        newer["data"]
            .as_array_mut()
            .unwrap()
            .push(json!({"id": "c/new", "supported_parameters": ["tools"]}));
        let server = MockServer::start(vec![Canned::json(200, newer.to_string())]).await;
        let tmp = tempfile::tempdir().unwrap();
        let cache = seeded_cache(tmp.path(), &server.base_url, 10);
        let p = provider(&server.base_url).with_model_cache(cache, TTL);
        assert!(p.check_agent_model("c/new", true).await.unwrap().tools);
        assert_eq!(server.requests().len(), 1);
    }

    #[tokio::test]
    async fn stale_or_foreign_cache_is_refreshed_and_downloads_are_cached() {
        let server = MockServer::start(vec![
            Canned::json(200, catalog_body().to_string()),
            Canned::json(200, catalog_body().to_string()),
        ])
        .await;
        let tmp = tempfile::tempdir().unwrap();
        // Stale: used at once, refreshed in the background.
        let cache = seeded_cache(tmp.path(), &server.base_url, 7200);
        let before = cache.load(PROVIDER_NAME).unwrap().fetched_at;
        let p = provider(&server.base_url).with_model_cache(cache.clone(), TTL);
        assert!(p.check_agent_model("a/tools", true).await.is_ok());
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while cache.load(PROVIDER_NAME).unwrap().fetched_at == before {
            assert!(
                std::time::Instant::now() < deadline,
                "no background refresh"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(server.requests().len(), 1);
        assert!(
            cache
                .load(PROVIDER_NAME)
                .unwrap()
                .is_fresh(now_secs(), TTL, &server.base_url)
        );

        // A list from another base URL is ignored: downloaded and replaced.
        let tmp = tempfile::tempdir().unwrap();
        let cache = seeded_cache(tmp.path(), "http://elsewhere/v1", 10);
        let p = provider(&server.base_url).with_model_cache(cache.clone(), TTL);
        assert!(p.check_agent_model("a/tools", true).await.is_ok());
        assert_eq!(server.requests().len(), 2);
        assert_eq!(cache.load(PROVIDER_NAME).unwrap().base_url, server.base_url);
        // The next provider (the next session) needs no download.
        let p = provider(&server.base_url).with_model_cache(cache, TTL);
        assert!(p.check_agent_model("a/tools", true).await.is_ok());
        assert_eq!(server.requests().len(), 2);
    }

    #[tokio::test]
    async fn retries_upstream_errors() {
        let failing = sse_body(&[(
            "",
            json!({"id": "g", "error": {"code": 502, "message": "upstream down"},
                   "choices": [{"delta": {"content": ""}, "finish_reason": "error"}]}),
        )]);
        let ok = sse_body(&[(
            "",
            json!({"id": "g", "choices": [{"delta": {"content": "hi"}, "finish_reason": "stop"}]}),
        )]) + "data: [DONE]\n\n";
        let server = MockServer::start(vec![Canned::sse(failing), Canned::sse(ok)]).await;
        let p = provider(&server.base_url);
        let resp = p
            .complete(ChatRequest::new("x/y"), CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(resp.message.text(), "hi");
        assert_eq!(resp.retries.len(), 1);
        assert_eq!(reasoning_hidden(&resp), None);
    }

    #[test]
    fn attribution_header_rules() {
        let h = attribution_headers(Some("harness"));
        assert_eq!(h[TITLE_HEADER], "harness");
        assert!(h.get(REFERER).is_none());
        let h = attribution_headers(Some("https://example.org/app"));
        assert_eq!(h[REFERER], "https://example.org/app");
        assert!(attribution_headers(None).is_empty());
        assert!(attribution_headers(Some("  ")).is_empty());
        assert!(attribution_headers(Some("bad\nname")).is_empty());
    }
}
