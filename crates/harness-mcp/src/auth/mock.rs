//! Test support: a local mock of an OAuth-protected MCP server and its
//! authorization server, and a [`MockBrowser`] that "clicks through" the
//! consent page. Everything runs on `127.0.0.1`.
//!
//! [`MockOAuthServer`] serves, on one port:
//!
//! - `POST`/`DELETE /mcp`: the protected MCP endpoint. Without a valid
//!   `Authorization: Bearer` token it answers `401` with
//!   `WWW-Authenticate: Bearer resource_metadata=…, scope=…`; with one it
//!   forwards the request to an in-process
//!   [`crate::test_server::HttpServer`]. A token without
//!   [`MockConfig::required_scope`] gets `403 insufficient_scope`.
//! - `GET /.well-known/oauth-protected-resource/mcp`: Protected Resource
//!   Metadata (RFC 9728).
//! - `GET /.well-known/oauth-authorization-server`: Authorization Server
//!   Metadata (RFC 8414), with `S256` PKCE and `iss` in responses.
//! - `POST /register`: Dynamic Client Registration (when
//!   [`MockConfig::dynamic_registration`]).
//! - `GET /authorize`: redirects to the loopback `redirect_uri` with a code
//!   (or `error=access_denied` when [`MockConfig::deny`]), `state` and `iss`.
//! - `POST /token`: the code exchange (checks PKCE, client, redirect URI
//!   and `resource`) and refresh (rotating the refresh token, or failing
//!   with [`MockConfig::refresh_error`]).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use harness_providers::chatgpt::pkce::challenge_for;
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use super::{Browser, parse_query};
use crate::test_server::{HttpServer, Options};

/// The pre-registered client id the mock accepts.
pub const PRE_REGISTERED_CLIENT: &str = "pre-registered-client";

/// The scope the mock's resource asks for.
pub const MOCK_SCOPE: &str = "mcp:tools";

/// Mock behaviour.
#[derive(Debug, Clone)]
pub struct MockConfig {
    /// Offer Dynamic Client Registration. Default true.
    pub dynamic_registration: bool,
    /// The user declines on the consent page.
    pub deny: bool,
    /// Refresh fails with this OAuth error code (e.g. `invalid_grant`).
    pub refresh_error: Option<String>,
    /// Access token lifetime (`expires_in`). Default 3600.
    pub expires_in: u64,
    /// Issue refresh tokens. Default true.
    pub issue_refresh_tokens: bool,
    /// Send `resource_metadata` in the `401` challenge. Default true.
    pub challenge_header: bool,
    /// Claim this issuer in the metadata instead of the real one.
    pub wrong_issuer: Option<String>,
    /// Put this `iss` on the callback instead of the real issuer.
    pub wrong_callback_iss: Option<String>,
    /// MCP requests need this scope (else `403 insufficient_scope`).
    pub required_scope: Option<String>,
}

impl Default for MockConfig {
    fn default() -> Self {
        Self {
            dynamic_registration: true,
            deny: false,
            refresh_error: None,
            expires_in: 3600,
            issue_refresh_tokens: true,
            challenge_header: true,
            wrong_issuer: None,
            wrong_callback_iss: None,
            required_scope: None,
        }
    }
}

/// One request the mock received (no bodies of token calls are kept
/// beyond their form fields).
#[derive(Debug, Clone)]
pub struct MockRequest {
    /// `GET`, `POST`, ...
    pub method: String,
    /// Path without the query.
    pub path: String,
    /// Query parameters.
    pub query: Vec<(String, String)>,
    /// Lower-cased header names and values.
    pub headers: Vec<(String, String)>,
    /// The body.
    pub body: String,
}

impl MockRequest {
    /// Query or form parameter `k`.
    pub fn param(&self, k: &str) -> Option<String> {
        self.query
            .iter()
            .chain(&parse_query(&self.body))
            .find(|(n, _)| n == k)
            .map(|(_, v)| v.clone())
    }

    /// Header `k` (lower-case).
    pub fn header(&self, k: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n == k)
            .map(|(_, v)| v.as_str())
    }
}

struct Grant {
    client_id: String,
    redirect_uri: String,
    challenge: String,
    resource: String,
    scope: String,
}

#[derive(Default)]
struct State {
    config: MockConfig,
    counter: u64,
    clients: HashMap<String, Vec<String>>,
    codes: HashMap<String, Grant>,
    /// Valid access tokens and their scopes.
    access: HashMap<String, String>,
    /// Valid refresh tokens: (client id, scope).
    refresh: HashMap<String, (String, String)>,
    requests: Vec<MockRequest>,
}

/// The mock authorization server plus protected MCP endpoint.
pub struct MockOAuthServer {
    /// Base URL (`http://127.0.0.1:<port>`), also the issuer.
    pub base_url: String,
    state: Arc<Mutex<State>>,
    task: tokio::task::JoinHandle<()>,
    _inner: HttpServer,
}

impl Drop for MockOAuthServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

struct Reply {
    status: u16,
    content_type: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Reply {
    fn json(status: u16, v: Value) -> Self {
        Self {
            status,
            content_type: "application/json".into(),
            headers: Vec::new(),
            body: v.to_string().into_bytes(),
        }
    }

    fn redirect(location: String) -> Self {
        Self {
            status: 302,
            content_type: "text/plain".into(),
            headers: vec![("location".into(), location)],
            body: Vec::new(),
        }
    }
}

impl MockOAuthServer {
    /// Starts the mock with `config` in front of a modern test server.
    pub async fn start(config: MockConfig) -> Self {
        let inner = HttpServer::start(Options::default()).await;
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let base_url = format!("http://{}", listener.local_addr().expect("addr"));
        let mut st = State {
            config,
            ..State::default()
        };
        st.clients.insert(PRE_REGISTERED_CLIENT.into(), Vec::new());
        let state = Arc::new(Mutex::new(st));
        let (s, base, inner_url) = (state.clone(), base_url.clone(), inner.url.clone());
        let task = tokio::spawn(async move {
            let http = reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .expect("client");
            while let Ok((sock, _)) = listener.accept().await {
                let (s, base, inner_url, http) =
                    (s.clone(), base.clone(), inner_url.clone(), http.clone());
                tokio::spawn(async move { serve(sock, &s, &base, &inner_url, &http).await });
            }
        });
        Self {
            base_url,
            state,
            task,
            _inner: inner,
        }
    }

    /// The protected MCP endpoint.
    pub fn mcp_url(&self) -> String {
        format!("{}/mcp", self.base_url)
    }

    /// Changes the behaviour.
    pub fn configure(&self, f: impl FnOnce(&mut MockConfig)) {
        f(&mut self.state.lock().expect("lock").config);
    }

    /// Every request so far.
    pub fn requests(&self) -> Vec<MockRequest> {
        self.state.lock().expect("lock").requests.clone()
    }

    /// Requests to `path`.
    pub fn requests_to(&self, path: &str) -> Vec<MockRequest> {
        self.requests()
            .into_iter()
            .filter(|r| r.path == path)
            .collect()
    }

    /// Revokes every access token (refresh tokens stay valid).
    pub fn revoke_access_tokens(&self) {
        self.state.lock().expect("lock").access.clear();
    }

    /// Revokes every access and refresh token (the grant is gone).
    pub fn revoke_all(&self) {
        let mut s = self.state.lock().expect("lock");
        s.access.clear();
        s.refresh.clear();
    }

    /// Registered dynamic clients.
    pub fn dynamic_clients(&self) -> Vec<String> {
        self.state
            .lock()
            .expect("lock")
            .clients
            .keys()
            .filter(|c| *c != PRE_REGISTERED_CLIENT)
            .cloned()
            .collect()
    }
}

async fn read_request(
    sock: &mut TcpStream,
) -> Option<(String, String, Vec<(String, String)>, Vec<u8>)> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    let head_end = loop {
        if let Some(p) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break p + 4;
        }
        let n = sock.read(&mut chunk).await.ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&chunk[..n]);
    };
    let head = String::from_utf8_lossy(&buf[..head_end]).into_owned();
    let mut lines = head.lines();
    let mut first = lines.next()?.split_whitespace();
    let method = first.next()?.to_owned();
    let target = first.next()?.to_owned();
    let headers: Vec<(String, String)> = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_owned()))
        .collect();
    let len: usize = headers
        .iter()
        .find(|(k, _)| k == "content-length")
        .and_then(|(_, v)| v.parse().ok())
        .unwrap_or(0);
    while buf.len() < head_end + len {
        let n = sock.read(&mut chunk).await.ok()?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
    }
    Some((method, target, headers, buf[head_end..].to_vec()))
}

async fn serve(
    mut sock: TcpStream,
    st: &Mutex<State>,
    base: &str,
    inner_url: &str,
    http: &reqwest::Client,
) {
    let Some((method, target, headers, body)) = read_request(&mut sock).await else {
        return;
    };
    let (path, query) = target.split_once('?').unwrap_or((&target, ""));
    let req = MockRequest {
        method,
        path: path.to_owned(),
        query: parse_query(query),
        headers,
        body: String::from_utf8_lossy(&body).into_owned(),
    };
    st.lock().expect("lock").requests.push(req.clone());
    let reply = if req.path == "/mcp" {
        match authorize_mcp(st, base, &req) {
            Some(r) => r,
            None => proxy(http, inner_url, &req, body).await,
        }
    } else {
        handle(st, base, &req)
    };
    let mut head = format!(
        "HTTP/1.1 {} X\r\ncontent-type: {}\r\ncontent-length: {}\r\nconnection: close\r\n",
        reply.status,
        reply.content_type,
        reply.body.len()
    );
    for (k, v) in &reply.headers {
        head.push_str(&format!("{k}: {v}\r\n"));
    }
    head.push_str("\r\n");
    let _ = sock.write_all(head.as_bytes()).await;
    let _ = sock.write_all(&reply.body).await;
    let _ = sock.shutdown().await;
}

/// `Some(error reply)` unless the request carries a valid token.
fn authorize_mcp(st: &Mutex<State>, base: &str, req: &MockRequest) -> Option<Reply> {
    let s = st.lock().expect("lock");
    let metadata = format!("{base}/.well-known/oauth-protected-resource/mcp");
    let token = req
        .header("authorization")
        .and_then(|h| h.strip_prefix("Bearer "));
    let Some(scope) = token.and_then(|t| s.access.get(t)) else {
        let mut r = Reply::json(401, json!({"error": "invalid_token"}));
        if s.config.challenge_header {
            r.headers.push((
                "www-authenticate".into(),
                format!(r#"Bearer resource_metadata="{metadata}", scope="{MOCK_SCOPE}""#),
            ));
        } else {
            r.headers.push(("www-authenticate".into(), "Bearer".into()));
        }
        return Some(r);
    };
    if let Some(need) = &s.config.required_scope
        && !scope.split_whitespace().any(|x| x == need)
    {
        let mut r = Reply::json(403, json!({"error": "insufficient_scope"}));
        r.headers.push((
            "www-authenticate".into(),
            format!(
                r#"Bearer error="insufficient_scope", scope="{need}", resource_metadata="{metadata}""#
            ),
        ));
        return Some(r);
    }
    None
}

async fn proxy(http: &reqwest::Client, inner_url: &str, req: &MockRequest, body: Vec<u8>) -> Reply {
    let method =
        reqwest::Method::from_bytes(req.method.as_bytes()).unwrap_or(reqwest::Method::POST);
    let mut out = http.request(method, inner_url).body(body);
    for (k, v) in &req.headers {
        if !matches!(
            k.as_str(),
            "host" | "authorization" | "content-length" | "connection"
        ) {
            out = out.header(k.as_str(), v.as_str());
        }
    }
    let Ok(resp) = out.send().await else {
        return Reply::json(502, json!({"error": "inner server unreachable"}));
    };
    let status = resp.status().as_u16();
    let content_type = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("application/json")
        .to_owned();
    let mut headers = Vec::new();
    if let Some(sid) = resp
        .headers()
        .get("mcp-session-id")
        .and_then(|v| v.to_str().ok())
    {
        headers.push(("mcp-session-id".into(), sid.to_owned()));
    }
    let body = resp.bytes().await.map(|b| b.to_vec()).unwrap_or_default();
    Reply {
        status,
        content_type,
        headers,
        body,
    }
}

fn handle(st: &Mutex<State>, base: &str, req: &MockRequest) -> Reply {
    let mut s = st.lock().expect("lock");
    s.counter += 1;
    let n = s.counter;
    match (req.method.as_str(), req.path.as_str()) {
        ("GET", "/.well-known/oauth-protected-resource/mcp") => Reply::json(
            200,
            json!({
                "resource": format!("{base}/mcp"),
                "authorization_servers": [base],
                "scopes_supported": [MOCK_SCOPE],
            }),
        ),
        ("GET", "/.well-known/oauth-authorization-server") => {
            let issuer = s
                .config
                .wrong_issuer
                .clone()
                .unwrap_or_else(|| base.to_owned());
            let mut m = json!({
                "issuer": issuer,
                "authorization_endpoint": format!("{base}/authorize"),
                "token_endpoint": format!("{base}/token"),
                "response_types_supported": ["code"],
                "grant_types_supported": ["authorization_code", "refresh_token"],
                "code_challenge_methods_supported": ["S256"],
                "scopes_supported": [MOCK_SCOPE, "offline_access"],
                "authorization_response_iss_parameter_supported": true,
            });
            if s.config.dynamic_registration {
                m["registration_endpoint"] = json!(format!("{base}/register"));
            }
            Reply::json(200, m)
        }
        ("POST", "/register") if s.config.dynamic_registration => {
            let v: Value = serde_json::from_str(&req.body).unwrap_or(Value::Null);
            let uris: Vec<String> = v["redirect_uris"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|u| u.as_str().map(str::to_owned))
                        .collect()
                })
                .unwrap_or_default();
            if uris.is_empty() || v["token_endpoint_auth_method"] != "none" {
                return Reply::json(400, json!({"error": "invalid_client_metadata"}));
            }
            let id = format!("dyn-client-{n}");
            s.clients.insert(id.clone(), uris.clone());
            Reply::json(
                201,
                json!({"client_id": id, "redirect_uris": uris, "token_endpoint_auth_method": "none"}),
            )
        }
        ("GET", "/authorize") => {
            let p = |k: &str| req.param(k).unwrap_or_default();
            let (client_id, redirect) = (p("client_id"), p("redirect_uri"));
            let Some(allowed) = s.clients.get(&client_id) else {
                return Reply::json(400, json!({"error": "invalid_client"}));
            };
            if !allowed.is_empty() && !allowed.contains(&redirect) {
                return Reply::json(
                    400,
                    json!({"error": "invalid_request", "error_description": "redirect_uri"}),
                );
            }
            if p("code_challenge_method") != "S256" || p("resource").is_empty() {
                return Reply::json(400, json!({"error": "invalid_request"}));
            }
            let iss = s
                .config
                .wrong_callback_iss
                .clone()
                .unwrap_or_else(|| base.to_owned());
            let mut q = vec![("state", p("state")), ("iss", iss)];
            if s.config.deny {
                q.push(("error", "access_denied".into()));
            } else {
                let code = format!("code-{n}");
                s.codes.insert(
                    code.clone(),
                    Grant {
                        client_id,
                        redirect_uri: redirect.clone(),
                        challenge: p("code_challenge"),
                        resource: p("resource"),
                        scope: p("scope"),
                    },
                );
                q.push(("code", code));
            }
            let refs: Vec<(&str, &str)> = q.iter().map(|(k, v)| (*k, v.as_str())).collect();
            Reply::redirect(format!("{redirect}?{}", super::form_encode(&refs)))
        }
        ("POST", "/token") => token(&mut s, n, req),
        _ => Reply::json(404, json!({"error": "not_found"})),
    }
}

fn token(s: &mut State, n: u64, req: &MockRequest) -> Reply {
    let p = |k: &str| req.param(k).unwrap_or_default();
    let bad = |code: &str| Reply::json(400, json!({"error": code}));
    let (client_id, scope) = match p("grant_type").as_str() {
        "authorization_code" => {
            let Some(g) = s.codes.remove(&p("code")) else {
                return bad("invalid_grant");
            };
            if g.client_id != p("client_id")
                || g.redirect_uri != p("redirect_uri")
                || g.resource != p("resource")
                || challenge_for(&p("code_verifier")) != g.challenge
            {
                return bad("invalid_grant");
            }
            (g.client_id, g.scope)
        }
        "refresh_token" => {
            if let Some(code) = s.config.refresh_error.clone() {
                return bad(&code);
            }
            if p("resource").is_empty() {
                return bad("invalid_request");
            }
            match s.refresh.remove(&p("refresh_token")) {
                Some((c, scope)) if c == p("client_id") => (c, scope),
                _ => return bad("invalid_grant"),
            }
        }
        _ => return bad("unsupported_grant_type"),
    };
    let access = format!("mock-access-{n}");
    s.access.insert(access.clone(), scope.clone());
    let mut out = json!({
        "access_token": access,
        "token_type": "Bearer",
        "expires_in": s.config.expires_in,
        "scope": scope,
    });
    if s.config.issue_refresh_tokens {
        let refresh = format!("mock-refresh-{n}");
        s.refresh.insert(refresh.clone(), (client_id, scope));
        out["refresh_token"] = json!(refresh);
    }
    Reply::json(200, out)
}

/// A browser that follows the authorization redirect to the loopback
/// callback, like a user who clicks "Allow".
#[derive(Debug, Default, Clone)]
pub struct MockBrowser {
    opened: Arc<Mutex<Vec<String>>>,
    redirects: Arc<Mutex<Vec<String>>>,
    /// A browser on another machine: the redirect to the loopback callback
    /// is recorded ([`MockBrowser::next_redirect`]) but never followed.
    pub remote: bool,
}

impl MockBrowser {
    /// A new browser.
    pub fn new() -> Self {
        Self::default()
    }

    /// A browser on another machine (see [`MockBrowser::remote`]).
    pub fn remote() -> Self {
        Self {
            remote: true,
            ..Self::default()
        }
    }

    /// Waits up to 10s for the next loopback address a remote browser was
    /// sent to: what the user would copy from the address bar.
    pub async fn next_redirect(&self) -> String {
        for _ in 0..1000 {
            if let Some(r) = {
                let mut r = self.redirects.lock().expect("lock");
                (!r.is_empty()).then(|| r.remove(0))
            } {
                return r;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("the browser was never redirected");
    }

    /// Every URL opened so far.
    pub fn opened(&self) -> Vec<String> {
        self.opened.lock().expect("lock").clone()
    }
}

impl Browser for MockBrowser {
    fn open(&self, url: &str) -> Result<(), String> {
        self.opened.lock().expect("lock").push(url.to_owned());
        let url = url.to_owned();
        let (remote, redirects) = (self.remote, self.redirects.clone());
        tokio::spawn(async move {
            let client = reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .expect("client");
            let Ok(resp) = client.get(&url).send().await else {
                return;
            };
            if let Some(location) = resp.headers().get("location").and_then(|v| v.to_str().ok()) {
                if remote {
                    redirects.lock().expect("lock").push(location.to_owned());
                    return;
                }
                let _ = client.get(location).send().await;
            }
        });
        Ok(())
    }
}
