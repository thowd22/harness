//! OAuth for remote MCP servers against the local mock authorization
//! server (`harness_mcp::auth::mock`): success (dynamic and pre-registered
//! clients), denied consent, refresh, expired refresh token, revoked
//! token, step-up scopes, issuer checks, and the refusal to authorize
//! interactively outside `harness login mcp`.

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
