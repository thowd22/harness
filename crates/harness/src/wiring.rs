//! Builds the runtime pieces from `harness.ini`: the redactor, the model
//! registry, providers, the agent profile and its system prompt.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use harness_agent::agent_loop::CostFn;
use harness_config::agents::{Agent, Effort, PromptRef, ValidationContext};
use harness_config::discovery::{BuiltinFile, Catalog, Roots};
use harness_config::ini::Config;
use harness_config::ini::config::AuthMode;
use harness_config::paths::DataDir;
use harness_config::prompts::{PromptVars, SessionPrompt};
use harness_config::trust::TrustStore;
use harness_events::redact::Redactor;
use harness_providers::anthropic::{AnthropicConfig, AnthropicProvider};
use harness_providers::catalog::{
    AuthMethod, CatalogConfig, CatalogService, CredentialState, Credentials, ModelListCache,
    ModelListSource,
};
use harness_providers::chatgpt::{ChatGptAuth, Endpoints, TokenStore};
use harness_providers::fake::FakeProvider;
use harness_providers::http::HttpConfig;
use harness_providers::model::{ReasoningEffort, ThinkingConfig, Usage};
use harness_providers::openai::OpenAiProvider;
use harness_providers::openrouter::{OpenRouterOptions, OpenRouterProvider};
use harness_providers::pricing::Price;
use harness_providers::provider::Provider;
use harness_providers::registry::{Registry, Resolved};

/// Name of the agent used when none is given.
pub const DEFAULT_AGENT: &str = harness_config::builtins::DEFAULT_AGENT;

/// Every built-in file: default prompts, agents and the bundled workflows.
pub fn builtins() -> &'static [BuiltinFile] {
    harness_workflow::bundled::with_builtins(harness_config::builtins::BUILTINS)
}

/// The discovery roots for `cwd`: built-ins, the global config directory and
/// the project's `.harness/` when it is trusted.
pub fn roots(data: &DataDir, cwd: &Path) -> Roots {
    Roots::resolve(builtins(), &TrustStore::from_data_dir(data), Some(cwd))
}

/// Loads the catalog of prompts, agents, workflows and skills for `cwd`,
/// with the built-in identity layer per `[general] identity_prompt`.
pub fn catalog(cfg: &Config, data: &DataDir, cwd: &Path) -> Catalog {
    catalog_from(cfg, &roots(data, cwd))
}

/// Loads the catalog from `roots`, applying `[general] identity_prompt`.
pub fn catalog_from(cfg: &Config, roots: &Roots) -> Catalog {
    let mut catalog = Catalog::load(roots);
    catalog.set_identity_prompt(cfg.general.identity_prompt);
    catalog
}

/// Agent validation context: `[models]`, the built-in model aliases and the
/// configured remotes.
pub fn validation_context(cfg: &Config) -> ValidationContext {
    let mut ctx = ValidationContext::from_config(cfg);
    ctx.models = ctx.models.with_aliases(
        harness_providers::registry::BUILTIN_ALIASES
            .iter()
            .map(|(alias, _)| *alias),
    );
    ctx
}

/// Every resolvable credential from `[provider.*]`, plus the stored
/// ChatGPT tokens and MCP OAuth tokens, for redaction and env scrubbing.
/// Failures are reported and skipped.
pub fn credentials(cfg: &Config) -> Vec<String> {
    let mut out = chatgpt_secrets(&chatgpt_store(cfg));
    out.extend(mcp_token_store(cfg).secrets());
    for (name, provider) in &cfg.providers {
        match provider.resolve_api_key() {
            Ok(Some(secret)) => out.push(secret.expose().to_owned()),
            Ok(None) => {}
            Err(e) => tracing::warn!(provider = %name, error = %e, "could not resolve api key"),
        }
    }
    out
}

/// A redactor masking `secrets` plus the built-in patterns.
pub fn redactor(secrets: &[String]) -> Arc<Redactor> {
    Arc::new(Redactor::new(secrets.iter()))
}

/// The model registry with `[models]` aliases and `[pricing]` overrides.
pub fn registry(cfg: &Config) -> Result<Arc<Registry>, String> {
    let cache = ModelListCache::new(DataDir::from_config(cfg).model_lists());
    let live = harness_config::ini::config::PROVIDER_IDS
        .iter()
        .filter_map(|p| cache.load(p))
        .flat_map(|list| {
            list.models
                .iter()
                .map(|m| harness_providers::catalog::live_model_info(&list.provider, m))
                .collect::<Vec<_>>()
        });
    let registry = Registry::builtin()
        .with_live_models(live)
        .with_aliases(cfg.models.iter())
        .map_err(|e| format!("[models]: {e}"))?
        .with_price_overrides(cfg.pricing.iter().map(|(k, p)| {
            (
                k,
                Price {
                    input: p.input,
                    output: p.output,
                    cache_read: p.cache_read,
                    cache_write: p.cache_write,
                },
            )
        }));
    Ok(Arc::new(registry))
}

/// The API key for `name` from `harness.ini`.
fn api_key(cfg: &Config, name: &str) -> Result<harness_config::secrets::Secret, String> {
    cfg.providers
        .get(name)
        .cloned()
        .unwrap_or_default()
        .resolve_api_key()
        .map_err(|e| format!("[provider.{name}]: {e}"))?
        .ok_or_else(|| {
            format!(
                "no API key for `{name}`: set api_key, api_key_cmd or api_key_env in [provider.{name}]"
            )
        })
}

/// The OpenRouter `session_id` of this process (`[provider.openrouter]
/// session_id`, on by default): one id per harness process, so every
/// request of the session (subagents and compaction included) routes to
/// one upstream.
fn openrouter_session_id() -> String {
    static ID: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    ID.get_or_init(|| format!("harness-{}", harness_events::schema::RunId::new()))
        .clone()
}

fn openrouter(cfg: &Config, key: &str) -> Result<OpenRouterProvider, String> {
    let pcfg = cfg
        .providers
        .get(harness_providers::openrouter::PROVIDER_NAME)
        .cloned()
        .unwrap_or_default();
    let options = OpenRouterOptions {
        app_name: pcfg.app_name.clone(),
        provider_order: pcfg.provider_order.clone(),
        allow_fallbacks: pcfg.allow_fallbacks,
        cache_ttl: pcfg.cache_ttl.map(|t| {
            use harness_config::ini::config::CacheTtl as Ini;
            use harness_providers::openrouter::CacheTtl;
            match t {
                Ini::FiveMinutes => CacheTtl::FiveMinutes,
                // `mixed` is refused in [provider.openrouter].
                Ini::OneHour | Ini::Mixed => CacheTtl::OneHour,
            }
        }),
        session_id: pcfg.session_id.unwrap_or(true).then(openrouter_session_id),
        reasoning_exclude: pcfg.reasoning_exclude == Some(true),
        service_tier: pcfg.service_tier.map(|t| {
            use harness_config::ini::config::OpenRouterServiceTier as Ini;
            use harness_providers::openrouter::ServiceTier;
            match t {
                Ini::Default => ServiceTier::Default,
                Ini::Fast => ServiceTier::Fast,
            }
        }),
    };
    // Session start checks the model against the cached `/models` list
    // (shared with `/model`) instead of downloading 800 KB each time.
    let cache = ModelListCache::new(DataDir::from_config(cfg).model_lists());
    Ok(
        OpenRouterProvider::with_key(key, pcfg.base_url.as_deref(), options)
            .map_err(|e| format!("openrouter: {e}"))?
            .with_model_cache(cache, cfg.general.model_list_ttl),
    )
}

/// The Anthropic provider settings from `[provider.anthropic]`: base URL,
/// request-body gzip (`gzip_requests`, `gzip_level`), `thinking_display`,
/// `cache_ttl` and `cache_ttl_auto_1h`.
pub fn anthropic_config(cfg: &Config) -> AnthropicConfig {
    use harness_config::ini::config::GzipRequests as Ini;
    use harness_providers::anthropic::GzipRequests;
    let mut conf = AnthropicConfig::new();
    let Some(p) = cfg.providers.get(harness_providers::anthropic::NAME) else {
        return conf;
    };
    if let Some(b) = &p.base_url {
        conf = conf.with_base_url(b);
    }
    conf.gzip = match p.gzip_requests {
        Some(Ini::Off) => GzipRequests::Off,
        Some(Ini::Auto) | None => GzipRequests::Auto,
    };
    if let Some(level) = p.gzip_level {
        conf.gzip_level = level;
    }
    {
        use harness_config::ini::config::{CacheTtl as Ttl, ThinkingDisplay as Disp};
        use harness_providers::anthropic::{CacheTtl, ThinkingDisplay};
        if let Some(d) = p.thinking_display {
            conf.request.thinking_display = match d {
                Disp::Summarized => ThinkingDisplay::Summarized,
                Disp::Omitted => ThinkingDisplay::Omitted,
                Disp::Updates => ThinkingDisplay::Updates,
            };
        }
        if let Some(t) = p.cache_ttl {
            conf.request.cache_ttl = match t {
                Ttl::FiveMinutes => CacheTtl::FiveMinutes,
                Ttl::OneHour => CacheTtl::OneHour,
                Ttl::Mixed => CacheTtl::Mixed,
            };
        }
    }
    if p.cache_ttl_auto_1h == Some(true) {
        conf.long_wait = Some(harness_providers::cache_hint::LongWait::process());
    }
    conf
}

/// The MCP OAuth token store: `<data_dir>/auth/mcp`, inside the built-in
/// protected `auth` directory.
pub fn mcp_token_store(cfg: &Config) -> harness_mcp::auth::McpTokenStore {
    harness_mcp::auth::McpTokenStore::new(DataDir::from_config(cfg).mcp_auth())
}

/// The Sign in with ChatGPT token store: `<data_dir>/auth`, a built-in
/// protected path.
pub fn chatgpt_store(cfg: &Config) -> TokenStore {
    TokenStore::new(DataDir::from_config(cfg).auth())
}

/// The access, refresh and ID tokens in `store`, so the redactor masks
/// them everywhere.
pub fn chatgpt_secrets(store: &TokenStore) -> Vec<String> {
    let Ok(record) = store.load() else {
        return Vec::new();
    };
    record
        .tokens
        .map(|t| {
            [Some(t.access_token), t.refresh_token, t.id_token]
                .into_iter()
                .flatten()
                .map(|t| t.expose().to_owned())
                .collect()
        })
        .unwrap_or_default()
}

/// OpenAI's sign-in service and API; `[provider.openai] base_url` moves
/// the API.
pub fn chatgpt_endpoints(cfg: &Config) -> Endpoints {
    let mut e = Endpoints::openai();
    if let Some(b) = cfg
        .providers
        .get("openai")
        .and_then(|p| p.base_url.as_deref())
    {
        e.api_base_url = b.trim_end_matches('/').to_owned();
    }
    e
}

/// Whether this process may use personal credentials (Sign in with
/// ChatGPT) without `chatgpt_headless`. Only the interactive TUI sets it.
static INTERACTIVE: AtomicBool = AtomicBool::new(false);

/// Marks this process as an interactive session (the TUI), where Sign in
/// with ChatGPT is always allowed. Headless runs, workflows and evals leave
/// it unset.
pub fn set_interactive(interactive: bool) {
    INTERACTIVE.store(interactive, Ordering::SeqCst);
}

/// Refuses `auth = chatgpt` outside an interactive session unless
/// `[provider.openai] chatgpt_headless = true`, and always in CI: the
/// credential is the user's own ChatGPT plan.
pub fn check_chatgpt_allowed(
    cfg: &Config,
    interactive: bool,
    env: &dyn Fn(&str) -> Option<String>,
) -> Result<(), String> {
    if interactive {
        return Ok(());
    }
    let ci = env("CI").is_some_and(|v| !matches!(v.trim(), "" | "0" | "false"));
    if ci {
        return Err(
            "Sign in with ChatGPT is a personal credential and is not used in CI \
             (CI is set); give this run an OpenAI API key (auth = api_key in [provider.openai])"
                .into(),
        );
    }
    let allowed = cfg
        .providers
        .get("openai")
        .is_some_and(|p| p.chatgpt_headless);
    if allowed {
        Ok(())
    } else {
        Err(
            "headless and eval runs do not use Sign in with ChatGPT because it is a personal \
             credential; set chatgpt_headless = true in [provider.openai] to allow it on this \
             machine, or use an OpenAI API key (auth = api_key)"
                .into(),
        )
    }
}

/// The OpenAI provider in ChatGPT mode, sharing one token cache per
/// process. Never falls back to an API key.
fn chatgpt_provider(cfg: &Config, interactive: bool) -> Result<OpenAiProvider, String> {
    check_chatgpt_allowed(cfg, interactive, &|k| std::env::var(k).ok())?;
    let endpoints = chatgpt_endpoints(cfg);
    let http = HttpConfig::new(endpoints.api_base_url.clone());
    let auth = ChatGptAuth::shared(chatgpt_store(cfg), endpoints);
    OpenAiProvider::with_chatgpt(auth, http).map_err(|e| format!("openai: {e}"))
}

/// True when `[provider.openai] auth = chatgpt`.
pub fn openai_uses_chatgpt(cfg: &Config) -> bool {
    cfg.providers
        .get("openai")
        .is_some_and(|p| p.auth_mode() == AuthMode::ChatGpt)
}

/// True when `[provider.openai] prompt_cache_retention = 24h` opts in to
/// extended prompt-cache retention (off by default).
pub fn openai_extended_cache_retention(cfg: &Config) -> bool {
    cfg.providers.get("openai").is_some_and(|p| {
        p.prompt_cache_retention
            == Some(harness_config::ini::config::OpenAiCacheRetention::TwentyFourHours)
    })
}

/// The provider for `resolved`, with its key from `harness.ini`, without
/// network checks.
pub fn provider_sync(
    cfg: &Config,
    registry: &Arc<Registry>,
    resolved: &Resolved,
) -> Result<Arc<dyn Provider>, String> {
    let name = resolved.reference.provider.as_str();
    if name == harness_providers::openai::PROVIDER_NAME && openai_uses_chatgpt(cfg) {
        return Ok(Arc::new(chatgpt_provider(
            cfg,
            INTERACTIVE.load(Ordering::SeqCst),
        )?));
    }
    let key = api_key(cfg, name)?;
    let base = cfg.providers.get(name).and_then(|p| p.base_url.clone());
    let err = |e: harness_providers::provider::ProviderError| format!("{name}: {e}");
    Ok(match name {
        harness_providers::anthropic::NAME => Arc::new(
            AnthropicProvider::new(key.expose(), anthropic_config(cfg))
                .map_err(err)?
                .with_registry(registry.clone()),
        ),
        harness_providers::openai::PROVIDER_NAME => Arc::new(
            OpenAiProvider::with_key(key.expose(), base.as_deref())
                .map_err(err)?
                .with_extended_cache_retention(openai_extended_cache_retention(cfg)),
        ),
        harness_providers::openrouter::PROVIDER_NAME => Arc::new(openrouter(cfg, key.expose())?),
        other => return Err(format!("unknown provider `{other}`")),
    })
}

/// The provider for `resolved`. OpenRouter models are checked against the
/// live catalog: an agent with tools on a model without tool support is
/// rejected here, at load time, with a clear error.
pub async fn provider(
    cfg: &Config,
    registry: &Arc<Registry>,
    resolved: &Resolved,
    needs_tools: bool,
) -> Result<Arc<dyn Provider>, String> {
    if resolved.reference.provider != harness_providers::openrouter::PROVIDER_NAME {
        return provider_sync(cfg, registry, resolved);
    }
    let key = api_key(cfg, &resolved.reference.provider)?;
    let p = openrouter(cfg, key.expose())?;
    p.check_agent_model(&resolved.reference.model, needs_tools)
        .await
        .map_err(|e| format!("openrouter: {e}"))?;
    Ok(Arc::new(p))
}

/// The fake provider replaying `script`.
pub fn fake_provider(script: &Path) -> Result<Arc<dyn Provider>, String> {
    let script =
        harness_providers::fake::FakeScript::from_yaml_file(script).map_err(|e| e.to_string())?;
    Ok(Arc::new(FakeProvider::new(script)))
}

/// Pricing function for the loop (`None` = unknown cost).
pub fn cost_fn(resolved: &Resolved) -> CostFn {
    let resolved = resolved.clone();
    Arc::new(move |u: &Usage| resolved.cost(u))
}

/// The agent called `name` from the catalog.
pub fn agent(catalog: &Catalog, name: &str) -> Result<Agent, String> {
    match catalog.agents.get(name) {
        Some(a) => Ok(a.clone()),
        None => {
            let known: Vec<&str> = catalog
                .agents
                .active()
                .iter()
                .map(|e| e.item.name.as_str())
                .collect();
            Err(format!(
                "unknown agent `{name}`; known agents: {}",
                known.join(", ")
            ))
        }
    }
}

/// Renders the agent's system prompt for `cwd`, followed by the provenance
/// section that explains tool-result markers to the model.
pub fn system_prompt(catalog: &Catalog, agent: &Agent, cwd: &Path) -> Result<String, String> {
    let body = match &agent.prompt {
        PromptRef::Inline(text) => text.replace("{cwd}", &cwd.display().to_string()),
        PromptRef::Named(name) => {
            let prompt = catalog
                .prompts
                .get(name)
                .ok_or_else(|| format!("agent `{}` uses unknown prompt `{name}`", agent.name))?;
            SessionPrompt::new(prompt, PromptVars::detect(cwd))
                .map_err(|e| format!("prompt `{name}`: {e}"))?
                .text()
                .to_owned()
        }
    };
    Ok(format!(
        "{body}\n\n{}",
        harness_policy::provenance::SYSTEM_PROMPT_SECTION
    ))
}

/// The reasoning settings of the session's agent: the `/effort` override
/// when one is set, else the agent YAML's `effort` (`None`: no reasoning
/// fields, the model default).
pub fn thinking(agent: &Agent, session: Option<Effort>) -> Option<ThinkingConfig> {
    session.or(agent.effort).map(|e| ThinkingConfig {
        budget_tokens: None,
        effort: Some(match e {
            Effort::Low => ReasoningEffort::Low,
            Effort::Medium => ReasoningEffort::Medium,
            Effort::High => ReasoningEffort::High,
        }),
    })
}

/// The project root: `--cwd` or the current directory, canonicalized.
pub fn project_dir(arg: Option<&PathBuf>) -> Result<PathBuf, String> {
    let dir = match arg {
        Some(d) => d.clone(),
        None => std::env::current_dir().map_err(|e| format!("current directory: {e}"))?,
    };
    dir.canonicalize()
        .map_err(|e| format!("{}: {e}", dir.display()))
}

/// Credential state of every configured provider for the model catalog:
/// a resolvable API key is ready, a failing `api_key_cmd` is rejected and
/// no key is missing. Providers without a `[provider.*]` section count as
/// missing.
pub fn catalog_credentials(cfg: &Config) -> Credentials {
    let mut creds = Credentials::new();
    for name in harness_config::ini::config::PROVIDER_IDS {
        if *name == "openai" && openai_uses_chatgpt(cfg) {
            creds.set(name, AuthMethod::ApiKey, CredentialState::Missing);
            creds.set(
                name,
                AuthMethod::ChatGpt,
                chatgpt_state(&chatgpt_store(cfg)),
            );
            continue;
        }
        let state = match cfg.providers.get(*name).map(|p| p.resolve_api_key()) {
            Some(Ok(Some(_))) => CredentialState::Ready,
            Some(Err(e)) => CredentialState::Rejected(e.to_string()),
            Some(Ok(None)) | None => CredentialState::Missing,
        };
        creds.set(name, AuthMethod::ApiKey, state);
    }
    creds
}

/// The catalog state of the ChatGPT sign-in in `store`.
fn chatgpt_state(store: &TokenStore) -> CredentialState {
    match store.load() {
        Ok(r) => match r.tokens {
            Some(t) if t.plan_usage() => CredentialState::Ready,
            Some(_) => CredentialState::Rejected("ChatGPT plan usage was not granted".into()),
            None => CredentialState::Missing,
        },
        Err(e) => CredentialState::Rejected(e.to_string()),
    }
}

/// Live model-list sources for every provider with a resolvable API key,
/// honouring `base_url`. With `auth = chatgpt`, OpenAI's source is the
/// signed-in account's own plan list instead (none while signed out).
pub fn model_list_sources(cfg: &Config) -> Vec<ModelListSource> {
    harness_config::ini::config::PROVIDER_IDS
        .iter()
        .filter_map(|name| {
            let p = cfg.providers.get(*name)?;
            if *name == "openai" && p.auth_mode() == AuthMode::ChatGpt {
                return chatgpt_model_list_source(cfg);
            }
            let key = p.resolve_api_key().ok()??;
            ModelListSource::new(name, Some(key.expose()), p.base_url.as_deref())
        })
        .collect()
}

/// Drops `provider`'s cached live model list, so the next catalog refresh
/// fetches it again (after a sign-in changes what the account may use).
pub fn forget_model_list(cfg: &Config, provider: &str) {
    let path = ModelListCache::new(DataDir::from_config(cfg).model_lists()).path(provider);
    match std::fs::remove_file(&path) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => {
            tracing::warn!(path = %path.display(), error = %e, "could not drop the cached model list")
        }
    }
}

/// The ChatGPT account's model list, when signed in with plan usage.
fn chatgpt_model_list_source(cfg: &Config) -> Option<ModelListSource> {
    let store = chatgpt_store(cfg);
    let tokens = store.load().ok()?.tokens.filter(|t| t.plan_usage())?;
    let endpoints = chatgpt_endpoints(cfg);
    let base = endpoints.api_base_url.clone();
    let auth = ChatGptAuth::shared(store, endpoints);
    Some(ModelListSource::chatgpt(
        auth,
        Some(&base),
        &tokens.account.sub,
    ))
}

/// The model catalog service for `/model`: built-in models, `[models]`
/// aliases and live lists cached in `<data_dir>/model-lists` with
/// `[general] model_list_ttl`. Reads only the cache; call
/// [`CatalogService::spawn_refresh`] to refresh stale lists in the
/// background.
pub fn model_catalog(cfg: &Config, registry: &Arc<Registry>) -> CatalogService {
    let cache = ModelListCache::new(DataDir::from_config(cfg).model_lists());
    CatalogService::new(
        CatalogConfig::new(registry.clone(), catalog_credentials(cfg), cache)
            .with_sources(model_list_sources(cfg))
            .with_ttl(cfg.general.model_list_ttl),
    )
}

#[cfg(test)]
#[path = "wiring_pool_tests.rs"]
mod pool_tests;

#[cfg(test)]
mod catalog_tests {
    use super::*;

    #[test]
    fn catalog_credentials_and_sources_follow_harness_ini() {
        let tmp = tempfile::tempdir().unwrap();
        let ini = format!(
            "[general]\ndata_dir = {}\nmodel_list_ttl = 2h\n\
             [provider.openrouter]\napi_key_env = HARNESS_TEST_CATALOG_OR_KEY_UNSET\n\
             [provider.openai]\napi_key_cmd = exit 3\n",
            tmp.path().display()
        );
        let cfg = harness_config::ini::load_str(&ini, None).unwrap();
        let creds = catalog_credentials(&cfg);
        assert_eq!(
            creds.statuses("anthropic")[0].state,
            CredentialState::Missing
        );
        assert_eq!(
            creds.statuses("openrouter")[0].state,
            CredentialState::Missing
        );
        assert!(matches!(
            creds.statuses("openai")[0].state,
            CredentialState::Rejected(_)
        ));
        assert!(model_list_sources(&cfg).is_empty());

        let registry = registry(&cfg).unwrap();
        let service = model_catalog(&cfg, &registry);
        let catalog = service.catalog();
        assert!(!catalog.entries().is_empty());
        assert!(catalog.entries().iter().all(|e| !e.usable()));
        assert!(
            catalog.providers().iter().all(|p| !p.stale),
            "providers without a source have nothing to refresh"
        );
        assert!(service.spawn_refresh(false).is_none(), "nothing to fetch");
    }
}

#[cfg(test)]
mod identity_tests {
    use super::*;
    use harness_config::prompts::IDENTITY;

    #[test]
    fn identity_prompt_setting_controls_the_system_prompt() {
        let cwd = tempfile::tempdir().unwrap();
        let roots = Roots::new(builtins());
        for (ini, on) in [
            ("", true),
            ("[general]\nidentity_prompt = on\n", true),
            ("[general]\nidentity_prompt = off\n", false),
        ] {
            let cfg = harness_config::ini::load_str(ini, None).unwrap();
            let catalog = catalog_from(&cfg, &roots);
            for name in harness_config::builtins::AGENTS {
                let a = agent(&catalog, name).unwrap();
                let system = system_prompt(&catalog, &a, cwd.path()).unwrap();
                assert_eq!(system.starts_with(IDENTITY), on, "{ini:?} agent {name}");
                assert_eq!(system.contains(IDENTITY), on, "{ini:?} agent {name}");
            }
        }
    }
}

#[cfg(test)]
mod anthropic_config_tests {
    use super::*;
    use harness_providers::anthropic::GzipRequests;

    #[test]
    fn gzip_settings_come_from_provider_anthropic() {
        let cfg = harness_config::ini::load_str(
            "[provider.anthropic]\nbase_url = https://gw.example.com\ngzip_requests = off\ngzip_level = 7\n",
            None,
        )
        .unwrap();
        let conf = anthropic_config(&cfg);
        assert_eq!(conf.http.base_url, "https://gw.example.com");
        assert_eq!(conf.gzip, GzipRequests::Off);
        assert_eq!(conf.gzip_level, 7);
        assert_eq!(conf.request, Default::default(), "request options default");
        assert_eq!(conf.long_wait, None);
        let cfg = harness_config::ini::load_str(
            "[provider.anthropic]\nthinking_display = updates\ncache_ttl = mixed\ncache_ttl_auto_1h = true\n",
            None,
        )
        .unwrap();
        let conf = anthropic_config(&cfg);
        assert_eq!(
            conf.request.thinking_display,
            harness_providers::anthropic::ThinkingDisplay::Updates
        );
        assert_eq!(
            conf.request.cache_ttl,
            harness_providers::anthropic::CacheTtl::Mixed
        );
        assert_eq!(
            conf.long_wait,
            Some(harness_providers::cache_hint::LongWait::process())
        );
        let conf = anthropic_config(&harness_config::ini::load_str("", None).unwrap());
        assert_eq!(conf.gzip, GzipRequests::Auto);
        assert_eq!(conf.gzip_level, 3);
        assert_eq!(
            conf.http.base_url,
            harness_providers::anthropic::DEFAULT_BASE_URL
        );
    }
}

#[cfg(test)]
mod chatgpt_tests {
    use super::*;
    use harness_providers::catalog::live::ListKind;
    use harness_providers::chatgpt::store::{Account, AuthRecord, StoredTokens, Token};

    fn cfg(extra: &str, data: &Path) -> Config {
        harness_config::ini::load_str(
            &format!(
                "[general]\ndata_dir = {}\n[provider.openai]\napi_key = sk-should-not-be-used\n{extra}",
                data.display()
            ),
            None,
        )
        .unwrap()
    }

    fn signed_in(cfg: &Config) {
        chatgpt_store(cfg)
            .save(&AuthRecord {
                client_id: Some("oaiapp_1".into()),
                tokens: Some(StoredTokens {
                    access_token: Token::new("at-plan-secret"),
                    refresh_token: Some(Token::new("rt-plan-secret")),
                    id_token: None,
                    expires_at: u64::MAX / 2,
                    scopes: vec![harness_providers::chatgpt::PLAN_SCOPE.into()],
                    account: Account::default(),
                }),
            })
            .unwrap();
    }

    #[test]
    fn headless_runs_refuse_chatgpt_unless_allowed_and_never_in_ci() {
        let tmp = tempfile::tempdir().unwrap();
        let none = |_: &str| None;
        let ci = |k: &str| (k == "CI").then(|| "true".to_owned());
        let c = cfg("auth = chatgpt\n", tmp.path());
        assert!(check_chatgpt_allowed(&c, true, &none).is_ok(), "TUI");
        let e = check_chatgpt_allowed(&c, false, &none).unwrap_err();
        assert!(e.contains("chatgpt_headless = true"), "{e}");
        let allowed = cfg("auth = chatgpt\nchatgpt_headless = true\n", tmp.path());
        assert!(check_chatgpt_allowed(&allowed, false, &none).is_ok());
        let e = check_chatgpt_allowed(&allowed, false, &ci).unwrap_err();
        assert!(e.contains("not used in CI"), "{e}");
        let zero = |k: &str| (k == "CI").then(|| "0".to_owned());
        assert!(check_chatgpt_allowed(&allowed, false, &zero).is_ok());
        // The refusal is the provider error: no silent fallback to the key.
        assert!(chatgpt_provider(&c, false).is_err());
        assert!(chatgpt_provider(&c, true).unwrap().uses_chatgpt());
    }

    #[test]
    fn openai_extended_cache_retention_is_opt_in() {
        let tmp = tempfile::tempdir().unwrap();
        let registry = Arc::new(Registry::builtin());
        let resolved = registry.resolve("openai/gpt-5").unwrap();
        let req = harness_providers::model::ChatRequest::new("gpt-5.5");
        let default = cfg("", tmp.path());
        assert!(!openai_extended_cache_retention(&default));
        let body = provider_sync(&default, &registry, &resolved)
            .unwrap()
            .request_body(&req)
            .unwrap();
        assert!(body.get("prompt_cache_retention").is_none(), "{body}");
        assert!(body["prompt_cache_key"].is_string());
        let explicit = cfg("prompt_cache_retention = default\n", tmp.path());
        assert!(!openai_extended_cache_retention(&explicit));
        let opted = cfg("prompt_cache_retention = 24h\n", tmp.path());
        assert!(openai_extended_cache_retention(&opted));
        let body = provider_sync(&opted, &registry, &resolved)
            .unwrap()
            .request_body(&req)
            .unwrap();
        assert_eq!(body["prompt_cache_retention"], "24h");
    }

    #[test]
    fn auth_selects_the_openai_method() {
        let tmp = tempfile::tempdir().unwrap();
        let registry = Arc::new(Registry::builtin());
        let resolved = registry.resolve("openai/gpt-5").unwrap();
        let key = cfg("", tmp.path());
        assert!(!openai_uses_chatgpt(&key));
        assert!(provider_sync(&key, &registry, &resolved).is_ok());
        let explicit = cfg("auth = api_key\n", tmp.path());
        assert!(!openai_uses_chatgpt(&explicit));
        let chat = cfg("auth = chatgpt\n", tmp.path());
        assert!(openai_uses_chatgpt(&chat));
        // This test process is not interactive, so the default refuses. The
        // reason follows the real environment: CI runners set CI, which is
        // checked first.
        let err = provider_sync(&chat, &registry, &resolved).err().unwrap();
        let in_ci = std::env::var("CI").is_ok_and(|v| !matches!(v.trim(), "" | "0" | "false"));
        let expected = if in_ci { "not used in CI" } else { "headless" };
        assert!(err.contains(expected), "{err}");
    }

    #[test]
    fn catalog_and_redactor_see_the_chatgpt_sign_in() {
        let tmp = tempfile::tempdir().unwrap();
        let c = cfg("auth = chatgpt\n", tmp.path());
        let creds = catalog_credentials(&c);
        let state = |m: AuthMethod| {
            creds
                .statuses("openai")
                .into_iter()
                .find(|s| s.method == m)
                .map(|s| s.state)
        };
        assert_eq!(state(AuthMethod::ChatGpt), Some(CredentialState::Missing));
        assert_eq!(state(AuthMethod::ApiKey), Some(CredentialState::Missing));
        assert!(
            model_list_sources(&c).is_empty(),
            "no API-key listing in chatgpt mode"
        );
        signed_in(&c);
        let creds = catalog_credentials(&c);
        assert_eq!(
            creds
                .statuses("openai")
                .into_iter()
                .find(|s| s.method == AuthMethod::ChatGpt)
                .map(|s| s.state),
            Some(CredentialState::Ready)
        );
        // Signed in: OpenAI's list is the account's plan list.
        let sources = model_list_sources(&c);
        assert_eq!(sources.len(), 1);
        assert_eq!(sources[0].kind, ListKind::ChatGpt);
        assert!(sources[0].origin().contains("#chatgpt:"));
        assert!(!format!("{:?}", sources[0]).contains("at-plan-secret"));
        let secrets = credentials(&c);
        assert!(secrets.contains(&"at-plan-secret".to_owned()));
        assert!(secrets.contains(&"rt-plan-secret".to_owned()));
        let r = redactor(&secrets);
        let out = r.redact("token at-plan-secret and rt-plan-secret");
        assert!(!out.contains("plan-secret"), "{out}");
    }

    #[test]
    fn runs_learn_models_from_cached_live_lists_and_sign_in_drops_the_list() {
        let tmp = tempfile::tempdir().unwrap();
        let c = cfg("auth = chatgpt\n", tmp.path());
        let cache = ModelListCache::new(DataDir::from_config(&c).model_lists());
        let astra = harness_providers::catalog::LiveModel {
            id: "gpt-6-astra".into(),
            context_window: Some(272_000),
            tools: Some(true),
            reasoning: Some(true),
            vision: Some(true),
            plan: Some(true),
            rank: Some(0),
            ..Default::default()
        };
        cache
            .store(&harness_providers::catalog::LiveList::new(
                "openai",
                "x",
                0,
                vec![astra],
            ))
            .unwrap();
        let resolved = registry(&c).unwrap().resolve("openai/gpt-6-astra").unwrap();
        assert!(resolved.warnings.is_empty(), "{:?}", resolved.warnings);
        assert_eq!(resolved.capabilities().context_window, 272_000);
        assert_eq!(
            resolved.info.thinking,
            harness_providers::registry::ThinkingStyle::Effort
        );
        forget_model_list(&c, "openai");
        assert!(cache.load("openai").is_none());
        // Dropping a list that is not there is fine.
        forget_model_list(&c, "openai");
        assert!(
            !registry(&c)
                .unwrap()
                .resolve("openai/gpt-6-astra")
                .unwrap()
                .warnings
                .is_empty()
        );
    }

    #[test]
    fn token_store_is_a_protected_path() {
        let tmp = tempfile::tempdir().unwrap();
        let c = cfg("auth = chatgpt\n", tmp.path());
        let data = DataDir::from_config(&c);
        assert!(chatgpt_store(&c).dir().starts_with(data.root()));
        assert!(data.protected_paths(None).contains(&data.auth()));

        // Agent tools cannot read or copy the token file, even in auto mode.
        use harness_policy::rules::{Action, Decision, PolicyConfig, PolicyEngine};
        let project = tmp.path().join("project");
        std::fs::create_dir_all(&project).unwrap();
        let engine = PolicyEngine::new(&PolicyConfig::from_config(&c), &project).unwrap();
        let file = chatgpt_store(&c).auth_path();
        let mode = harness_policy::rules::Mode::Auto;
        for action in [
            Action::Read {
                tool: "read".into(),
                path: file.clone(),
            },
            Action::Bash {
                command: format!("cat {}", file.display()),
            },
            Action::Bash {
                command: format!("scp {} host:", file.display()),
            },
        ] {
            let v = engine.check(&action, mode, &project);
            assert_eq!(v.decision, Decision::Deny, "{action:?}");
        }
    }
}

#[cfg(test)]
mod openrouter_tests {
    use super::*;
    use harness_providers::openrouter::CacheTtl;

    #[test]
    fn openrouter_cache_ttl_follows_harness_ini() {
        for (ini, want) in [
            ("", None),
            (
                "[provider.openrouter]\ncache_ttl = 5m\n",
                Some(CacheTtl::FiveMinutes),
            ),
            (
                "[provider.openrouter]\ncache_ttl = 1h\n",
                Some(CacheTtl::OneHour),
            ),
        ] {
            let cfg = harness_config::ini::load_str(ini, None).unwrap();
            let p = openrouter(&cfg, "test-key").unwrap();
            assert_eq!(p.options().cache_ttl, want, "{ini}");
        }
    }

    #[test]
    fn openrouter_session_id_is_on_by_default_and_stable_per_process() {
        let cfg =
            harness_config::ini::load_str("[provider.openrouter]\nsession_id = false\n", None)
                .unwrap();
        assert_eq!(openrouter(&cfg, "k").unwrap().options().session_id, None);
        let cfg = harness_config::ini::load_str("", None).unwrap();
        let a = openrouter(&cfg, "k").unwrap().options().session_id.clone();
        let b = openrouter(&cfg, "k").unwrap().options().session_id.clone();
        assert!(
            a.as_deref()
                .is_some_and(|id| id.starts_with("harness-") && id.len() <= 256)
        );
        assert_eq!(a, b);
    }

    #[test]
    fn openrouter_reasoning_exclude_is_off_unless_set() {
        let cfg = harness_config::ini::load_str("", None).unwrap();
        assert!(!openrouter(&cfg, "k").unwrap().options().reasoning_exclude);
        let cfg = harness_config::ini::load_str(
            "[provider.openrouter]\nreasoning_exclude = true\n",
            None,
        )
        .unwrap();
        assert!(openrouter(&cfg, "k").unwrap().options().reasoning_exclude);
    }

    #[test]
    fn openrouter_service_tier_is_unset_unless_set() {
        use harness_providers::openrouter::ServiceTier;
        let cfg = harness_config::ini::load_str("", None).unwrap();
        assert_eq!(openrouter(&cfg, "k").unwrap().options().service_tier, None);
        let cfg =
            harness_config::ini::load_str("[provider.openrouter]\nservice_tier = fast\n", None)
                .unwrap();
        assert_eq!(
            openrouter(&cfg, "k").unwrap().options().service_tier,
            Some(ServiceTier::Fast)
        );
    }
}

/// TASK-163 AC3: the `/effort` override reaches each provider's wire
/// format through the same wiring as a session (registry, `harness.ini`
/// keys, `provider_sync`), and models that do not reason get no
/// reasoning fields.
#[cfg(test)]
mod effort_wire_tests {
    use super::*;
    use harness_config::discovery::{Artifact, Kind, Scope, SourceFile};
    use harness_providers::model::{ChatRequest, Message};
    use serde_json::{Value, json};

    fn agent(yaml_effort: Option<&str>) -> Agent {
        let effort = yaml_effort.map_or(String::new(), |e| format!("effort: {e}\n"));
        Agent::load(&SourceFile::inline(
            Scope::Global,
            Kind::Agent,
            "agents/a.yaml",
            format!("name: a\nprompt_text: x\n{effort}"),
        ))
        .unwrap()
    }

    /// The body `model` would be sent with `agent` and the session effort.
    fn body(model: &str, agent: &Agent, session: Option<Effort>) -> Value {
        let tmp = tempfile::tempdir().unwrap();
        let ini = format!(
            "[general]\ndata_dir = {}\n\
             [provider.anthropic]\napi_key = test-a\n\
             [provider.openai]\napi_key = test-o\n\
             [provider.openrouter]\napi_key = test-r\n",
            tmp.path().display()
        );
        let cfg = harness_config::ini::load_str(&ini, None).unwrap();
        let registry = registry(&cfg).unwrap();
        let resolved = registry.resolve(model).unwrap();
        let provider = provider_sync(&cfg, &registry, &resolved).unwrap();
        let mut request = ChatRequest::new(resolved.reference.model.clone());
        request.messages = vec![Message::user("hi")];
        request.thinking = thinking(agent, session);
        provider.request_body(&request).expect("a wire format")
    }

    #[test]
    fn anthropic_adaptive_models_get_output_config_effort() {
        let a = agent(None);
        let b = body("anthropic/claude-opus-5-5", &a, Some(Effort::Low));
        assert_eq!(b["thinking"]["type"], "adaptive");
        assert_eq!(b["output_config"], json!({"effort": "low"}));
        // `default` on an agent without `effort`: no reasoning fields.
        let b = body("anthropic/claude-opus-5-5", &a, None);
        assert!(b.get("thinking").is_none() && b.get("output_config").is_none());
        // `default` restores the agent YAML's effort; the override wins.
        let a = agent(Some("high"));
        let b = body("anthropic/claude-opus-5-5", &a, None);
        assert_eq!(b["output_config"], json!({"effort": "high"}));
        let b = body("anthropic/claude-opus-5-5", &a, Some(Effort::Medium));
        assert_eq!(b["output_config"], json!({"effort": "medium"}));
    }

    #[test]
    fn anthropic_budget_models_get_a_thinking_budget() {
        use harness_providers::anthropic::request::effort_budget;
        use harness_providers::model::ReasoningEffort;
        let a = agent(None);
        for (e, r) in [
            (Effort::Low, ReasoningEffort::Low),
            (Effort::Medium, ReasoningEffort::Medium),
            (Effort::High, ReasoningEffort::High),
        ] {
            let b = body("anthropic/claude-haiku-4-5", &a, Some(e));
            assert_eq!(
                b["thinking"],
                json!({"type": "enabled", "budget_tokens": effort_budget(r)}),
                "{e:?}"
            );
            assert!(b.get("output_config").is_none());
        }
    }

    #[test]
    fn openrouter_and_openai_get_reasoning_effort() {
        let a = agent(None);
        let b = body(
            "openrouter/anthropic/claude-opus-5.5",
            &a,
            Some(Effort::High),
        );
        assert_eq!(b["reasoning"], json!({"effort": "high"}));
        let b = body("openrouter/anthropic/claude-opus-5.5", &a, None);
        assert!(b.get("reasoning").is_none());
        let b = body("openai/gpt-5", &a, Some(Effort::Low));
        assert_eq!(b["reasoning"]["effort"], "low");
        let b = body("openai/gpt-5", &a, None);
        assert!(b["reasoning"].get("effort").is_none());
    }

    #[test]
    fn models_that_do_not_reason_get_no_reasoning_fields() {
        let a = agent(None);
        let b = body("openai/gpt-4o", &a, Some(Effort::High));
        assert!(b.get("reasoning").is_none(), "{b}");
        // OpenRouter catalog models without `reasoning` are covered in
        // harness-providers (`reasoning_is_left_out_for_catalog_models...`).
    }
}
