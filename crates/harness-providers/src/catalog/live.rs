//! Live model lists: what each provider's `GET /models` endpoint says,
//! normalized into [`LiveModel`]s, and the [`ModelListSource`] that fetches
//! them.

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use reqwest::header::{AUTHORIZATION, HeaderMap, HeaderValue};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::chatgpt::ChatGptAuth;
use crate::http::error_message;
use crate::model::ReasoningEffort;
use crate::pricing::Price;
use crate::provider::ProviderError;

/// Anthropic returns at most this many models per page.
const ANTHROPIC_PAGE_LIMIT: u32 = 1000;

/// Pages fetched at most, as a guard against a server that never says
/// `has_more: false`.
const MAX_PAGES: usize = 20;

/// One model as a provider's live list describes it. Every fact is optional
/// because providers publish very different amounts of detail: OpenAI only
/// lists ids, Anthropic adds limits and capabilities, OpenRouter adds
/// prices and supported parameters.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct LiveModel {
    /// Provider-local model id (`claude-opus-5-5`, `gpt-5.5`,
    /// `anthropic/claude-sonnet-5.5`).
    pub id: String,
    /// Human-readable name, if the provider gives one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    /// Context window in tokens.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window: Option<u32>,
    /// Maximum output tokens.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u32>,
    /// List price, USD per million tokens.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub price: Option<Price>,
    /// Tool calling support, if the list says.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<bool>,
    /// Reasoning / thinking support, if the list says.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<bool>,
    /// Image input support, if the list says.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vision: Option<bool>,
    /// `Some(true)` when the list is the signed-in ChatGPT account's own
    /// ([`ListKind::ChatGpt`]): the plan covers exactly these models.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan: Option<bool>,
    /// Position in the server's own ordering, when the server orders its
    /// list for display (ChatGPT); lower comes first.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rank: Option<u32>,
    /// The reasoning effort levels the model accepts, if the list says
    /// (ChatGPT `supported_reasoning_levels`, Anthropic
    /// `capabilities.effort`). Unknown level names are left out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub efforts: Option<Vec<ReasoningEffort>>,
}

/// The response shape a [`ModelListSource`] speaks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ListKind {
    /// `GET {base}/v1/models` with `x-api-key`, paginated by `after_id`.
    Anthropic,
    /// `GET {base}/models` with a bearer token; ids only.
    OpenAi,
    /// `GET {base}/models` with a bearer token; full facts and prices.
    OpenRouter,
    /// `GET {base}/models` with a Sign in with ChatGPT access token: the
    /// account's plan models in a `models` array (`slug`, `display_name`,
    /// `visibility`, `context_window`, ...).
    ChatGpt,
}

impl ListKind {
    /// The kind for a provider id, if harness knows how to list it.
    pub fn for_provider(provider: &str) -> Option<Self> {
        match provider {
            crate::anthropic::NAME => Some(Self::Anthropic),
            crate::openai::PROVIDER_NAME => Some(Self::OpenAi),
            crate::openrouter::PROVIDER_NAME => Some(Self::OpenRouter),
            _ => None,
        }
    }

    /// The default base URL for this kind.
    pub fn default_base_url(self) -> &'static str {
        match self {
            Self::Anthropic => crate::anthropic::DEFAULT_BASE_URL,
            Self::OpenAi | Self::ChatGpt => crate::openai::DEFAULT_BASE_URL,
            Self::OpenRouter => crate::openrouter::DEFAULT_BASE_URL,
        }
    }

    /// Request path appended to the base URL.
    pub fn path(self) -> &'static str {
        match self {
            Self::Anthropic => "v1/models",
            Self::OpenAi | Self::OpenRouter | Self::ChatGpt => "models",
        }
    }
}

/// Where and how to fetch one provider's live model list.
///
/// The API key (or the ChatGPT sign-in) is held only to build the request;
/// `Debug` never prints it.
#[derive(Clone)]
pub struct ModelListSource {
    /// Provider id (`anthropic`, `openai`, `openrouter`).
    pub provider: String,
    /// Response shape.
    pub kind: ListKind,
    /// Base URL (the provider's `base_url`, or the kind's default).
    pub base_url: String,
    /// Whole-request timeout.
    pub timeout: Duration,
    api_key: Option<String>,
    chatgpt: Option<Arc<ChatGptAuth>>,
    origin: String,
}

impl fmt::Debug for ModelListSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ModelListSource")
            .field("provider", &self.provider)
            .field("kind", &self.kind)
            .field("base_url", &self.base_url)
            .field("api_key", &self.api_key.as_ref().map(|_| "<redacted>"))
            .field("chatgpt", &self.chatgpt.is_some())
            .field("origin", &self.origin)
            .finish()
    }
}

/// Appended to every [`ModelListSource::origin`]. Changing it makes every
/// cached list stale, so lists cached before a parser learned a new fact
/// (v2: effort levels) are fetched again; until then the old list is still
/// used.
const FACTS_TAG: &str = "#facts:3";

impl ModelListSource {
    /// A source for `provider` (which must be one [`ListKind::for_provider`]
    /// knows) authenticated with `api_key`. `base_url` defaults to the
    /// provider's public endpoint.
    pub fn new(provider: &str, api_key: Option<&str>, base_url: Option<&str>) -> Option<Self> {
        let kind = ListKind::for_provider(provider)?;
        let base_url = base_url.unwrap_or(kind.default_base_url()).to_owned();
        Some(Self {
            provider: provider.to_owned(),
            kind,
            origin: format!("{base_url}{FACTS_TAG}"),
            base_url,
            timeout: Duration::from_secs(20),
            api_key: api_key
                .map(str::trim)
                .filter(|k| !k.is_empty())
                .map(str::to_owned),
            chatgpt: None,
        })
    }

    /// The `openai` list of the account signed in with ChatGPT in `auth`,
    /// from the API at `base_url` (the default API when `None`). Each fetch
    /// asks `auth` for a fresh access token. `account` (the sign-in's
    /// `sub`) is part of [`ModelListSource::origin`], so a cached list of
    /// another account is never taken as fresh.
    pub fn chatgpt(auth: Arc<ChatGptAuth>, base_url: Option<&str>, account: &str) -> Self {
        let kind = ListKind::ChatGpt;
        let base_url = base_url
            .unwrap_or(kind.default_base_url())
            .trim_end_matches('/')
            .to_owned();
        Self {
            provider: crate::openai::PROVIDER_NAME.to_owned(),
            kind,
            origin: format!("{base_url}#chatgpt:{}{FACTS_TAG}", account_tag(account)),
            base_url,
            timeout: Duration::from_secs(20),
            api_key: None,
            chatgpt: Some(auth),
        }
    }

    /// What a cached list must have come from to count as fresh: the base
    /// URL, plus the account for a ChatGPT list.
    pub fn origin(&self) -> &str {
        &self.origin
    }

    /// The URL of the first page.
    pub fn url(&self) -> String {
        format!(
            "{}/{}",
            self.base_url.trim_end_matches('/'),
            self.kind.path()
        )
    }

    fn headers(&self) -> Result<HeaderMap, ProviderError> {
        let mut h = HeaderMap::new();
        let Some(key) = &self.api_key else {
            return Ok(h);
        };
        let bad =
            || ProviderError::InvalidRequest("the API key contains invalid characters".into());
        match self.kind {
            ListKind::Anthropic => {
                let mut v = HeaderValue::from_str(key).map_err(|_| bad())?;
                v.set_sensitive(true);
                h.insert("x-api-key", v);
                h.insert(
                    "anthropic-version",
                    HeaderValue::from_static(crate::anthropic::API_VERSION),
                );
            }
            ListKind::OpenAi | ListKind::OpenRouter | ListKind::ChatGpt => {
                let mut v = HeaderValue::from_str(&format!("Bearer {key}")).map_err(|_| bad())?;
                v.set_sensitive(true);
                h.insert(AUTHORIZATION, v);
            }
        }
        Ok(h)
    }

    /// Fetches and parses the whole list (following Anthropic pagination).
    /// HTTP 401/403 come back as [`ProviderError::Auth`].
    pub async fn fetch(&self, client: &reqwest::Client) -> Result<Vec<LiveModel>, ProviderError> {
        let mut headers = self.headers()?;
        if let Some(auth) = &self.chatgpt {
            let token = auth.access_token().await?;
            let mut v =
                HeaderValue::from_str(&format!("Bearer {}", token.expose())).map_err(|_| {
                    ProviderError::InvalidRequest(
                        "the access token contains invalid characters".into(),
                    )
                })?;
            v.set_sensitive(true);
            headers.insert(AUTHORIZATION, v);
        }
        let mut out = Vec::new();
        let mut after: Option<String> = None;
        for _ in 0..MAX_PAGES {
            let mut url = self.url();
            if self.kind == ListKind::Anthropic {
                url.push_str(&format!("?limit={ANTHROPIC_PAGE_LIMIT}"));
                if let Some(a) = &after {
                    url.push_str("&after_id=");
                    url.push_str(&query_escape(a));
                }
            }
            let req = client
                .get(url)
                .headers(headers.clone())
                .timeout(self.timeout);
            let response = req.send().await.map_err(|e| {
                if e.is_timeout() {
                    ProviderError::Timeout(self.timeout)
                } else {
                    ProviderError::Connection(e.to_string())
                }
            })?;
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
                .map_err(|e| ProviderError::Decode(format!("{} model list: {e}", self.provider)))?;
            let page = parse(self.kind, &body)?;
            out.extend(page.models);
            match page.next_after {
                Some(next) if self.kind == ListKind::Anthropic => after = Some(next),
                _ => return Ok(out),
            }
        }
        Ok(out)
    }
}

/// Percent-encodes everything but unreserved URL characters.
fn query_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(char::from(b));
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// One parsed page.
#[derive(Debug, Clone, PartialEq)]
pub struct Page {
    /// Models on this page.
    pub models: Vec<LiveModel>,
    /// The cursor for the next page, when there is one.
    pub next_after: Option<String>,
}

/// A short, stable tag for a ChatGPT account id, so cache files name the
/// account without storing it.
fn account_tag(account: &str) -> String {
    use aws_lc_rs::digest::{SHA256, digest};
    digest(&SHA256, account.as_bytes()).as_ref()[..6]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Parses one response body of the given kind.
pub fn parse(kind: ListKind, body: &Value) -> Result<Page, ProviderError> {
    if kind == ListKind::ChatGpt {
        return parse_chatgpt(body);
    }
    let data = body
        .get("data")
        .and_then(Value::as_array)
        .ok_or_else(|| ProviderError::Decode("model list response has no `data` array".into()))?;
    let models = match kind {
        ListKind::Anthropic => data.iter().filter_map(anthropic_model).collect(),
        ListKind::OpenAi => data.iter().filter_map(openai_model).collect(),
        ListKind::OpenRouter => data.iter().filter_map(openrouter_model).collect(),
        ListKind::ChatGpt => unreachable!("parsed by parse_chatgpt"),
    };
    let next_after = (kind == ListKind::Anthropic
        && body.get("has_more").and_then(Value::as_bool) == Some(true))
    .then(|| {
        body.get("last_id")
            .and_then(Value::as_str)
            .map(str::to_owned)
    })
    .flatten();
    Ok(Page { models, next_after })
}

/// The ChatGPT account list: entries with `visibility: "list"` (the ones
/// meant for a model picker) that the API accepts, in the server's order,
/// with `slug` as the id and `display_name` as the name.
fn parse_chatgpt(body: &Value) -> Result<Page, ProviderError> {
    let data = body
        .get("models")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            ProviderError::Decode("ChatGPT model list response has no `models` array".into())
        })?;
    let models = data
        .iter()
        .filter(|v| v.get("visibility").and_then(Value::as_str) == Some("list"))
        .filter(|v| v.get("supported_in_api").and_then(Value::as_bool) != Some(false))
        .filter_map(|v| {
            let id = v.get("slug")?.as_str()?.trim();
            (!id.is_empty()).then(|| id.to_owned()).map(|id| (id, v))
        })
        .enumerate()
        .map(|(i, (id, v))| {
            let levels = v
                .get("supported_reasoning_levels")
                .and_then(Value::as_array);
            let efforts = levels.map(|l| {
                let mut e: Vec<ReasoningEffort> = l
                    .iter()
                    .filter_map(|x| x.get("effort").and_then(Value::as_str))
                    .filter_map(ReasoningEffort::parse)
                    // The list offers `ultra`, but the Responses API
                    // rejects it (it takes `none` to `max`); it is sent as
                    // `max` instead.
                    .filter(|e| *e != ReasoningEffort::Ultra)
                    .collect();
                e.sort();
                e.dedup();
                e
            });
            let vision = v
                .get("input_modalities")
                .and_then(Value::as_array)
                .map(|m| m.iter().any(|x| x.as_str() == Some("image")));
            LiveModel {
                display_name: v
                    .get("display_name")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                context_window: u32_at(v, "/context_window"),
                tools: Some(true),
                reasoning: levels.map(|l| !l.is_empty()),
                vision,
                plan: Some(true),
                efforts,
                rank: Some(u32::try_from(i).unwrap_or(u32::MAX)),
                id,
                ..LiveModel::default()
            }
        })
        .collect();
    Ok(Page {
        models,
        next_after: None,
    })
}

fn u32_at(v: &Value, pointer: &str) -> Option<u32> {
    v.pointer(pointer)
        .and_then(Value::as_u64)
        .filter(|n| *n > 0)
        .map(|n| u32::try_from(n).unwrap_or(u32::MAX))
}

fn anthropic_model(v: &Value) -> Option<LiveModel> {
    let id = v.get("id")?.as_str()?.to_owned();
    let supported = |p: &str| v.pointer(p).and_then(Value::as_bool);
    Some(LiveModel {
        display_name: v
            .get("display_name")
            .and_then(Value::as_str)
            .map(str::to_owned),
        context_window: u32_at(v, "/max_input_tokens"),
        max_output_tokens: u32_at(v, "/max_tokens"),
        price: None,
        // Every Claude model on the Messages API supports tool use.
        tools: Some(true),
        reasoning: supported("/capabilities/thinking/supported"),
        vision: supported("/capabilities/image_input/supported"),
        efforts: v.pointer("/capabilities/effort").map(|e| {
            if e.get("supported").and_then(Value::as_bool) == Some(false) {
                return Vec::new();
            }
            ReasoningEffort::ALL
                .into_iter()
                .filter(|l| {
                    e.pointer(&format!("/{l}/supported"))
                        .and_then(Value::as_bool)
                        == Some(true)
                })
                .collect()
        }),
        id,
        ..LiveModel::default()
    })
}

/// OpenAI model families that cannot serve a chat turn at all (embeddings,
/// speech, images, moderation, realtime audio, legacy completions). They are
/// left out of the catalog.
const OPENAI_NON_CHAT: &[&str] = &[
    "embedding",
    "whisper",
    "tts",
    "dall-e",
    "gpt-image",
    "moderation",
    "realtime",
    "transcribe",
    "audio",
    "davinci",
    "babbage",
    "sora",
];

/// True when an OpenAI model id names a chat-capable model.
pub fn openai_is_chat_model(id: &str) -> bool {
    let chat_family = ["gpt-", "o1", "o3", "o4", "codex", "chatgpt-"]
        .iter()
        .any(|p| id.starts_with(p));
    chat_family && !OPENAI_NON_CHAT.iter().any(|x| id.contains(x))
}

/// True when an OpenAI chat model supports function calling. Search
/// previews and `chatgpt-*` snapshots do not.
pub fn openai_supports_tools(id: &str) -> bool {
    !(id.contains("-search") || id.starts_with("chatgpt-"))
}

fn openai_model(v: &Value) -> Option<LiveModel> {
    let id = v.get("id")?.as_str()?;
    if !openai_is_chat_model(id) {
        return None;
    }
    Some(LiveModel {
        id: id.to_owned(),
        tools: Some(openai_supports_tools(id)),
        ..LiveModel::default()
    })
}

/// An OpenRouter per-token price string as USD per million tokens. Negative
/// values mean "variable" (routers) and are treated as unknown.
fn per_million(v: &Value) -> Option<f64> {
    let n: f64 = match v {
        Value::String(s) => s.trim().parse().ok()?,
        Value::Number(n) => n.as_f64()?,
        _ => return None,
    };
    (n.is_finite() && n >= 0.0).then(|| (n * 1_000_000.0 * 1e6).round() / 1e6)
}

fn openrouter_model(v: &Value) -> Option<LiveModel> {
    let base = crate::openrouter::ModelInfo::from_json(v)?;
    let p = &v["pricing"];
    let price = match (per_million(&p["prompt"]), per_million(&p["completion"])) {
        (Some(input), Some(output)) => Some(Price {
            input,
            output,
            cache_read: per_million(&p["input_cache_read"]),
            cache_write: per_million(&p["input_cache_write"]),
        }),
        _ => None,
    };
    let nonzero = |n: u32| (n > 0).then_some(n);
    Some(LiveModel {
        display_name: v.get("name").and_then(Value::as_str).map(str::to_owned),
        context_window: nonzero(base.context_length),
        max_output_tokens: nonzero(base.max_completion_tokens),
        price,
        tools: Some(base.supports_tools),
        reasoning: Some(base.supports_reasoning),
        vision: Some(base.supports_images),
        id: base.id,
        ..LiveModel::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn openai_filters_non_chat_models() {
        for id in [
            "gpt-5.5",
            "o3",
            "codex-mini-latest",
            "gpt-4o-search-preview",
        ] {
            assert!(openai_is_chat_model(id), "{id}");
        }
        for id in [
            "text-embedding-3-large",
            "whisper-1",
            "tts-1",
            "dall-e-3",
            "gpt-image-1",
            "omni-moderation-latest",
            "gpt-4o-realtime-preview",
            "gpt-4o-transcribe",
        ] {
            assert!(!openai_is_chat_model(id), "{id}");
        }
        assert!(!openai_supports_tools("gpt-4o-search-preview"));
        assert!(!openai_supports_tools("chatgpt-4o-latest"));
        assert!(openai_supports_tools("gpt-5.5"));
    }

    #[test]
    fn openrouter_prices_are_per_million() {
        assert_eq!(per_million(&json!("0.000003")), Some(3.0));
        assert_eq!(per_million(&json!("0.0000001")), Some(0.1));
        assert_eq!(per_million(&json!("-1")), None);
        assert_eq!(per_million(&json!(null)), None);
    }

    #[test]
    fn openrouter_cache_prices_are_read_and_charged() {
        // The catalog entry of an Anthropic model, as OpenRouter lists it.
        let m = openrouter_model(&json!({"id": "anthropic/claude-haiku-5.5",
            "pricing": {"prompt": "0.0000001", "completion": "0.0000005",
                "input_cache_read": "0.00000001", "input_cache_write": "0.000000125"}}))
        .unwrap();
        let price = m.price.unwrap();
        assert_eq!(
            (price.cache_read, price.cache_write),
            (Some(0.01), Some(0.125))
        );
        let usage = crate::model::Usage {
            input_tokens: 1_000_000,
            cache_read_tokens: 1_000_000,
            cache_write_tokens: 1_000_000,
            ..Default::default()
        };
        assert!((price.cost(&usage) - (0.1 + 0.01 + 0.125)).abs() < 1e-9);
    }

    #[test]
    fn debug_never_prints_the_key() {
        let s = ModelListSource::new("openai", Some("sk-secret"), None).unwrap();
        let d = format!("{s:?}");
        assert!(!d.contains("sk-secret"), "{d}");
        assert!(d.contains("<redacted>"));
        assert!(ModelListSource::new("fake", None, None).is_none());
        assert_eq!(
            ModelListSource::new("anthropic", None, Some("http://x/"))
                .unwrap()
                .url(),
            "http://x/v1/models"
        );
    }
}
