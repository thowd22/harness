//! Test support: a local mock of the ChatGPT authorization server and
//! Responses API, a [`MockBrowser`] that "clicks through" the consent page,
//! and a [`TestSigner`] for RS256 ID tokens. Everything runs on
//! `127.0.0.1` with no real account.
//!
//! The mock implements the parts of the flow harness uses:
//!
//! - `GET /api/accounts/authorize` redirects to the loopback `redirect_uri`
//!   with a code, the `state` and (on first registration) an issued
//!   `client_id`, or with `error=access_denied` when [`MockConfig::deny`].
//! - `POST /api/accounts/oauth/token` checks PKCE, the client id and the
//!   redirect URI, and answers with tokens and a signed ID token; refresh
//!   rotates the refresh token, and [`MockConfig::refresh_error`] makes it
//!   fail.
//! - `GET /.well-known/jwks.json` serves the signing key.
//! - `POST /v1/responses` accepts only the current access token and
//!   streams a short answer, or a usage-limit error when
//!   [`MockConfig::usage_limited`].

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use aws_lc_rs::rsa::{KeyPair, KeySize};
use aws_lc_rs::signature::{KeyPair as _, RSA_PKCS1_SHA256};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use super::pkce::challenge_for;
use super::{
    Browser, DYNAMIC_CLIENT_ID, Endpoints, PLAN_SCOPE, USAGE_LIMIT_CODE, parse_query, unix_now,
};

/// Signs RS256 JWTs with a fresh 2048-bit key.
pub struct TestSigner {
    key: KeyPair,
    kid: String,
}

impl std::fmt::Debug for TestSigner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "TestSigner({})", self.kid)
    }
}

/// Reads one DER length at `b[*i..]`.
fn der_len(b: &[u8], i: &mut usize) -> usize {
    let first = b[*i];
    *i += 1;
    if first < 0x80 {
        return first as usize;
    }
    let n = (first & 0x7f) as usize;
    let mut len = 0usize;
    for _ in 0..n {
        len = (len << 8) | b[*i] as usize;
        *i += 1;
    }
    len
}

/// Splits a PKCS#1 `RSAPublicKey` (SEQUENCE { n INTEGER, e INTEGER }) into
/// its big-endian modulus and exponent, without leading zeros.
fn pkcs1_components(der: &[u8]) -> (Vec<u8>, Vec<u8>) {
    let mut i = 0;
    assert_eq!(der[i], 0x30, "SEQUENCE");
    i += 1;
    der_len(der, &mut i);
    let mut int = || {
        assert_eq!(der[i], 0x02, "INTEGER");
        i += 1;
        let len = der_len(der, &mut i);
        let v = der[i..i + len].to_vec();
        i += len;
        let skip = v.iter().take_while(|b| **b == 0).count();
        v[skip..].to_vec()
    };
    let n = int();
    let e = int();
    (n, e)
}

impl TestSigner {
    /// A new key with id `kid`.
    pub fn new(kid: &str) -> Self {
        Self {
            key: KeyPair::generate(KeySize::Rsa2048).expect("RSA key"),
            kid: kid.to_owned(),
        }
    }

    /// The JWKS document with this key.
    pub fn jwks(&self) -> Value {
        let (n, e) = pkcs1_components(self.key.public_key().as_ref());
        json!({"keys": [{"kty": "RSA", "kid": self.kid, "alg": "RS256", "use": "sig",
            "n": URL_SAFE_NO_PAD.encode(n), "e": URL_SAFE_NO_PAD.encode(e)}]})
    }

    /// A signed JWT with `claims`.
    pub fn sign(&self, claims: &Value) -> String {
        let header = json!({"alg": "RS256", "typ": "JWT", "kid": self.kid});
        let input = format!(
            "{}.{}",
            URL_SAFE_NO_PAD.encode(serde_json::to_vec(&header).expect("json")),
            URL_SAFE_NO_PAD.encode(serde_json::to_vec(claims).expect("json"))
        );
        let mut sig = vec![0u8; self.key.public_modulus_len()];
        self.key
            .sign(
                &RSA_PKCS1_SHA256,
                &aws_lc_rs::rand::SystemRandom::new(),
                input.as_bytes(),
                &mut sig,
            )
            .expect("sign");
        format!("{input}.{}", URL_SAFE_NO_PAD.encode(sig))
    }
}

/// How the mock behaves; change it between steps with
/// [`MockAuthServer::configure`].
#[derive(Debug, Clone)]
pub struct MockConfig {
    /// The client id issued on first registration.
    pub issued_client_id: String,
    /// Decline consent (`error=access_denied`).
    pub deny: bool,
    /// Grant [`PLAN_SCOPE`].
    pub grant_plan: bool,
    /// Access token lifetime in seconds.
    pub access_ttl: u64,
    /// Fail refreshes with this OAuth error code.
    pub refresh_error: Option<String>,
    /// Answer Responses calls with a usage-limit error.
    pub usage_limited: bool,
}

impl Default for MockConfig {
    fn default() -> Self {
        Self {
            issued_client_id: "oaiapp_mock123".into(),
            deny: false,
            grant_plan: true,
            access_ttl: 3600,
            refresh_error: None,
            usage_limited: false,
        }
    }
}

/// A request the mock received.
#[derive(Debug, Clone)]
pub struct MockRequest {
    /// Method.
    pub method: String,
    /// Path without the query.
    pub path: String,
    /// Decoded query parameters.
    pub query: Vec<(String, String)>,
    /// Lower-cased headers.
    pub headers: Vec<(String, String)>,
    /// Body text.
    pub body: String,
}

impl MockRequest {
    /// Query parameter `k`.
    pub fn param(&self, k: &str) -> Option<&str> {
        self.query
            .iter()
            .find(|(n, _)| n == k)
            .map(|(_, v)| v.as_str())
    }

    /// Form field `k` of the body.
    pub fn form(&self, k: &str) -> Option<String> {
        parse_query(&self.body)
            .into_iter()
            .find(|(n, _)| n == k)
            .map(|(_, v)| v)
    }

    /// Header `k` (lower case).
    pub fn header(&self, k: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n == k)
            .map(|(_, v)| v.as_str())
    }

    /// The body as JSON.
    pub fn json(&self) -> Value {
        serde_json::from_str(&self.body).unwrap_or(Value::Null)
    }
}

struct Pending {
    challenge: String,
    client_id: String,
    redirect_uri: String,
    nonce: String,
}

#[derive(Default)]
struct State {
    config: MockConfig,
    codes: HashMap<String, Pending>,
    access: Option<String>,
    refresh: Option<String>,
    counter: u64,
    requests: Vec<MockRequest>,
}

/// The mock server.
pub struct MockAuthServer {
    /// `http://127.0.0.1:<port>`.
    pub base_url: String,
    state: Arc<Mutex<State>>,
}

impl std::fmt::Debug for MockAuthServer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "MockAuthServer({})", self.base_url)
    }
}

struct Reply {
    status: u16,
    headers: Vec<(String, String)>,
    content_type: &'static str,
    body: String,
}

impl Reply {
    fn json(status: u16, v: Value) -> Self {
        Self {
            status,
            headers: Vec::new(),
            content_type: "application/json",
            body: v.to_string(),
        }
    }
}

impl MockAuthServer {
    /// Starts the server with `config`.
    pub async fn start(config: MockConfig) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let base_url = format!("http://{}", listener.local_addr().expect("addr"));
        let state = Arc::new(Mutex::new(State {
            config,
            ..State::default()
        }));
        let signer = Arc::new(TestSigner::new("mock-key"));
        let (st, base) = (state.clone(), base_url.clone());
        tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else {
                    return;
                };
                let (st, signer, base) = (st.clone(), signer.clone(), base.clone());
                tokio::spawn(async move {
                    let Some(req) = read_request(&mut sock).await else {
                        return;
                    };
                    let reply = handle(&st, &signer, &base, req);
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
                    let _ = sock.write_all(reply.body.as_bytes()).await;
                    let _ = sock.shutdown().await;
                });
            }
        });
        Self { base_url, state }
    }

    /// Endpoints pointing at this server.
    pub fn endpoints(&self) -> Endpoints {
        Endpoints::local(&self.base_url)
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

    /// The access token the API currently accepts.
    pub fn current_access_token(&self) -> Option<String> {
        self.state.lock().expect("lock").access.clone()
    }

    /// The refresh token the server currently accepts.
    pub fn current_refresh_token(&self) -> Option<String> {
        self.state.lock().expect("lock").refresh.clone()
    }
}

async fn read_request(sock: &mut tokio::net::TcpStream) -> Option<MockRequest> {
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
    let body = String::from_utf8_lossy(&buf[head_end..]).into_owned();
    let (path, query) = target.split_once('?').unwrap_or((&target, ""));
    Some(MockRequest {
        method,
        path: path.to_owned(),
        query: parse_query(query),
        headers,
        body,
    })
}

fn oauth_error(code: &str) -> Reply {
    Reply::json(
        400,
        json!({"error": code, "error_description": format!("mock: {code}")}),
    )
}

fn handle(st: &Mutex<State>, signer: &TestSigner, base: &str, req: MockRequest) -> Reply {
    let mut s = st.lock().expect("lock");
    s.requests.push(req.clone());
    s.counter += 1;
    let n = s.counter;
    match (req.method.as_str(), req.path.as_str()) {
        ("GET", "/api/accounts/authorize") => {
            let redirect = req.param("redirect_uri").unwrap_or_default().to_owned();
            let state = req.param("state").unwrap_or_default().to_owned();
            let mut q: Vec<(&str, String)> = vec![("state", state)];
            if s.config.deny {
                q.push(("error", "access_denied".into()));
            } else {
                let code = format!("code-{n}");
                let requested = req.param("client_id").unwrap_or_default();
                let client_id = if requested == DYNAMIC_CLIENT_ID {
                    q.push(("client_id", s.config.issued_client_id.clone()));
                    s.config.issued_client_id.clone()
                } else {
                    requested.to_owned()
                };
                s.codes.insert(
                    code.clone(),
                    Pending {
                        challenge: req.param("code_challenge").unwrap_or_default().to_owned(),
                        client_id,
                        redirect_uri: redirect.clone(),
                        nonce: req.param("nonce").unwrap_or_default().to_owned(),
                    },
                );
                q.push(("code", code));
            }
            let pairs: Vec<(&str, &str)> = q.iter().map(|(k, v)| (*k, v.as_str())).collect();
            Reply {
                status: 302,
                headers: vec![(
                    "location".into(),
                    format!("{redirect}?{}", super::form_encode(&pairs)),
                )],
                content_type: "text/plain",
                body: String::new(),
            }
        }
        ("POST", "/api/accounts/oauth/token") => {
            let form = |k: &str| req.form(k).unwrap_or_default();
            let scope = if s.config.grant_plan {
                super::SCOPES.to_owned()
            } else {
                super::SCOPES.replace(&format!(" {PLAN_SCOPE}"), "")
            };
            let ttl = s.config.access_ttl;
            match form("grant_type").as_str() {
                "authorization_code" => {
                    let Some(p) = s.codes.remove(&form("code")) else {
                        return oauth_error("invalid_grant");
                    };
                    if p.client_id != form("client_id")
                        || p.redirect_uri != form("redirect_uri")
                        || p.challenge != challenge_for(&form("code_verifier"))
                    {
                        return oauth_error("invalid_grant");
                    }
                    let (access, refresh) = (format!("at-{n}"), format!("rt-{n}"));
                    s.access = Some(access.clone());
                    s.refresh = Some(refresh.clone());
                    let id_token = signer.sign(&json!({
                        "iss": base, "aud": p.client_id, "sub": "user-123",
                        "email": "user@example.com", "exp": unix_now() + 3600, "nonce": p.nonce,
                    }));
                    Reply::json(
                        200,
                        json!({"access_token": access, "refresh_token": refresh, "id_token": id_token,
                               "token_type": "Bearer", "expires_in": ttl, "scope": scope}),
                    )
                }
                "refresh_token" => {
                    if let Some(code) = s.config.refresh_error.clone() {
                        return oauth_error(&code);
                    }
                    if s.refresh.as_deref() != Some(form("refresh_token").as_str()) {
                        return oauth_error("refresh_token_reused");
                    }
                    let (access, refresh) = (format!("at-{n}"), format!("rt-{n}"));
                    s.access = Some(access.clone());
                    s.refresh = Some(refresh.clone());
                    Reply::json(
                        200,
                        json!({"access_token": access, "refresh_token": refresh,
                               "token_type": "Bearer", "expires_in": ttl, "scope": scope}),
                    )
                }
                _ => oauth_error("unsupported_grant_type"),
            }
        }
        ("GET", "/.well-known/jwks.json") => Reply::json(200, signer.jwks()),
        ("POST", "/v1/responses") => {
            let auth = req.header("authorization").unwrap_or_default();
            if s.access
                .as_deref()
                .map(|a| format!("Bearer {a}"))
                .as_deref()
                != Some(auth)
            {
                return Reply::json(
                    401,
                    json!({"error": {"code": "subscription_sharing_invalid_user", "message": "invalid token"}}),
                );
            }
            if s.config.usage_limited {
                return Reply::json(
                    429,
                    json!({"error": {"code": USAGE_LIMIT_CODE, "message": "You have reached your ChatGPT usage limit for this app"}}),
                );
            }
            let events = [
                json!({"type": "response.created", "response": {"id": "resp_mock", "model": "gpt-5"}}),
                json!({"type": "response.output_text.delta", "output_index": 0, "content_index": 0, "delta": "hello from your plan"}),
                json!({"type": "response.completed", "response": {"id": "resp_mock", "status": "completed",
                    "usage": {"input_tokens": 3, "output_tokens": 4}}}),
            ];
            Reply {
                status: 200,
                headers: Vec::new(),
                content_type: "text/event-stream",
                body: events.iter().map(|e| format!("data: {e}\n\n")).collect(),
            }
        }
        _ => Reply::json(404, json!({"error": "not_found"})),
    }
}

/// A [`Browser`] that follows the authorization URL like a user who
/// approves (or declines, per the mock) the consent page: it requests the
/// URL and follows the redirect to the loopback callback.
#[derive(Debug, Clone, Default)]
pub struct MockBrowser {
    opened: Arc<Mutex<Vec<String>>>,
    /// Replace the callback's `state` with this value (a forged callback).
    pub forge_state: Option<String>,
}

impl MockBrowser {
    /// A browser that follows redirects as given.
    pub fn new() -> Self {
        Self::default()
    }

    /// A browser that replaces the callback's `state` with `state`.
    pub fn forging_state(state: &str) -> Self {
        Self {
            forge_state: Some(state.to_owned()),
            ..Self::default()
        }
    }

    /// Every URL opened so far.
    pub fn opened(&self) -> Vec<String> {
        self.opened.lock().expect("lock").clone()
    }
}

impl Browser for MockBrowser {
    fn open(&self, url: &str) -> Result<(), String> {
        self.opened.lock().expect("lock").push(url.to_owned());
        let (url, forge) = (url.to_owned(), self.forge_state.clone());
        tokio::spawn(async move {
            let client = reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .expect("client");
            let Ok(resp) = client.get(&url).send().await else {
                return;
            };
            let Some(mut location) = resp
                .headers()
                .get("location")
                .and_then(|v| v.to_str().ok())
                .map(str::to_owned)
            else {
                return;
            };
            if let Some(f) = forge {
                let (base, q) = location.split_once('?').unwrap_or((&location, ""));
                let pairs: Vec<(String, String)> = parse_query(q)
                    .into_iter()
                    .map(|(k, v)| if k == "state" { (k, f.clone()) } else { (k, v) })
                    .collect();
                let refs: Vec<(&str, &str)> = pairs
                    .iter()
                    .map(|(k, v)| (k.as_str(), v.as_str()))
                    .collect();
                location = format!("{base}?{}", super::form_encode(&refs));
            }
            let _ = client.get(&location).send().await;
        });
        Ok(())
    }
}
