//! OpenAI provider, using the Responses API (`POST /responses`) with
//! streaming.
//!
//! - Calls are stateless (`store: false`); the full conversation is sent on
//!   every turn.
//! - For reasoning models (o-series, `gpt-5*`, `codex-*`) the request asks
//!   for reasoning summaries (`reasoning.summary = "auto"`) and for
//!   `reasoning.encrypted_content`. Summaries stream live as
//!   [`StreamEvent::ReasoningDelta`](crate::provider::StreamEvent); the
//!   encrypted reasoning item arrives as an opaque block (provider `openai`)
//!   that is sent back unchanged on the next turn, so reasoning survives
//!   tool loops. When a model returns only encrypted reasoning, a
//!   [`notice::Notice::ReasoningHidden`] block flags it as not exposed.
//! - [`ThinkingConfig`](crate::model::ThinkingConfig) maps to
//!   `reasoning.effort` via [`request::effort_for`].
//! - Function tools are offered with `parallel_tool_calls: true`; each call
//!   becomes its own tool-use block.
//! - Usage reports cached input tokens as cache reads and reasoning tokens.

pub mod notice;
pub mod request;

#[cfg(test)]
pub(crate) mod mock;
mod stream;
pub(crate) mod transport;

use std::sync::Arc;

use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

use crate::chatgpt::ChatGptAuth;

use crate::http::{HttpClient, HttpConfig};
use crate::model::{Capabilities, ChatRequest, ReasoningEffort};
use crate::provider::{EventStream, Provider, ProviderError};
use crate::registry::{ModelRef, Registry};

/// Provider name, as used in `harness.ini` (`[provider.openai]`) and on
/// opaque blocks this provider produces.
pub const PROVIDER_NAME: &str = "openai";

/// Default API base URL.
pub const DEFAULT_BASE_URL: &str = "https://api.openai.com/v1";

/// Request path appended to the base URL.
pub const RESPONSES_PATH: &str = "responses";

/// True for `gpt-5` and every later major version (`gpt-5.6-sol`,
/// `gpt-6-astra`, `gpt-10`): the reasoning generation. A `provider/`
/// prefix is ignored.
pub fn is_gpt5_or_later(model: &str) -> bool {
    let m = model.rsplit('/').next().unwrap_or(model);
    let Some(rest) = m.strip_prefix("gpt-") else {
        return false;
    };
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    digits.parse::<u32>().is_ok_and(|major| major >= 5)
}

/// True for models that take `reasoning` parameters: the o-series,
/// `gpt-5` and later (except `*-chat*` variants) and `codex-*`. A
/// `provider/` prefix is ignored.
pub fn is_reasoning_model(model: &str) -> bool {
    let m = model.rsplit('/').next().unwrap_or(model);
    let reasoning_family =
        is_gpt5_or_later(m) || ["o1", "o3", "o4", "codex"].iter().any(|p| m.starts_with(p));
    reasoning_family && !m.contains("-chat")
}

/// The `reasoning.effort` levels an OpenAI model id accepts, when no live
/// list says: GPT-5 takes `minimal` to `high`, GPT-5.1 `low` to `high`
/// (`xhigh` on `codex-max`), GPT-5.2 and later `low` to `xhigh`, the
/// o-series and `codex-*` `low` to `high`. Empty for models that do not
/// reason.
pub fn default_efforts(model: &str) -> Vec<ReasoningEffort> {
    use ReasoningEffort::{High, Low, Medium, Minimal, XHigh};
    let m = model.rsplit('/').next().unwrap_or(model);
    if !is_reasoning_model(m) {
        return Vec::new();
    }
    let Some((major, minor)) = gpt_version(m) else {
        return vec![Low, Medium, High];
    };
    match (major, minor) {
        (5, 0) => vec![Minimal, Low, Medium, High],
        (5, 1) if m.contains("codex-max") => vec![Low, Medium, High, XHigh],
        (5, 1) => vec![Low, Medium, High],
        _ => vec![Low, Medium, High, XHigh],
    }
}

/// `(major, minor)` of a `gpt-N[.M]` id (`gpt-5` is `(5, 0)`).
fn gpt_version(model: &str) -> Option<(u32, u32)> {
    let rest = model.strip_prefix("gpt-")?;
    let digits = |s: &str| -> String { s.chars().take_while(char::is_ascii_digit).collect() };
    let major_s = digits(rest);
    let major = major_s.parse().ok()?;
    let minor = rest[major_s.len()..]
        .strip_prefix('.')
        .and_then(|r| digits(r).parse().ok())
        .unwrap_or(0);
    Some((major, minor))
}

/// Best-known capabilities of an OpenAI model id.
pub fn model_capabilities(model: &str) -> Capabilities {
    let m = model.rsplit('/').next().unwrap_or(model);
    let (context_window, max_output_tokens) = if is_gpt5_or_later(m) || m.starts_with("codex") {
        (400_000, 128_000)
    } else if m.starts_with("o1") || m.starts_with("o3") || m.starts_with("o4") {
        (200_000, 100_000)
    } else if m.starts_with("gpt-4.1") {
        (1_047_576, 32_768)
    } else if m.starts_with("gpt-4o") {
        (128_000, 16_384)
    } else {
        (128_000, 0)
    };
    Capabilities {
        tools: true,
        parallel_tool_calls: true,
        thinking: is_reasoning_model(m),
        caching: true,
        images: true,
        context_window,
        max_output_tokens,
    }
}

/// How requests are authenticated.
#[derive(Debug, Clone)]
enum Auth {
    /// An API key, sent as a default header.
    ApiKey,
    /// Sign in with ChatGPT: a fresh access token per call.
    ChatGpt(Arc<ChatGptAuth>),
}

/// The OpenAI provider.
#[derive(Debug, Clone)]
pub struct OpenAiProvider {
    client: HttpClient,
    auth: Auth,
    /// `[provider.openai] prompt_cache_retention = 24h`: send
    /// [`request::cache_retention_for`] on API-key requests.
    extended_cache_retention: bool,
    /// Model facts (with live-list models) for the effort levels each
    /// model accepts; [`default_efforts`] when unset or unknown.
    registry: Option<Arc<Registry>>,
}

impl OpenAiProvider {
    /// A provider using `api_key` and the transport settings in `http`
    /// (whose `base_url` is usually [`DEFAULT_BASE_URL`]). The key is added
    /// as a sensitive `Authorization: Bearer` default header.
    pub fn new(api_key: &str, mut http: HttpConfig) -> Result<Self, ProviderError> {
        http.default_headers
            .extend(transport::bearer_headers(api_key)?);
        Ok(Self {
            client: HttpClient::new(http)?,
            auth: Auth::ApiKey,
            extended_cache_retention: false,
            registry: None,
        })
    }

    /// A provider using the user's ChatGPT plan through `auth` (Sign in
    /// with ChatGPT). Every call gets a current access token from `auth`,
    /// which refreshes it before it expires; bodies are sent with
    /// `store: false` and streaming, without the fields the plan route
    /// rejects ([`crate::chatgpt::UNSUPPORTED_REQUEST_FIELDS`]). `http`'s
    /// `base_url` is usually `auth.endpoints().api_base_url`.
    pub fn with_chatgpt(auth: Arc<ChatGptAuth>, http: HttpConfig) -> Result<Self, ProviderError> {
        Ok(Self {
            client: HttpClient::new(http)?,
            auth: Auth::ChatGpt(auth),
            extended_cache_retention: false,
            registry: None,
        })
    }

    /// Opts in to extended prompt-cache retention
    /// (`[provider.openai] prompt_cache_retention = 24h`): API-key requests
    /// for [`request::EXTENDED_RETENTION_MODELS`] then carry
    /// `prompt_cache_retention: "24h"`. Off by default, so the
    /// organisation's own default applies (Zero Data Retention
    /// organisations keep in-memory caching). ChatGPT mode never sends it.
    pub fn with_extended_cache_retention(mut self, on: bool) -> Self {
        self.extended_cache_retention = on;
        self
    }

    /// Uses `registry` for the effort levels each model accepts (live
    /// ChatGPT lists report them per model).
    pub fn with_registry(mut self, registry: Arc<Registry>) -> Self {
        self.registry = Some(registry);
        self
    }

    /// The `reasoning.effort` levels `model` accepts.
    pub fn efforts(&self, model: &str) -> Vec<ReasoningEffort> {
        self.registry
            .as_ref()
            .and_then(|r| r.lookup(&ModelRef::new(PROVIDER_NAME, model)))
            .map(|i| i.efforts)
            .filter(|e| !e.is_empty())
            .unwrap_or_else(|| default_efforts(model))
    }

    /// True when this provider uses Sign in with ChatGPT.
    pub fn uses_chatgpt(&self) -> bool {
        matches!(self.auth, Auth::ChatGpt(_))
    }

    /// A provider with default transport settings. `base_url` defaults to
    /// [`DEFAULT_BASE_URL`].
    pub fn with_key(api_key: &str, base_url: Option<&str>) -> Result<Self, ProviderError> {
        Self::new(
            api_key,
            HttpConfig::new(base_url.unwrap_or(DEFAULT_BASE_URL)),
        )
    }

    /// The JSON body that [`Provider::stream`] would send for `request`.
    pub fn request_body(&self, request: &ChatRequest) -> Result<serde_json::Value, ProviderError> {
        let mut body = request::build_body(request, is_reasoning_model(&request.model))?;
        // A level the model does not take goes as the nearest one it does.
        if let (Some(e), Some(slot)) = (
            request.thinking.and_then(|t| t.effort),
            body.pointer_mut("/reasoning/effort"),
        ) {
            *slot = e.nearest(&self.efforts(&request.model)).as_str().into();
        }
        if self.uses_chatgpt() {
            request::for_chatgpt(&mut body);
        } else if self.extended_cache_retention {
            request::apply_cache_retention(&mut body, &request.model);
        }
        Ok(body)
    }
}

#[async_trait]
impl Provider for OpenAiProvider {
    fn name(&self) -> &str {
        PROVIDER_NAME
    }

    fn request_body(&self, request: &ChatRequest) -> Option<serde_json::Value> {
        OpenAiProvider::request_body(self, request).ok()
    }

    async fn prewarm(&self) {
        if let Err(e) = self.client.warm("models").await {
            tracing::debug!(error = %e, "openai connection pre-warm failed");
        }
    }

    fn capabilities(&self, model: &str) -> Capabilities {
        model_capabilities(model)
    }

    async fn stream(
        &self,
        request: ChatRequest,
        cancel: CancellationToken,
    ) -> Result<EventStream, ProviderError> {
        let body = self.request_body(&request)?;
        let headers = match &self.auth {
            Auth::ApiKey => Default::default(),
            Auth::ChatGpt(auth) => {
                let token = auth.access_token().await?;
                transport::bearer_headers(token.expose())?
            }
        };
        Ok(transport::open_stream(
            self.client.clone(),
            RESPONSES_PATH,
            headers,
            body,
            cancel,
            stream::ResponsesMapper::default,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::mock::{Canned, MockServer, sse_body};
    use super::notice::{HiddenReason, reasoning_hidden};
    use super::*;
    use crate::model::{
        ContentBlock, Message, ReasoningEffort, Role, StopKind, ThinkingConfig, ToolSpec, Usage,
    };
    use crate::provider::{StreamEvent, collect};
    use crate::retry::RetryPolicy;
    use futures::StreamExt;
    use serde_json::{Value, json};
    use std::time::Duration;

    fn provider(base: &str) -> OpenAiProvider {
        let mut http = HttpConfig::new(base);
        http.retry = RetryPolicy {
            base_delay: Duration::from_millis(1),
            max_delay: Duration::from_millis(5),
            ..RetryPolicy::default()
        };
        OpenAiProvider::new("sk-test", http).unwrap()
    }

    fn tool_request() -> ChatRequest {
        let mut r = ChatRequest::new("o4-mini");
        r.messages = vec![Message::user("list and read")];
        r.tools = vec![
            ToolSpec {
                name: "bash".into(),
                description: "run".into(),
                input_schema: json!({"type": "object"}),
            },
            ToolSpec {
                name: "read".into(),
                description: "read".into(),
                input_schema: json!({"type": "object"}),
            },
        ];
        r.thinking = Some(ThinkingConfig {
            effort: Some(ReasoningEffort::Low),
            budget_tokens: None,
        });
        r
    }

    fn turn_with_parallel_tools() -> String {
        let reasoning = json!({"type": "reasoning", "id": "rs_1", "encrypted_content": "ENC1",
            "summary": [{"type": "summary_text", "text": "Plan: list then read."}]});
        sse_body(&[
            (
                "response.created",
                json!({"type": "response.created", "response": {"id": "resp_1", "model": "o4-mini-2025"}}),
            ),
            (
                "response.output_item.added",
                json!({"type": "response.output_item.added", "output_index": 0, "item": {"type": "reasoning", "id": "rs_1"}}),
            ),
            (
                "response.reasoning_summary_text.delta",
                json!({"type": "response.reasoning_summary_text.delta", "output_index": 0, "summary_index": 0, "delta": "Plan: list "}),
            ),
            (
                "response.reasoning_summary_text.delta",
                json!({"type": "response.reasoning_summary_text.delta", "output_index": 0, "summary_index": 0, "delta": "then read."}),
            ),
            (
                "response.output_item.done",
                json!({"type": "response.output_item.done", "output_index": 0, "item": reasoning}),
            ),
            (
                "response.output_item.added",
                json!({"type": "response.output_item.added", "output_index": 1, "item": {"type": "function_call", "id": "fc_1", "call_id": "call_1", "name": "bash", "arguments": ""}}),
            ),
            (
                "response.output_item.added",
                json!({"type": "response.output_item.added", "output_index": 2, "item": {"type": "function_call", "id": "fc_2", "call_id": "call_2", "name": "read", "arguments": ""}}),
            ),
            (
                "response.function_call_arguments.delta",
                json!({"type": "response.function_call_arguments.delta", "output_index": 1, "delta": "{\"command\":"}),
            ),
            (
                "response.function_call_arguments.delta",
                json!({"type": "response.function_call_arguments.delta", "output_index": 2, "delta": "{\"path\":\"a.rs\"}"}),
            ),
            (
                "response.function_call_arguments.delta",
                json!({"type": "response.function_call_arguments.delta", "output_index": 1, "delta": "\"ls\"}"}),
            ),
            (
                "response.output_item.done",
                json!({"type": "response.output_item.done", "output_index": 1, "item": {"type": "function_call", "call_id": "call_1", "name": "bash", "arguments": "{\"command\":\"ls\"}"}}),
            ),
            (
                "response.output_item.done",
                json!({"type": "response.output_item.done", "output_index": 2, "item": {"type": "function_call", "call_id": "call_2", "name": "read", "arguments": "{\"path\":\"a.rs\"}"}}),
            ),
            (
                "response.completed",
                json!({"type": "response.completed", "response": {"id": "resp_1", "status": "completed", "usage": {
                "input_tokens": 1000, "input_tokens_details": {"cached_tokens": 800},
                "output_tokens": 120, "output_tokens_details": {"reasoning_tokens": 64}}}}),
            ),
        ])
    }

    #[tokio::test]
    async fn streams_parallel_tool_calls_reasoning_and_usage() {
        let server = MockServer::start(vec![Canned::sse(turn_with_parallel_tools())]).await;
        let p = provider(&server.base_url);
        let stream = p
            .stream(tool_request(), CancellationToken::new())
            .await
            .unwrap();
        let events: Vec<StreamEvent> = stream.map(Result::unwrap).collect().await;
        // Reasoning summary streams live, before any tool call.
        assert_eq!(
            events[1],
            StreamEvent::ReasoningDelta {
                index: 0,
                text: "Plan: list ".into()
            }
        );
        let mut builder = crate::provider::ResponseBuilder::new();
        for e in events {
            builder.apply(e).unwrap();
        }
        let resp = builder.finish().unwrap();
        assert_eq!(resp.id.as_deref(), Some("resp_1"));
        assert_eq!(resp.stop.kind, StopKind::ToolUse);
        assert_eq!(
            resp.usage,
            Usage {
                input_tokens: 200,
                output_tokens: 120,
                cache_read_tokens: 800,
                cache_write_tokens: 0,
                reasoning_tokens: 64
            }
        );
        let calls: Vec<_> = resp.message.tool_uses().collect();
        assert_eq!(
            calls,
            vec![
                ("call_1", "bash", &json!({"command": "ls"})),
                ("call_2", "read", &json!({"path": "a.rs"}))
            ]
        );
        assert_eq!(
            resp.message.content[0],
            ContentBlock::Reasoning {
                text: "Plan: list then read.".into(),
                signature: None
            }
        );
        assert!(matches!(&resp.message.content[1],
            ContentBlock::Opaque { provider, data } if provider == "openai" && data["encrypted_content"] == "ENC1"));
        assert_eq!(reasoning_hidden(&resp), None);

        let req = &server.requests()[0];
        assert_eq!(req.method, "POST");
        assert_eq!(req.path, "/v1/responses");
        assert_eq!(req.header("authorization"), Some("Bearer sk-test"));
        let body = req.json();
        assert_eq!(body["store"], json!(false));
        assert_eq!(body["parallel_tool_calls"], json!(true));
        assert_eq!(body["reasoning"]["effort"], json!("low"));
        assert_eq!(body["reasoning"]["summary"], json!("auto"));
        assert_eq!(body["include"], json!(["reasoning.encrypted_content"]));
    }

    #[test]
    fn effort_is_sent_as_a_level_the_model_accepts() {
        use crate::catalog::{LiveModel, live_model_info};
        let with = |model: &str, e| {
            let mut r = ChatRequest::new(model);
            r.thinking = Some(ThinkingConfig {
                budget_tokens: None,
                effort: Some(e),
            });
            r
        };
        let p = OpenAiProvider::with_key("k", None).unwrap();
        let sent = |p: &OpenAiProvider, model, e| {
            p.request_body(&with(model, e)).unwrap()["reasoning"]["effort"].clone()
        };
        assert_eq!(
            sent(&p, "gpt-5", ReasoningEffort::Minimal),
            json!("minimal")
        );
        assert_eq!(sent(&p, "gpt-5", ReasoningEffort::Max), json!("high"));
        assert_eq!(sent(&p, "gpt-5.1", ReasoningEffort::Minimal), json!("low"));
        assert_eq!(
            sent(&p, "gpt-6-astra", ReasoningEffort::Ultra),
            json!("xhigh")
        );
        // A live list's levels win over the defaults.
        let live = LiveModel {
            id: "gpt-6-astra".into(),
            reasoning: Some(true),
            efforts: Some(ReasoningEffort::ALL[1..6].to_vec()),
            ..LiveModel::default()
        };
        let registry =
            Registry::builtin().with_live_models([live_model_info(PROVIDER_NAME, &live)]);
        let p = p.with_registry(Arc::new(registry));
        assert_eq!(
            sent(&p, "gpt-6-astra", ReasoningEffort::Ultra),
            json!("max")
        );
        assert_eq!(sent(&p, "gpt-6-astra", ReasoningEffort::Max), json!("max"));
        assert_eq!(
            sent(&p, "gpt-6-astra", ReasoningEffort::Minimal),
            json!("low")
        );
    }

    #[tokio::test]
    async fn encrypted_reasoning_round_trips_on_the_next_turn() {
        let second = sse_body(&[
            (
                "",
                json!({"type": "response.created", "response": {"id": "resp_2", "model": "o4-mini"}}),
            ),
            (
                "",
                json!({"type": "response.output_item.done", "output_index": 0, "item": {"type": "reasoning", "id": "rs_2", "encrypted_content": "ENC2", "summary": []}}),
            ),
            (
                "",
                json!({"type": "response.output_text.delta", "output_index": 1, "content_index": 0, "delta": "Done."}),
            ),
            (
                "",
                json!({"type": "response.output_item.done", "output_index": 1, "item": {"type": "message", "content": [{"type": "output_text", "text": "Done."}]}}),
            ),
            (
                "",
                json!({"type": "response.completed", "response": {"id": "resp_2", "usage": {"input_tokens": 5, "output_tokens": 2}}}),
            ),
        ]);
        let server = MockServer::start(vec![
            Canned::sse(turn_with_parallel_tools()),
            Canned::sse(second),
        ])
        .await;
        let p = provider(&server.base_url);
        let mut req = tool_request();
        let first = p
            .complete(req.clone(), CancellationToken::new())
            .await
            .unwrap();
        req.messages.push(first.message.clone());
        req.messages.push(Message {
            role: Role::User,
            content: vec![
                ContentBlock::tool_result("call_1", "a.rs", false),
                ContentBlock::tool_result("call_2", "fn main(){}", false),
            ],
        });
        let second = p.complete(req, CancellationToken::new()).await.unwrap();
        assert_eq!(second.message.text(), "Done.");
        assert_eq!(second.stop.kind, StopKind::EndTurn);
        // Encrypted-only reasoning is flagged as not exposed.
        assert_eq!(reasoning_hidden(&second), Some(HiddenReason::EncryptedOnly));

        let body = server.requests()[1].json();
        let input = body["input"].as_array().unwrap();
        let types: Vec<&str> = input.iter().map(|i| i["type"].as_str().unwrap()).collect();
        assert_eq!(
            types,
            vec![
                "message",
                "reasoning",
                "function_call",
                "function_call",
                "function_call_output",
                "function_call_output"
            ]
        );
        assert_eq!(
            input[1],
            json!({"type": "reasoning", "id": "rs_1", "encrypted_content": "ENC1",
                   "summary": [{"type": "summary_text", "text": "Plan: list then read."}]})
        );
        assert_eq!(input[2]["call_id"], json!("call_1"));
        assert_eq!(input[5]["output"], json!("fn main(){}"));
        assert_eq!(body["store"], json!(false));
    }

    #[tokio::test]
    async fn retries_mid_stream_server_errors_and_http_429() {
        let failing = sse_body(&[
            (
                "",
                json!({"type": "response.created", "response": {"id": "r0"}}),
            ),
            (
                "",
                json!({"type": "response.output_text.delta", "output_index": 0, "delta": "partial"}),
            ),
            (
                "",
                json!({"type": "error", "code": "server_error", "message": "boom"}),
            ),
        ]);
        let ok = sse_body(&[
            (
                "",
                json!({"type": "response.output_text.delta", "output_index": 0, "delta": "hello"}),
            ),
            (
                "",
                json!({"type": "response.completed", "response": {"id": "r1", "usage": {}}}),
            ),
        ]);
        let server = MockServer::start(vec![
            Canned::json(429, r#"{"error":{"message":"slow down"}}"#),
            Canned::sse(failing),
            Canned::sse(ok),
        ])
        .await;
        let p = provider(&server.base_url);
        let resp = p
            .complete(ChatRequest::new("gpt-4.1"), CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(resp.message.text(), "hello");
        assert_eq!(resp.retries.len(), 2);
        assert!(resp.retries[1].discard_partial);
        assert_eq!(server.requests().len(), 3);
        let body = server.requests()[0].json();
        assert!(body.get("reasoning").is_none());
    }

    #[tokio::test]
    async fn auth_errors_are_not_retried() {
        let server = MockServer::start(vec![Canned::json(
            401,
            r#"{"error":{"message":"bad key"}}"#,
        )])
        .await;
        let p = provider(&server.base_url);
        let err = p
            .complete(ChatRequest::new("gpt-4o"), CancellationToken::new())
            .await
            .unwrap_err();
        assert_eq!(err, ProviderError::Auth("bad key".into()));
        assert_eq!(server.requests().len(), 1);
    }

    #[tokio::test]
    async fn truncated_stream_is_a_retryable_error() {
        let partial = sse_body(&[(
            "",
            json!({"type": "response.output_text.delta", "output_index": 0, "delta": "x"}),
        )]);
        let server = MockServer::start(vec![Canned::sse(partial)]).await;
        let mut http = HttpConfig::new(server.base_url.clone());
        http.retry = RetryPolicy::none();
        let p = OpenAiProvider::new("k", http).unwrap();
        let err = collect(
            p.stream(ChatRequest::new("gpt-4o"), CancellationToken::new())
                .await
                .unwrap(),
        )
        .await
        .unwrap_err();
        assert!(err.root().retry_reason().is_some(), "{err:?}");
    }

    #[test]
    fn reasoning_model_detection_and_capabilities() {
        for m in [
            "o3",
            "o4-mini",
            "gpt-5",
            "gpt-5-mini",
            "openai/o1",
            "codex-mini-latest",
        ] {
            assert!(is_reasoning_model(m), "{m}");
        }
        for m in ["gpt-4o", "gpt-4.1-mini", "gpt-5-chat-latest"] {
            assert!(!is_reasoning_model(m), "{m}");
        }
        // Later generations are reasoning models too, whatever their name.
        for m in ["gpt-5.6-sol", "gpt-6-astra", "gpt-6.1-terra", "gpt-10"] {
            assert!(is_reasoning_model(m), "{m}");
            assert_eq!(model_capabilities(m).context_window, 400_000, "{m}");
        }
        for m in ["gpt-4.5", "gpt-reserve", "gpt-", "gpt-x5"] {
            assert!(!is_gpt5_or_later(m), "{m}");
        }
        use ReasoningEffort::{High, Low, Medium, Minimal, XHigh};
        assert_eq!(default_efforts("gpt-5-mini"), [Minimal, Low, Medium, High]);
        assert_eq!(default_efforts("openai/gpt-5.1"), [Low, Medium, High]);
        assert_eq!(
            default_efforts("gpt-5.1-codex-max"),
            [Low, Medium, High, XHigh]
        );
        assert_eq!(default_efforts("gpt-5.2"), [Low, Medium, High, XHigh]);
        assert_eq!(default_efforts("gpt-6-astra"), [Low, Medium, High, XHigh]);
        assert_eq!(default_efforts("o3"), [Low, Medium, High]);
        assert!(default_efforts("gpt-4o").is_empty());
        let c = model_capabilities("gpt-5");
        assert!(c.thinking && c.tools && c.parallel_tool_calls);
        assert_eq!(c.context_window, 400_000);
        assert!(!model_capabilities("gpt-4o").thinking);
        let p = OpenAiProvider::with_key("k", None).unwrap();
        assert_eq!(p.name(), "openai");
        let body: Value = p.request_body(&ChatRequest::new("o3")).unwrap();
        assert_eq!(body["model"], json!("o3"));
    }

    #[test]
    fn extended_cache_retention_is_opt_in_for_api_key_requests() {
        let req = ChatRequest::new("gpt-5.5");
        let default = OpenAiProvider::with_key("k", None).unwrap();
        let body = default.request_body(&req).unwrap();
        assert!(body["prompt_cache_key"].is_string());
        assert!(
            body.get("prompt_cache_retention").is_none(),
            "off by default: the org's retention policy applies"
        );
        let opted = OpenAiProvider::with_key("k", None)
            .unwrap()
            .with_extended_cache_retention(true);
        let body = opted.request_body(&req).unwrap();
        assert_eq!(body["prompt_cache_retention"], json!("24h"));
        assert_eq!(
            body["prompt_cache_key"],
            default.request_body(&req).unwrap()["prompt_cache_key"]
        );
        // Models without extended retention never get it.
        let body = opted.request_body(&ChatRequest::new("gpt-4o")).unwrap();
        assert!(body.get("prompt_cache_retention").is_none());
    }
}
