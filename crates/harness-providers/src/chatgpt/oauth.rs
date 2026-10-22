//! The browser sign-in (authorization code with PKCE and a loopback
//! redirect) and the token endpoint calls (code exchange and refresh).

use std::time::Duration;

use serde::Deserialize;
use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use super::jwt::{Expected, Jwks, verify_id_token};
use super::pkce::{Pkce, random_token};
use super::store::{Account, AuthRecord, StoredTokens, Token, TokenStore};
use super::{
    AGENT_NAME, CALLBACK_PATH, ChatGptError, DEFAULT_CALLBACK_PORT, DYNAMIC_CLIENT_ID, Endpoints,
    PLAN_SCOPE, SCOPES, SIGN_IN_TIMEOUT, TERMINAL_REFRESH_CODES, form_encode, parse_query,
    unix_now,
};

/// Opens the authorization URL for the user.
pub trait Browser: Send + Sync {
    /// Shows `url` to the user, usually in the system browser. An error
    /// stops the sign-in and is shown with the URL.
    fn open(&self, url: &str) -> Result<(), String>;
}

/// The system browser: `xdg-open` on Linux, `open` on macOS, `start` on
/// Windows. `$BROWSER` wins when set.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemBrowser;

impl Browser for SystemBrowser {
    fn open(&self, url: &str) -> Result<(), String> {
        let (program, args): (String, Vec<String>) = match std::env::var("BROWSER") {
            Ok(b) if !b.trim().is_empty() => (b, vec![url.to_owned()]),
            _ if cfg!(target_os = "macos") => ("open".into(), vec![url.to_owned()]),
            _ if cfg!(windows) => (
                "cmd".into(),
                vec!["/C".into(), "start".into(), String::new(), url.to_owned()],
            ),
            _ => ("xdg-open".into(), vec![url.to_owned()]),
        };
        std::process::Command::new(&program)
            .args(&args)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map(|_| ())
            .map_err(|e| format!("{program}: {e}"))
    }
}

/// The token endpoint's answer.
#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    id_token: Option<String>,
    #[serde(default)]
    expires_in: Option<u64>,
    #[serde(default)]
    scope: Option<String>,
}

/// A failed token endpoint call.
enum TokenFailure {
    /// An OAuth error body (`error`, `error_description`).
    OAuth { code: String, message: String },
    /// Network, 5xx or an unreadable answer.
    Network(String),
}

/// A client for OAuth calls: no redirects, 30s timeout.
pub(crate) fn http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .expect("HTTP client")
}

async fn token_call(
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
    let text = resp
        .text()
        .await
        .map_err(|e| TokenFailure::Network(e.without_url().to_string()))?;
    if status.is_success() {
        return serde_json::from_str(&text).map_err(|_| {
            TokenFailure::Network("the token endpoint sent an unreadable answer".into())
        });
    }
    let v: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
    let code = v["error"]
        .as_str()
        .or_else(|| v.pointer("/error/code").and_then(Value::as_str))
        .map(str::to_owned);
    match code {
        Some(code) if !status.is_server_error() => Err(TokenFailure::OAuth {
            code,
            message: v["error_description"]
                .as_str()
                .or_else(|| v.pointer("/error/message").and_then(Value::as_str))
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

fn scopes_of(scope: Option<&str>) -> Vec<String> {
    scope
        .unwrap_or("")
        .split_whitespace()
        .map(str::to_owned)
        .collect()
}

/// The outcome of [`refresh`].
pub(crate) enum Refreshed {
    /// New tokens.
    Tokens(StoredTokens),
    /// The refresh token is dead (expired, revoked or reused).
    Terminal(String),
    /// Plan usage is no longer granted.
    NoPlanScope,
}

/// Exchanges `old`'s refresh token for new tokens, rotating the refresh
/// token.
pub(crate) async fn refresh(
    http: &reqwest::Client,
    endpoints: &Endpoints,
    client_id: &str,
    old: &StoredTokens,
    now: u64,
) -> Result<Refreshed, ChatGptError> {
    let Some(rt) = &old.refresh_token else {
        return Ok(Refreshed::Terminal("no refresh token was issued".into()));
    };
    let form = [
        ("grant_type", "refresh_token"),
        ("client_id", client_id),
        ("refresh_token", rt.expose()),
        ("resource", endpoints.resource.as_str()),
    ];
    let t = match token_call(http, &endpoints.token_url, &form).await {
        Ok(t) => t,
        Err(TokenFailure::OAuth { code, .. })
            if TERMINAL_REFRESH_CODES.contains(&code.as_str()) =>
        {
            return Ok(Refreshed::Terminal(code));
        }
        Err(TokenFailure::OAuth { code, message }) => {
            return Err(ChatGptError::OAuth { code, message });
        }
        Err(TokenFailure::Network(m)) => return Err(ChatGptError::Network(m)),
    };
    let scopes = match t.scope.as_deref() {
        Some(s) => scopes_of(Some(s)),
        None => old.scopes.clone(),
    };
    if !scopes.iter().any(|s| s == PLAN_SCOPE) {
        return Ok(Refreshed::NoPlanScope);
    }
    Ok(Refreshed::Tokens(StoredTokens {
        access_token: Token::new(t.access_token),
        refresh_token: t
            .refresh_token
            .map(Token::new)
            .or_else(|| old.refresh_token.clone()),
        id_token: t.id_token.map(Token::new).or_else(|| old.id_token.clone()),
        expires_at: now + t.expires_in.unwrap_or(3600),
        scopes,
        account: old.account.clone(),
    }))
}

/// One browser sign-in.
#[derive(Debug, Clone)]
pub struct SignIn {
    /// Where to sign in.
    pub endpoints: Endpoints,
    /// Where the registration and tokens go.
    pub store: TokenStore,
    /// Loopback port of the first registration (default
    /// [`DEFAULT_CALLBACK_PORT`]; tests use `0`).
    pub port: u16,
    /// How long to wait for the browser.
    pub timeout: Duration,
}

/// The query of the loopback callback.
struct Callback {
    params: Vec<(String, String)>,
}

impl Callback {
    fn get(&self, k: &str) -> Option<&str> {
        self.params
            .iter()
            .find(|(n, _)| n == k)
            .map(|(_, v)| v.as_str())
    }
}

impl SignIn {
    /// A sign-in against OpenAI storing into `store`.
    pub fn new(store: TokenStore) -> Self {
        Self {
            endpoints: Endpoints::openai(),
            store,
            port: DEFAULT_CALLBACK_PORT,
            timeout: SIGN_IN_TIMEOUT,
        }
    }

    /// Uses `endpoints` (a mock server in tests).
    pub fn with_endpoints(mut self, endpoints: Endpoints) -> Self {
        self.endpoints = endpoints;
        self
    }

    /// Uses `port` for the first registration.
    pub fn with_port(mut self, port: u16) -> Self {
        self.port = port;
        self
    }

    /// Waits at most `timeout` for the browser.
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    async fn listen(&self, registered: bool) -> Result<TcpListener, ChatGptError> {
        match TcpListener::bind(("127.0.0.1", self.port)).await {
            Ok(l) => Ok(l),
            // Only the first registration is tied to its port.
            Err(_) if registered => TcpListener::bind(("127.0.0.1", 0))
                .await
                .map_err(|e| ChatGptError::Port(0, e.to_string())),
            Err(e) => Err(ChatGptError::Port(self.port, e.to_string())),
        }
    }

    /// Runs the sign-in: listen on 127.0.0.1, open `browser` at the
    /// authorization URL, wait for the callback, exchange the code, verify
    /// the ID token and the plan scope, and save the tokens.
    pub async fn run(&self, browser: &dyn Browser) -> Result<Account, ChatGptError> {
        let host_id = self.store.host_id()?;
        let record = self.store.load()?;
        let registered = record.client_id.clone();
        let listener = self.listen(registered.is_some()).await?;
        let port = listener
            .local_addr()
            .map_err(|e| ChatGptError::Port(self.port, e.to_string()))?
            .port();
        let redirect_uri = format!("http://127.0.0.1:{port}{CALLBACK_PATH}");
        let state = random_token(32);
        let nonce = random_token(32);
        let pkce = Pkce::new();

        let client_id = registered.as_deref().unwrap_or(DYNAMIC_CLIENT_ID);
        let mut query: Vec<(&str, &str)> = vec![("client_id", client_id)];
        if registered.is_none() {
            query.push(("agent_name_hint", AGENT_NAME));
        }
        let hint = record.tokens.as_ref().and_then(|t| t.account.email.clone());
        query.extend([
            ("ext_agent_host_id", host_id.as_str()),
            ("response_type", "code"),
            ("redirect_uri", redirect_uri.as_str()),
            ("scope", SCOPES),
            ("resource", self.endpoints.resource.as_str()),
            ("state", state.as_str()),
            ("nonce", nonce.as_str()),
            ("code_challenge_method", "S256"),
            ("code_challenge", pkce.challenge.as_str()),
        ]);
        if let Some(h) = hint.as_deref() {
            query.push(("login_hint", h));
        }
        let url = format!("{}?{}", self.endpoints.authorize_url, form_encode(&query));
        browser
            .open(&url)
            .map_err(|e| ChatGptError::Browser(e, url.clone()))?;

        let callback = tokio::time::timeout(self.timeout, accept_callback(&listener, &state))
            .await
            .map_err(|_| ChatGptError::Timeout(self.timeout))??;

        // The issued client id is saved before the exchange, so a later
        // attempt reuses it even if this code expires.
        let client_id = match (&registered, callback.get("client_id")) {
            (Some(id), _) => id.clone(),
            (None, Some(id)) if !id.is_empty() && id != DYNAMIC_CLIENT_ID => {
                self.store.save_client_id(id)?;
                id.to_owned()
            }
            (None, _) => return Err(ChatGptError::Registration),
        };
        let code = callback
            .get("code")
            .filter(|c| !c.is_empty())
            .ok_or_else(|| ChatGptError::OAuth {
                code: "missing_code".into(),
                message: "the callback had no authorization code".into(),
            })?;

        let http = http_client();
        let form = [
            ("grant_type", "authorization_code"),
            ("client_id", client_id.as_str()),
            ("code", code),
            ("code_verifier", pkce.verifier()),
            ("redirect_uri", redirect_uri.as_str()),
            ("resource", self.endpoints.resource.as_str()),
        ];
        let t = token_call(&http, &self.endpoints.token_url, &form)
            .await
            .map_err(|f| match f {
                TokenFailure::OAuth { code, message } => ChatGptError::OAuth { code, message },
                TokenFailure::Network(m) => ChatGptError::Network(m),
            })?;

        let id_token = t
            .id_token
            .as_deref()
            .ok_or_else(|| ChatGptError::IdToken("the token response had no ID token".into()))?;
        let jwks = fetch_jwks(&http, &self.endpoints.jwks_url).await?;
        let now = unix_now();
        let claims = verify_id_token(
            id_token,
            &jwks,
            Expected {
                issuer: &self.endpoints.issuer,
                audience: &client_id,
                nonce: &nonce,
                now,
            },
        )
        .map_err(ChatGptError::IdToken)?;

        let scopes = scopes_of(t.scope.as_deref());
        if !scopes.iter().any(|s| s == PLAN_SCOPE) {
            return Err(ChatGptError::PlanUsageNotGranted);
        }
        let account = Account {
            sub: claims.sub,
            email: claims.email,
        };
        self.store.save(&AuthRecord {
            client_id: Some(client_id),
            tokens: Some(StoredTokens {
                access_token: Token::new(t.access_token),
                refresh_token: t.refresh_token.map(Token::new),
                id_token: Some(Token::new(id_token)),
                expires_at: now + t.expires_in.unwrap_or(3600),
                scopes,
                account: account.clone(),
            }),
        })?;
        super::session::forget_cached(self.store.dir());
        Ok(account)
    }
}

async fn fetch_jwks(http: &reqwest::Client, url: &str) -> Result<Jwks, ChatGptError> {
    let resp = http
        .get(url)
        .send()
        .await
        .map_err(|e| ChatGptError::Network(e.without_url().to_string()))?;
    if !resp.status().is_success() {
        return Err(ChatGptError::Network(format!(
            "JWKS answered HTTP {}",
            resp.status().as_u16()
        )));
    }
    resp.json()
        .await
        .map_err(|_| ChatGptError::IdToken("the issuer's JWKS is unreadable".into()))
}

const PAGE_OK: &str =
    "Signed in to harness with ChatGPT. You can close this tab and return to the terminal.";

/// Accepts connections until one is the callback for `state`.
async fn accept_callback(listener: &TcpListener, state: &str) -> Result<Callback, ChatGptError> {
    loop {
        let (mut sock, _) = listener
            .accept()
            .await
            .map_err(|e| ChatGptError::Network(e.to_string()))?;
        let Some(target) = read_target(&mut sock).await else {
            continue;
        };
        let (path, query) = target.split_once('?').unwrap_or((&target, ""));
        if path != CALLBACK_PATH {
            respond(&mut sock, 404, "Not found.").await;
            continue;
        }
        let cb = Callback {
            params: parse_query(query),
        };
        if cb.get("state") != Some(state) {
            respond(
                &mut sock,
                400,
                "This sign-in link is stale. Start again from harness.",
            )
            .await;
            return Err(ChatGptError::StateMismatch);
        }
        if let Some(err) = cb.get("error") {
            let err = err.to_owned();
            if err == "access_denied" {
                respond(
                    &mut sock,
                    200,
                    "Sign-in was declined. Nothing was saved; you can close this tab.",
                )
                .await;
                return Err(ChatGptError::Denied);
            }
            respond(&mut sock, 400, "Sign-in failed. See harness for details.").await;
            return Err(ChatGptError::OAuth {
                code: err,
                message: cb
                    .get("error_description")
                    .unwrap_or("")
                    .chars()
                    .take(200)
                    .collect(),
            });
        }
        respond(&mut sock, 200, PAGE_OK).await;
        return Ok(cb);
    }
}

/// Reads one request head and returns its target (`/path?query`).
async fn read_target(sock: &mut TcpStream) -> Option<String> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 1024];
    while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
        let n = tokio::time::timeout(Duration::from_secs(10), sock.read(&mut chunk))
            .await
            .ok()?
            .ok()?;
        if n == 0 || buf.len() > 16 * 1024 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
    }
    let head = String::from_utf8_lossy(&buf);
    let line = head.lines().next()?;
    let mut parts = line.split_whitespace();
    (parts.next()? == "GET").then_some(())?;
    parts.next().map(str::to_owned)
}

async fn respond(sock: &mut TcpStream, status: u16, text: &str) {
    let reason = match status {
        200 => "OK",
        404 => "Not Found",
        _ => "Bad Request",
    };
    let body = format!(
        "<!doctype html><meta charset=\"utf-8\"><title>harness</title><p style=\"font-family:sans-serif\">{text}</p>"
    );
    let resp = format!(
        "HTTP/1.1 {status} {reason}\r\ncontent-type: text/html; charset=utf-8\r\ncontent-length: {}\r\nconnection: close\r\ncache-control: no-store\r\n\r\n{body}",
        body.len()
    );
    let _ = sock.write_all(resp.as_bytes()).await;
    let _ = sock.shutdown().await;
}
