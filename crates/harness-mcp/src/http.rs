//! The streamable HTTP transport.
//!
//! Every JSON-RPC message is its own `POST` to the MCP endpoint with
//! `Accept: application/json, text/event-stream`. The response is either a
//! single JSON object or an SSE stream carrying request-scoped
//! notifications followed by the response.
//!
//! In the modern era each POST carries `MCP-Protocol-Version`, `Mcp-Method`
//! and (for `tools/call`, `resources/read`, `prompts/get`) `Mcp-Name`, plus
//! `Mcp-Param-*` headers mirrored from tool arguments (see
//! [`crate::headers`]). In the legacy era the server may assign an
//! `Mcp-Session-Id`, which is sent back on every request and ended with
//! `DELETE` on shutdown.
//!
//! With `auth = oauth` every request carries `Authorization: Bearer` from
//! an [`OAuthSession`]; a `401` refreshes the token once and retries, and a
//! `403 insufficient_scope` is remembered for the next authorization. A
//! server that asks for OAuth without it fails with [`McpError::Auth`]
//! naming `harness login mcp <name>`.
//!
//! Redirects are never followed: the configured `header.*` credentials and
//! the JSON-RPC body must reach only the configured URL, so a `3xx` is an
//! error naming where the server pointed.

use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use futures::StreamExt;
use harness_events::redact::Redactor;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use serde_json::Value;

use crate::auth::discovery::parse_www_authenticate;
use crate::auth::{AuthError, OAuthSession, Token, login_command, secure_endpoint};
use crate::error::{McpError, Result};
use crate::protocol::{self, Era, Incoming, Notification, RpcError};
use tokio::sync::broadcast;

/// Largest response body accepted (bytes).
const MAX_BODY: usize = 16 * 1024 * 1024;

/// A streamable-HTTP connection to one server.
pub struct HttpTransport {
    server: String,
    url: String,
    client: reqwest::Client,
    /// Configured `header.*` values (marked sensitive).
    headers: HeaderMap,
    redactor: Arc<Redactor>,
    next_id: AtomicU64,
    /// Era and protocol version in use, once known.
    protocol: Mutex<Option<(Era, String)>>,
    /// Legacy `Mcp-Session-Id`.
    session: Mutex<Option<String>>,
    /// OAuth tokens (`auth = oauth`).
    auth: Option<Arc<OAuthSession>>,
    /// Server notifications, for [`HttpTransport::subscribe`].
    notifications: broadcast::Sender<Notification>,
}

impl HttpTransport {
    /// A transport for `url` sending `headers` (name, value) on every
    /// request. Fails when a header name or value is not valid HTTP.
    pub fn new(
        server: &str,
        url: &str,
        headers: Vec<(String, harness_config::secrets::Secret)>,
        redactor: Arc<Redactor>,
    ) -> Result<Self> {
        let mut map = HeaderMap::new();
        for (name, value) in headers {
            let n = HeaderName::from_bytes(name.as_bytes()).map_err(|_| McpError::Secret {
                server: server.into(),
                key: format!("header.{name}"),
                message: "not a valid HTTP header name".into(),
            })?;
            let mut v = HeaderValue::from_str(value.expose()).map_err(|_| McpError::Secret {
                server: server.into(),
                key: format!("header.{name}"),
                message: "the value contains characters not allowed in an HTTP header".into(),
            })?;
            v.set_sensitive(true);
            map.insert(n, v);
        }
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(10))
            // reqwest strips only standard auth headers on cross-origin
            // redirects; custom `header.*` credentials would be re-sent.
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|e| McpError::Http {
                server: server.into(),
                status: None,
                rpc: None,
                message: format!("cannot build the HTTP client: {e}"),
            })?;
        Ok(Self {
            server: server.into(),
            url: url.into(),
            client,
            headers: map,
            redactor,
            next_id: AtomicU64::new(1),
            protocol: Mutex::new(None),
            session: Mutex::new(None),
            auth: None,
            notifications: broadcast::channel(protocol::NOTIFICATION_BUFFER).0,
        })
    }

    /// Notifications the server sends from now on (on any response
    /// stream, [`listen`](Self::listen) or [`listen_get`](Self::listen_get)).
    pub fn subscribe(&self) -> broadcast::Receiver<Notification> {
        self.notifications.subscribe()
    }

    /// Modern servers: opens a `subscriptions/listen` stream with `params`
    /// (which must carry the request `_meta`) and delivers its
    /// notifications until the server ends it (`Ok`) or the stream breaks.
    pub async fn listen(&self, params: Value) -> Result<()> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let method = "subscriptions/listen";
        let body = protocol::request(id, method, params);
        self.exchange(&body, id, method, &[]).await.map(|_| ())
    }

    /// Legacy servers: opens the `GET` stream for server messages and
    /// delivers its notifications until it ends. A server without one
    /// answers `405`, returned as an [`McpError::Http`] with that status.
    pub async fn listen_get(&self) -> Result<()> {
        let mut req = self
            .client
            .get(&self.url)
            .headers(self.headers.clone())
            .header("Accept", "text/event-stream");
        if let Some((_, version)) = self.protocol() {
            req = req.header("MCP-Protocol-Version", version);
        }
        if let Some(s) = self.session() {
            req = req.header("Mcp-Session-Id", s);
        }
        if let Some(t) = self.token().await? {
            req = req.header("Authorization", bearer(&t));
        }
        let resp = req.send().await.map_err(|e| {
            self.err(
                None,
                None,
                format!("cannot reach {}: {}", self.url, root_cause(&e)),
            )
        })?;
        let status = resp.status();
        if !status.is_success() {
            return Err(self.err(
                Some(status.as_u16()),
                None,
                format!("HTTP {status} for the notification stream"),
            ));
        }
        self.read_sse(resp, u64::MAX, "GET").await.map(|_| ())
    }

    /// Sends `Authorization: Bearer` from `session` on every request.
    pub fn with_oauth(mut self, session: Arc<OAuthSession>) -> Self {
        self.auth = Some(session);
        self
    }

    fn auth_error(&self, e: AuthError) -> McpError {
        McpError::Auth {
            server: self.server.clone(),
            message: self.redactor.redact(&e.to_string()).into_owned(),
        }
    }

    /// The current access token, if this server uses OAuth. Refuses a
    /// plain `http://` URL on a non-loopback host, where the token would
    /// travel unencrypted.
    async fn token(&self) -> Result<Option<Token>> {
        if self.auth.is_some() && !secure_endpoint(&self.url) {
            return Err(self.auth_error(AuthError::InsecureUrl {
                server: self.server.clone(),
                url: self.url.clone(),
            }));
        }
        match &self.auth {
            Some(a) => a
                .access_token()
                .await
                .map(Some)
                .map_err(|e| self.auth_error(e)),
            None => Ok(None),
        }
    }

    /// Sends a POST with the access token; after a `401` refreshes the
    /// token once and resends. `send` errors become [`McpError::Http`].
    async fn send(
        &self,
        body: &Value,
        method: &str,
        extra: &[(String, String)],
    ) -> Result<reqwest::Response> {
        let token = self.token().await?;
        let resp = self.send_once(body, method, extra, token.as_ref()).await?;
        if resp.status().as_u16() != 401 {
            return Ok(resp);
        }
        let (Some(auth), Some(rejected)) = (&self.auth, token) else {
            return match self.oauth_challenge(&resp, method) {
                Some(e) => Err(e),
                None => Ok(resp),
            };
        };
        tracing::info!(server = %self.server, "MCP server rejected the access token; refreshing");
        let fresh = auth
            .refresh_after_rejection(&rejected)
            .await
            .map_err(|e| self.auth_error(e))?;
        let resp = self.send_once(body, method, extra, Some(&fresh)).await?;
        if resp.status().as_u16() == 401 {
            return Err(McpError::Auth {
                server: self.server.clone(),
                message: format!(
                    "the server rejected the refreshed OAuth token (HTTP 401 for `{method}`); run `{}` to authorize again",
                    login_command(&self.server)
                ),
            });
        }
        Ok(resp)
    }

    async fn send_once(
        &self,
        body: &Value,
        method: &str,
        extra: &[(String, String)],
        token: Option<&Token>,
    ) -> Result<reqwest::Response> {
        self.post(body, method, extra, token)
            .send()
            .await
            .map_err(|e| {
                self.err(
                    None,
                    None,
                    format!("cannot reach {}: {}", self.url, root_cause(&e)),
                )
            })
    }

    /// For a `401` without OAuth: the error saying to set up OAuth when the
    /// server's challenge asks for it.
    fn oauth_challenge(&self, resp: &reqwest::Response, method: &str) -> Option<McpError> {
        let challenge = parse_www_authenticate(
            resp.headers()
                .get_all("www-authenticate")
                .iter()
                .filter_map(|v| v.to_str().ok()),
        );
        if challenge.is_some_and(|c| c.resource_metadata.is_some() || c.scope.is_some())
            || resp.headers().get("www-authenticate").is_some_and(|v| {
                v.to_str()
                    .is_ok_and(|v| v.trim().eq_ignore_ascii_case("bearer"))
            })
        {
            return Some(McpError::Auth {
                server: self.server.clone(),
                message: format!(
                    "HTTP 401 for `{method}`: the server asks for OAuth authorization; set auth = oauth in [mcp.{0}] and run `{1}` (or keep auth = none and set header.Authorization)",
                    self.server,
                    login_command(&self.server)
                ),
            });
        }
        None
    }

    /// Sets the era and version used for headers from now on.
    pub fn set_protocol(&self, era: Era, version: &str) {
        if let Ok(mut p) = self.protocol.lock() {
            *p = Some((era, version.to_owned()));
        }
    }

    /// Clears a legacy session (after it expired).
    pub fn clear_session(&self) {
        if let Ok(mut s) = self.session.lock() {
            *s = None;
        }
    }

    fn protocol(&self) -> Option<(Era, String)> {
        self.protocol.lock().ok().and_then(|p| p.clone())
    }

    fn session(&self) -> Option<String> {
        self.session.lock().ok().and_then(|s| s.clone())
    }

    fn err(&self, status: Option<u16>, rpc: Option<RpcError>, message: String) -> McpError {
        McpError::Http {
            server: self.server.clone(),
            status,
            rpc,
            message: self.redactor.redact(&message).into_owned(),
        }
    }

    fn post(
        &self,
        body: &Value,
        method: &str,
        extra: &[(String, String)],
        token: Option<&Token>,
    ) -> reqwest::RequestBuilder {
        let mut req = self
            .client
            .post(&self.url)
            .headers(self.headers.clone())
            .header("Accept", "application/json, text/event-stream")
            .header("Content-Type", "application/json");
        match self.protocol() {
            Some((Era::Modern, version)) => {
                req = req
                    .header("MCP-Protocol-Version", version)
                    .header("Mcp-Method", method);
                for (k, v) in extra {
                    req = req.header(k.as_str(), v.as_str());
                }
            }
            Some((Era::Legacy, version)) if method != "initialize" => {
                req = req.header("MCP-Protocol-Version", version);
            }
            _ => {}
        }
        if let Some(s) = self.session() {
            req = req.header("Mcp-Session-Id", s);
        }
        if let Some(t) = token {
            req = req.header("Authorization", bearer(t));
        }
        req.body(serde_json::to_vec(body).expect("JSON values serialize"))
    }

    /// Sends a request and waits up to `limit` for its response. `extra`
    /// headers (`Mcp-Name`, `Mcp-Param-*`) are sent only in the modern era.
    pub async fn request(
        &self,
        method: &str,
        params: Value,
        extra: &[(String, String)],
        limit: Duration,
    ) -> Result<Value> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let body = protocol::request(id, method, params);
        match tokio::time::timeout(limit, self.exchange(&body, id, method, extra)).await {
            Ok(r) => r,
            // Dropping the response stream is the cancellation signal.
            Err(_) => Err(McpError::Timeout {
                server: self.server.clone(),
                method: method.into(),
                limit,
            }),
        }
    }

    async fn exchange(
        &self,
        body: &Value,
        id: u64,
        method: &str,
        extra: &[(String, String)],
    ) -> Result<Value> {
        let resp = self.send(body, method, extra).await?;
        let status = resp.status();
        if let Some(sid) = resp
            .headers()
            .get("mcp-session-id")
            .and_then(|v| v.to_str().ok())
            && let Ok(mut s) = self.session.lock()
        {
            *s = Some(sid.to_owned());
        }
        let is_sse = resp
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .is_some_and(|ct| ct.starts_with("text/event-stream"));
        if status.as_u16() == 404 && self.session().is_some() {
            return Err(McpError::SessionExpired(self.server.clone()));
        }
        if status.is_redirection() {
            return Err(self.redirect_error(&resp, method));
        }
        if !status.is_success() {
            let resp_headers = resp.headers().clone();
            let text = read_body(resp).await.unwrap_or_default();
            let rpc = serde_json::from_slice::<Value>(&text)
                .ok()
                .and_then(|v| v.get("error").and_then(RpcError::from_value));
            let detail = match &rpc {
                Some(e) => e.to_string(),
                None => String::from_utf8_lossy(&text).chars().take(200).collect(),
            };
            if status.as_u16() == 403
                && let Some(e) = self.insufficient_scope(&resp_headers, method)
            {
                return Err(e);
            }
            let hint = match status.as_u16() {
                401 | 403 if self.auth.is_none() => {
                    " (check the header.* credentials in the server's [mcp.<name>] section)"
                }
                _ => "",
            };
            return Err(self.err(
                Some(status.as_u16()),
                rpc,
                format!("HTTP {status} for `{method}`: {detail}{hint}"),
            ));
        }
        if is_sse {
            return self.read_sse(resp, id, method).await;
        }
        let text = read_body(resp)
            .await
            .map_err(|m| self.err(Some(status.as_u16()), None, m))?;
        let value: Value = serde_json::from_slice(&text).map_err(|e| McpError::Protocol {
            server: self.server.clone(),
            message: format!("response to `{method}` is not JSON ({e})"),
        })?;
        self.outcome(&value, id, method).unwrap_or_else(|| {
            Err(McpError::Protocol {
                server: self.server.clone(),
                message: format!("the response to `{method}` has the wrong id or shape"),
            })
        })
    }

    /// The outcome if `value` is the response to request `id`.
    fn outcome(&self, value: &Value, id: u64, method: &str) -> Option<Result<Value>> {
        match protocol::parse_incoming(value) {
            Ok(Incoming::Response { id: rid, outcome })
                if rid.as_u64() == Some(id) || rid.is_null() =>
            {
                Some(outcome.map_err(|error| McpError::Rpc {
                    server: self.server.clone(),
                    method: method.into(),
                    error,
                }))
            }
            _ => None,
        }
    }

    async fn read_sse(&self, resp: reqwest::Response, id: u64, method: &str) -> Result<Value> {
        let mut stream = resp.bytes_stream();
        let mut buf: Vec<u8> = Vec::new();
        let mut data = String::new();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|e| {
                self.err(
                    None,
                    None,
                    format!("the response stream broke: {}", root_cause(&e)),
                )
            })?;
            buf.extend_from_slice(&chunk);
            if buf.len() > MAX_BODY {
                return Err(self.err(None, None, "the response stream is too large".into()));
            }
            while let Some(pos) = buf.iter().position(|&b| b == b'\n') {
                let mut line: Vec<u8> = buf.drain(..=pos).collect();
                line.pop();
                if line.last() == Some(&b'\r') {
                    line.pop();
                }
                let line = String::from_utf8_lossy(&line).into_owned();
                if line.is_empty() {
                    if !data.is_empty() {
                        let event = std::mem::take(&mut data);
                        if let Some(r) = self.sse_event(&event, id, method).await {
                            return r;
                        }
                    }
                } else if line.starts_with(':') {
                    // Comment / keep-alive.
                } else if let Some(rest) = line.strip_prefix("data:") {
                    if !data.is_empty() {
                        data.push('\n');
                    }
                    data.push_str(rest.strip_prefix(' ').unwrap_or(rest));
                }
            }
        }
        if !data.is_empty()
            && let Some(r) = self.sse_event(&data, id, method).await
        {
            return r;
        }
        Err(self.err(
            None,
            None,
            format!(
                "the response stream for `{method}` ended without a response; retry the request"
            ),
        ))
    }

    async fn sse_event(&self, data: &str, id: u64, method: &str) -> Option<Result<Value>> {
        let Ok(value) = serde_json::from_str::<Value>(data) else {
            tracing::warn!(server = %self.server, "MCP server sent an SSE event that is not JSON; ignored");
            return None;
        };
        if let Some(r) = self.outcome(&value, id, method) {
            return Some(r);
        }
        match protocol::parse_incoming(&value) {
            Ok(Incoming::Notification { method, params }) => {
                tracing::debug!(server = %self.server, method = %method, "MCP notification");
                let _ = self.notifications.send(Notification { method, params });
            }
            Ok(Incoming::Request { id: rid, method: m }) => {
                // Legacy servers may ask the client things on the stream;
                // harness offers no client features, so it declines.
                if matches!(self.protocol(), Some((Era::Legacy, _))) {
                    let reply = serde_json::json!({"jsonrpc": "2.0", "id": rid, "error": {
                        "code": protocol::METHOD_NOT_FOUND,
                        "message": format!("harness does not support `{m}`"),
                    }});
                    let token = self.token().await.ok().flatten();
                    let _ = self.post(&reply, "", &[], token.as_ref()).send().await;
                }
            }
            _ => {
                tracing::warn!(server = %self.server, "MCP server sent an unexpected SSE message; ignored");
            }
        }
        None
    }

    /// Sends a notification (expects `202 Accepted`).
    pub async fn notify(&self, method: &str, params: Value, limit: Duration) -> Result<()> {
        let body = protocol::notification(method, params);
        let resp = tokio::time::timeout(limit, self.send(&body, method, &[]))
            .await
            .map_err(|_| McpError::Timeout {
                server: self.server.clone(),
                method: method.into(),
                limit,
            })??;
        if resp.status().is_success() {
            Ok(())
        } else if resp.status().is_redirection() {
            Err(self.redirect_error(&resp, method))
        } else {
            Err(self.err(
                Some(resp.status().as_u16()),
                None,
                format!("HTTP {} for `{method}`", resp.status()),
            ))
        }
    }

    /// The error for `403 insufficient_scope` on an OAuth server; the
    /// scopes are remembered for the next `harness login mcp`.
    fn insufficient_scope(&self, headers: &HeaderMap, method: &str) -> Option<McpError> {
        let auth = self.auth.as_ref()?;
        let c = parse_www_authenticate(
            headers
                .get_all("www-authenticate")
                .iter()
                .filter_map(|v| v.to_str().ok()),
        )?;
        if c.error.as_deref() != Some("insufficient_scope") {
            return None;
        }
        let scope = c.scope.unwrap_or_default();
        auth.record_step_up(&scope);
        Some(McpError::Auth {
            server: self.server.clone(),
            message: format!(
                "`{method}` needs more OAuth permissions (scope: {}); run `{}` to grant them",
                if scope.is_empty() {
                    "not given"
                } else {
                    scope.as_str()
                },
                login_command(&self.server)
            ),
        })
    }

    /// The error for a `3xx` response (redirects are not followed).
    fn redirect_error(&self, resp: &reqwest::Response, method: &str) -> McpError {
        let target = resp
            .headers()
            .get("location")
            .and_then(|v| v.to_str().ok())
            .map(|l| {
                resp.url()
                    .join(l)
                    .map_or_else(|_| l.to_owned(), |u| u.to_string())
            });
        let to = target.map_or_else(|| "another location".to_owned(), |t| format!("`{t}`"));
        self.err(
            Some(resp.status().as_u16()),
            None,
            format!(
                "HTTP {} for `{method}`: the server redirected to {to}; redirects are not followed so credentials stay with the configured server. Set url in [mcp.{}] to the final endpoint",
                resp.status(),
                self.server
            ),
        )
    }

    /// Ends a legacy session with `DELETE` (modern servers have none).
    pub async fn shutdown(&self, limit: Duration) {
        let Some(session) = self.session() else {
            return;
        };
        let mut req = self
            .client
            .delete(&self.url)
            .headers(self.headers.clone())
            .header("Mcp-Session-Id", session);
        if let Some((_, v)) = self.protocol() {
            req = req.header("MCP-Protocol-Version", v);
        }
        if let Some(t) = self.token().await.ok().flatten() {
            req = req.header("Authorization", bearer(&t));
        }
        let _ = tokio::time::timeout(limit, req.send()).await;
        self.clear_session();
    }
}

/// `Bearer <token>`, marked sensitive.
fn bearer(t: &Token) -> HeaderValue {
    let mut v = HeaderValue::from_str(&format!("Bearer {}", t.expose()))
        .unwrap_or_else(|_| HeaderValue::from_static("Bearer invalid"));
    v.set_sensitive(true);
    v
}

async fn read_body(resp: reqwest::Response) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    let mut stream = resp.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk =
            chunk.map_err(|e| format!("reading the response failed: {}", root_cause(&e)))?;
        out.extend_from_slice(&chunk);
        if out.len() > MAX_BODY {
            return Err("the response is too large".into());
        }
    }
    Ok(out)
}

/// The innermost error message (reqwest wraps the useful part).
fn root_cause(e: &(dyn std::error::Error + 'static)) -> String {
    let mut cur = e;
    while let Some(next) = cur.source() {
        cur = next;
    }
    cur.to_string()
}
