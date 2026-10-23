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
    let registry = Registry::builtin()
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
                Ini::OneHour => CacheTtl::OneHour,
            }
        }),
    };
    OpenRouterProvider::with_key(key, pcfg.base_url.as_deref(), options)
        .map_err(|e| format!("openrouter: {e}"))
}

/// The Anthropic provider settings from `[provider.anthropic]`: base URL and
/// request-body gzip (`gzip_requests`, `gzip_level`).
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
        harness_providers::openai::PROVIDER_NAME => {
            Arc::new(OpenAiProvider::with_key(key.expose(), base.as_deref()).map_err(err)?)
        }
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

/// The agent's effort as reasoning settings.
pub fn thinking(agent: &Agent) -> Option<ThinkingConfig> {
    agent.effort.map(|e| ThinkingConfig {
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
/// honouring `base_url`.
pub fn model_list_sources(cfg: &Config) -> Vec<ModelListSource> {
    harness_config::ini::config::PROVIDER_IDS
        .iter()
        .filter_map(|name| {
            let p = cfg.providers.get(*name)?;
            if *name == "openai" && p.auth_mode() == AuthMode::ChatGpt {
                return None;
            }
            let key = p.resolve_api_key().ok()??;
            ModelListSource::new(name, Some(key.expose()), p.base_url.as_deref())
        })
        .collect()
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
        let secrets = credentials(&c);
        assert!(secrets.contains(&"at-plan-secret".to_owned()));
        assert!(secrets.contains(&"rt-plan-secret".to_owned()));
        let r = redactor(&secrets);
        let out = r.redact("token at-plan-secret and rt-plan-secret");
        assert!(!out.contains("plan-secret"), "{out}");
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
}
