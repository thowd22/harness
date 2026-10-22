//! Model catalog tests: recorded model-list fixtures for Anthropic, OpenAI
//! and OpenRouter, merging with the registry and aliases, usability from
//! credentials, Sign in with ChatGPT, and the cached background refresh
//! against a local mock server.
//!
//! `fixtures/models/openrouter.json` is a trimmed recording of the real
//! OpenRouter `GET /models` response (2026-10-22). The Anthropic and OpenAI
//! fixtures follow those APIs' documented response shapes.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use harness_providers::catalog::live::{ListKind, parse};
use harness_providers::catalog::{
    AuthMethod, Availability, Catalog, CatalogConfig, CatalogService, CredentialState, Credentials,
    LiveList, ModelListCache, ModelListSource, PriceOrigin, chatgpt_plan_model,
};
use harness_providers::pricing::Price;
use harness_providers::registry::{ModelRef, Registry};
use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

const ANTHROPIC_P1: &str = include_str!("fixtures/models/anthropic-page1.json");
const ANTHROPIC_P2: &str = include_str!("fixtures/models/anthropic-page2.json");
const OPENAI: &str = include_str!("fixtures/models/openai.json");
const OPENROUTER: &str = include_str!("fixtures/models/openrouter.json");

fn json(s: &str) -> Value {
    serde_json::from_str(s).unwrap()
}

fn live_lists() -> BTreeMap<String, LiveList> {
    let mut anthropic = parse(ListKind::Anthropic, &json(ANTHROPIC_P1))
        .unwrap()
        .models;
    anthropic.extend(
        parse(ListKind::Anthropic, &json(ANTHROPIC_P2))
            .unwrap()
            .models,
    );
    let openai = parse(ListKind::OpenAi, &json(OPENAI)).unwrap().models;
    let openrouter = parse(ListKind::OpenRouter, &json(OPENROUTER))
        .unwrap()
        .models;
    [
        ("anthropic", anthropic),
        ("openai", openai),
        ("openrouter", openrouter),
    ]
    .into_iter()
    .map(|(p, m)| (p.to_owned(), LiveList::new(p, "fixture", 0, m)))
    .collect()
}

fn registry() -> Registry {
    Registry::builtin()
        .with_aliases([
            ("sol", "openai/gpt-5.6-sol"),
            ("mystery", "openai/gpt-99-preview"),
            ("router-sonnet", "openrouter/anthropic/claude-sonnet-5.5"),
        ])
        .unwrap()
        .with_price_overrides([("sol", Price::new(1.0, 2.0))])
}

fn all_keys() -> Credentials {
    Credentials::new()
        .with("anthropic", AuthMethod::ApiKey, CredentialState::Ready)
        .with("openai", AuthMethod::ApiKey, CredentialState::Ready)
        .with("openrouter", AuthMethod::ApiKey, CredentialState::Ready)
}

fn entry<'a>(
    c: &'a Catalog,
    reference: &str,
    auth: AuthMethod,
) -> &'a harness_providers::catalog::CatalogEntry {
    let r = ModelRef::parse(reference).unwrap();
    c.get(&r)
        .find(|e| e.auth == auth)
        .unwrap_or_else(|| panic!("no {reference} entry with {auth}"))
}

#[test]
fn recorded_fixtures_parse_for_each_provider() {
    let p1 = parse(ListKind::Anthropic, &json(ANTHROPIC_P1)).unwrap();
    assert_eq!(p1.next_after.as_deref(), Some("claude-sonnet-5-5"));
    assert_eq!(p1.models.len(), 3);
    let opus = &p1.models[1];
    assert_eq!(opus.id, "claude-opus-5-5");
    assert_eq!(opus.display_name.as_deref(), Some("Claude Opus 5.5"));
    assert_eq!(
        (opus.context_window, opus.max_output_tokens),
        (Some(1_000_000), Some(128_000))
    );
    assert_eq!(
        (opus.tools, opus.reasoning, opus.vision),
        (Some(true), Some(true), Some(true))
    );
    let p2 = parse(ListKind::Anthropic, &json(ANTHROPIC_P2)).unwrap();
    assert_eq!(p2.next_after, None);

    let openai = parse(ListKind::OpenAi, &json(OPENAI)).unwrap().models;
    let ids: Vec<&str> = openai.iter().map(|m| m.id.as_str()).collect();
    assert_eq!(
        ids,
        [
            "gpt-5.6-sol",
            "gpt-5.5",
            "gpt-5.1",
            "gpt-5-mini",
            "gpt-4.1",
            "o3",
            "gpt-4o-search-preview",
            "chatgpt-4o-latest",
            "codex-mini-latest"
        ],
        "embeddings, speech, image, moderation and realtime models are left out"
    );

    let or = parse(ListKind::OpenRouter, &json(OPENROUTER))
        .unwrap()
        .models;
    assert_eq!(or.len(), 7);
    let sonnet = or
        .iter()
        .find(|m| m.id == "anthropic/claude-sonnet-5.5")
        .unwrap();
    assert_eq!(sonnet.price, Some(Price::with_cache(2.0, 10.0, 0.1, 2.5)));
    assert_eq!(sonnet.context_window, Some(1_000_000));
    assert_eq!(sonnet.tools, Some(true));
    let moondream = or
        .iter()
        .find(|m| m.id == "moondream/moondream3-preview")
        .unwrap();
    assert_eq!(moondream.tools, Some(false));
    assert_eq!(moondream.vision, Some(true));
    let router = or.iter().find(|m| m.id == "typesafe/jev-router").unwrap();
    assert_eq!(router.price, None, "variable (-1) prices are unknown");
}

#[test]
fn catalog_merges_registry_aliases_and_live_lists() {
    let c = Catalog::build(&registry(), &all_keys(), &live_lists());

    // Built-in and live: built-in facts win for direct providers.
    let opus = entry(&c, "anthropic/claude-opus-5-5", AuthMethod::ApiKey);
    assert!(opus.sources.builtin && opus.sources.live);
    assert_eq!(opus.aliases, ["opus"]);
    assert_eq!(opus.price, Some(Price::with_cache(4.0, 20.0, 0.2, 5.0)));
    assert_eq!(opus.price_origin, PriceOrigin::Builtin);

    // Live only: facts come from the live list.
    let mythos = entry(&c, "anthropic/claude-mythos-5-1", AuthMethod::ApiKey);
    assert!(!mythos.sources.builtin && mythos.sources.live);
    assert_eq!(mythos.display_name, "Claude Mythos 5.1");
    assert_eq!(mythos.context_window, 1_000_000);
    assert_eq!(mythos.price, None);
    assert_eq!(mythos.price_origin, PriceOrigin::Unknown);

    // Built-in only (not in the live list) is still listed.
    let gpt4o = entry(&c, "openai/gpt-4o", AuthMethod::ApiKey);
    assert!(gpt4o.sources.builtin && !gpt4o.sources.live);

    // Alias to a live model, with a [pricing] override keyed by alias.
    let sol = entry(&c, "openai/gpt-5.6-sol", AuthMethod::ApiKey);
    assert_eq!(sol.aliases, ["sol"]);
    assert!(sol.sources.alias && sol.sources.live);
    assert_eq!(sol.price, Some(Price::new(1.0, 2.0)));
    assert_eq!(sol.price_origin, PriceOrigin::Override);
    assert!(sol.capabilities.reasoning && sol.capabilities.tools);
    assert_eq!(sol.context_window, 400_000);

    // Alias to a model no other source knows.
    let mystery = entry(&c, "openai/gpt-99-preview", AuthMethod::ApiKey);
    assert!(mystery.sources.alias && !mystery.sources.builtin && !mystery.sources.live);
    assert_eq!(c.find("mystery").unwrap().reference, mystery.reference);

    // OpenRouter: live prices and limits win.
    let or = entry(
        &c,
        "openrouter/anthropic/claude-sonnet-5.5",
        AuthMethod::ApiKey,
    );
    assert_eq!(or.display_name, "Anthropic: Claude Sonnet 5.5");
    assert_eq!(or.price_origin, PriceOrigin::Live);
    assert_eq!(or.input_price(), Some(2.0));
    assert_eq!(or.output_price(), Some(10.0));
    assert_eq!(or.max_output_tokens, 128_000);
    assert_eq!(or.aliases, ["router-sonnet"]);
    assert!(!or.sources.builtin);

    // Grouped by provider in a fixed order, sorted by id within a provider.
    let providers: Vec<&str> = c.entries().iter().map(|e| e.provider()).collect();
    let mut grouped = providers.clone();
    grouped.dedup();
    assert_eq!(grouped, ["anthropic", "openai", "openrouter"]);
    let anthropic: Vec<&str> = c.by_provider("anthropic").map(|e| e.model()).collect();
    let mut sorted = anthropic.clone();
    sorted.sort_unstable();
    assert_eq!(anthropic, sorted);
}

#[test]
fn entries_carry_every_column_the_picker_shows() {
    let c = Catalog::build(&registry(), &all_keys(), &live_lists());
    let e = entry(&c, "anthropic/claude-sonnet-5-5", AuthMethod::ApiKey);
    assert_eq!(e.model(), "claude-sonnet-5-5");
    assert_eq!(e.provider(), "anthropic");
    assert_eq!(e.aliases, ["sonnet"]);
    assert_eq!(e.context_window, 1_000_000);
    assert_eq!((e.input_price(), e.output_price()), (Some(2.0), Some(10.0)));
    assert!(e.capabilities.tools && e.capabilities.reasoning && e.capabilities.vision);
    assert_eq!(e.auth, AuthMethod::ApiKey);
    assert_eq!(e.auth.to_string(), "API key");
    let v = serde_json::to_value(e).unwrap();
    for key in [
        "reference",
        "display_name",
        "aliases",
        "context_window",
        "price",
        "capabilities",
        "auth",
        "availability",
    ] {
        assert!(v.get(key).is_some(), "{key} missing from {v}");
    }
    assert_eq!(v["reference"], "anthropic/claude-sonnet-5-5");
    assert_eq!(v["auth"], "api_key");
}

#[test]
fn only_providers_with_working_credentials_are_usable() {
    let creds = Credentials::new()
        .with("anthropic", AuthMethod::ApiKey, CredentialState::Ready)
        .with(
            "openrouter",
            AuthMethod::ApiKey,
            CredentialState::Rejected("HTTP 401".into()),
        );
    let c = Catalog::build(&registry(), &creds, &live_lists());
    let opus = entry(&c, "anthropic/claude-opus-5-5", AuthMethod::ApiKey);
    assert_eq!(opus.availability, Availability::Usable);
    assert!(opus.usable() && opus.usable_for_agents() && opus.reason().is_none());

    let gpt = entry(&c, "openai/gpt-5.5", AuthMethod::ApiKey);
    assert!(!gpt.usable() && !gpt.usable_for_agents());
    let reason = gpt.reason().unwrap();
    assert!(reason.contains("no openai API key"), "{reason}");
    assert!(reason.contains("[provider.openai]"), "{reason}");

    let or = entry(
        &c,
        "openrouter/anthropic/claude-sonnet-5.5",
        AuthMethod::ApiKey,
    );
    assert!(!or.usable());
    assert!(or.reason().unwrap().contains("HTTP 401"));

    let usable: Vec<&str> = c.usable_for_agents().map(|e| e.provider()).collect();
    assert!(!usable.is_empty() && usable.iter().all(|p| *p == "anthropic"));
}

#[test]
fn models_without_tools_are_not_usable_for_agents() {
    let c = Catalog::build(&registry(), &all_keys(), &live_lists());
    for r in [
        "openrouter/moondream/moondream3-preview",
        "openai/gpt-4o-search-preview",
        "openai/chatgpt-4o-latest",
    ] {
        let e = entry(&c, r, AuthMethod::ApiKey);
        assert_eq!(e.availability, Availability::NoTools, "{r}");
        assert!(e.usable(), "{r} can still chat");
        assert!(!e.usable_for_agents(), "{r}");
        assert!(
            e.reason()
                .unwrap()
                .contains("does not support tool calling")
        );
        assert!(!e.capabilities.tools);
    }
}

#[test]
fn sign_in_with_chatgpt_models_are_distinguished_from_api_key_models() {
    assert!(chatgpt_plan_model("gpt-5.5"));
    assert!(chatgpt_plan_model("codex-mini-latest"));
    assert!(!chatgpt_plan_model("gpt-4.1"));
    assert!(!chatgpt_plan_model("o3"));

    // ChatGPT only.
    let creds = Credentials::new().with("openai", AuthMethod::ChatGpt, CredentialState::Ready);
    let c = Catalog::build(&registry(), &creds, &live_lists());
    let plan = entry(&c, "openai/gpt-5.5", AuthMethod::ChatGpt);
    assert!(plan.usable_for_agents());
    assert_eq!(plan.auth.to_string(), "ChatGPT");
    assert_eq!(plan.price, None);
    assert_eq!(plan.price_origin, PriceOrigin::Plan);
    assert_eq!(c.get(&plan.reference).count(), 1);
    let api_only = entry(&c, "openai/gpt-4.1", AuthMethod::ApiKey);
    assert!(!api_only.usable());
    assert!(
        api_only
            .reason()
            .unwrap()
            .contains("not included in Sign in with ChatGPT"),
        "{:?}",
        api_only.reason()
    );

    // Both: plan models are listed once per method, metered and plan.
    let both = creds.with("openai", AuthMethod::ApiKey, CredentialState::Ready);
    let c = Catalog::build(&registry(), &both, &live_lists());
    let r = ModelRef::parse("openai/gpt-5.5").unwrap();
    let auths: Vec<AuthMethod> = c.get(&r).map(|e| e.auth).collect();
    assert_eq!(auths, [AuthMethod::ApiKey, AuthMethod::ChatGpt]);
    let metered = entry(&c, "openai/gpt-5.5", AuthMethod::ApiKey);
    assert_eq!(metered.price_origin, PriceOrigin::Unknown);
    let gpt5 = entry(&c, "openai/gpt-5", AuthMethod::ApiKey);
    assert_eq!(gpt5.price_origin, PriceOrigin::Builtin);
    assert_eq!(
        c.get(&ModelRef::parse("openai/gpt-4.1").unwrap()).count(),
        1
    );

    // Not signed in.
    let missing = Credentials::new().with("openai", AuthMethod::ChatGpt, CredentialState::Missing);
    let c = Catalog::build(&registry(), &missing, &live_lists());
    let e = entry(&c, "openai/gpt-5.5", AuthMethod::ChatGpt);
    assert!(e.reason().unwrap().contains("not signed in with ChatGPT"));
}

// ---------------------------------------------------------------------------
// Service: cache, TTL, background refresh against a mock server.

#[derive(Clone)]
struct Mock {
    base: String,
    paths: Arc<Mutex<Vec<String>>>,
}

/// Serves model lists by request path. `status` overrides every reply.
async fn mock(status: Option<u16>) -> Mock {
    mock_with_delay(status, Duration::ZERO).await
}

/// Like [`mock`], answering each request after `delay`.
async fn mock_with_delay(status: Option<u16>, delay: Duration) -> Mock {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let paths = Arc::new(Mutex::new(Vec::new()));
    let rec = paths.clone();
    tokio::spawn(async move {
        while let Ok((mut sock, _)) = listener.accept().await {
            let rec = rec.clone();
            tokio::spawn(async move {
                let mut buf = Vec::new();
                let mut tmp = [0u8; 4096];
                while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
                    let n = sock.read(&mut tmp).await.unwrap_or(0);
                    if n == 0 {
                        return;
                    }
                    buf.extend_from_slice(&tmp[..n]);
                }
                let head = String::from_utf8_lossy(&buf).to_string();
                let path = head.split_whitespace().nth(1).unwrap_or("").to_owned();
                let auth = head
                    .lines()
                    .find(|l| {
                        let l = l.to_ascii_lowercase();
                        l.starts_with("authorization:") || l.starts_with("x-api-key:")
                    })
                    .map(|l| l.to_ascii_lowercase());
                rec.lock().unwrap().push(format!(
                    "{path} {}",
                    if auth.is_some() { "auth" } else { "anon" }
                ));
                let body = if path.starts_with("/anthropic/v1/models") {
                    if path.contains("after_id=claude-sonnet-5-5") {
                        ANTHROPIC_P2
                    } else {
                        ANTHROPIC_P1
                    }
                } else if path.starts_with("/openai/models") {
                    OPENAI
                } else if path.starts_with("/openrouter/models") {
                    OPENROUTER
                } else {
                    "{}"
                };
                tokio::time::sleep(delay).await;
                let (code, body) = match status {
                    Some(c) => (c, r#"{"error":{"message":"invalid key"}}"#),
                    None => (200, body),
                };
                let head = format!(
                    "HTTP/1.1 {code} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                    body.len()
                );
                let _ = sock.write_all(head.as_bytes()).await;
                let _ = sock.write_all(body.as_bytes()).await;
                let _ = sock.shutdown().await;
            });
        }
    });
    Mock { base, paths }
}

fn sources(base: &str) -> Vec<ModelListSource> {
    ["anthropic", "openai", "openrouter"]
        .iter()
        .map(|p| ModelListSource::new(p, Some("test-key"), Some(&format!("{base}/{p}"))).unwrap())
        .collect()
}

struct TestClock(Arc<AtomicU64>);

impl TestClock {
    fn new(t: u64) -> Self {
        Self(Arc::new(AtomicU64::new(t)))
    }
    fn clock(&self) -> harness_providers::catalog::Clock {
        let t = self.0.clone();
        Arc::new(move || t.load(Ordering::SeqCst))
    }
    fn advance(&self, secs: u64) {
        self.0.fetch_add(secs, Ordering::SeqCst);
    }
}

fn service(cache: &ModelListCache, base: &str, clock: &TestClock) -> CatalogService {
    CatalogService::new(
        CatalogConfig::new(Arc::new(registry()), all_keys(), cache.clone())
            .with_sources(sources(base))
            .with_ttl(Duration::from_secs(3600))
            .with_clock(clock.clock()),
    )
}

#[tokio::test]
async fn refresh_fetches_caches_and_publishes_live_lists() {
    let tmp = tempfile::tempdir().unwrap();
    let cache = ModelListCache::in_data_dir(tmp.path());
    let server = mock_with_delay(None, Duration::from_millis(200)).await;
    let clock = TestClock::new(1_000_000);
    let svc = service(&cache, &server.base, &clock);

    // Before any refresh: built-in and alias entries only, every list stale.
    let first = svc.catalog();
    assert!(first.entries().iter().all(|e| !e.sources.live));
    assert!(!first.entries().is_empty());
    assert_eq!(svc.stale_providers(), ["anthropic", "openai", "openrouter"]);
    assert!(
        first
            .providers()
            .iter()
            .all(|p| p.stale && p.fetched_at.is_none())
    );

    let mut rx = svc.subscribe();
    let handle = svc.spawn_refresh(false).expect("stale lists are refreshed");
    let wait = Duration::from_secs(10);
    tokio::time::timeout(wait, rx.wait_for(|c| c.refreshing()))
        .await
        .expect("refresh indicator is published")
        .unwrap();
    assert!(svc.spawn_refresh(false).is_none(), "one refresh at a time");
    tokio::time::timeout(wait, rx.wait_for(|c| !c.refreshing()))
        .await
        .expect("refresh finishes")
        .unwrap();
    let report = handle.await.unwrap();
    assert_eq!(report.refreshed, ["anthropic", "openai", "openrouter"]);
    assert!(report.failed.is_empty(), "{:?}", report.failed);

    let c = svc.catalog();
    assert!(!c.refreshing());
    assert!(
        c.get(&ModelRef::parse("anthropic/claude-mythos-5-1").unwrap())
            .next()
            .is_some()
    );
    assert!(
        c.get(&ModelRef::parse("openrouter/moondream/moondream3-preview").unwrap())
            .next()
            .is_some()
    );
    let st = c.provider_status("anthropic").unwrap();
    assert_eq!(
        (st.fetched_at, st.live_models, st.stale),
        (Some(1_000_000), 6, false)
    );
    assert!(svc.stale_providers().is_empty());

    // Anthropic pagination followed after_id; every request authenticated.
    let paths = server.paths.lock().unwrap().clone();
    assert!(
        paths.contains(&"/anthropic/v1/models?limit=1000 auth".to_owned()),
        "{paths:?}"
    );
    assert!(
        paths.contains(
            &"/anthropic/v1/models?limit=1000&after_id=claude-sonnet-5-5 auth".to_owned()
        ),
        "{paths:?}"
    );
    assert_eq!(paths.len(), 4, "{paths:?}");

    // Cached on disk, one file per provider.
    for p in ["anthropic", "openai", "openrouter"] {
        let list = cache.load(p).unwrap();
        assert_eq!(list.fetched_at, 1_000_000);
        assert!(!list.models.is_empty());
    }
    // A fresh cache means there is nothing to do.
    assert!(svc.spawn_refresh(false).is_none());
}

#[tokio::test]
async fn cached_catalog_is_instant_and_works_offline() {
    let tmp = tempfile::tempdir().unwrap();
    let cache = ModelListCache::in_data_dir(tmp.path());
    let clock = TestClock::new(5_000_000);
    {
        let server = mock(None).await;
        let svc = service(&cache, &server.base, &clock);
        svc.refresh(false).await;
    }
    // "Offline": the sources point at a port nobody listens on.
    let dead = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        format!("http://{}", l.local_addr().unwrap())
    };
    let svc = service(&cache, &dead, &clock);
    let c = svc.catalog();
    // Base URLs differ from the cached ones, so the lists are stale, but
    // they are still used right away.
    assert!(
        c.get(&ModelRef::parse("anthropic/claude-mythos-5-1").unwrap())
            .next()
            .is_some()
    );
    assert!(c.provider_status("openai").unwrap().stale);

    let report = svc.refresh(false).await;
    assert_eq!(report.failed.len(), 3, "{report:?}");
    let c = svc.catalog();
    assert!(
        c.get(&ModelRef::parse("anthropic/claude-mythos-5-1").unwrap())
            .next()
            .is_some()
    );
    let st = c.provider_status("anthropic").unwrap();
    assert!(
        st.error.as_deref().unwrap().contains("connection"),
        "{st:?}"
    );
    // A network failure does not make credentials unusable.
    assert!(entry(&c, "anthropic/claude-opus-5-5", AuthMethod::ApiKey).usable());
}

#[tokio::test]
async fn ttl_expiry_triggers_a_refresh_and_rejected_keys_disable_models() {
    let tmp = tempfile::tempdir().unwrap();
    let cache = ModelListCache::in_data_dir(tmp.path());
    let ok = mock(None).await;
    let clock = TestClock::new(10_000);
    service(&cache, &ok.base, &clock).refresh(false).await;

    // Same base URL, within the TTL: fresh, no fetch.
    let svc = service(&cache, &ok.base, &clock);
    assert!(svc.stale_providers().is_empty());
    assert!(svc.spawn_refresh(false).is_none());
    clock.advance(3_600);
    assert_eq!(
        svc.stale_providers().len(),
        3,
        "past the TTL every list is stale"
    );
    let report = svc.refresh(false).await;
    assert_eq!(report.refreshed.len(), 3);
    assert_eq!(cache.load("openai").unwrap().fetched_at, 13_600);

    // A 401 marks the key rejected: its models are no longer usable.
    let denied = mock(Some(401)).await;
    let svc = service(&cache, &denied.base, &clock);
    let report = svc.refresh(true).await;
    assert_eq!(report.failed.len(), 3);
    let c = svc.catalog();
    let e = entry(&c, "openai/gpt-5.5", AuthMethod::ApiKey);
    assert!(!e.usable());
    assert!(
        e.reason().unwrap().contains("rejected by openai"),
        "{:?}",
        e.reason()
    );
    assert!(matches!(
        c.provider_status("openai").unwrap().auth[0].state,
        CredentialState::Rejected(_)
    ));
    // The cached list survives the failure.
    assert_eq!(cache.load("openai").unwrap().fetched_at, 13_600);

    // New credentials clear the rejection.
    svc.set_credentials(all_keys(), sources(&ok.base));
    assert!(entry(&svc.catalog(), "openai/gpt-5.5", AuthMethod::ApiKey).usable());
}

/// Live check against the real OpenRouter API. Run with
/// `OPENROUTER_API_KEY=... cargo test -p harness-providers --test model_catalog -- --ignored`.
#[tokio::test]
#[ignore = "live: needs network and OPENROUTER_API_KEY"]
async fn live_openrouter_list() {
    let Ok(key) = std::env::var("OPENROUTER_API_KEY") else {
        eprintln!("OPENROUTER_API_KEY not set; skipping");
        return;
    };
    let tmp = tempfile::tempdir().unwrap();
    let svc = CatalogService::new(
        CatalogConfig::new(
            Arc::new(Registry::builtin()),
            Credentials::new().with("openrouter", AuthMethod::ApiKey, CredentialState::Ready),
            ModelListCache::in_data_dir(tmp.path()),
        )
        .with_sources(vec![
            ModelListSource::new("openrouter", Some(&key), None).unwrap(),
        ]),
    );
    let report = svc.refresh(false).await;
    assert_eq!(report.refreshed, ["openrouter"], "{report:?}");
    let c = svc.catalog();
    let n = c
        .by_provider("openrouter")
        .filter(|e| e.usable_for_agents())
        .count();
    assert!(n > 10, "only {n} usable OpenRouter models");
    assert!(
        c.by_provider("openrouter")
            .any(|e| e.price_origin == PriceOrigin::Live)
    );
}
