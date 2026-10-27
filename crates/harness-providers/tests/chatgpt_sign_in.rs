//! Sign in with ChatGPT against a local mock authorization server: success,
//! denied consent, missing plan scope, refresh and an expired refresh
//! token, plus Responses calls in ChatGPT mode. No real account is used.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use harness_providers::chatgpt::mock::{MockAuthServer, MockBrowser, MockConfig};
use harness_providers::chatgpt::session::AuthStatus;
use harness_providers::chatgpt::store::{AuthRecord, TokenStore};
use harness_providers::chatgpt::{
    ChatGptAuth, ChatGptError, DYNAMIC_CLIENT_ID, MANAGE_USAGE_URL, PLAN_SCOPE, SCOPES, SignIn,
};
use harness_providers::http::HttpConfig;
use harness_providers::model::{ChatRequest, Message};
use harness_providers::openai::OpenAiProvider;
use harness_providers::provider::{Provider, ProviderError};
use harness_providers::retry::RetryPolicy;
use tokio_util::sync::CancellationToken;

fn sign_in(server: &MockAuthServer, store: &TokenStore) -> SignIn {
    SignIn::new(store.clone())
        .with_endpoints(server.endpoints())
        .with_port(0)
        .with_timeout(Duration::from_secs(10))
}

fn store() -> (tempfile::TempDir, TokenStore) {
    let tmp = tempfile::tempdir().unwrap();
    let store = TokenStore::new(tmp.path().join("auth"));
    (tmp, store)
}

fn provider(server: &MockAuthServer, auth: Arc<ChatGptAuth>) -> OpenAiProvider {
    let mut http = HttpConfig::new(server.endpoints().api_base_url);
    http.retry = RetryPolicy {
        base_delay: Duration::from_millis(1),
        max_delay: Duration::from_millis(2),
        ..RetryPolicy::default()
    };
    OpenAiProvider::with_chatgpt(auth, http).unwrap()
}

fn request() -> ChatRequest {
    let mut r = ChatRequest::new("gpt-5");
    r.messages = vec![Message::user("hi")];
    r.temperature = Some(0.3);
    r.max_tokens = Some(500);
    r
}

#[tokio::test]
async fn first_sign_in_registers_verifies_and_stores_private_tokens() {
    let server = MockAuthServer::start(MockConfig::default()).await;
    let (_tmp, store) = store();
    let browser = MockBrowser::new();
    let account = sign_in(&server, &store).run(&browser).await.unwrap();
    assert_eq!(account.email.as_deref(), Some("user@example.com"));
    assert_eq!(account.sub, "user-123");

    // The authorization request.
    let auth = &server.requests_to("/api/accounts/authorize")[0];
    let host_id = store.host_id().unwrap();
    assert!(host_id.starts_with("urn:uuid:"));
    assert_eq!(auth.param("client_id"), Some(DYNAMIC_CLIENT_ID));
    assert_eq!(auth.param("agent_name_hint"), Some("harness"));
    assert_eq!(auth.param("ext_agent_host_id"), Some(host_id.as_str()));
    assert_eq!(auth.param("response_type"), Some("code"));
    assert_eq!(auth.param("scope"), Some(SCOPES));
    assert!(SCOPES.contains("offline_access") && SCOPES.contains("resource.invoke"));
    assert_eq!(auth.param("resource"), Some("https://api.openai.com/v1"));
    assert_eq!(auth.param("code_challenge_method"), Some("S256"));
    for p in ["state", "nonce", "code_challenge"] {
        assert!(auth.param(p).is_some_and(|v| v.len() >= 43), "{p}");
    }
    let redirect = auth.param("redirect_uri").unwrap();
    assert!(redirect.starts_with("http://127.0.0.1:"), "{redirect}");
    assert!(redirect.ends_with("/auth/callback"));
    assert_eq!(browser.opened().len(), 1);

    // The code exchange used the issued client id and the PKCE verifier.
    let token = &server.requests_to("/api/accounts/oauth/token")[0];
    assert_eq!(
        token.form("grant_type").as_deref(),
        Some("authorization_code")
    );
    assert_eq!(token.form("client_id").as_deref(), Some("oaiapp_mock123"));
    assert!(token.form("code_verifier").is_some());
    assert_eq!(token.form("redirect_uri").as_deref(), Some(redirect));
    assert_eq!(server.requests_to("/.well-known/jwks.json").len(), 1);

    let record = store.load().unwrap();
    assert_eq!(record.client_id.as_deref(), Some("oaiapp_mock123"));
    let tokens = record.tokens.unwrap();
    assert!(tokens.plan_usage());
    assert_eq!(tokens.account.email.as_deref(), Some("user@example.com"));
    assert_eq!(
        Some(tokens.access_token.expose().to_owned()),
        server.current_access_token()
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(store.auth_path())
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
    }

    // A second sign-in reuses the registration and gets fresh values.
    let second = MockBrowser::new();
    sign_in(&server, &store).run(&second).await.unwrap();
    let again = &server.requests_to("/api/accounts/authorize")[1];
    assert_eq!(again.param("client_id"), Some("oaiapp_mock123"));
    assert_eq!(again.param("agent_name_hint"), None);
    assert_eq!(again.param("ext_agent_host_id"), Some(host_id.as_str()));
    assert_eq!(again.param("login_hint"), Some("user@example.com"));
    for p in ["state", "nonce", "code_challenge"] {
        assert_ne!(auth.param(p), again.param(p), "{p} must be fresh");
    }
}

#[tokio::test]
async fn denied_consent_saves_nothing() {
    let server = MockAuthServer::start(MockConfig {
        deny: true,
        ..MockConfig::default()
    })
    .await;
    let (_tmp, store) = store();
    let err = sign_in(&server, &store)
        .run(&MockBrowser::new())
        .await
        .unwrap_err();
    assert_eq!(err, ChatGptError::Denied);
    assert!(err.to_string().contains("declined"));
    assert!(server.requests_to("/api/accounts/oauth/token").is_empty());
    assert_eq!(store.load().unwrap(), AuthRecord::default());
}

#[tokio::test]
async fn missing_plan_scope_fails_clearly_and_stores_no_tokens() {
    let server = MockAuthServer::start(MockConfig {
        grant_plan: false,
        ..MockConfig::default()
    })
    .await;
    let (_tmp, store) = store();
    let err = sign_in(&server, &store)
        .run(&MockBrowser::new())
        .await
        .unwrap_err();
    assert_eq!(err, ChatGptError::PlanUsageNotGranted);
    let msg = err.to_string();
    assert!(
        msg.contains(PLAN_SCOPE) && msg.contains("plan usage was not granted"),
        "{msg}"
    );
    let record = store.load().unwrap();
    assert!(record.tokens.is_none(), "identity alone is not stored");
    assert_eq!(
        record.client_id.as_deref(),
        Some("oaiapp_mock123"),
        "registration kept"
    );
}

#[tokio::test]
async fn forged_state_is_never_exchanged() {
    // The forged callback is refused and the sign-in keeps waiting for the
    // real one, which never comes here.
    let server = MockAuthServer::start(MockConfig::default()).await;
    let (_tmp, store) = store();
    let browser = MockBrowser::forging_state("attacker");
    let err = sign_in(&server, &store)
        .with_timeout(Duration::from_millis(500))
        .run(&browser)
        .await
        .unwrap_err();
    assert!(matches!(err, ChatGptError::Timeout(_)), "{err:?}");
    assert!(server.requests_to("/api/accounts/oauth/token").is_empty());
    assert!(store.load().unwrap().tokens.is_none());
}

#[tokio::test]
async fn a_browser_that_never_returns_times_out() {
    struct Nowhere;
    impl harness_providers::chatgpt::Browser for Nowhere {
        fn open(&self, _: &str) -> Result<(), String> {
            Ok(())
        }
    }
    let server = MockAuthServer::start(MockConfig::default()).await;
    let (_tmp, store) = store();
    let err = sign_in(&server, &store)
        .with_timeout(Duration::from_millis(200))
        .run(&Nowhere)
        .await
        .unwrap_err();
    assert!(matches!(err, ChatGptError::Timeout(_)), "{err:?}");
}

#[tokio::test]
async fn tokens_refresh_before_expiry_and_rotate() {
    let server = MockAuthServer::start(MockConfig::default()).await;
    let (_tmp, store) = store();
    sign_in(&server, &store)
        .run(&MockBrowser::new())
        .await
        .unwrap();
    let first = store.load().unwrap().tokens.unwrap();

    let now = Arc::new(AtomicU64::new(first.expires_at - 3000));
    let clock = now.clone();
    let auth = Arc::new(
        ChatGptAuth::new(store.clone(), server.endpoints())
            .with_clock(Arc::new(move || clock.load(Ordering::SeqCst))),
    );
    // Fresh: no refresh.
    assert_eq!(auth.access_token().await.unwrap(), first.access_token);
    assert!(server.requests_to("/api/accounts/oauth/token").len() == 1);

    // Within the five-minute skew: refreshed, refresh token rotated.
    now.store(first.expires_at - 60, Ordering::SeqCst);
    let token = auth.access_token().await.unwrap();
    assert_ne!(token, first.access_token);
    let refresh = &server.requests_to("/api/accounts/oauth/token")[1];
    assert_eq!(refresh.form("grant_type").as_deref(), Some("refresh_token"));
    assert_eq!(refresh.form("client_id").as_deref(), Some("oaiapp_mock123"));
    assert_eq!(
        refresh.form("refresh_token").as_deref(),
        first.refresh_token.as_ref().map(|t| t.expose())
    );
    let stored = store.load().unwrap().tokens.unwrap();
    assert_eq!(stored.access_token, token);
    assert_ne!(stored.refresh_token, first.refresh_token);
    assert_eq!(
        stored.refresh_token.as_ref().map(|t| t.expose().to_owned()),
        server.current_refresh_token()
    );

    // The provider calls the Responses API with the refreshed token.
    let resp = provider(&server, auth.clone())
        .complete(request(), CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(resp.message.text(), "hello from your plan");
    let call = server.requests_to("/v1/responses").pop().unwrap();
    assert_eq!(
        call.header("authorization"),
        Some(format!("Bearer {}", token.expose()).as_str())
    );
    let body = call.json();
    assert_eq!(body["store"], false);
    assert_eq!(body["stream"], true);
    assert!(body.get("temperature").is_none());
    assert!(body.get("max_output_tokens").is_none());
}

#[tokio::test]
async fn concurrent_callers_share_one_refresh() {
    let server = MockAuthServer::start(MockConfig::default()).await;
    let (_tmp, store) = store();
    sign_in(&server, &store)
        .run(&MockBrowser::new())
        .await
        .unwrap();
    let exp = store.load().unwrap().tokens.unwrap().expires_at;
    let auth = Arc::new(
        ChatGptAuth::new(store.clone(), server.endpoints()).with_clock(Arc::new(move || exp)),
    );
    let calls: Vec<_> = (0..4)
        .map(|_| {
            let a = auth.clone();
            tokio::spawn(async move { a.access_token().await })
        })
        .collect();
    for c in calls {
        c.await.unwrap().unwrap();
    }
    assert_eq!(
        server.requests_to("/api/accounts/oauth/token").len(),
        2,
        "one exchange and one refresh"
    );
}

#[tokio::test]
async fn expired_refresh_token_clears_tokens_and_asks_to_sign_in_again() {
    let server = MockAuthServer::start(MockConfig::default()).await;
    let (_tmp, store) = store();
    sign_in(&server, &store)
        .run(&MockBrowser::new())
        .await
        .unwrap();
    let exp = store.load().unwrap().tokens.unwrap().expires_at;
    server.configure(|c| c.refresh_error = Some("invalid_grant".into()));
    let auth = Arc::new(
        ChatGptAuth::new(store.clone(), server.endpoints()).with_clock(Arc::new(move || exp + 10)),
    );
    let err = auth.access_token().await.unwrap_err();
    assert_eq!(err, ChatGptError::ReauthRequired("invalid_grant".into()));
    assert!(err.to_string().contains("harness login openai"));
    let record = store.load().unwrap();
    assert!(record.tokens.is_none());
    assert_eq!(record.client_id.as_deref(), Some("oaiapp_mock123"));
    assert_eq!(auth.status().unwrap(), AuthStatus::SignedOut);

    // The provider reports it as an authentication error.
    let err = provider(&server, auth)
        .complete(request(), CancellationToken::new())
        .await
        .unwrap_err();
    assert!(
        matches!(err, ProviderError::Auth(ref m) if m.contains("not signed in")),
        "{err:?}"
    );
}

#[tokio::test]
async fn temporary_refresh_failures_keep_the_tokens() {
    let server = MockAuthServer::start(MockConfig::default()).await;
    let (_tmp, store) = store();
    sign_in(&server, &store)
        .run(&MockBrowser::new())
        .await
        .unwrap();
    let exp = store.load().unwrap().tokens.unwrap().expires_at;
    let mut endpoints = server.endpoints();
    endpoints.token_url = "http://127.0.0.1:9/unreachable".into();
    let auth = ChatGptAuth::new(store.clone(), endpoints).with_clock(Arc::new(move || exp));
    let err = auth.access_token().await.unwrap_err();
    assert!(matches!(err, ChatGptError::Network(_)), "{err:?}");
    assert!(store.load().unwrap().tokens.is_some());
}

#[tokio::test]
async fn usage_limit_is_not_retried_and_offers_manage_usage() {
    let server = MockAuthServer::start(MockConfig {
        usage_limited: true,
        ..MockConfig::default()
    })
    .await;
    let (_tmp, store) = store();
    sign_in(&server, &store)
        .run(&MockBrowser::new())
        .await
        .unwrap();
    let auth = Arc::new(ChatGptAuth::new(store, server.endpoints()));
    let err = provider(&server, auth)
        .complete(request(), CancellationToken::new())
        .await
        .unwrap_err();
    assert!(
        matches!(err.root(), ProviderError::UsageLimit { .. }),
        "{err:?}"
    );
    let action = err.action().unwrap();
    assert_eq!(action.label, "Manage usage");
    assert_eq!(action.url, MANAGE_USAGE_URL);
    assert!(err.to_string().contains("does not switch to an API key"));
    assert_eq!(server.requests_to("/v1/responses").len(), 1, "no retry");
}
