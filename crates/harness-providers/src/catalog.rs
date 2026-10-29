//! The catalog of models the user can pick, for `/model` and anything else
//! that lists models.
//!
//! [`Catalog::build`] merges three sources:
//!
//! 1. the built-in [`Registry`] table (facts and list prices),
//! 2. model aliases (built-in and `[models]` in `harness.ini`), whose
//!    targets are always listed even when no other source knows them,
//! 3. live model lists from Anthropic, OpenAI and OpenRouter
//!    ([`live::ModelListSource`]), cached on disk ([`cache`]).
//!
//! Each [`CatalogEntry`] says which provider serves the model, how it
//! authenticates ([`AuthMethod`]) and whether it can be used now
//! ([`Availability`]): only models whose provider has working
//! [`Credentials`] are usable, and models without tool calling are flagged
//! as unusable for agents.
//!
//! [`CatalogService`] keeps a catalog current: it builds one from the cache
//! instantly (so it works offline), refreshes stale lists in the
//! background, and publishes every new catalog on a watch channel.
//!
//! ```no_run
//! # async fn demo(service: harness_providers::catalog::CatalogService) {
//! let catalog = service.catalog(); // instant, from the cache
//! for entry in catalog.entries().iter().filter(|e| e.usable_for_agents()) {
//!     println!("{} via {} ({})", entry.reference, entry.provider(), entry.auth);
//! }
//! service.spawn_refresh(false); // refetch stale lists in the background
//! # }
//! ```

pub mod cache;
pub mod live;
mod service;

use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Serialize};

pub use cache::{LiveList, ModelListCache};
pub use live::{LiveModel, ModelListSource};
pub use service::{CatalogConfig, CatalogService, Clock, RefreshReport};

use crate::model::Capabilities;
use crate::pricing::Price;
use crate::registry::{
    CONSERVATIVE_CAPABILITIES, ModelInfo, ModelRef, OPENROUTER, Registry, ThinkingStyle,
};

/// Providers in display order. Other providers sort after these, by name.
pub const PROVIDER_ORDER: &[&str] = &["anthropic", "openai", "openrouter"];

/// How harness authenticates to a model's provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthMethod {
    /// An API key from `harness.ini` (`api_key`, `api_key_cmd` or
    /// `api_key_env`); usage is billed per token.
    ApiKey,
    /// Sign in with ChatGPT (OpenAI only); usage counts against the user's
    /// ChatGPT plan instead of being billed per token.
    ChatGpt,
}

impl AuthMethod {
    /// Short label for tables: `API key` or `ChatGPT`.
    pub fn label(self) -> &'static str {
        match self {
            Self::ApiKey => "API key",
            Self::ChatGpt => "ChatGPT",
        }
    }
}

impl fmt::Display for AuthMethod {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

/// Whether a credential works.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "state", content = "message")]
pub enum CredentialState {
    /// Configured and not known to be rejected.
    Ready,
    /// Not configured.
    Missing,
    /// Configured but unusable: it could not be resolved (a failing
    /// `api_key_cmd`) or the provider rejected it (HTTP 401/403).
    Rejected(String),
}

/// One way of authenticating to a provider and whether it works.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthStatus {
    /// The method.
    pub method: AuthMethod,
    /// Its state.
    pub state: CredentialState,
}

/// Credential states per provider. A provider with no entry counts as
/// [`AuthMethod::ApiKey`] / [`CredentialState::Missing`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Credentials {
    by_provider: BTreeMap<String, Vec<AuthStatus>>,
}

impl Credentials {
    /// No credentials at all.
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds or replaces the state of `method` for `provider`.
    pub fn with(mut self, provider: &str, method: AuthMethod, state: CredentialState) -> Self {
        self.set(provider, method, state);
        self
    }

    /// Adds or replaces the state of `method` for `provider`.
    pub fn set(&mut self, provider: &str, method: AuthMethod, state: CredentialState) {
        let list = self.by_provider.entry(provider.to_owned()).or_default();
        match list.iter_mut().find(|s| s.method == method) {
            Some(s) => s.state = state,
            None => {
                list.push(AuthStatus { method, state });
                list.sort_by_key(|s| s.method);
            }
        }
    }

    /// The configured methods of `provider`, defaulting to a missing API key.
    pub fn statuses(&self, provider: &str) -> Vec<AuthStatus> {
        match self.by_provider.get(provider) {
            Some(v) if !v.is_empty() => v.clone(),
            _ => vec![AuthStatus {
                method: AuthMethod::ApiKey,
                state: CredentialState::Missing,
            }],
        }
    }

    /// True when `provider` has at least one working method.
    pub fn is_ready(&self, provider: &str) -> bool {
        self.statuses(provider)
            .iter()
            .any(|s| s.state == CredentialState::Ready)
    }

    /// Providers with an explicit entry.
    pub fn providers(&self) -> impl Iterator<Item = &str> {
        self.by_provider.keys().map(String::as_str)
    }
}

/// The capabilities the catalog shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ModelCapabilities {
    /// Tool (function) calling. Agents need it.
    pub tools: bool,
    /// Reasoning / extended thinking.
    pub reasoning: bool,
    /// Image input.
    pub vision: bool,
}

impl From<Capabilities> for ModelCapabilities {
    fn from(c: Capabilities) -> Self {
        Self {
            tools: c.tools,
            reasoning: c.thinking,
            vision: c.images,
        }
    }
}

/// Where an entry's price came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PriceOrigin {
    /// A `[pricing]` override.
    Override,
    /// The provider's live list (OpenRouter).
    Live,
    /// The built-in table.
    Builtin,
    /// Included in the ChatGPT plan; not billed per token.
    Plan,
    /// Unknown.
    Unknown,
}

/// Whether an entry can be used, and why not.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "reason")]
pub enum Availability {
    /// Credentials work and the model can call tools.
    Usable,
    /// Credentials work but the model cannot call tools: it can chat, but
    /// it cannot run an agent.
    NoTools,
    /// Not usable; the reason says how to fix it.
    Unavailable(String),
}

/// Which sources contributed to an entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct EntrySources {
    /// The built-in table.
    pub builtin: bool,
    /// An alias points at it.
    pub alias: bool,
    /// The provider's live list includes it.
    pub live: bool,
}

/// One model the user could pick.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CatalogEntry {
    /// `provider/model`.
    pub reference: ModelRef,
    /// Human-readable name.
    pub display_name: String,
    /// Aliases that resolve to this model, sorted.
    pub aliases: Vec<String>,
    /// Context window in tokens (0 if unknown).
    pub context_window: u32,
    /// Maximum output tokens (0 if unknown).
    pub max_output_tokens: u32,
    /// Price in USD per million tokens; `None` when unknown or included in
    /// a plan (see `price_origin`).
    pub price: Option<Price>,
    /// Where `price` came from.
    pub price_origin: PriceOrigin,
    /// Tools, reasoning and vision support.
    pub capabilities: ModelCapabilities,
    /// How the provider is reached for this entry.
    pub auth: AuthMethod,
    /// Whether it can be used now.
    pub availability: Availability,
    /// Which sources know the model.
    pub sources: EntrySources,
}

impl CatalogEntry {
    /// The provider id.
    pub fn provider(&self) -> &str {
        &self.reference.provider
    }

    /// The provider-local model id.
    pub fn model(&self) -> &str {
        &self.reference.model
    }

    /// Input price, USD per million tokens.
    pub fn input_price(&self) -> Option<f64> {
        self.price.map(|p| p.input)
    }

    /// Output price, USD per million tokens.
    pub fn output_price(&self) -> Option<f64> {
        self.price.map(|p| p.output)
    }

    /// True when credentials work (chat is possible).
    pub fn usable(&self) -> bool {
        !matches!(self.availability, Availability::Unavailable(_))
    }

    /// True when the model can run an agent: credentials work and it
    /// supports tools.
    pub fn usable_for_agents(&self) -> bool {
        self.availability == Availability::Usable
    }

    /// Why the entry is not fully usable, if it is not.
    pub fn reason(&self) -> Option<String> {
        match &self.availability {
            Availability::Usable => None,
            Availability::NoTools => Some(format!(
                "`{}` does not support tool calling, so it cannot run agents",
                self.reference
            )),
            Availability::Unavailable(r) => Some(r.clone()),
        }
    }
}

/// The state of one provider in a catalog.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderStatus {
    /// Provider id.
    pub provider: String,
    /// Configured auth methods and their states.
    pub auth: Vec<AuthStatus>,
    /// When the live list in use was fetched (Unix seconds), if any.
    pub fetched_at: Option<u64>,
    /// Models in the live list in use.
    pub live_models: usize,
    /// True when the provider has a live source whose cached list is
    /// missing, older than the TTL or from another base URL.
    pub stale: bool,
    /// The last refresh error, if the last refresh failed.
    pub error: Option<String>,
}

/// A merged, sorted list of models.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Catalog {
    entries: Vec<CatalogEntry>,
    providers: Vec<ProviderStatus>,
    refreshing: bool,
}

/// True for OpenAI models that Sign in with ChatGPT can use, by name: GPT-5
/// and later and the Codex family, with tool support. The account's own
/// model list ([`live::ListKind::ChatGpt`]) replaces this rule whenever it
/// has been fetched; the rule is the fallback before that.
pub fn chatgpt_plan_model(model: &str) -> bool {
    (crate::openai::is_gpt5_or_later(model) || model.starts_with("codex"))
        && live::openai_is_chat_model(model)
        && live::openai_supports_tools(model)
}

fn provider_rank(p: &str) -> (usize, &str) {
    (
        PROVIDER_ORDER
            .iter()
            .position(|x| *x == p)
            .unwrap_or(PROVIDER_ORDER.len()),
        p,
    )
}

#[derive(Default)]
struct Candidate {
    builtin: Option<ModelInfo>,
    live: Option<LiveModel>,
    aliased: bool,
}

impl Catalog {
    /// Merges `registry` (built-in models, aliases and price overrides) with
    /// the `live` lists (keyed by provider id) and marks usability from
    /// `credentials`. Provider statuses are left empty; [`CatalogService`]
    /// fills them.
    pub fn build(
        registry: &Registry,
        credentials: &Credentials,
        live: &BTreeMap<String, LiveList>,
    ) -> Self {
        let mut candidates: BTreeMap<ModelRef, Candidate> = BTreeMap::new();
        for info in registry.models() {
            candidates
                .entry(info.reference.clone())
                .or_default()
                .builtin = Some(info.clone());
        }
        for (provider, list) in live {
            for m in &list.models {
                let reference = ModelRef::new(provider, &m.id);
                let c = candidates.entry(reference.clone()).or_default();
                if c.builtin.is_none() {
                    // OpenRouter ids may reuse a vendor's built-in facts.
                    c.builtin = registry.lookup(&reference);
                }
                c.live = Some(m.clone());
            }
        }
        let mut aliases: BTreeMap<ModelRef, Vec<String>> = BTreeMap::new();
        for (alias, target) in registry.aliases() {
            aliases
                .entry(target.clone())
                .or_default()
                .push(alias.clone());
            let c = candidates.entry(target.clone()).or_default();
            c.aliased = true;
            if c.builtin.is_none() {
                c.builtin = registry.lookup(target);
            }
        }

        // The signed-in ChatGPT account's own list, once fetched, says
        // exactly which models the plan covers.
        let plan_list = live
            .get(crate::openai::PROVIDER_NAME)
            .is_some_and(|l| l.models.iter().any(|m| m.plan == Some(true)));
        let mut ranks: BTreeMap<ModelRef, u32> = BTreeMap::new();
        let mut entries = Vec::new();
        for (reference, c) in candidates {
            if let Some(rank) = c.live.as_ref().and_then(|l| l.rank) {
                ranks.insert(reference.clone(), rank);
            }
            let names = aliases.remove(&reference).unwrap_or_default();
            entries.extend(entries_for(
                registry,
                credentials,
                &reference,
                &c,
                names,
                plan_list,
            ));
        }
        // Within a provider, a server-ordered list keeps its order and comes
        // first; everything else is sorted by id.
        let rank = |e: &CatalogEntry| ranks.get(&e.reference).copied().unwrap_or(u32::MAX);
        entries.sort_by(|a, b| {
            provider_rank(a.provider())
                .cmp(&provider_rank(b.provider()))
                .then_with(|| rank(a).cmp(&rank(b)))
                .then_with(|| a.model().cmp(b.model()))
                .then_with(|| a.auth.cmp(&b.auth))
        });
        Self {
            entries,
            providers: Vec::new(),
            refreshing: false,
        }
    }

    /// Every entry, grouped by provider ([`PROVIDER_ORDER`] first) and
    /// sorted by model id; a list the server orders for display (the
    /// ChatGPT account's) keeps that order at the top of its provider.
    pub fn entries(&self) -> &[CatalogEntry] {
        &self.entries
    }

    /// Entries of one provider.
    pub fn by_provider<'a>(
        &'a self,
        provider: &str,
    ) -> impl Iterator<Item = &'a CatalogEntry> + use<'a> {
        let provider = provider.to_owned();
        self.entries
            .iter()
            .filter(move |e| e.provider() == provider)
    }

    /// Entries for `reference` (one per auth method).
    pub fn get<'a>(
        &'a self,
        reference: &ModelRef,
    ) -> impl Iterator<Item = &'a CatalogEntry> + use<'a> {
        let reference = reference.clone();
        self.entries
            .iter()
            .filter(move |e| e.reference == reference)
    }

    /// The entry an alias or `provider/model` name refers to (the usable
    /// one first).
    pub fn find(&self, name: &str) -> Option<&CatalogEntry> {
        let name = name.trim();
        let mut matching: Vec<&CatalogEntry> = self
            .entries
            .iter()
            .filter(|e| e.aliases.iter().any(|a| a == name) || e.reference.to_string() == name)
            .collect();
        matching.sort_by_key(|e| !e.usable());
        matching.first().copied()
    }

    /// Entries that can run agents.
    pub fn usable_for_agents(&self) -> impl Iterator<Item = &CatalogEntry> {
        self.entries.iter().filter(|e| e.usable_for_agents())
    }

    /// Per-provider credential and live-list state.
    pub fn providers(&self) -> &[ProviderStatus] {
        &self.providers
    }

    /// The status of one provider.
    pub fn provider_status(&self, provider: &str) -> Option<&ProviderStatus> {
        self.providers.iter().find(|p| p.provider == provider)
    }

    /// True while a background refresh is running; a picker shows a
    /// refresh indicator.
    pub fn refreshing(&self) -> bool {
        self.refreshing
    }

    pub(crate) fn with_status(mut self, providers: Vec<ProviderStatus>, refreshing: bool) -> Self {
        self.providers = providers;
        self.refreshing = refreshing;
        self
    }
}

fn unavailable_reason(provider: &str, status: &AuthStatus) -> Option<String> {
    match (&status.state, status.method) {
        (CredentialState::Ready, _) => None,
        (CredentialState::Missing, AuthMethod::ApiKey) => Some(format!(
            "no {provider} API key: set api_key, api_key_cmd or api_key_env in [provider.{provider}]"
        )),
        (CredentialState::Missing, AuthMethod::ChatGpt) => {
            Some("not signed in with ChatGPT: sign in to use plan models".into())
        }
        (CredentialState::Rejected(msg), m) => Some(format!(
            "the {provider} {} does not work: {msg}",
            match m {
                AuthMethod::ApiKey => "API key",
                AuthMethod::ChatGpt => "ChatGPT sign-in",
            }
        )),
    }
}

/// Runtime facts for a model only a live list knows: the list's limits and
/// capabilities, with [`fallback_capabilities`] for whatever it leaves out.
/// A model that reasons gets adaptive thinking on Anthropic and effort
/// levels elsewhere. Plan models (the ChatGPT account list) are priced at
/// zero: the plan has no per-token charge.
pub fn live_model_info(provider: &str, m: &LiveModel) -> ModelInfo {
    let reference = ModelRef::new(provider, &m.id);
    let fb = fallback_capabilities(&reference);
    let capabilities = Capabilities {
        tools: m.tools.unwrap_or(fb.tools),
        parallel_tool_calls: fb.parallel_tool_calls,
        thinking: m.reasoning.unwrap_or(fb.thinking),
        caching: fb.caching,
        images: m.vision.unwrap_or(fb.images),
        context_window: m.context_window.unwrap_or(fb.context_window),
        max_output_tokens: m.max_output_tokens.unwrap_or(fb.max_output_tokens),
    };
    let thinking = match (capabilities.thinking, provider) {
        (false, _) => ThinkingStyle::None,
        (true, crate::anthropic::NAME) => ThinkingStyle::Adaptive,
        (true, _) => ThinkingStyle::Effort,
    };
    let efforts = match &m.efforts {
        Some(e) if capabilities.thinking => e.clone(),
        _ => crate::registry::default_efforts(&reference, capabilities.thinking),
    };
    ModelInfo {
        display_name: m.display_name.clone().unwrap_or_else(|| m.id.clone()),
        capabilities,
        thinking,
        efforts,
        price: if m.plan == Some(true) {
            Some(Price::with_cache(0.0, 0.0, 0.0, 0.0))
        } else {
            m.price
        },
        reference,
    }
}

/// Best-known facts for a model that neither the built-in table nor a live
/// list fully describes.
fn fallback_capabilities(reference: &ModelRef) -> Capabilities {
    match reference.provider.as_str() {
        "anthropic" => Capabilities {
            tools: true,
            parallel_tool_calls: true,
            thinking: !reference.model.starts_with("claude-3-")
                || reference.model.starts_with("claude-3-7"),
            caching: true,
            images: true,
            context_window: 200_000,
            max_output_tokens: 8_192,
        },
        "openai" => {
            let mut c = crate::openai::model_capabilities(&reference.model);
            c.tools = live::openai_supports_tools(&reference.model);
            c
        }
        _ => CONSERVATIVE_CAPABILITIES,
    }
}

fn entries_for(
    registry: &Registry,
    credentials: &Credentials,
    reference: &ModelRef,
    c: &Candidate,
    aliases: Vec<String>,
    plan_list: bool,
) -> Vec<CatalogEntry> {
    let provider = reference.provider.as_str();
    let fallback = fallback_capabilities(reference);
    let builtin_caps = c.builtin.as_ref().map(|b| b.capabilities);
    let live = c.live.as_ref();
    // OpenRouter's list is authoritative for OpenRouter; elsewhere the
    // built-in table is more detailed than the live list.
    let prefer_live = provider == OPENROUTER;
    let pick = |live_v: Option<u32>, builtin_v: Option<u32>, fb: u32| -> u32 {
        let (first, second) = if prefer_live {
            (live_v, builtin_v)
        } else {
            (builtin_v, live_v)
        };
        first.or(second).unwrap_or(fb)
    };
    let pick_bool = |live_v: Option<bool>, builtin_v: Option<bool>, fb: bool| -> bool {
        let (first, second) = if prefer_live {
            (live_v, builtin_v)
        } else {
            (builtin_v, live_v)
        };
        first.or(second).unwrap_or(fb)
    };
    let context_window = pick(
        live.and_then(|l| l.context_window),
        builtin_caps.map(|b| b.context_window),
        fallback.context_window,
    );
    let max_output_tokens = pick(
        live.and_then(|l| l.max_output_tokens),
        builtin_caps.map(|b| b.max_output_tokens),
        fallback.max_output_tokens,
    );
    let capabilities = ModelCapabilities {
        tools: pick_bool(
            live.and_then(|l| l.tools),
            builtin_caps.map(|b| b.tools),
            fallback.tools,
        ),
        reasoning: pick_bool(
            live.and_then(|l| l.reasoning),
            builtin_caps.map(|b| b.thinking),
            fallback.thinking,
        ),
        vision: pick_bool(
            live.and_then(|l| l.vision),
            builtin_caps.map(|b| b.images),
            fallback.images,
        ),
    };
    let display_name = if prefer_live {
        live.and_then(|l| l.display_name.clone())
            .or_else(|| c.builtin.as_ref().map(|b| b.display_name.clone()))
    } else {
        c.builtin
            .as_ref()
            .map(|b| b.display_name.clone())
            .or_else(|| live.and_then(|l| l.display_name.clone()))
    }
    .unwrap_or_else(|| reference.model.clone());
    let (metered_price, metered_origin) = match (
        registry.price_override(reference, &aliases),
        live.and_then(|l| l.price),
        c.builtin.as_ref().and_then(|b| b.price),
    ) {
        (Some(p), _, _) => (Some(p), PriceOrigin::Override),
        (None, Some(p), _) if prefer_live => (Some(p), PriceOrigin::Live),
        (None, _, Some(p)) => (Some(p), PriceOrigin::Builtin),
        (None, Some(p), None) => (Some(p), PriceOrigin::Live),
        (None, None, None) => (None, PriceOrigin::Unknown),
    };
    let sources = EntrySources {
        // Built-in facts reached through the OpenRouter vendor fallback do
        // not make an OpenRouter model a built-in entry.
        builtin: registry.models().any(|m| &m.reference == reference),
        alias: c.aliased,
        live: live.is_some(),
    };

    let make = |auth: AuthMethod, availability: Availability| {
        let (price, price_origin) = if auth == AuthMethod::ChatGpt {
            (None, PriceOrigin::Plan)
        } else {
            (metered_price, metered_origin)
        };
        CatalogEntry {
            reference: reference.clone(),
            display_name: display_name.clone(),
            aliases: aliases.clone(),
            context_window,
            max_output_tokens,
            price,
            price_origin,
            capabilities,
            auth,
            availability,
            sources,
        }
    };
    let ready = |status: &AuthStatus| match unavailable_reason(provider, status) {
        None if capabilities.tools => Availability::Usable,
        None => Availability::NoTools,
        Some(r) => Availability::Unavailable(r),
    };

    let statuses = credentials.statuses(provider);
    let plan = provider == crate::openai::PROVIDER_NAME
        && if plan_list {
            live.is_some_and(|l| l.plan == Some(true))
        } else {
            chatgpt_plan_model(&reference.model)
        };
    let mut out = Vec::new();
    for status in &statuses {
        match status.method {
            AuthMethod::ApiKey => out.push(make(AuthMethod::ApiKey, ready(status))),
            AuthMethod::ChatGpt if plan => out.push(make(AuthMethod::ChatGpt, ready(status))),
            AuthMethod::ChatGpt => {}
        }
    }
    if out.is_empty() {
        // Only ChatGPT is configured and the plan does not cover this model.
        out.push(make(
            AuthMethod::ApiKey,
            Availability::Unavailable(format!(
                "`{reference}` is not included in Sign in with ChatGPT: add an OpenAI API key to use it"
            )),
        ));
    }
    out
}
