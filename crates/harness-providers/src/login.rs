//! Checking an API key before `harness login` saves it.
//!
//! [`verify_api_key`] makes one cheap authenticated request per provider and
//! never generates tokens:
//!
//! | provider | request |
//! |---|---|
//! | anthropic | `GET {base}/v1/models?limit=1` with `x-api-key` |
//! | openai | `GET {base}/models` with a bearer token |
//! | openrouter | `GET {base}/key` with a bearer token (its model list is public, so it would accept any key) |
//!
//! HTTP 401 and 403 come back as [`ProviderError::Auth`]. Error messages
//! are scrubbed of the key in case a server echoes it.

use std::time::Duration;

use reqwest::header::{AUTHORIZATION, HeaderMap, HeaderValue};
use serde_json::Value;

use crate::http::error_message;
use crate::provider::ProviderError;

/// Default time limit for the check.
pub const VERIFY_TIMEOUT: Duration = Duration::from_secs(20);

/// What a successful check learned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyCheck {
    /// Provider id.
    pub provider: String,
    /// The URL that accepted the key.
    pub url: String,
    /// A short, non-secret detail (`1 model visible`).
    pub detail: Option<String>,
}

/// The URL [`verify_api_key`] requests for `provider`, or `None` for an
/// unknown provider. `base_url` defaults to the provider's public endpoint.
pub fn verify_url(provider: &str, base_url: Option<&str>) -> Option<String> {
    let (default, path) = match provider {
        crate::anthropic::NAME => (crate::anthropic::DEFAULT_BASE_URL, "v1/models?limit=1"),
        crate::openai::PROVIDER_NAME => (crate::openai::DEFAULT_BASE_URL, "models"),
        crate::openrouter::PROVIDER_NAME => (crate::openrouter::DEFAULT_BASE_URL, "key"),
        _ => return None,
    };
    let base = base_url.filter(|b| !b.trim().is_empty()).unwrap_or(default);
    Some(format!("{}/{path}", base.trim_end_matches('/')))
}

fn headers(provider: &str, key: &str) -> Result<HeaderMap, ProviderError> {
    let bad = || ProviderError::InvalidRequest("the API key contains invalid characters".into());
    let mut h = HeaderMap::new();
    if provider == crate::anthropic::NAME {
        let mut v = HeaderValue::from_str(key).map_err(|_| bad())?;
        v.set_sensitive(true);
        h.insert("x-api-key", v);
        h.insert(
            "anthropic-version",
            HeaderValue::from_static(crate::anthropic::API_VERSION),
        );
    } else {
        let mut v = HeaderValue::from_str(&format!("Bearer {key}")).map_err(|_| bad())?;
        v.set_sensitive(true);
        h.insert(AUTHORIZATION, v);
    }
    Ok(h)
}

fn scrub(message: String, key: &str) -> String {
    if key.len() >= 4 && message.contains(key) {
        message.replace(key, "[redacted]")
    } else {
        message
    }
}

/// A short detail about the check. OpenRouter's `/key` answer is not used:
/// its `label` is a partial copy of the key.
fn detail(provider: &str, body: &Value) -> Option<String> {
    if provider == crate::openrouter::PROVIDER_NAME {
        return None;
    }
    body.get("data").and_then(Value::as_array).map(|d| {
        let n = d.len();
        format!("{n} model{} visible", if n == 1 { "" } else { "s" })
    })
}

/// A reusable HTTP client for [`verify_api_key`], so callers need not
/// depend on `reqwest` themselves.
#[derive(Debug, Clone, Default)]
pub struct KeyVerifier {
    client: reqwest::Client,
}

impl KeyVerifier {
    /// A verifier with default transport settings.
    pub fn new() -> Self {
        Self::default()
    }

    /// [`verify_api_key`] with this client.
    pub async fn verify(
        &self,
        provider: &str,
        key: &str,
        base_url: Option<&str>,
        timeout: Duration,
    ) -> Result<KeyCheck, ProviderError> {
        verify_api_key(&self.client, provider, key, base_url, timeout).await
    }
}

/// Checks `key` against `provider` with one cheap authenticated request.
///
/// `base_url` is the provider's `base_url` setting, if any. Returns
/// [`ProviderError::Auth`] for a rejected key and other variants for
/// network or server problems.
pub async fn verify_api_key(
    client: &reqwest::Client,
    provider: &str,
    key: &str,
    base_url: Option<&str>,
    timeout: Duration,
) -> Result<KeyCheck, ProviderError> {
    let url = verify_url(provider, base_url)
        .ok_or_else(|| ProviderError::InvalidRequest(format!("unknown provider `{provider}`")))?;
    let key = key.trim();
    if key.is_empty() {
        return Err(ProviderError::InvalidRequest("the API key is empty".into()));
    }
    tracing::debug!(provider, url, "verifying API key");
    let response = client
        .get(&url)
        .headers(headers(provider, key)?)
        .timeout(timeout)
        .send()
        .await
        .map_err(|e| {
            if e.is_timeout() {
                ProviderError::Timeout(timeout)
            } else {
                ProviderError::Connection(scrub(e.to_string(), key))
            }
        })?;
    let status = response.status().as_u16();
    let text = response
        .text()
        .await
        .map_err(|e| ProviderError::Connection(scrub(e.to_string(), key)))?;
    if !(200..300).contains(&status) {
        return Err(ProviderError::from_status(
            status,
            scrub(error_message(&text), key),
            None,
        ));
    }
    let body: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
    Ok(KeyCheck {
        provider: provider.to_owned(),
        detail: detail(provider, &body).map(|d| scrub(d, key)),
        url,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_per_provider() {
        assert_eq!(
            verify_url("anthropic", None).unwrap(),
            "https://api.anthropic.com/v1/models?limit=1"
        );
        assert_eq!(
            verify_url("openai", Some("http://h/v1/")).unwrap(),
            "http://h/v1/models"
        );
        assert_eq!(
            verify_url("openrouter", None).unwrap(),
            "https://openrouter.ai/api/v1/key"
        );
        assert!(verify_url("fake", None).is_none());
    }

    #[test]
    fn scrubs_echoed_keys() {
        assert_eq!(
            scrub("bad key sk-12345".into(), "sk-12345"),
            "bad key [redacted]"
        );
    }
}
