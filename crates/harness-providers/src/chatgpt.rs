//! Sign in with ChatGPT: use a ChatGPT Plus or Pro plan with the OpenAI
//! provider instead of a pay-per-token API key.
//!
//! The flow follows OpenAI's guide for open-source and locally hosted apps
//! (<https://developers.openai.com/siwc/token-sharing-open-source>):
//!
//! 1. A per-installation `ext_agent_host_id` (`urn:uuid:…`) is created once
//!    and kept in `chatgpt-host.json`.
//! 2. [`oauth::SignIn`] listens on `127.0.0.1:1455` (another port on later
//!    sign-ins), opens the system browser at the authorization endpoint
//!    with a fresh `state`, `nonce` and PKCE (`S256`) challenge, and asks
//!    for [`SCOPES`]: identity, `offline_access` (a refresh token) and
//!    `resource.invoke` with [`PLAN_SCOPE`].
//! 3. On the first sign-in OpenAI registers the client
//!    ([`DYNAMIC_CLIENT_ID`]) and returns the issued `client_id` on the
//!    callback. It is saved before the code is exchanged and reused from
//!    then on; there is no client secret.
//! 4. The code is exchanged at the token endpoint, the ID token's
//!    signature (RS256, against the issuer's JWKS), issuer, audience,
//!    expiry and nonce are verified ([`jwt`]), and the granted scopes must
//!    include [`PLAN_SCOPE`]: identity alone does not allow plan usage.
//! 5. Tokens go to the protected token store ([`store::TokenStore`],
//!    `<data_dir>/auth/chatgpt-auth.json`, mode `0600`). Agent tools can
//!    never read the data directory, and the token store is never synced to
//!    remote hosts.
//! 6. [`session::ChatGptAuth`] hands out access tokens to the OpenAI
//!    provider and refreshes them (rotating the refresh token) shortly
//!    before they expire. A refresh token that is expired or revoked clears
//!    the stored tokens and asks the user to sign in again.
//!
//! Requests in ChatGPT mode go to the Responses API with `store: false`
//! and streaming, without the fields the plan route rejects
//! ([`UNSUPPORTED_REQUEST_FIELDS`]). Hitting the plan's usage limit is a
//! [`ProviderError::UsageLimit`] whose action is *Manage usage*
//! ([`MANAGE_USAGE_URL`]); harness never falls back to an API key on its
//! own.

pub mod jwt;
pub mod mock;
pub mod oauth;
pub mod pkce;
pub mod session;
pub mod store;

use std::time::Duration;

use crate::provider::ProviderError;

pub use oauth::{Browser, SignIn, SystemBrowser};
pub use session::ChatGptAuth;
pub use store::{Account, TokenStore};

/// The authorization server.
pub const ISSUER: &str = "https://auth.openai.com";

/// Authorization endpoint path under [`ISSUER`].
pub const AUTHORIZE_PATH: &str = "api/accounts/authorize";

/// Token endpoint path under [`ISSUER`].
pub const TOKEN_PATH: &str = "api/accounts/oauth/token";

/// JWKS path under [`ISSUER`] (the keys that sign ID tokens).
pub const JWKS_PATH: &str = ".well-known/jwks.json";

/// The protected resource the tokens are for.
pub const RESOURCE: &str = "https://api.openai.com/v1";

/// The scope that allows inference on the user's ChatGPT plan.
pub const PLAN_SCOPE: &str = "chatgpt.tokens.use.direct";

/// Every scope requested at sign-in.
pub const SCOPES: &str =
    "openid profile email offline_access resource.invoke chatgpt.tokens.use.direct";

/// The `client_id` sent on the first sign-in, before OpenAI has issued one.
/// It is never stored.
pub const DYNAMIC_CLIENT_ID: &str = "dynamic_agent_client";

/// The app name shown on the consent page (`agent_name_hint`).
pub const AGENT_NAME: &str = "harness";

/// The loopback port of the first registration. Later sign-ins prefer it
/// and fall back to any free port.
pub const DEFAULT_CALLBACK_PORT: u16 = 1455;

/// The redirect path on the loopback listener.
pub const CALLBACK_PATH: &str = "/auth/callback";

/// Where the user manages plan usage.
pub const MANAGE_USAGE_URL: &str = "https://chatgpt.com/settings/usage";

/// The error code for a reached plan usage limit (HTTP 429 or a
/// `response.failed` event).
pub const USAGE_LIMIT_CODE: &str = "subscription_sharing_usage_limit_exceeded";

/// Refresh this long before the access token expires.
pub const REFRESH_SKEW: Duration = Duration::from_secs(300);

/// How long [`oauth::SignIn`] waits for the browser to come back.
pub const SIGN_IN_TIMEOUT: Duration = Duration::from_secs(300);

/// Responses API request fields the ChatGPT plan route rejects; they are
/// removed from request bodies in ChatGPT mode.
pub const UNSUPPORTED_REQUEST_FIELDS: &[&str] = &[
    "background",
    "conversation",
    "max_output_tokens",
    "max_tool_calls",
    "metadata",
    "moderation",
    "previous_response_id",
    "prompt",
    "prompt_cache_retention",
    "safety_identifier",
    "temperature",
    "top_logprobs",
    "top_p",
    "truncation",
    "user",
];

/// Refresh error codes that mean the refresh token can never work again.
pub const TERMINAL_REFRESH_CODES: &[&str] = &[
    "invalid_grant",
    "invalid_refresh_token",
    "token_expired",
    "refresh_token_expired",
    "refresh_token_invalidated",
    "refresh_token_reused",
];

/// The command that signs in again, for error messages.
pub const SIGN_IN_COMMAND: &str = "harness login openai";

/// [`ProviderError::UsageLimit`] when `code` is [`USAGE_LIMIT_CODE`].
pub fn usage_limit_error(code: &str, message: &str) -> Option<ProviderError> {
    (code == USAGE_LIMIT_CODE).then(|| ProviderError::UsageLimit {
        message: if message.trim().is_empty() {
            "the ChatGPT plan usage limit for harness was reached".into()
        } else {
            message.to_owned()
        },
        manage_url: MANAGE_USAGE_URL.into(),
    })
}

/// Where the authorization server and the API live. [`Endpoints::openai`]
/// is the real service; tests point [`Endpoints::local`] at a mock.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Endpoints {
    /// Expected `iss` of ID tokens.
    pub issuer: String,
    /// Authorization endpoint.
    pub authorize_url: String,
    /// Token endpoint.
    pub token_url: String,
    /// JWKS with the ID-token signing keys.
    pub jwks_url: String,
    /// The `resource` parameter.
    pub resource: String,
    /// Base URL of the Responses API.
    pub api_base_url: String,
}

impl Endpoints {
    /// OpenAI's production endpoints.
    pub fn openai() -> Self {
        Self::with_issuer(ISSUER, RESOURCE)
    }

    /// Endpoints under `issuer`, with the API at `api_base_url`.
    pub fn with_issuer(issuer: &str, api_base_url: &str) -> Self {
        let base = issuer.trim_end_matches('/');
        Self {
            issuer: base.to_owned(),
            authorize_url: format!("{base}/{AUTHORIZE_PATH}"),
            token_url: format!("{base}/{TOKEN_PATH}"),
            jwks_url: format!("{base}/{JWKS_PATH}"),
            resource: RESOURCE.to_owned(),
            api_base_url: api_base_url.trim_end_matches('/').to_owned(),
        }
    }

    /// A local server that is both the issuer and the API (`{base}/v1`).
    pub fn local(base: &str) -> Self {
        let base = base.trim_end_matches('/');
        Self::with_issuer(base, &format!("{base}/v1"))
    }
}

/// Why signing in, refreshing or reading the token store failed. Messages
/// are short, say what to do next, and never contain tokens.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ChatGptError {
    /// No tokens are stored.
    #[error("not signed in with ChatGPT; run `{SIGN_IN_COMMAND}` and choose Sign in with ChatGPT")]
    NotSignedIn,
    /// The user declined on the consent page.
    #[error("ChatGPT sign-in was declined in the browser; nothing was saved")]
    Denied,
    /// Signed in, but plan usage (`chatgpt.tokens.use.direct`) was not
    /// granted.
    #[error(
        "signed in, but ChatGPT plan usage was not granted ({PLAN_SCOPE} is missing), so harness cannot use your plan. Run `{SIGN_IN_COMMAND}` again and allow plan usage, or use an OpenAI API key instead. No tokens were saved"
    )]
    PlanUsageNotGranted,
    /// The browser did not come back in time.
    #[error("no answer from the browser within {}s; run `{SIGN_IN_COMMAND}` to try again", .0.as_secs())]
    Timeout(Duration),
    /// The loopback port is taken.
    #[error(
        "cannot listen on 127.0.0.1:{0} for the sign-in callback ({1}); close the program using it and try again"
    )]
    Port(u16, String),
    /// The browser could not be opened.
    #[error("could not open a browser ({0}); open this URL yourself: {1}")]
    Browser(String, String),
    /// The authorization server returned an OAuth error.
    #[error("ChatGPT sign-in failed: {code}{}", if .message.is_empty() { String::new() } else { format!(" ({})", .message) })]
    OAuth {
        /// OAuth `error` code.
        code: String,
        /// `error_description`, if any.
        message: String,
    },
    /// The first sign-in did not return an issued `client_id`.
    #[error(
        "OpenAI did not issue a client id for harness during sign-in; registration is incomplete, try again"
    )]
    Registration,
    /// The ID token failed verification.
    #[error("the ChatGPT ID token failed verification: {0}; nothing was saved")]
    IdToken(String),
    /// The refresh token is expired or revoked; stored tokens were
    /// cleared.
    #[error(
        "the ChatGPT sign-in expired or was revoked ({0}); run `{SIGN_IN_COMMAND}` to sign in again"
    )]
    ReauthRequired(String),
    /// A network or server problem; nothing was changed.
    #[error("could not reach the ChatGPT sign-in service: {0}")]
    Network(String),
    /// Reading or writing the token store failed.
    #[error("ChatGPT token store: {0}")]
    Store(String),
}

impl From<ChatGptError> for ProviderError {
    fn from(e: ChatGptError) -> Self {
        match e {
            ChatGptError::Network(m) => ProviderError::Connection(m),
            ChatGptError::NotSignedIn
            | ChatGptError::ReauthRequired(_)
            | ChatGptError::PlanUsageNotGranted => ProviderError::Auth(e.to_string()),
            other => ProviderError::Other(other.to_string()),
        }
    }
}

/// Seconds since the Unix epoch.
pub(crate) fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// Percent-encodes `s` for a query string or form body (RFC 3986
/// unreserved characters stay as they are).
pub(crate) fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// `application/x-www-form-urlencoded` body (also a query string).
pub(crate) fn form_encode(pairs: &[(&str, &str)]) -> String {
    pairs
        .iter()
        .map(|(k, v)| format!("{}={}", percent_encode(k), percent_encode(v)))
        .collect::<Vec<_>>()
        .join("&")
}

/// Decodes `%XX` escapes and `+` (space).
pub(crate) fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < b.len() => {
                let hex = |c: u8| (c as char).to_digit(16);
                match (hex(b[i + 1]), hex(b[i + 2])) {
                    (Some(h), Some(l)) => {
                        out.push((h * 16 + l) as u8);
                        i += 2;
                    }
                    _ => out.push(b'%'),
                }
            }
            c => out.push(c),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Parses `a=1&b=2` into pairs, decoding each part.
pub(crate) fn parse_query(q: &str) -> Vec<(String, String)> {
    q.split('&')
        .filter(|p| !p.is_empty())
        .map(|p| {
            let (k, v) = p.split_once('=').unwrap_or((p, ""));
            (percent_decode(k), percent_decode(v))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn form_encoding_round_trips() {
        let enc = form_encode(&[
            ("scope", SCOPES),
            ("redirect_uri", "http://127.0.0.1:1455/auth/callback"),
        ]);
        assert!(enc.starts_with("scope=openid%20profile"), "{enc}");
        assert!(enc.contains("redirect_uri=http%3A%2F%2F127.0.0.1%3A1455%2Fauth%2Fcallback"));
        let back = parse_query(&enc);
        assert_eq!(back[0], ("scope".into(), SCOPES.into()));
        assert_eq!(back[1].1, "http://127.0.0.1:1455/auth/callback");
        assert_eq!(percent_decode("a+b%2"), "a b%2");
    }

    #[test]
    fn endpoints_and_usage_limit() {
        let e = Endpoints::openai();
        assert_eq!(
            e.authorize_url,
            "https://auth.openai.com/api/accounts/authorize"
        );
        assert_eq!(
            e.token_url,
            "https://auth.openai.com/api/accounts/oauth/token"
        );
        assert_eq!(e.api_base_url, "https://api.openai.com/v1");
        let l = Endpoints::local("http://127.0.0.1:9/");
        assert_eq!(l.api_base_url, "http://127.0.0.1:9/v1");
        assert!(usage_limit_error("rate_limit_exceeded", "x").is_none());
        let e = usage_limit_error(USAGE_LIMIT_CODE, "").unwrap();
        assert!(
            e.to_string()
                .contains("Manage usage at https://chatgpt.com/settings/usage")
        );
        let p: ProviderError = ChatGptError::NotSignedIn.into();
        assert!(matches!(p, ProviderError::Auth(m) if m.contains("harness login openai")));
    }
}
