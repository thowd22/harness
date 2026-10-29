//! The Sign in with ChatGPT model list: the signed-in account's plan models
//! from `GET {api}/models` with the ChatGPT access token, how the catalog
//! uses it for plan availability and order, and how the runtime registry
//! learns models that only live lists know.
//!
//! `fixtures/models/chatgpt.json` follows the shape of a real response
//! (2026-10-28), trimmed to the fields harness reads plus a few others.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use harness_providers::catalog::live::{ListKind, LiveModel, parse};
use harness_providers::catalog::{
    AuthMethod, Catalog, CatalogConfig, CatalogService, CredentialState, Credentials, LiveList,
    ModelListCache, ModelListSource, PriceOrigin, live_model_info,
};
use harness_providers::chatgpt::store::{AuthRecord, StoredTokens, Token};
use harness_providers::chatgpt::{Account, ChatGptAuth, TokenStore};
use harness_providers::chatgpt::{Endpoints, PLAN_SCOPE};
use harness_providers::registry::{ModelRef, Registry, ThinkingStyle};
use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

const CHATGPT: &str = include_str!("fixtures/models/chatgpt.json");
const TOKEN: &str = "chatgpt-access-token-sentinel";

fn fixture() -> Vec<LiveModel> {
    let body: Value = serde_json::from_str(CHATGPT).unwrap();
    parse(ListKind::ChatGpt, &body).unwrap().models
}

fn plan_lists() -> BTreeMap<String, LiveList> {
    BTreeMap::from([(
        "openai".to_owned(),
        LiveList::new("openai", "fixture", 0, fixture()),
    )])
}

fn chatgpt_only() -> Credentials {
    Credentials::new().with("openai", AuthMethod::ChatGpt, CredentialState::Ready)
}

#[test]
fn the_list_keeps_listed_api_models_in_server_order() {
    let models = fixture();
    let ids: Vec<&str> = models.iter().map(|m| m.id.as_str()).collect();
    // Hidden entries and ones the API does not serve are left out.
    assert_eq!(ids, ["gpt-6.1-sol", "gpt-6-astra", "gpt-5.6-luna"]);
    let ranks: Vec<Option<u32>> = models.iter().map(|m| m.rank).collect();
    assert_eq!(ranks, [Some(0), Some(1), Some(2)]);
    let sol = &models[0];
    assert_eq!(sol.display_name.as_deref(), Some("GPT-6.1-Sol"));
    assert_eq!(sol.context_window, Some(500_000));
    assert_eq!(sol.vision, Some(true));
    assert_eq!(sol.reasoning, Some(true));
    assert_eq!(sol.tools, Some(true));
    assert_eq!(sol.plan, Some(true));
    let luna = &models[2];
    assert_eq!(luna.vision, Some(false));
    assert_eq!(luna.reasoning, Some(false));

    // Effort levels come from `supported_reasoning_levels`, sorted, with
    // names harness does not know (`turbo`) and `ultra`, which the
    // Responses API rejects, left out.
    use harness_providers::model::ReasoningEffort::*;
    assert_eq!(
        sol.efforts.as_deref(),
        Some(&[Low, Medium, High, XHigh, Max][..])
    );
    assert_eq!(models[1].efforts.as_deref(), Some(&[Low, Medium, High][..]));
    let info = live_model_info("openai", sol);
    assert_eq!(info.efforts, [Low, Medium, High, XHigh, Max]);
    assert_eq!(info.effort(Minimal), Low);
    assert_eq!(info.effort(Ultra), Max);
    // A model that does not reason accepts no level.
    assert!(live_model_info("openai", luna).efforts.is_empty());

    let err = parse(ListKind::ChatGpt, &serde_json::json!({"data": []})).unwrap_err();
    assert!(err.to_string().contains("no `models` array"), "{err}");
}

#[test]
fn the_account_list_decides_plan_models_and_their_order() {
    let c = Catalog::build(&Registry::builtin(), &chatgpt_only(), &plan_lists());
    let openai: Vec<(&str, AuthMethod, bool)> = c
        .by_provider("openai")
        .map(|e| (e.model(), e.auth, e.usable_for_agents()))
        .collect();
    // The account's models first, in its order, usable with the plan.
    assert_eq!(
        &openai[..3],
        [
            ("gpt-6.1-sol", AuthMethod::ChatGpt, true),
            ("gpt-6-astra", AuthMethod::ChatGpt, true),
            ("gpt-5.6-luna", AuthMethod::ChatGpt, true),
        ]
    );
    let astra = c
        .get(&ModelRef::new("openai", "gpt-6-astra"))
        .next()
        .unwrap();
    assert_eq!(astra.display_name, "GPT-6-Astra");
    assert_eq!(astra.context_window, 272_000);
    assert_eq!(astra.price_origin, PriceOrigin::Plan);
    // A built-in model the account does not list is not on the plan, even
    // though its name fits the fallback rule.
    let gpt5 = c.get(&ModelRef::new("openai", "gpt-5")).next().unwrap();
    assert!(!gpt5.usable());
    assert!(
        gpt5.reason()
            .unwrap()
            .contains("not included in Sign in with ChatGPT"),
        "{:?}",
        gpt5.reason()
    );

    // Before the list is fetched the name rule still applies.
    let c = Catalog::build(&Registry::builtin(), &chatgpt_only(), &BTreeMap::new());
    let gpt5 = c.get(&ModelRef::new("openai", "gpt-5")).next().unwrap();
    assert_eq!(gpt5.auth, AuthMethod::ChatGpt);
    assert!(gpt5.usable());
}

#[test]
fn runs_use_the_facts_of_models_only_a_live_list_knows() {
    let mut infos: Vec<_> = fixture()
        .iter()
        .map(|m| live_model_info("openai", m))
        .collect();
    infos.push(live_model_info(
        "anthropic",
        &LiveModel {
            id: "claude-nova-6".into(),
            context_window: Some(1_000_000),
            max_output_tokens: Some(128_000),
            tools: Some(true),
            reasoning: Some(true),
            vision: Some(true),
            ..LiveModel::default()
        },
    ));
    infos.push(live_model_info(
        "openrouter",
        &LiveModel {
            id: "newlab/model-x".into(),
            context_window: Some(64_000),
            max_output_tokens: Some(8_000),
            tools: Some(true),
            reasoning: Some(false),
            vision: Some(false),
            ..LiveModel::default()
        },
    ));
    // A live entry for a built-in model never replaces the built-in facts.
    infos.push(live_model_info(
        "openai",
        &LiveModel {
            id: "gpt-5".into(),
            context_window: Some(1),
            ..LiveModel::default()
        },
    ));
    let reg = Registry::builtin().with_live_models(infos);

    let sol = reg.resolve("openai/gpt-6.1-sol").unwrap();
    assert!(sol.warnings.is_empty(), "{:?}", sol.warnings);
    let caps = sol.capabilities();
    assert_eq!(caps.context_window, 500_000);
    assert!(caps.thinking && caps.images && caps.tools);
    assert_eq!(sol.info.thinking, ThinkingStyle::Effort);
    assert_eq!(sol.info.display_name, "GPT-6.1-Sol");
    // Plan models cost nothing per token.
    let p = sol.info.price.unwrap();
    assert_eq!((p.input, p.output), (0.0, 0.0));
    // A bare id resolves when one provider has it.
    assert_eq!(
        reg.resolve("gpt-6-astra").unwrap().reference,
        ModelRef::new("openai", "gpt-6-astra")
    );
    let luna = reg.resolve("openai/gpt-5.6-luna").unwrap();
    assert_eq!(luna.info.thinking, ThinkingStyle::None);
    assert!(!luna.capabilities().images);

    let nova = reg.resolve("anthropic/claude-nova-6").unwrap();
    assert_eq!(nova.info.thinking, ThinkingStyle::Adaptive);
    assert_eq!(nova.capabilities().context_window, 1_000_000);
    assert_eq!(nova.capabilities().max_output_tokens, 128_000);

    let x = reg.resolve("openrouter/newlab/model-x").unwrap();
    assert_eq!(x.capabilities().context_window, 64_000);
    assert_eq!(x.info.thinking, ThinkingStyle::None);

    assert_eq!(
        reg.resolve("openai/gpt-5")
            .unwrap()
            .capabilities()
            .context_window,
        Registry::builtin()
            .resolve("openai/gpt-5")
            .unwrap()
            .capabilities()
            .context_window
    );
    // Unknown models still get the conservative defaults and a warning.
    let unknown = reg.resolve("openai/gpt-6-mystery").unwrap();
    assert!(!unknown.warnings.is_empty());
}

/// A signed-in store whose access token stays fresh for the test.
fn signed_in(dir: &std::path::Path, sub: &str) -> TokenStore {
    let store = TokenStore::new(dir.join("auth"));
    store
        .save(&AuthRecord {
            client_id: Some("app_test".into()),
            tokens: Some(StoredTokens {
                access_token: Token::new(TOKEN),
                refresh_token: None,
                id_token: None,
                expires_at: u64::MAX / 2,
                scopes: vec![PLAN_SCOPE.into()],
                account: Account {
                    sub: sub.into(),
                    email: None,
                },
            }),
        })
        .unwrap();
    store
}

/// Serves the fixture at `/v1/models` to the right bearer token and 401
/// to anything else; records each request's path and whether the token
/// was right.
async fn chatgpt_api() -> (String, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}/v1", listener.local_addr().unwrap());
    let seen = Arc::new(Mutex::new(Vec::new()));
    let rec = seen.clone();
    tokio::spawn(async move {
        while let Ok((mut sock, _)) = listener.accept().await {
            let mut buf = Vec::new();
            let mut tmp = [0u8; 4096];
            while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
                let n = sock.read(&mut tmp).await.unwrap_or(0);
                if n == 0 {
                    break;
                }
                buf.extend_from_slice(&tmp[..n]);
            }
            let head = String::from_utf8_lossy(&buf).to_string();
            let path = head.split_whitespace().nth(1).unwrap_or("").to_owned();
            let ok = head
                .lines()
                .any(|l| l.eq_ignore_ascii_case(&format!("authorization: Bearer {TOKEN}")));
            rec.lock()
                .unwrap()
                .push(format!("{path} {}", if ok { "token" } else { "no-token" }));
            let (code, body) = if ok && path == "/v1/models" {
                (200, CHATGPT)
            } else {
                (401, r#"{"error":{"message":"bad token"}}"#)
            };
            let head = format!(
                "HTTP/1.1 {code} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                body.len()
            );
            let _ = sock.write_all(head.as_bytes()).await;
            let _ = sock.write_all(body.as_bytes()).await;
            let _ = sock.shutdown().await;
        }
    });
    (base, seen)
}

#[tokio::test]
async fn the_service_fetches_the_list_with_the_access_token_and_caches_it_per_account() {
    let (base, seen) = chatgpt_api().await;
    let tmp = tempfile::tempdir().unwrap();
    let store = signed_in(tmp.path(), "user-1");
    let auth = Arc::new(ChatGptAuth::new(
        store,
        Endpoints::with_issuer("http://127.0.0.1:9", &base),
    ));
    let source = ModelListSource::chatgpt(auth.clone(), Some(&base), "user-1");
    assert_eq!(source.kind, ListKind::ChatGpt);
    assert!(!format!("{source:?}").contains(TOKEN));
    let cache = ModelListCache::new(tmp.path().join("model-lists"));
    let service = CatalogService::new(
        CatalogConfig::new(Arc::new(Registry::builtin()), chatgpt_only(), cache.clone())
            .with_sources(vec![source])
            .with_ttl(Duration::from_secs(3600)),
    );
    let report = service.refresh(false).await;
    assert_eq!(report.refreshed, ["openai"], "{report:?}");
    assert_eq!(*seen.lock().unwrap(), ["/v1/models token"]);
    let catalog = service.catalog();
    let first = catalog.by_provider("openai").next().unwrap();
    assert_eq!(
        (first.model(), first.auth),
        ("gpt-6.1-sol", AuthMethod::ChatGpt)
    );
    assert!(first.usable_for_agents());

    // The cache holds the models, never the token.
    let cached = std::fs::read_to_string(cache.path("openai")).unwrap();
    assert!(cached.contains("gpt-6-astra"));
    assert!(!cached.contains(TOKEN));

    // Fresh for this account; another account's sign-in refetches.
    let same = CatalogService::new(
        CatalogConfig::new(Arc::new(Registry::builtin()), chatgpt_only(), cache.clone())
            .with_sources(vec![ModelListSource::chatgpt(
                auth.clone(),
                Some(&base),
                "user-1",
            )])
            .with_ttl(Duration::from_secs(3600)),
    );
    assert!(same.stale_providers().is_empty());
    let other = CatalogService::new(
        CatalogConfig::new(Arc::new(Registry::builtin()), chatgpt_only(), cache)
            .with_sources(vec![ModelListSource::chatgpt(auth, Some(&base), "user-2")])
            .with_ttl(Duration::from_secs(3600)),
    );
    assert_eq!(other.stale_providers(), ["openai"]);
}

#[tokio::test]
async fn a_signed_out_account_reports_an_error_and_keeps_the_fallback() {
    let (base, seen) = chatgpt_api().await;
    let tmp = tempfile::tempdir().unwrap();
    // No tokens at all.
    let store = TokenStore::new(tmp.path().join("auth"));
    let auth = Arc::new(ChatGptAuth::new(
        store,
        Endpoints::with_issuer("http://127.0.0.1:9", &base),
    ));
    let service = CatalogService::new(
        CatalogConfig::new(
            Arc::new(Registry::builtin()),
            chatgpt_only(),
            ModelListCache::new(tmp.path().join("model-lists")),
        )
        .with_sources(vec![ModelListSource::chatgpt(auth, Some(&base), "user-1")]),
    );
    let report = service.refresh(false).await;
    assert_eq!(report.failed.len(), 1, "{report:?}");
    // Nothing was sent without a token.
    assert!(seen.lock().unwrap().is_empty());
    // The name rule still offers the built-in plan models.
    let catalog = service.catalog();
    let gpt5 = catalog
        .get(&ModelRef::new("openai", "gpt-5"))
        .next()
        .unwrap();
    assert_eq!(gpt5.auth, AuthMethod::ChatGpt);
}
