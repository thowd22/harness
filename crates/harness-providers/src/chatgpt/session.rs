//! Access tokens for requests: loaded from the token store and refreshed
//! before they expire.
//!
//! One [`ChatGptAuth`] per token store is shared by the whole process
//! ([`ChatGptAuth::shared`]), and refreshes are serialized, so concurrent
//! agents never spend the same rotating refresh token twice. Before a
//! refresh the store is re-read, so tokens another harness process just
//! refreshed are picked up instead of refreshed again.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use tokio::sync::Mutex as AsyncMutex;

use super::oauth::{Refreshed, http_client, refresh};
use super::store::{Account, StoredTokens, Token, TokenStore};
use super::{ChatGptError, Endpoints, REFRESH_SKEW, unix_now};

/// A clock returning Unix seconds.
pub type Clock = Arc<dyn Fn() -> u64 + Send + Sync>;

/// The signed-in state, for status displays.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthStatus {
    /// Tokens are stored.
    SignedIn {
        /// The account.
        account: Account,
        /// Access token expiry (Unix seconds).
        expires_at: u64,
        /// Plan usage granted.
        plan_usage: bool,
    },
    /// No tokens.
    SignedOut,
}

/// Hands out ChatGPT access tokens, refreshing them when needed.
pub struct ChatGptAuth {
    store: TokenStore,
    endpoints: Endpoints,
    http: reqwest::Client,
    skew: Duration,
    clock: Clock,
    tokens: AsyncMutex<Option<StoredTokens>>,
}

impl std::fmt::Debug for ChatGptAuth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChatGptAuth")
            .field("store", &self.store)
            .field("endpoints", &self.endpoints)
            .finish_non_exhaustive()
    }
}

type Shared = Mutex<HashMap<PathBuf, Arc<ChatGptAuth>>>;

fn shared_map() -> &'static Shared {
    static MAP: OnceLock<Shared> = OnceLock::new();
    MAP.get_or_init(Default::default)
}

/// Drops the shared instance for `dir` so the next [`ChatGptAuth::shared`]
/// re-reads the store (after a sign-in or logout).
pub(crate) fn forget_cached(dir: &Path) {
    shared_map()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(dir);
}

impl ChatGptAuth {
    /// A new instance for `store`. Prefer [`ChatGptAuth::shared`].
    pub fn new(store: TokenStore, endpoints: Endpoints) -> Self {
        Self {
            store,
            endpoints,
            http: http_client(),
            skew: REFRESH_SKEW,
            clock: Arc::new(unix_now),
            tokens: AsyncMutex::new(None),
        }
    }

    /// The process-wide instance for `store`'s directory.
    pub fn shared(store: TokenStore, endpoints: Endpoints) -> Arc<Self> {
        let mut map = shared_map().lock().unwrap_or_else(|e| e.into_inner());
        map.entry(store.dir().to_path_buf())
            .and_modify(|a| {
                if a.endpoints != endpoints {
                    *a = Arc::new(Self::new(store.clone(), endpoints.clone()));
                }
            })
            .or_insert_with(|| Arc::new(Self::new(store.clone(), endpoints.clone())))
            .clone()
    }

    /// Uses `clock` instead of the system clock (tests).
    pub fn with_clock(mut self, clock: Clock) -> Self {
        self.clock = clock;
        self
    }

    /// Refreshes this long before expiry (default [`REFRESH_SKEW`]).
    pub fn with_skew(mut self, skew: Duration) -> Self {
        self.skew = skew;
        self
    }

    /// The token store.
    pub fn store(&self) -> &TokenStore {
        &self.store
    }

    /// The endpoints.
    pub fn endpoints(&self) -> &Endpoints {
        &self.endpoints
    }

    /// The stored state, read from disk.
    pub fn status(&self) -> Result<AuthStatus, ChatGptError> {
        Ok(match self.store.load()?.tokens {
            Some(t) => AuthStatus::SignedIn {
                plan_usage: t.plan_usage(),
                account: t.account,
                expires_at: t.expires_at,
            },
            None => AuthStatus::SignedOut,
        })
    }

    fn fresh(&self, t: &StoredTokens, now: u64) -> bool {
        t.expires_at > now + self.skew.as_secs()
    }

    /// A valid access token, refreshed first when it expires within the
    /// skew. A dead refresh token clears the stored tokens and returns
    /// [`ChatGptError::ReauthRequired`]; network failures leave them alone.
    pub async fn access_token(&self) -> Result<Token, ChatGptError> {
        let mut guard = self.tokens.lock().await;
        let now = (self.clock)();
        if let Some(t) = guard.as_ref().filter(|t| self.fresh(t, now)) {
            return Ok(t.access_token.clone());
        }
        // Not loaded, or about to expire: the store may hold newer tokens
        // (another process refreshed, or the user signed in again).
        let record = self.store.load()?;
        let Some(stored) = record.tokens else {
            *guard = None;
            return Err(ChatGptError::NotSignedIn);
        };
        if !stored.plan_usage() {
            return Err(ChatGptError::PlanUsageNotGranted);
        }
        if self.fresh(&stored, now) {
            let token = stored.access_token.clone();
            *guard = Some(stored);
            return Ok(token);
        }
        let client_id = record.client_id.ok_or(ChatGptError::NotSignedIn)?;
        match refresh(&self.http, &self.endpoints, &client_id, &stored, now).await? {
            Refreshed::Tokens(new) => {
                self.store.save(&super::store::AuthRecord {
                    client_id: Some(client_id),
                    tokens: Some(new.clone()),
                })?;
                tracing::info!("refreshed the ChatGPT access token");
                let token = new.access_token.clone();
                *guard = Some(new);
                Ok(token)
            }
            Refreshed::Terminal(code) => {
                self.store.clear_tokens()?;
                *guard = None;
                tracing::warn!(code = %code, "ChatGPT refresh token rejected; stored tokens cleared");
                Err(ChatGptError::ReauthRequired(code))
            }
            Refreshed::NoPlanScope => {
                self.store.clear_tokens()?;
                *guard = None;
                Err(ChatGptError::PlanUsageNotGranted)
            }
        }
    }

    /// Deletes the stored tokens (keeping the registration). Returns true
    /// when tokens were stored.
    pub async fn sign_out(&self) -> Result<bool, ChatGptError> {
        let mut guard = self.tokens.lock().await;
        *guard = None;
        self.store.clear_tokens()
    }
}
