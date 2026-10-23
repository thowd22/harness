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
//!   and rejects models without tool support at agent load.

pub mod models;
pub mod request;
mod stream;

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use async_trait::async_trait;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue, REFERER};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

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

/// The OpenRouter provider.
#[derive(Debug, Clone)]
pub struct OpenRouterProvider {
    client: HttpClient,
    catalog_client: reqwest::Client,
    options: OpenRouterOptions,
    catalog: Arc<RwLock<Option<HashMap<String, ModelInfo>>>>,
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
        let catalog_client = reqwest::Client::builder()
            .connect_timeout(http.connect_timeout)
            .timeout(http.request_timeout)
            .default_headers(http.default_headers.clone())
            .user_agent(concat!("harness/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|e| ProviderError::Other(format!("building HTTP client: {e}")))?;
        Ok(Self {
            client: HttpClient::new(http)?,
            catalog_client,
            options,
            catalog: Arc::default(),
        })
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
        let map = models.into_iter().map(|m| (m.id.clone(), m)).collect();
        *self.catalog.write().unwrap_or_else(|e| e.into_inner()) = Some(map);
    }

    /// Catalog entry for `model`, if the catalog is loaded and lists it.
    pub fn model_info(&self, model: &str) -> Option<ModelInfo> {
        self.catalog
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()?
            .get(model)
            .cloned()
    }

    /// Fetches `GET /models` and caches it. Returns the number of models.
    pub async fn refresh_models(&self) -> Result<usize, ProviderError> {
        let url = self.client.url(MODELS_PATH);
        let response = self
            .catalog_client
            .get(url)
            .send()
            .await
            .map_err(|e| ProviderError::Connection(e.to_string()))?;
        let status = response.status().as_u16();
        let text = response
            .text()
            .await
            .map_err(|e| ProviderError::Connection(e.to_string()))?;
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
        Ok(n)
    }

    /// Validates `model` when an agent is loaded: fetches the catalog if it
    /// is not cached yet, fails for unknown models, and fails with a clear
    /// message when `needs_tools` is set but the model cannot call tools.
    pub async fn check_agent_model(
        &self,
        model: &str,
        needs_tools: bool,
    ) -> Result<Capabilities, ProviderError> {
        let loaded = self
            .catalog
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .is_some();
        if !loaded {
            self.refresh_models().await?;
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
