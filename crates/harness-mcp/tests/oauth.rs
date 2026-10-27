//! OAuth for remote MCP servers against the local mock authorization
//! server (`harness_mcp::auth::mock`): success (dynamic and pre-registered
//! clients), denied consent, refresh, expired refresh token, revoked
//! token, step-up scopes, issuer checks, and the refusal to authorize
//! interactively outside `harness login mcp`; and the device authorization
//! grant (RFC 8628): success, polling intervals, denial, expiry, timeout,
//! cancel and the loopback fallback.

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::{fast_opts, inline};
use harness_config::ini::{McpAuth, McpServer};
use harness_mcp::auth::mock::{
    MOCK_SCOPE, MockBrowser, MockConfig, MockOAuthServer, PRE_REGISTERED_CLIENT,
};
use harness_mcp::auth::{
    AuthError, Authorize, ClientRegistration, McpAuthRecord, McpTokenStore, McpTokens,
    OAuthSession, Registration, Token,
};
use harness_mcp::test_server::{HttpServer, Options};
use harness_mcp::{ClientOptions, McpError, ServerHandle};

struct Setup {
    mock: MockOAuthServer,
    _dir: tempfile::TempDir,
    store: McpTokenStore,
    config: McpServer,
}

async fn setup(config: MockConfig) -> Setup {
    let mock = MockOAuthServer::start(config).await;
    let dir = tempfile::tempdir().unwrap();
    let store = McpTokenStore::new(dir.path().join("auth/mcp"));
    let mut cfg = McpServer::http(mock.mcp_url());
    cfg.auth = McpAuth::OAuth;
    cfg.timeout = Duration::from_secs(20);
    Setup {
        mock,
        _dir: dir,
        store,
        config: cfg,
    }
}

impl Setup {
    fn authorize(&self) -> Authorize {
        Authorize::new("docs", &self.config, self.store.clone())
            .unwrap()
            .with_timeout(Duration::from_secs(20))
    }

    fn opts(&self) -> ClientOptions {
        ClientOptions {
            auth_dir: Some(self.store.dir().to_path_buf()),
            ..fast_opts()
        }
    }

    fn handle(&self) -> ServerHandle {
        ServerHandle::new("docs", self.config.clone(), Arc::new(self.opts()))
    }

    fn stored_access(&self) -> Option<String> {
        self.store
            .load("docs")
            .unwrap()
            .and_then(|r| r.tokens)
            .map(|t| t.access_token.expose().to_owned())
    }
}

fn param(url: &str, k: &str) -> Option<String> {
    reqwest::Url::parse(url)
        .unwrap()
        .query_pairs()
        .find(|(n, _)| n == k)
        .map(|(_, v)| v.into_owned())
}

#[tokio::test]
async fn authorizes_with_dynamic_registration_and_uses_the_token() {
    let s = setup(MockConfig::default()).await;
    let browser = MockBrowser::new();
    let done = s.authorize().run(&browser).await.unwrap();
    assert_eq!(done.registration, Registration::Dynamic);
    assert_eq!(done.issuer, s.mock.base_url);
    assert!(done.refreshable);

    // The authorization request: PKCE S256, state, resource, scope and a
    // loopback redirect.
    let url = &browser.opened()[0];
    assert_eq!(param(url, "code_challenge_method").as_deref(), Some("S256"));
    assert_eq!(param(url, "code_challenge").unwrap().len(), 43);
    assert!(param(url, "state").unwrap().len() >= 32);
    assert_eq!(param(url, "resource"), Some(s.mock.mcp_url()));
    assert_eq!(
        param(url, "scope").as_deref(),
        Some(format!("{MOCK_SCOPE} offline_access").as_str())
    );
    assert!(
        param(url, "redirect_uri")
            .unwrap()
            .starts_with("http://127.0.0.1:")
    );
    let reg = s.mock.requests_to("/register");
    assert_eq!(reg.len(), 1);
    assert!(
        reg[0].body.contains("\"application_type\":\"native\""),
        "{}",
        reg[0].body
    );
    let token_req = &s.mock.requests_to("/token")[0];
    assert_eq!(
        token_req.param("grant_type").as_deref(),
        Some("authorization_code")
    );
    assert_eq!(token_req.param("resource"), Some(s.mock.mcp_url()));
    assert!(token_req.param("code_verifier").is_some());

    // The token file is private.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(s.store.path("docs"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
    }

    // MCP requests carry the token on every request.
    let h = s.handle();
    let tools = h.list_tools().await.unwrap();
    assert!(!tools.is_empty());
    let access = s.stored_access().unwrap();
    let mcp = s.mock.requests_to("/mcp");
    let authorized: Vec<_> = mcp
        .iter()
        .filter(|r| r.header("authorization") == Some(format!("Bearer {access}").as_str()))
        .collect();
    assert!(authorized.len() >= 2, "{mcp:?}");
    h.shutdown().await;

    // A second login reuses the dynamic registration for the same issuer.
    s.authorize().run(&MockBrowser::new()).await.unwrap();
    assert_eq!(s.mock.requests_to("/register").len(), 1);
    assert_eq!(s.mock.dynamic_clients().len(), 1);
}

#[tokio::test]
async fn pre_registered_client_and_missing_registration() {
    let mut s = setup(MockConfig {
        dynamic_registration: false,
        ..MockConfig::default()
    })
    .await;
    let e = s.authorize().run(&MockBrowser::new()).await.unwrap_err();
    assert!(matches!(e, AuthError::Registration(_)), "{e}");
    assert!(e.to_string().contains("oauth_client_id"), "{e}");
    assert!(s.stored_access().is_none());

    s.config.oauth_client_id = Some(PRE_REGISTERED_CLIENT.into());
    let done = s.authorize().run(&MockBrowser::new()).await.unwrap();
    assert_eq!(done.registration, Registration::PreRegistered);
    assert!(s.handle().list_tools().await.is_ok());
}

#[tokio::test]
async fn denied_consent_saves_no_tokens() {
    let s = setup(MockConfig {
        deny: true,
        ..MockConfig::default()
    })
    .await;
    let e = s.authorize().run(&MockBrowser::new()).await.unwrap_err();
    assert_eq!(e, AuthError::Denied);
    assert!(s.stored_access().is_none());
    assert!(s.mock.requests_to("/token").is_empty());
}

#[tokio::test]
async fn expiring_tokens_are_refreshed_with_rotation() {
    // 30 s lifetime is inside the 60 s refresh skew: the first use refreshes.
    let s = setup(MockConfig {
        expires_in: 30,
        ..MockConfig::default()
    })
    .await;
    s.authorize().run(&MockBrowser::new()).await.unwrap();
    let first = s.store.load("docs").unwrap().unwrap().tokens.unwrap();
    let h = s.handle();
    h.list_tools().await.unwrap();
    let refreshes: Vec<_> = s
        .mock
        .requests_to("/token")
        .into_iter()
        .filter(|r| r.param("grant_type").as_deref() == Some("refresh_token"))
        .collect();
    assert!(!refreshes.is_empty());
    assert_eq!(
        refreshes[0].param("refresh_token").as_deref(),
        first.refresh_token.as_ref().map(|t| t.expose())
    );
    assert_eq!(refreshes[0].param("resource"), Some(s.mock.mcp_url()));
    let now = s.store.load("docs").unwrap().unwrap().tokens.unwrap();
    assert_ne!(now.access_token, first.access_token);
    assert_ne!(now.refresh_token, first.refresh_token, "rotated");
    h.shutdown().await;
}

#[tokio::test]
async fn expired_refresh_token_clears_tokens_and_says_how_to_reauthorize() {
    let s = setup(MockConfig {
        expires_in: 30,
        refresh_error: Some("invalid_grant".into()),
        ..MockConfig::default()
    })
    .await;
    s.authorize().run(&MockBrowser::new()).await.unwrap();
    let e = s.handle().list_tools().await.unwrap_err();
    assert!(matches!(e, McpError::Auth { .. }), "{e}");
    let msg = e.to_string();
    assert!(msg.contains("expired or was revoked"), "{msg}");
    assert!(msg.contains("harness login mcp docs"), "{msg}");
    let r = s.store.load("docs").unwrap().unwrap();
    assert!(r.tokens.is_none(), "tokens cleared");
    assert!(!r.client.client_id.is_empty(), "registration kept");
}

#[tokio::test]
async fn revoked_access_token_is_refreshed_and_a_revoked_grant_asks_to_reauthorize() {
    let s = setup(MockConfig::default()).await;
    s.authorize().run(&MockBrowser::new()).await.unwrap();
    let before = s.stored_access().unwrap();
    let h = s.handle();
    h.list_tools().await.unwrap();

    // The access token is revoked: 401, refresh, retry.
    s.mock.revoke_access_tokens();
    h.list_tools().await.unwrap();
    let after = s.stored_access().unwrap();
    assert_ne!(before, after);

    // The whole grant is revoked: the refresh fails for good.
    s.mock.revoke_all();
    let e = h.list_tools().await.unwrap_err();
    assert!(matches!(e, McpError::Auth { .. }), "{e}");
    assert!(e.to_string().contains("harness login mcp docs"), "{e}");
    assert!(s.stored_access().is_none());
    h.shutdown().await;
}

#[tokio::test]
async fn runs_never_authorize_interactively() {
    let s = setup(MockConfig::default()).await;
    let e = s.handle().list_tools().await.unwrap_err();
    assert!(matches!(e, McpError::Auth { .. }), "{e}");
    let msg = e.to_string();
    assert!(msg.contains("needs OAuth authorization"), "{msg}");
    assert!(msg.contains("harness login mcp docs"), "{msg}");
    assert!(msg.contains("never open a browser"), "{msg}");
    assert!(
        harness_mcp::doctor::fix_hint("docs", &e).contains("harness login mcp docs"),
        "doctor says how to authorize"
    );
    assert!(s.mock.requests_to("/authorize").is_empty());
    assert!(s.mock.requests_to("/register").is_empty());
    assert!(
        s.mock.requests_to("/mcp").is_empty(),
        "no token, no request"
    );

    // Without a token store (an eval or embedder that gave none) too.
    let h = ServerHandle::new("docs", s.config.clone(), Arc::new(fast_opts()));
    let e = h.list_tools().await.unwrap_err();
    assert!(e.to_string().contains("harness login mcp docs"), "{e}");
}

#[tokio::test]
async fn oauth_server_without_auth_oauth_explains_the_setting() {
    let s = setup(MockConfig::default()).await;
    let mut cfg = s.config.clone();
    cfg.auth = McpAuth::None;
    let h = ServerHandle::new("docs", cfg, Arc::new(s.opts()));
    let e = h.list_tools().await.unwrap_err();
    let msg = e.to_string();
    assert!(msg.contains("set auth = oauth in [mcp.docs]"), "{msg}");
    assert!(msg.contains("harness login mcp docs"), "{msg}");
}

#[tokio::test]
async fn static_bearer_header_keeps_working() {
    let srv = HttpServer::start_with(
        Options::default(),
        Some(("Authorization", "Bearer static-key")),
    )
    .await;
    let mut cfg = McpServer::http(&srv.url);
    cfg.timeout = Duration::from_secs(20);
    cfg.headers
        .insert("Authorization".into(), inline("Bearer static-key"));
    let h = ServerHandle::new("static", cfg, Arc::new(fast_opts()));
    assert!(!h.list_tools().await.unwrap().is_empty());
    h.shutdown().await;
}

#[tokio::test]
async fn well_known_fallback_without_resource_metadata() {
    let s = setup(MockConfig {
        challenge_header: false,
        ..MockConfig::default()
    })
    .await;
    s.authorize().run(&MockBrowser::new()).await.unwrap();
    assert!(
        !s.mock
            .requests_to("/.well-known/oauth-protected-resource/mcp")
            .is_empty()
    );
    assert!(s.handle().list_tools().await.is_ok());
}

#[tokio::test]
async fn issuer_mismatches_are_refused() {
    let s = setup(MockConfig {
        wrong_issuer: Some("https://honest.example".into()),
        ..MockConfig::default()
    })
    .await;
    let e = s.authorize().run(&MockBrowser::new()).await.unwrap_err();
    assert!(e.to_string().contains("claims issuer"), "{e}");
    assert!(s.mock.requests_to("/authorize").is_empty());

    let s = setup(MockConfig {
        wrong_callback_iss: Some("https://evil.example".into()),
        ..MockConfig::default()
    })
    .await;
    let e = s.authorize().run(&MockBrowser::new()).await.unwrap_err();
    assert!(matches!(e, AuthError::IssuerMismatch(_)), "{e}");
    assert!(
        s.mock.requests_to("/token").is_empty(),
        "the code went nowhere"
    );
    assert!(s.stored_access().is_none());
}

#[tokio::test]
async fn insufficient_scope_is_remembered_for_the_next_login() {
    let s = setup(MockConfig {
        required_scope: Some("mcp:admin".into()),
        ..MockConfig::default()
    })
    .await;
    s.authorize().run(&MockBrowser::new()).await.unwrap();
    let e = s.handle().list_tools().await.unwrap_err();
    let msg = e.to_string();
    assert!(
        msg.contains("needs more OAuth permissions (scope: mcp:admin)"),
        "{msg}"
    );
    assert_eq!(
        s.store.load("docs").unwrap().unwrap().step_up_scopes,
        ["mcp:admin"]
    );
    let browser = MockBrowser::new();
    s.authorize().run(&browser).await.unwrap();
    let scope = param(&browser.opened()[0], "scope").unwrap();
    assert!(scope.split(' ').any(|x| x == "mcp:admin"), "{scope}");
    assert!(scope.split(' ').any(|x| x == MOCK_SCOPE), "{scope}");
    assert!(s.handle().list_tools().await.is_ok());
}

#[tokio::test]
async fn concurrent_rejections_refresh_once() {
    let s = setup(MockConfig::default()).await;
    s.authorize().run(&MockBrowser::new()).await.unwrap();
    let (a, b) = (s.handle(), s.handle());
    a.list_tools().await.unwrap();
    b.list_tools().await.unwrap();
    s.mock.revoke_access_tokens();
    let (ra, rb) = tokio::join!(a.list_tools(), b.list_tools());
    ra.unwrap();
    rb.unwrap();
    let refreshes = s
        .mock
        .requests_to("/token")
        .into_iter()
        .filter(|r| r.param("grant_type").as_deref() == Some("refresh_token"))
        .count();
    assert_eq!(refreshes, 1, "the rotating refresh token is spent once");
    a.shutdown().await;
    b.shutdown().await;
}

/// A plain `http://` URL on a non-loopback host never gets a bearer token:
/// login refuses before any request, a run with stored tokens refuses
/// before attaching them, and discovery skips cleartext metadata even on
/// the server's own origin. (`.invalid` never resolves, so a request that
/// slipped through would fail as a network error instead.)
#[tokio::test]
async fn oauth_refuses_cleartext_http_to_remote_hosts() {
    const URL: &str = "http://mcp.example.invalid/mcp";
    let dir = tempfile::tempdir().unwrap();
    let store = McpTokenStore::new(dir.path().join("auth/mcp"));
    let mut cfg = McpServer::http(URL);
    cfg.auth = McpAuth::OAuth;

    let browser = MockBrowser::new();
    let e = Authorize::new("docs", &cfg, store.clone())
        .unwrap()
        .with_timeout(Duration::from_secs(5))
        .run(&browser)
        .await
        .unwrap_err();
    assert!(matches!(e, AuthError::InsecureUrl { .. }), "{e}");
    assert!(e.to_string().contains("https://"), "{e}");
    assert!(browser.opened().is_empty());
    assert!(store.load("docs").unwrap().is_none());

    // Tokens stored for this URL (say, from before the url was edited)
    // are not sent.
    let record = McpAuthRecord {
        resource: harness_mcp::auth::canonical_resource(URL).unwrap(),
        resource_indicator: URL.into(),
        issuer: "https://auth.example.invalid".into(),
        token_endpoint: "https://auth.example.invalid/token".into(),
        client: ClientRegistration {
            client_id: "c".into(),
            client_secret: None,
            kind: Registration::Dynamic,
            redirect_uri: None,
            device_grant: false,
        },
        scopes: vec![],
        step_up_scopes: vec![],
        tokens: Some(McpTokens {
            access_token: Token::new("at-cleartext-secret"),
            refresh_token: None,
            expires_at: None,
            scopes: vec![],
        }),
    };
    store.save("docs", &record).unwrap();
    let session = OAuthSession::new(store.clone(), "docs", URL);
    let e = session.access_token().await.unwrap_err();
    assert!(matches!(e, AuthError::InsecureUrl { .. }), "{e}");

    let opts = ClientOptions {
        auth_dir: Some(store.dir().to_path_buf()),
        ..fast_opts()
    };
    let h = ServerHandle::new("docs", cfg, Arc::new(opts));
    let e = h.list_tools().await.unwrap_err();
    assert!(matches!(e, McpError::Auth { .. }), "{e}");
    assert!(e.to_string().contains("not https://"), "{e}");
    assert!(!e.to_string().contains("at-cleartext-secret"), "{e}");
    h.shutdown().await;

    // Same-origin cleartext Protected Resource Metadata is not fetched,
    // whether from the well-known URLs or a challenge.
    let http = reqwest::Client::new();
    let e = harness_mcp::auth::discovery::protected_resource(&http, URL, None)
        .await
        .unwrap_err();
    assert!(matches!(e, AuthError::Discovery(_)), "{e}");
    let challenge = harness_mcp::auth::discovery::Challenge {
        resource_metadata: Some(
            "http://mcp.example.invalid/.well-known/oauth-protected-resource".into(),
        ),
        ..Default::default()
    };
    let e = harness_mcp::auth::discovery::protected_resource(&http, URL, Some(&challenge))
        .await
        .unwrap_err();
    assert!(matches!(e, AuthError::Discovery(_)), "{e}");
}

/// Pastes `text` into a running authorization and returns its answer.
async fn paste(tx: &harness_providers::signin::PasteSender, text: &str) -> Result<(), String> {
    let (p, reply) = harness_providers::signin::Paste::new(text);
    tx.send(p).unwrap();
    reply.await.unwrap()
}

#[tokio::test]
async fn a_remote_browser_finishes_by_pasting_the_address() {
    let s = setup(MockConfig::default()).await;
    let browser = MockBrowser::remote();
    let (tx, mut rx) = harness_providers::signin::paste_channel();
    let flow = s.authorize();
    let user = async {
        let address = browser.next_redirect().await;
        // A bare code cannot carry the issuer this server signs with.
        let code = param(&address, "code").unwrap();
        let e = paste(&tx, &code).await.unwrap_err();
        assert!(e.contains("whole address"), "{e}");
        let e = paste(&tx, &address.replace("state=", "state=old"))
            .await
            .unwrap_err();
        assert!(e.contains("another sign-in attempt"), "{e}");
        assert!(s.mock.requests_to("/token").is_empty());
        assert_eq!(paste(&tx, &address).await, Ok(()));
    };
    let (done, ()) = tokio::join!(flow.run_with(&browser, &mut rx), user);
    let done = done.unwrap();
    assert_eq!(done.registration, Registration::Dynamic);
    let token = &s.mock.requests_to("/token")[0];
    assert!(token.param("code_verifier").is_some());
    assert_eq!(
        token.param("redirect_uri"),
        param(&browser.opened()[0], "redirect_uri")
    );
    assert!(s.stored_access().is_some());
    assert!(s.handle().list_tools().await.is_ok());
}

#[tokio::test]
async fn a_pasted_address_from_another_issuer_is_refused() {
    let s = setup(MockConfig {
        wrong_callback_iss: Some("https://evil.example".into()),
        ..MockConfig::default()
    })
    .await;
    let browser = MockBrowser::remote();
    let (tx, mut rx) = harness_providers::signin::paste_channel();
    let flow = s.authorize();
    let user = async {
        let address = browser.next_redirect().await;
        assert_eq!(paste(&tx, &address).await, Ok(()));
    };
    let (r, ()) = tokio::join!(flow.run_with(&browser, &mut rx), user);
    assert!(matches!(r.unwrap_err(), AuthError::IssuerMismatch(_)));
    assert!(s.mock.requests_to("/token").is_empty());
    assert!(s.stored_access().is_none());
}

#[tokio::test]
async fn an_abandoned_authorization_times_out() {
    let s = setup(MockConfig::default()).await;
    let (_tx, mut rx) = harness_providers::signin::paste_channel();
    let e = s
        .authorize()
        .with_timeout(Duration::from_millis(300))
        .run_with(&MockBrowser::remote(), &mut rx)
        .await
        .unwrap_err();
    assert!(matches!(e, AuthError::Timeout(_)), "{e}");
    assert!(s.stored_access().is_none());
}

#[tokio::test]
async fn the_port_forward_carries_the_browser_back() {
    let s = setup(MockConfig::default()).await;
    let browser = MockBrowser::remote();
    let (_tx, mut rx) = harness_providers::signin::paste_channel();
    let flow = s.authorize();
    let user = async {
        let address = browser.next_redirect().await;
        let port = harness_providers::signin::redirect_port(&browser.opened()[0]).unwrap();
        // `ssh -L port:127.0.0.1:port`, with the browser's end on another
        // port because both ends share this machine in the test.
        let tunnel = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let local = tunnel.local_addr().unwrap().port();
        tokio::spawn(async move {
            while let Ok((mut a, _)) = tunnel.accept().await {
                tokio::spawn(async move {
                    if let Ok(mut b) = tokio::net::TcpStream::connect(("127.0.0.1", port)).await {
                        let _ = tokio::io::copy_bidirectional(&mut a, &mut b).await;
                    }
                });
            }
        });
        let through = address.replace(&format!(":{port}/"), &format!(":{local}/"));
        let page = reqwest::get(&through).await.unwrap();
        assert!(page.status().is_success());
    };
    let (r, ()) = tokio::join!(flow.run_with(&browser, &mut rx), user);
    r.unwrap();
    assert!(s.stored_access().is_some());
}

#[tokio::test]
async fn a_stale_callback_is_refused_and_the_authorization_keeps_waiting() {
    let s = setup(MockConfig::default()).await;
    let browser = MockBrowser::remote();
    let (_tx, mut rx) = harness_providers::signin::paste_channel();
    let flow = s.authorize();
    let user = async {
        let address = browser.next_redirect().await;
        let page = reqwest::get(address.replace("state=", "state=old"))
            .await
            .unwrap();
        assert_eq!(page.status().as_u16(), 400);
        assert!(page.text().await.unwrap().contains("stale"));
        let page = reqwest::get(&address).await.unwrap();
        assert!(page.status().is_success());
    };
    let (r, ()) = tokio::join!(flow.run_with(&browser, &mut rx), user);
    r.unwrap();
    assert!(s.stored_access().is_some());
}

// TASK-165: the device authorization grant (RFC 8628) for servers that
// advertise it, used when the browser is on another machine.

/// One of the server's seconds in these tests.
const UNIT: Duration = Duration::from_millis(20);

/// A screen on another device: records every device code shown and, when
/// `approve`, opens its link like a user who confirms the code. Loopback
/// links are only recorded.
#[derive(Clone, Default)]
struct Screen {
    approve: bool,
    shown: Arc<std::sync::Mutex<Vec<harness_providers::signin::DeviceCode>>>,
    opened: Arc<std::sync::Mutex<Vec<String>>>,
}

impl Screen {
    fn approving() -> Self {
        Self {
            approve: true,
            ..Self::default()
        }
    }

    fn shown(&self) -> Vec<harness_providers::signin::DeviceCode> {
        self.shown.lock().unwrap().clone()
    }
}

impl harness_mcp::auth::Browser for Screen {
    fn open(&self, url: &str) -> Result<(), String> {
        self.opened.lock().unwrap().push(url.to_owned());
        Ok(())
    }

    fn show_device_code(&self, code: &harness_providers::signin::DeviceCode) -> Result<(), String> {
        self.shown.lock().unwrap().push(code.clone());
        if self.approve {
            let link = code.link().to_owned();
            tokio::spawn(async move {
                let _ = reqwest::get(&link).await;
            });
        }
        Ok(())
    }
}

fn device_config() -> MockConfig {
    MockConfig {
        device_authorization: true,
        ..MockConfig::default()
    }
}

impl Setup {
    fn device(&self) -> Authorize {
        self.authorize()
            .with_device_grant(true)
            .with_poll_unit(UNIT)
    }
}

/// Times of the device-grant polls.
fn polls(mock: &MockOAuthServer) -> Vec<std::time::Instant> {
    mock.requests_to("/token")
        .into_iter()
        .filter(|r| {
            r.param("grant_type").as_deref() == Some(harness_mcp::auth::device::DEVICE_GRANT)
        })
        .map(|r| r.at)
        .collect()
}

#[tokio::test]
async fn device_grant_shows_the_code_and_stores_the_tokens() {
    let s = setup(device_config()).await;
    let screen = Screen::approving();
    let done = s.device().run(&screen).await.unwrap();
    assert_eq!(done.registration, Registration::Dynamic);
    assert!(done.refreshable);

    // No loopback redirect: nothing was opened, /authorize never asked.
    assert!(screen.opened.lock().unwrap().is_empty());
    assert!(s.mock.requests_to("/authorize").is_empty());
    let shown = screen.shown();
    assert_eq!(shown.len(), 1);
    let code = &shown[0];
    assert!(code.user_code.starts_with("WDJB-"));
    assert_eq!(code.verification_uri, format!("{}/device", s.mock.base_url));
    assert_eq!(
        code.link(),
        format!("{}/device?user_code={}", s.mock.base_url, *code.user_code)
    );
    assert_eq!(code.expires_in, 600);

    // The registration asks for the device grant; the device request and
    // the poll carry the client, the resource and (request only) scopes.
    let reg = &s.mock.requests_to("/register")[0];
    assert!(
        reg.body
            .contains("urn:ietf:params:oauth:grant-type:device_code"),
        "{}",
        reg.body
    );
    let dev = &s.mock.requests_to("/device_authorization")[0];
    assert_eq!(
        dev.param("client_id"),
        Some(s.mock.dynamic_clients()[0].clone())
    );
    assert_eq!(dev.param("resource"), Some(s.mock.mcp_url()));
    assert_eq!(
        dev.param("scope").as_deref(),
        Some(format!("{MOCK_SCOPE} offline_access").as_str())
    );
    let poll = s.mock.requests_to("/token").pop().unwrap();
    assert!(
        poll.param("device_code")
            .unwrap()
            .starts_with("device-secret-")
    );
    assert_eq!(poll.param("resource"), Some(s.mock.mcp_url()));
    assert!(poll.param("code_verifier").is_none());

    // The tokens are in the MCP token store, and MCP requests use them.
    assert!(s.stored_access().is_some());
    let h = s.handle();
    assert!(!h.list_tools().await.unwrap().is_empty());
    h.shutdown().await;
}

#[tokio::test]
async fn device_polling_honours_interval_pending_and_slow_down() {
    let s = setup(MockConfig {
        device_interval: Some(2),
        device_script: vec![
            "authorization_pending".into(),
            "slow_down".into(),
            "authorization_pending".into(),
        ],
        ..device_config()
    })
    .await;
    s.device().run(&Screen::approving()).await.unwrap();
    let start = s.mock.requests_to("/device_authorization")[0].at;
    let polls_at = polls(&s.mock);
    assert_eq!(polls_at.len(), 4, "pending, slow_down, pending, success");
    let times: Vec<std::time::Instant> = std::iter::once(start).chain(polls_at).collect();
    let gaps: Vec<Duration> = times.windows(2).map(|w| w[1] - w[0]).collect();
    // `interval` (2) before the first poll and after authorization_pending;
    // 5 more after slow_down, and it stays slower.
    for g in &gaps[..2] {
        assert!(*g >= UNIT * 2, "{gaps:?}");
    }
    for g in &gaps[2..] {
        assert!(*g >= UNIT * 7, "{gaps:?}");
    }
    assert!(s.stored_access().is_some());

    // Without `interval` the client waits the RFC's 5 seconds.
    let s = setup(MockConfig {
        device_interval: None,
        ..device_config()
    })
    .await;
    s.device().run(&Screen::approving()).await.unwrap();
    let start = s.mock.requests_to("/device_authorization")[0].at;
    assert!(polls(&s.mock)[0] - start >= UNIT * 5);
}

#[tokio::test]
async fn device_denial_saves_no_tokens() {
    let s = setup(MockConfig {
        deny: true,
        ..device_config()
    })
    .await;
    let e = s.device().run(&Screen::approving()).await.unwrap_err();
    assert_eq!(e, AuthError::Denied);
    assert!(s.stored_access().is_none());
}

#[tokio::test]
async fn device_codes_expire_and_abandoned_ones_time_out() {
    // The server says the code expired.
    let s = setup(MockConfig {
        device_script: vec!["authorization_pending".into(), "expired_token".into()],
        ..device_config()
    })
    .await;
    let e = s.device().run(&Screen::approving()).await.unwrap_err();
    assert_eq!(e, AuthError::DeviceExpired);
    assert!(e.to_string().contains("run the sign-in again"), "{e}");
    assert!(s.stored_access().is_none());

    // Nobody approves: harness stops at `expires_in` by itself.
    let s = setup(MockConfig {
        device_expires_in: 5,
        ..device_config()
    })
    .await;
    let e = s.device().run(&Screen::default()).await.unwrap_err();
    assert_eq!(e, AuthError::DeviceExpired);

    // ... or at its own timeout, when that comes first.
    let s = setup(device_config()).await;
    let e = s
        .device()
        .with_timeout(Duration::from_millis(150))
        .run(&Screen::default())
        .await
        .unwrap_err();
    assert!(matches!(e, AuthError::Timeout(_)), "{e}");
    assert!(s.stored_access().is_none());
}

#[tokio::test]
async fn cancelling_a_device_grant_stops_polling() {
    let s = setup(device_config()).await;
    let screen = Screen::default();
    let flow = s.device();
    tokio::select! {
        r = flow.run(&screen) => panic!("nobody approved: {r:?}"),
        () = async {
            while polls(&s.mock).len() < 2 {
                tokio::time::sleep(UNIT).await;
            }
        } => {}
    }
    // The flow future is dropped: no more polls, and approving now is moot.
    let n = polls(&s.mock).len();
    let link = screen.shown()[0].link().to_owned();
    reqwest::get(&link).await.unwrap();
    tokio::time::sleep(UNIT * 10).await;
    assert_eq!(polls(&s.mock).len(), n);
    assert!(s.stored_access().is_none());
}

#[tokio::test]
async fn without_the_endpoint_or_on_a_desktop_the_loopback_flow_is_unchanged() {
    // The server offers no device grant: the loopback redirect, as before.
    let s = setup(MockConfig::default()).await;
    let browser = MockBrowser::new();
    s.device().run(&browser).await.unwrap();
    assert_eq!(browser.opened().len(), 1);
    assert!(
        !s.mock.requests_to("/register")[0]
            .body
            .contains("device_code")
    );

    // A desktop session does not ask for it even when the server offers it.
    let s = setup(device_config()).await;
    let browser = MockBrowser::new();
    s.authorize().run(&browser).await.unwrap();
    assert_eq!(browser.opened().len(), 1);
    assert!(s.mock.requests_to("/device_authorization").is_empty());
    assert!(
        !s.mock.requests_to("/register")[0]
            .body
            .contains("device_code")
    );
    assert_eq!(
        s.mock.requests_to("/token")[0]
            .param("grant_type")
            .as_deref(),
        Some("authorization_code")
    );

    // Over SSH against a server without it, paste-back still works.
    let s = setup(MockConfig::default()).await;
    let browser = MockBrowser::remote();
    let (tx, mut rx) = harness_providers::signin::paste_channel();
    let flow = s.device();
    let user = async {
        let address = browser.next_redirect().await;
        assert_eq!(paste(&tx, &address).await, Ok(()));
    };
    let (r, ()) = tokio::join!(flow.run_with(&browser, &mut rx), user);
    r.unwrap();
    assert!(s.stored_access().is_some());
}

#[tokio::test]
async fn a_refused_device_grant_falls_back_to_the_loopback_flow() {
    // The server refuses the device grant for this client
    // (unauthorized_client): the same run continues with the redirect,
    // and paste-back works as before.
    let s = setup(MockConfig {
        device_refuse: Some("unauthorized_client".into()),
        ..device_config()
    })
    .await;
    let browser = MockBrowser::remote();
    let (tx, mut rx) = harness_providers::signin::paste_channel();
    let flow = s.device();
    let user = async {
        let address = browser.next_redirect().await;
        assert_eq!(paste(&tx, &address).await, Ok(()));
    };
    let (r, ()) = tokio::join!(flow.run_with(&browser, &mut rx), user);
    r.unwrap();
    assert_eq!(s.mock.requests_to("/device_authorization").len(), 1);
    assert_eq!(browser.opened().len(), 1, "fell back to the redirect");
    assert!(s.stored_access().is_some());
}

#[tokio::test]
async fn a_registration_without_the_device_grant_is_replaced_for_it() {
    // A loopback login registered harness without the device grant; the
    // first device login registers again, later ones reuse that.
    let s = setup(device_config()).await;
    s.authorize().run(&MockBrowser::new()).await.unwrap();
    s.device().run(&Screen::approving()).await.unwrap();
    assert_eq!(s.mock.requests_to("/register").len(), 2);
    let record = s.store.load("docs").unwrap().unwrap();
    assert!(record.client.device_grant);
    s.device().run(&Screen::approving()).await.unwrap();
    assert_eq!(s.mock.requests_to("/register").len(), 2);
    // A loopback login keeps using it too.
    s.authorize().run(&MockBrowser::new()).await.unwrap();
    assert_eq!(s.mock.requests_to("/register").len(), 2);
}
