//! The browser authorization (`harness login mcp <name>`): discovery,
//! client registration, authorization code with PKCE and a loopback
//! redirect, and the code exchange.

use std::time::Duration;

use harness_config::ini::{McpServer, McpTransport};
use harness_providers::chatgpt::pkce::{Pkce, random_token};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use super::discovery::{self, AuthServer, Challenge};
use super::session::{TokenFailure, forget_cached, token_call, tokens_from};
use super::store::{ClientRegistration, McpAuthRecord, McpTokenStore, Registration};
use super::{
    AUTHORIZE_TIMEOUT, AuthError, Browser, CALLBACK_PATH, CLIENT_NAME, Token, canonical_resource,
    form_encode, http_client, parse_query, scopes_of, secure_endpoint, union_into, unix_now,
};

/// One browser authorization for a remote MCP server.
#[derive(Debug, Clone)]
pub struct Authorize {
    /// Server name (`[mcp.<name>]`).
    pub server: String,
    /// The MCP endpoint.
    pub url: String,
    /// `oauth_client_id`.
    pub client_id: Option<String>,
    /// `oauth_client_metadata_url`.
    pub client_metadata_url: Option<String>,
    /// `oauth_scopes`.
    pub scopes: Option<Vec<String>>,
    /// Where the registration and tokens go.
    pub store: McpTokenStore,
    /// Loopback port (`0`: any free port).
    pub port: u16,
    /// How long to wait for the browser.
    pub timeout: Duration,
}

/// What a successful authorization established.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Authorized {
    /// The authorization server.
    pub issuer: String,
    /// How the client id was obtained.
    pub registration: Registration,
    /// Granted scopes.
    pub scopes: Vec<String>,
    /// A refresh token was issued.
    pub refreshable: bool,
}

impl Authorize {
    /// An authorization for `[mcp.<server>]`; fails for a stdio server.
    pub fn new(server: &str, config: &McpServer, store: McpTokenStore) -> Result<Self, AuthError> {
        let McpTransport::Http { url } = &config.transport else {
            return Err(AuthError::Discovery(format!(
                "[mcp.{server}] is a stdio server; OAuth is only for remote servers (url). Give a stdio server its credentials with env.*"
            )));
        };
        Ok(Self {
            server: server.to_owned(),
            url: url.clone(),
            client_id: config.oauth_client_id.clone(),
            client_metadata_url: config.oauth_client_metadata_url.clone(),
            scopes: config.oauth_scopes.clone(),
            store,
            port: 0,
            timeout: AUTHORIZE_TIMEOUT,
        })
    }

    /// Waits at most `timeout` for the browser.
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Runs the flow: discover, register, open `browser` at the
    /// authorization URL, wait for the callback, exchange the code and
    /// save the tokens.
    pub async fn run(&self, browser: &dyn Browser) -> Result<Authorized, AuthError> {
        let resource = canonical_resource(&self.url)
            .ok_or_else(|| AuthError::Discovery(format!("`{}` is not a valid URL", self.url)))?;
        if !secure_endpoint(&self.url) {
            return Err(AuthError::InsecureUrl {
                server: self.server.clone(),
                url: self.url.clone(),
            });
        }
        let http = http_client();
        let challenge = discovery::probe(&http, &self.url).await?;
        let prm = discovery::protected_resource(&http, &self.url, challenge.as_ref()).await?;
        let issuer = prm.authorization_servers[0].clone();
        let meta = discovery::auth_server(&http, &issuer).await?;

        // A registration is reused only with the issuer that made it.
        let previous = self
            .store
            .load(&self.server)?
            .filter(|r| r.issuer == meta.issuer);

        let (listener, client) = self.client(&http, &meta, previous.as_ref()).await?;
        let port = listener
            .local_addr()
            .map_err(|e| AuthError::Port(e.to_string()))?
            .port();
        let redirect_uri = format!("http://127.0.0.1:{port}{CALLBACK_PATH}");

        let scopes = self.requested_scopes(
            challenge.as_ref(),
            prm.scopes_supported.as_deref(),
            &meta,
            previous.as_ref(),
        );
        // Save the registration first, so a cancelled attempt does not
        // register again next time; tokens for the same URL are kept.
        let mut record = McpAuthRecord {
            resource: resource.clone(),
            resource_indicator: prm.resource.clone(),
            issuer: meta.issuer.clone(),
            token_endpoint: meta.token_endpoint.clone(),
            client: client.clone(),
            scopes: previous
                .as_ref()
                .map(|p| p.scopes.clone())
                .unwrap_or_default(),
            step_up_scopes: previous
                .as_ref()
                .map(|p| p.step_up_scopes.clone())
                .unwrap_or_default(),
            tokens: previous
                .as_ref()
                .filter(|p| p.resource == resource)
                .and_then(|p| p.tokens.clone()),
        };
        self.store.save(&self.server, &record)?;

        let state = random_token(32);
        let pkce = Pkce::new();
        let scope_param = scopes.join(" ");
        let mut query: Vec<(&str, &str)> = vec![
            ("response_type", "code"),
            ("client_id", client.client_id.as_str()),
            ("redirect_uri", redirect_uri.as_str()),
            ("state", state.as_str()),
            ("code_challenge", pkce.challenge.as_str()),
            ("code_challenge_method", "S256"),
            ("resource", prm.resource.as_str()),
        ];
        if !scope_param.is_empty() {
            query.push(("scope", scope_param.as_str()));
        }
        let sep = if meta.authorization_endpoint.contains('?') {
            '&'
        } else {
            '?'
        };
        let url = format!(
            "{}{sep}{}",
            meta.authorization_endpoint,
            form_encode(&query)
        );
        browser
            .open(&url)
            .map_err(|e| AuthError::Browser(e, url.clone()))?;

        let params = tokio::time::timeout(self.timeout, accept_callback(&listener, &state, &meta))
            .await
            .map_err(|_| AuthError::Timeout(self.timeout))??;
        let code = params
            .iter()
            .find(|(k, _)| k == "code")
            .map(|(_, v)| v.clone())
            .filter(|c| !c.is_empty())
            .ok_or_else(|| AuthError::OAuth {
                code: "missing_code".into(),
                message: "the callback had no authorization code".into(),
            })?;

        let mut form = vec![
            ("grant_type", "authorization_code"),
            ("code", code.as_str()),
            ("redirect_uri", redirect_uri.as_str()),
            ("client_id", client.client_id.as_str()),
            ("code_verifier", pkce.verifier()),
            ("resource", prm.resource.as_str()),
        ];
        if let Some(s) = &client.client_secret {
            form.push(("client_secret", s.expose()));
        }
        let t = token_call(&http, &meta.token_endpoint, &form)
            .await
            .map_err(|f| match f {
                TokenFailure::OAuth { code, message } => AuthError::OAuth { code, message },
                TokenFailure::Network(m) => AuthError::Network(m),
            })?;
        let tokens = tokens_from(t, None, &scopes, unix_now());
        let out = Authorized {
            issuer: meta.issuer.clone(),
            registration: client.kind,
            scopes: tokens.scopes.clone(),
            refreshable: tokens.refresh_token.is_some(),
        };
        record.scopes = scopes;
        record.step_up_scopes.clear();
        record.tokens = Some(tokens);
        self.store.save(&self.server, &record)?;
        forget_cached(self.store.dir(), &self.server);
        tracing::info!(server = %self.server, issuer = %out.issuer, "MCP server authorized");
        Ok(out)
    }

    /// The scopes to request: `oauth_scopes`, else the challenge's `scope`,
    /// else the resource's `scopes_supported`; plus scopes granted or asked
    /// for before, and `offline_access` when the server offers it.
    fn requested_scopes(
        &self,
        challenge: Option<&Challenge>,
        supported: Option<&[String]>,
        meta: &AuthServer,
        previous: Option<&McpAuthRecord>,
    ) -> Vec<String> {
        if let Some(s) = &self.scopes {
            return s.clone();
        }
        let mut out = match challenge.and_then(|c| c.scope.as_deref()) {
            Some(s) => scopes_of(s),
            None => supported.map(<[String]>::to_vec).unwrap_or_default(),
        };
        if let Some(p) = previous {
            union_into(&mut out, &p.scopes);
            union_into(&mut out, &p.step_up_scopes);
        }
        let offline = "offline_access".to_owned();
        if meta
            .scopes_supported
            .as_ref()
            .is_some_and(|s| s.contains(&offline))
            && !out.is_empty()
        {
            union_into(&mut out, &[offline]);
        }
        out
    }

    /// The client to use, in the spec's priority order, and the listener
    /// for its redirect URI.
    async fn client(
        &self,
        http: &reqwest::Client,
        meta: &AuthServer,
        previous: Option<&McpAuthRecord>,
    ) -> Result<(TcpListener, ClientRegistration), AuthError> {
        if let Some(id) = &self.client_id {
            return Ok((
                listen(self.port).await?,
                ClientRegistration {
                    client_id: id.clone(),
                    client_secret: None,
                    kind: Registration::PreRegistered,
                    redirect_uri: None,
                },
            ));
        }
        if let Some(url) = &self.client_metadata_url
            && meta.client_id_metadata_document_supported
        {
            return Ok((
                listen(self.port).await?,
                ClientRegistration {
                    client_id: url.clone(),
                    client_secret: None,
                    kind: Registration::MetadataDocument,
                    redirect_uri: None,
                },
            ));
        }
        // Reuse an earlier dynamic registration when its port is free.
        if let Some(p) = previous
            && p.client.kind == Registration::Dynamic
            && let Some(port) = p
                .client
                .redirect_uri
                .as_deref()
                .and_then(|u| reqwest::Url::parse(u).ok())
                .and_then(|u| u.port())
            && let Ok(l) = TcpListener::bind(("127.0.0.1", port)).await
        {
            return Ok((l, p.client.clone()));
        }
        let Some(endpoint) = &meta.registration_endpoint else {
            return Err(AuthError::Registration(format!(
                "the authorization server `{}` supports neither dynamic client registration nor{} client metadata documents. Register harness with it (redirect URI http://127.0.0.1/callback, any port) and set oauth_client_id in [mcp.{}]",
                meta.issuer,
                if self.client_metadata_url.is_some() {
                    ""
                } else {
                    " (without oauth_client_metadata_url)"
                },
                self.server
            )));
        };
        let listener = listen(self.port).await?;
        let port = listener
            .local_addr()
            .map_err(|e| AuthError::Port(e.to_string()))?
            .port();
        let redirect_uri = format!("http://127.0.0.1:{port}{CALLBACK_PATH}");
        let body = json!({
            "client_name": CLIENT_NAME,
            "redirect_uris": [redirect_uri],
            "grant_types": ["authorization_code", "refresh_token"],
            "response_types": ["code"],
            "token_endpoint_auth_method": "none",
            "application_type": "native",
        });
        let resp = http
            .post(endpoint)
            .header("accept", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| AuthError::Network(e.without_url().to_string()))?;
        let status = resp.status();
        let text = zeroize::Zeroizing::new(resp.text().await.unwrap_or_default());
        let v: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
        if !status.is_success() {
            let why = v["error_description"]
                .as_str()
                .or_else(|| v["error"].as_str())
                .unwrap_or("")
                .chars()
                .take(200)
                .collect::<String>();
            return Err(AuthError::Registration(format!(
                "dynamic client registration with `{}` failed (HTTP {}{}); register harness yourself and set oauth_client_id in [mcp.{}]",
                meta.issuer,
                status.as_u16(),
                if why.is_empty() {
                    String::new()
                } else {
                    format!(": {why}")
                },
                self.server
            )));
        }
        let client_id = v["client_id"]
            .as_str()
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                AuthError::Registration("dynamic client registration returned no client_id".into())
            })?;
        Ok((
            listener,
            ClientRegistration {
                client_id: client_id.to_owned(),
                client_secret: v["client_secret"].as_str().map(Token::new),
                kind: Registration::Dynamic,
                redirect_uri: Some(redirect_uri),
            },
        ))
    }
}

async fn listen(port: u16) -> Result<TcpListener, AuthError> {
    TcpListener::bind(("127.0.0.1", port))
        .await
        .map_err(|e| AuthError::Port(e.to_string()))
}

const PAGE_OK: &str = "harness is now authorized for this MCP server. You can close this tab and return to the terminal.";

/// Accepts connections until one is the callback for `state`, and checks
/// its `iss` (RFC 9207) before anything else in it is used.
async fn accept_callback(
    listener: &TcpListener,
    state: &str,
    meta: &AuthServer,
) -> Result<Vec<(String, String)>, AuthError> {
    loop {
        let (mut sock, _) = listener
            .accept()
            .await
            .map_err(|e| AuthError::Network(e.to_string()))?;
        let Some(target) = read_target(&mut sock).await else {
            continue;
        };
        let (path, query) = target.split_once('?').unwrap_or((&target, ""));
        if path != CALLBACK_PATH {
            respond(&mut sock, 404, "Not found.").await;
            continue;
        }
        let params = parse_query(query);
        let get = |k: &str| params.iter().find(|(n, _)| n == k).map(|(_, v)| v.as_str());
        if get("state") != Some(state) {
            respond(
                &mut sock,
                400,
                "This authorization link is stale. Start again from harness.",
            )
            .await;
            return Err(AuthError::StateMismatch);
        }
        match get("iss") {
            Some(iss) if iss != meta.issuer => {
                respond(
                    &mut sock,
                    400,
                    "Authorization failed. See harness for details.",
                )
                .await;
                return Err(AuthError::IssuerMismatch(meta.issuer.clone()));
            }
            None if meta.authorization_response_iss_parameter_supported => {
                respond(
                    &mut sock,
                    400,
                    "Authorization failed. See harness for details.",
                )
                .await;
                return Err(AuthError::IssuerMismatch(meta.issuer.clone()));
            }
            _ => {}
        }
        if let Some(err) = get("error") {
            if err == "access_denied" {
                respond(
                    &mut sock,
                    200,
                    "Authorization was declined. Nothing was saved; you can close this tab.",
                )
                .await;
                return Err(AuthError::Denied);
            }
            let e = AuthError::OAuth {
                code: err.to_owned(),
                message: get("error_description")
                    .unwrap_or("")
                    .chars()
                    .take(200)
                    .collect(),
            };
            respond(
                &mut sock,
                400,
                "Authorization failed. See harness for details.",
            )
            .await;
            return Err(e);
        }
        respond(&mut sock, 200, PAGE_OK).await;
        return Ok(params);
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
