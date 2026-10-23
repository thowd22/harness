//! Access tokens for MCP requests: loaded from the token store and
//! refreshed before they expire or after the server rejects them.
//!
//! One [`OAuthSession`] per (store, server) is shared by the whole process
//! ([`OAuthSession::shared`]) and refreshes are serialized, so concurrent
//! agents never spend the same rotating refresh token twice. The store is
//! re-read before each refresh, so tokens that another harness process
//! just refreshed (or a new `harness login mcp`) are picked up instead.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use serde::Deserialize;
use serde_json::Value;
use tokio::sync::Mutex as AsyncMutex;

use super::store::{McpAuthRecord, McpTokenStore, McpTokens};
use super::{
    AuthError, REFRESH_SKEW, TERMINAL_REFRESH_CODES, Token, canonical_resource, form_encode,
    http_client, scopes_of, secure_endpoint, union_into, unix_now,
};

/// A clock returning Unix seconds.
pub type Clock = Arc<dyn Fn() -> u64 + Send + Sync>;

/// Hands out access tokens for one server, refreshing them when needed.
pub struct OAuthSession {
    server: String,
    /// The configured URL.
    url: String,
    resource: String,
    store: McpTokenStore,
    http: reqwest::Client,
    skew: Duration,
    clock: Clock,
    tokens: AsyncMutex<Option<McpTokens>>,
}

impl std::fmt::Debug for OAuthSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OAuthSession")
            .field("server", &self.server)
            .field("resource", &self.resource)
            .finish_non_exhaustive()
    }
}

type Shared = Mutex<HashMap<(PathBuf, String), Arc<OAuthSession>>>;

fn shared_map() -> &'static Shared {
    static MAP: OnceLock<Shared> = OnceLock::new();
    MAP.get_or_init(Default::default)
}

/// Drops the shared session for `server` in `dir`, so the next
/// [`OAuthSession::shared`] re-reads the store (after a login or logout).
pub(crate) fn forget_cached(dir: &Path, server: &str) {
    shared_map()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(&(dir.to_path_buf(), server.to_owned()));
}

/// The token endpoint's answer.
#[derive(Deserialize)]
pub(crate) struct TokenResponse {
    pub access_token: String,
    #[serde(default)]
    pub token_type: Option<String>,
    #[serde(default)]
    pub refresh_token: Option<String>,
    #[serde(default)]
    pub expires_in: Option<u64>,
    #[serde(default)]
    pub scope: Option<String>,
}

/// A failed token endpoint call.
pub(crate) enum TokenFailure {
    /// An OAuth error body.
    OAuth { code: String, message: String },
    /// Network, 5xx or an unreadable answer.
    Network(String),
}

/// POSTs `form` to the token endpoint.
pub(crate) async fn token_call(
    http: &reqwest::Client,
    url: &str,
    form: &[(&str, &str)],
) -> Result<TokenResponse, TokenFailure> {
    let resp = http
        .post(url)
        .header("content-type", "application/x-www-form-urlencoded")
        .header("accept", "application/json")
        .body(form_encode(form))
        .send()
        .await
        .map_err(|e| TokenFailure::Network(e.without_url().to_string()))?;
    let status = resp.status();
    let text = zeroize::Zeroizing::new(
        resp.text()
            .await
            .map_err(|e| TokenFailure::Network(e.without_url().to_string()))?,
    );
    if status.is_success() {
        let t: TokenResponse = serde_json::from_str(&text).map_err(|_| {
            TokenFailure::Network("the token endpoint sent an unreadable answer".into())
        })?;
        if let Some(tt) = &t.token_type
            && !tt.eq_ignore_ascii_case("bearer")
        {
            return Err(TokenFailure::OAuth {
                code: "unsupported_token_type".into(),
                message: format!("harness supports Bearer tokens, not {tt}"),
            });
        }
        return Ok(t);
    }
    let v: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
    match v["error"].as_str() {
        Some(code) if !status.is_server_error() => Err(TokenFailure::OAuth {
            code: code.to_owned(),
            message: v["error_description"]
                .as_str()
                .unwrap_or("")
                .chars()
                .take(200)
                .collect(),
        }),
        _ => Err(TokenFailure::Network(format!(
            "token endpoint answered HTTP {}",
            status.as_u16()
        ))),
    }
}

/// Tokens from a token response, keeping `old`'s refresh token and scopes
/// when the response leaves them out.
pub(crate) fn tokens_from(
    t: TokenResponse,
    old: Option<&McpTokens>,
    requested: &[String],
    now: u64,
) -> McpTokens {
    McpTokens {
        access_token: Token::new(t.access_token),
        refresh_token: t
            .refresh_token
            .map(Token::new)
            .or_else(|| old.and_then(|o| o.refresh_token.clone())),
        expires_at: t.expires_in.map(|s| now + s),
        scopes: match t.scope.as_deref() {
            Some(s) => scopes_of(s),
            None => old.map_or_else(|| requested.to_vec(), |o| o.scopes.clone()),
        },
    }
}

impl OAuthSession {
    /// A new session for `server` at `url`. Prefer [`OAuthSession::shared`].
    pub fn new(store: McpTokenStore, server: &str, url: &str) -> Self {
        Self {
            server: server.to_owned(),
            url: url.to_owned(),
            resource: canonical_resource(url).unwrap_or_else(|| url.to_owned()),
            store,
            http: http_client(),
            skew: REFRESH_SKEW,
            clock: Arc::new(unix_now),
            tokens: AsyncMutex::new(None),
        }
    }

    /// The process-wide session for `server` in `store`.
    pub fn shared(store: McpTokenStore, server: &str, url: &str) -> Arc<Self> {
        let resource = canonical_resource(url).unwrap_or_else(|| url.to_owned());
        let mut map = shared_map().lock().unwrap_or_else(|e| e.into_inner());
        map.entry((store.dir().to_path_buf(), server.to_owned()))
            .and_modify(|s| {
                if s.resource != resource {
                    *s = Arc::new(Self::new(store.clone(), server, url));
                }
            })
            .or_insert_with(|| Arc::new(Self::new(store.clone(), server, url)))
            .clone()
    }

    /// Uses `clock` instead of the system clock (tests).
    pub fn with_clock(mut self, clock: Clock) -> Self {
        self.clock = clock;
        self
    }

    /// The server name.
    pub fn server(&self) -> &str {
        &self.server
    }

    /// The token store.
    pub fn store(&self) -> &McpTokenStore {
        &self.store
    }

    /// Refuses a URL that would carry bearer tokens in cleartext.
    fn check_secure(&self) -> Result<(), AuthError> {
        if secure_endpoint(&self.url) {
            Ok(())
        } else {
            Err(AuthError::InsecureUrl {
                server: self.server.clone(),
                url: self.url.clone(),
            })
        }
    }

    fn fresh(&self, t: &McpTokens, now: u64) -> bool {
        t.expires_at.is_none_or(|e| e > now + self.skew.as_secs())
    }

    fn not_authorized(&self, reason: impl Into<String>) -> AuthError {
        AuthError::NotAuthorized {
            server: self.server.clone(),
            reason: reason.into(),
        }
    }

    /// The stored record, if it is for this server's URL. Fails first
    /// when the URL would carry the token without TLS.
    fn record(&self) -> Result<McpAuthRecord, AuthError> {
        self.check_secure()?;
        let Some(r) = self.store.load(&self.server)? else {
            return Err(self.not_authorized("not authorized yet"));
        };
        if r.resource != self.resource {
            return Err(self.not_authorized(format!(
                "the stored authorization is for {}, but url is {}",
                r.resource, self.resource
            )));
        }
        if r.tokens.is_none() {
            return Err(self.not_authorized("no tokens are stored"));
        }
        Ok(r)
    }

    /// A valid access token, refreshed first when it expires within the
    /// skew. Never opens a browser: without tokens it fails with
    /// [`AuthError::NotAuthorized`].
    pub async fn access_token(&self) -> Result<Token, AuthError> {
        self.check_secure()?;
        let mut guard = self.tokens.lock().await;
        let now = (self.clock)();
        if let Some(t) = guard.as_ref().filter(|t| self.fresh(t, now)) {
            return Ok(t.access_token.clone());
        }
        let record = match self.record() {
            Ok(r) => r,
            Err(e) => {
                *guard = None;
                return Err(e);
            }
        };
        let stored = record.tokens.clone().expect("checked by record()");
        if self.fresh(&stored, now) {
            let token = stored.access_token.clone();
            *guard = Some(stored);
            return Ok(token);
        }
        self.refresh(&mut guard, record, now).await
    }

    /// Called after the server answered `401` to `rejected`: returns newer
    /// stored tokens if another session already refreshed, else refreshes.
    pub async fn refresh_after_rejection(&self, rejected: &Token) -> Result<Token, AuthError> {
        let mut guard = self.tokens.lock().await;
        let now = (self.clock)();
        let record = match self.record() {
            Ok(r) => r,
            Err(e) => {
                *guard = None;
                return Err(e);
            }
        };
        let stored = record.tokens.clone().expect("checked by record()");
        if &stored.access_token != rejected {
            let token = stored.access_token.clone();
            *guard = Some(stored);
            return Ok(token);
        }
        self.refresh(&mut guard, record, now).await
    }

    async fn refresh(
        &self,
        guard: &mut Option<McpTokens>,
        mut record: McpAuthRecord,
        now: u64,
    ) -> Result<Token, AuthError> {
        let old = record.tokens.clone().expect("checked by record()");
        let Some(rt) = old.refresh_token.clone() else {
            self.store.clear_tokens(&self.server)?;
            *guard = None;
            return Err(AuthError::ReauthRequired {
                server: self.server.clone(),
                reason: "the access token expired and the server issued no refresh token".into(),
            });
        };
        let client_id = record.client.client_id.clone();
        let mut form = vec![
            ("grant_type", "refresh_token"),
            ("refresh_token", rt.expose()),
            ("client_id", client_id.as_str()),
            ("resource", record.resource_indicator.as_str()),
        ];
        let secret = record.client.client_secret.clone();
        if let Some(s) = &secret {
            form.push(("client_secret", s.expose()));
        }
        match token_call(&self.http, &record.token_endpoint, &form).await {
            Ok(t) => {
                let new = tokens_from(t, Some(&old), &record.scopes, now);
                record.tokens = Some(new.clone());
                self.store.save(&self.server, &record)?;
                tracing::info!(server = %self.server, "refreshed the MCP OAuth access token");
                let token = new.access_token.clone();
                *guard = Some(new);
                Ok(token)
            }
            Err(TokenFailure::OAuth { code, .. })
                if TERMINAL_REFRESH_CODES.contains(&code.as_str()) =>
            {
                self.store.clear_tokens(&self.server)?;
                *guard = None;
                tracing::warn!(server = %self.server, code = %code, "MCP refresh token rejected; stored tokens cleared");
                Err(AuthError::ReauthRequired {
                    server: self.server.clone(),
                    reason: code,
                })
            }
            Err(TokenFailure::OAuth { code, message }) => Err(AuthError::OAuth { code, message }),
            Err(TokenFailure::Network(m)) => Err(AuthError::Network(m)),
        }
    }

    /// Remembers scopes the server asked for (`insufficient_scope`), so
    /// the next `harness login mcp` requests them too.
    pub fn record_step_up(&self, scope: &str) {
        if let Ok(Some(mut r)) = self.store.load(&self.server) {
            let before = r.step_up_scopes.len();
            union_into(&mut r.step_up_scopes, &scopes_of(scope));
            if r.step_up_scopes.len() != before {
                let _ = self.store.save(&self.server, &r);
            }
        }
    }
}
