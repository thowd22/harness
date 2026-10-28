//! Model registry: `provider/model` references, aliases, built-in model
//! facts (context window, capabilities, prices) and price overrides.
//!
//! Users pick models explicitly. A model is named either by a reference such
//! as `anthropic/claude-opus-5-5` or by an alias from the `[models]` section
//! of `harness.ini` (`opus = anthropic/claude-opus-5-5`). [`Registry::resolve`]
//! turns either form into a [`Resolved`] model with its capabilities and the
//! price used for cost accounting.
//!
//! Models missing from the built-in table still work: they get
//! [`CONSERVATIVE_CAPABILITIES`] and a warning that cost is unknown, unless a
//! `[pricing]` override supplies a price.

use std::collections::BTreeMap;
use std::fmt;
use std::str::FromStr;
use std::sync::LazyLock;

use serde::{Deserialize, Serialize};

use crate::model::{Capabilities, Usage};
use crate::pricing::Price;

/// The OpenRouter provider id. Its model ids are themselves `vendor/model`.
pub const OPENROUTER: &str = "openrouter";

/// Capabilities assumed for a model that is not in the built-in table:
/// tools only, a 32k context window and 4k output tokens.
pub const CONSERVATIVE_CAPABILITIES: Capabilities = Capabilities {
    tools: true,
    parallel_tool_calls: false,
    thinking: false,
    caching: false,
    images: false,
    context_window: 32_000,
    max_output_tokens: 4_096,
};

/// Aliases available without any `[models]` section. User aliases with the
/// same name replace them.
pub const BUILTIN_ALIASES: &[(&str, &str)] = &[
    ("fable", "anthropic/claude-fable-5-1"),
    ("opus", "anthropic/claude-opus-5-5"),
    ("sonnet", "anthropic/claude-sonnet-5-5"),
    ("haiku", "anthropic/claude-haiku-5-5"),
];

/// A `provider/model` reference.
///
/// The provider is everything before the first `/`; the model id is the
/// rest, so `openrouter/anthropic/claude-opus-5-5` has provider `openrouter`
/// and model `anthropic/claude-opus-5-5`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ModelRef {
    /// Provider id, e.g. `anthropic`.
    pub provider: String,
    /// Provider-local model id, e.g. `claude-opus-5-5`.
    pub model: String,
}

impl ModelRef {
    /// A reference from its parts (not validated).
    pub fn new(provider: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            provider: provider.into(),
            model: model.into(),
        }
    }

    /// Parses `provider/model`.
    pub fn parse(s: &str) -> Result<Self, RegistryError> {
        let invalid = |reason: &str| RegistryError::InvalidRef {
            value: s.to_owned(),
            reason: reason.to_owned(),
        };
        if s.chars().any(char::is_whitespace) {
            return Err(invalid("must not contain whitespace"));
        }
        let (provider, model) = s
            .split_once('/')
            .ok_or_else(|| invalid("expected provider/model"))?;
        if provider.is_empty() {
            return Err(invalid("provider is empty"));
        }
        if model.is_empty() || model.ends_with('/') {
            return Err(invalid("model id is empty"));
        }
        Ok(Self::new(provider, model))
    }
}

impl fmt::Display for ModelRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.provider, self.model)
    }
}

impl FromStr for ModelRef {
    type Err = RegistryError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse(s)
    }
}

impl TryFrom<String> for ModelRef {
    type Error = RegistryError;
    fn try_from(s: String) -> Result<Self, Self::Error> {
        Self::parse(&s)
    }
}

impl From<ModelRef> for String {
    fn from(r: ModelRef) -> Self {
        r.to_string()
    }
}

/// How a model is asked to reason.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThinkingStyle {
    /// The model does not reason, or it is unknown.
    #[default]
    None,
    /// Anthropic extended thinking with a token budget
    /// (`thinking: {type: enabled, budget_tokens}`).
    Budget,
    /// Anthropic adaptive thinking steered by an effort level
    /// (`thinking: {type: adaptive}` plus `output_config.effort`).
    Adaptive,
    /// OpenAI-style `reasoning_effort` levels.
    Effort,
}

/// Facts about one model.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelInfo {
    /// The model's reference.
    pub reference: ModelRef,
    /// Human-readable name.
    pub display_name: String,
    /// What the model supports.
    pub capabilities: Capabilities,
    /// How reasoning is requested.
    pub thinking: ThinkingStyle,
    /// Built-in list price, if known.
    pub price: Option<Price>,
}

/// Where the price of a [`Resolved`] model came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PriceSource {
    /// A `[pricing]` override.
    Override,
    /// The built-in table.
    Builtin,
    /// No price is known; cost is reported as unknown.
    Unknown,
}

/// A model name resolved to a reference, facts and a price.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Resolved {
    /// The alias used, if the name was an alias.
    pub alias: Option<String>,
    /// The resolved reference.
    pub reference: ModelRef,
    /// Model facts (built-in, or conservative defaults when unknown).
    pub info: ModelInfo,
    /// False when the model is not in the built-in table.
    pub known: bool,
    /// The price used for cost accounting.
    pub price: Option<Price>,
    /// Where `price` came from.
    pub price_source: PriceSource,
    /// User-visible warnings, e.g. that cost is unknown.
    pub warnings: Vec<String>,
}

impl Resolved {
    /// Capabilities of the resolved model.
    pub fn capabilities(&self) -> Capabilities {
        self.info.capabilities
    }

    /// Cost of one call in USD, or `None` when no price is known.
    pub fn cost(&self, usage: &Usage) -> Option<f64> {
        crate::pricing::cost(self.price.as_ref(), usage)
    }

    /// True when calls to this model can be priced.
    pub fn cost_known(&self) -> bool {
        self.price.is_some()
    }
}

/// Registry errors.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RegistryError {
    /// A model reference is malformed.
    #[error("invalid model reference `{value}`: {reason}")]
    InvalidRef {
        /// The offending text.
        value: String,
        /// What is wrong.
        reason: String,
    },
    /// An alias name is malformed.
    #[error("invalid model alias `{0}`: alias names must be non-empty and must not contain `/`")]
    InvalidAlias(String),
    /// A bare name is neither an alias nor a unique built-in model id.
    #[error(
        "unknown model `{name}`: use an alias from [models] or a provider/model reference{hint}"
    )]
    UnknownModel {
        /// The name that was looked up.
        name: String,
        /// Suggestion, possibly empty.
        hint: String,
    },
}

/// The model registry: built-in models plus user aliases and price
/// overrides.
#[derive(Debug, Clone)]
pub struct Registry {
    models: BTreeMap<ModelRef, ModelInfo>,
    /// Models only a provider's live model list knows
    /// ([`Registry::with_live_models`]).
    live: BTreeMap<ModelRef, ModelInfo>,
    aliases: BTreeMap<String, ModelRef>,
    overrides: BTreeMap<String, Price>,
}

static SHARED: LazyLock<Registry> = LazyLock::new(Registry::builtin);

impl Default for Registry {
    fn default() -> Self {
        Self::builtin()
    }
}

impl Registry {
    /// The built-in table and [`BUILTIN_ALIASES`], with no overrides.
    pub fn builtin() -> Self {
        let models = builtin_models()
            .into_iter()
            .map(|m| (m.reference.clone(), m))
            .collect();
        let aliases = BUILTIN_ALIASES
            .iter()
            .map(|(a, r)| ((*a).to_owned(), ModelRef::parse(r).expect("valid builtin")))
            .collect();
        Self {
            models,
            live: BTreeMap::new(),
            aliases,
            overrides: BTreeMap::new(),
        }
    }

    /// A process-wide built-in registry, for providers that only need
    /// capabilities.
    pub fn shared() -> &'static Registry {
        &SHARED
    }

    /// An empty registry with no models, aliases or prices.
    pub fn empty() -> Self {
        Self {
            models: BTreeMap::new(),
            live: BTreeMap::new(),
            aliases: BTreeMap::new(),
            overrides: BTreeMap::new(),
        }
    }

    /// Adds or replaces aliases (`[models]`: alias -> `provider/model`).
    pub fn with_aliases<K, V>(
        mut self,
        aliases: impl IntoIterator<Item = (K, V)>,
    ) -> Result<Self, RegistryError>
    where
        K: AsRef<str>,
        V: AsRef<str>,
    {
        for (name, target) in aliases {
            let name = name.as_ref();
            if name.is_empty() || name.contains('/') {
                return Err(RegistryError::InvalidAlias(name.to_owned()));
            }
            self.aliases
                .insert(name.to_owned(), ModelRef::parse(target.as_ref())?);
        }
        Ok(self)
    }

    /// Adds or replaces price overrides (`[pricing]`). Keys may be
    /// `provider/model` references or alias names. An override replaces the
    /// whole built-in price; cache rates it leaves out use the default
    /// factors in [`crate::pricing`].
    pub fn with_price_overrides<K: AsRef<str>>(
        mut self,
        prices: impl IntoIterator<Item = (K, Price)>,
    ) -> Self {
        for (key, price) in prices {
            self.overrides.insert(key.as_ref().to_owned(), price);
        }
        self
    }

    /// Adds or replaces a model fact entry.
    pub fn with_model(mut self, info: ModelInfo) -> Self {
        self.models.insert(info.reference.clone(), info);
        self
    }

    /// Alias table, alias -> reference.
    pub fn aliases(&self) -> &BTreeMap<String, ModelRef> {
        &self.aliases
    }

    /// All known models, ordered by reference.
    pub fn models(&self) -> impl Iterator<Item = &ModelInfo> {
        self.models.values()
    }

    /// The `[pricing]` override for `reference`, keyed by the reference or
    /// by one of `aliases` (the reference wins). Unlike
    /// [`Registry::resolve`] this logs nothing, so it suits bulk listings.
    pub fn price_override(&self, reference: &ModelRef, aliases: &[String]) -> Option<Price> {
        self.overrides
            .get(&reference.to_string())
            .or_else(|| aliases.iter().find_map(|a| self.overrides.get(a)))
            .copied()
    }

    /// Adds models that only a provider's live model list knows (see
    /// [`crate::catalog::live_model_info`]), so a run on a model released
    /// after this build gets its real limits instead of
    /// [`CONSERVATIVE_CAPABILITIES`]. Built-in entries, and OpenRouter
    /// models the vendor fallback covers, keep their built-in facts.
    #[must_use]
    pub fn with_live_models(mut self, models: impl IntoIterator<Item = ModelInfo>) -> Self {
        for info in models {
            if self.lookup(&info.reference).is_none() {
                self.live.insert(info.reference.clone(), info);
            }
        }
        self
    }

    /// Facts for a reference: the built-in table first, then, for
    /// OpenRouter references, the vendor's own entry
    /// (`openrouter/anthropic/claude-sonnet-5.5` uses
    /// `anthropic/claude-sonnet-5-5`), then the live-list models.
    pub fn lookup(&self, reference: &ModelRef) -> Option<ModelInfo> {
        if let Some(info) = self.models.get(reference) {
            return Some(info.clone());
        }
        if let Some(info) = self.vendor_fallback(reference) {
            return Some(info);
        }
        self.live.get(reference).cloned()
    }

    fn vendor_fallback(&self, reference: &ModelRef) -> Option<ModelInfo> {
        if reference.provider != OPENROUTER {
            return None;
        }
        let inner = ModelRef::parse(&reference.model).ok()?;
        let vendor = self.models.get(&inner).or_else(|| {
            let dashed = ModelRef::new(&inner.provider, inner.model.replace('.', "-"));
            self.models.get(&dashed)
        })?;
        let mut info = vendor.clone();
        info.reference = reference.clone();
        Some(info)
    }

    /// Capabilities of `provider`'s `model`, or
    /// [`CONSERVATIVE_CAPABILITIES`] when unknown.
    pub fn capabilities(&self, provider: &str, model: &str) -> Capabilities {
        self.lookup(&ModelRef::new(provider, model))
            .map_or(CONSERVATIVE_CAPABILITIES, |i| i.capabilities)
    }

    /// Resolves an alias, a `provider/model` reference or a bare built-in
    /// model id (when exactly one direct provider has it).
    pub fn resolve(&self, name: &str) -> Result<Resolved, RegistryError> {
        let name = name.trim();
        let (alias, reference) = if let Some(r) = self.aliases.get(name) {
            (Some(name.to_owned()), r.clone())
        } else if name.contains('/') {
            (None, ModelRef::parse(name)?)
        } else {
            (None, self.bare(name)?)
        };
        Ok(self.describe(alias, reference))
    }

    fn bare(&self, name: &str) -> Result<ModelRef, RegistryError> {
        let matches: Vec<&ModelRef> = self
            .models
            .keys()
            .chain(self.live.keys())
            .filter(|r| r.provider != OPENROUTER && r.model == name)
            .collect();
        match matches.as_slice() {
            [one] => Ok((*one).clone()),
            _ => {
                let hint = if matches.is_empty() {
                    let aliases: Vec<&str> = self.aliases.keys().map(String::as_str).collect();
                    if aliases.is_empty() {
                        String::new()
                    } else {
                        format!(" (aliases: {})", aliases.join(", "))
                    }
                } else {
                    let refs: Vec<String> = matches.iter().map(ToString::to_string).collect();
                    format!(" (ambiguous: {})", refs.join(", "))
                };
                Err(RegistryError::UnknownModel {
                    name: name.to_owned(),
                    hint,
                })
            }
        }
    }

    fn describe(&self, alias: Option<String>, reference: ModelRef) -> Resolved {
        let found = self.lookup(&reference);
        let known = found.is_some();
        let info = found.unwrap_or_else(|| ModelInfo {
            reference: reference.clone(),
            display_name: reference.to_string(),
            capabilities: CONSERVATIVE_CAPABILITIES,
            thinking: ThinkingStyle::None,
            price: None,
        });
        let key = reference.to_string();
        let override_price = self.price_override(&reference, alias.as_slice());
        let (price, price_source) = match (override_price, info.price) {
            (Some(p), _) => (Some(p), PriceSource::Override),
            (None, Some(p)) => (Some(p), PriceSource::Builtin),
            (None, None) => (None, PriceSource::Unknown),
        };
        let mut warnings = Vec::new();
        if !known {
            warnings.push(format!(
                "model `{key}` is not in the built-in table: using conservative defaults \
                 ({}k context, {} output tokens, no images, no thinking)",
                CONSERVATIVE_CAPABILITIES.context_window / 1000,
                CONSERVATIVE_CAPABILITIES.max_output_tokens
            ));
        }
        if price.is_none() {
            warnings.push(format!(
                "cost of `{key}` is unknown: add `{key} = input,output` to [pricing] (USD per million tokens)"
            ));
        }
        for w in &warnings {
            tracing::warn!(target: "harness::models", "{w}");
        }
        Resolved {
            alias,
            reference,
            info,
            known,
            price,
            price_source,
            warnings,
        }
    }
}

const fn caps(
    context_window: u32,
    max_output_tokens: u32,
    thinking: bool,
    caching: bool,
    images: bool,
) -> Capabilities {
    Capabilities {
        tools: true,
        parallel_tool_calls: true,
        thinking,
        caching,
        images,
        context_window,
        max_output_tokens,
    }
}

struct Entry {
    provider: &'static str,
    id: &'static str,
    name: &'static str,
    caps: Capabilities,
    thinking: ThinkingStyle,
    price: Price,
}

const M: u32 = 1_000_000;

/// The built-in table, a snapshot of list prices (USD per million tokens)
/// as of 2026-10. `[pricing]` overrides any of them.
fn builtin_models() -> Vec<ModelInfo> {
    use ThinkingStyle::{Adaptive, Budget, Effort};
    let claude = |id, name, input: f64, output: f64, read: f64| Entry {
        provider: "anthropic",
        id,
        name,
        caps: caps(M, 128_000, true, true, true),
        thinking: Adaptive,
        price: Price::with_cache(input, output, read, input * 1.25),
    };
    let openai = |id, name, ctx, out, reasoning: bool, input: f64, output: f64, read: f64| Entry {
        provider: "openai",
        id,
        name,
        caps: caps(ctx, out, reasoning, true, true),
        thinking: if reasoning {
            Effort
        } else {
            ThinkingStyle::None
        },
        // OpenAI caches automatically and does not charge for cache writes.
        price: Price::with_cache(input, output, read, input),
    };
    let entries = [
        claude("claude-fable-5-1", "Claude Fable 5.1", 10.0, 50.0, 0.25),
        claude("claude-fable-5", "Claude Fable 5", 10.0, 50.0, 1.0),
        claude("claude-opus-5-5", "Claude Opus 5.5", 4.0, 20.0, 0.20),
        claude("claude-opus-5", "Claude Opus 5", 5.0, 25.0, 0.50),
        claude("claude-opus-4-8", "Claude Opus 4.8", 5.0, 25.0, 0.50),
        claude("claude-opus-4-7", "Claude Opus 4.7", 5.0, 25.0, 0.50),
        claude("claude-opus-4-6", "Claude Opus 4.6", 5.0, 25.0, 0.50),
        claude("claude-sonnet-5-5", "Claude Sonnet 5.5", 2.0, 10.0, 0.20),
        claude("claude-sonnet-5", "Claude Sonnet 5", 2.0, 10.0, 0.20),
        claude("claude-sonnet-4-6", "Claude Sonnet 4.6", 3.0, 15.0, 0.30),
        claude("claude-haiku-5-5", "Claude Haiku 5.5", 0.10, 0.50, 0.01),
        Entry {
            provider: "anthropic",
            id: "claude-haiku-4-5",
            name: "Claude Haiku 4.5",
            caps: caps(200_000, 64_000, true, true, true),
            thinking: Budget,
            price: Price::with_cache(1.0, 5.0, 0.10, 1.25),
        },
        openai(
            "gpt-5.1", "GPT-5.1", 400_000, 128_000, true, 1.25, 10.0, 0.125,
        ),
        openai("gpt-5", "GPT-5", 400_000, 128_000, true, 1.25, 10.0, 0.125),
        openai(
            "gpt-5-mini",
            "GPT-5 mini",
            400_000,
            128_000,
            true,
            0.25,
            2.0,
            0.025,
        ),
        openai(
            "gpt-5-nano",
            "GPT-5 nano",
            400_000,
            128_000,
            true,
            0.05,
            0.40,
            0.005,
        ),
        openai(
            "gpt-4.1", "GPT-4.1", 1_047_576, 32_768, false, 2.0, 8.0, 0.50,
        ),
        openai(
            "gpt-4.1-mini",
            "GPT-4.1 mini",
            1_047_576,
            32_768,
            false,
            0.40,
            1.60,
            0.10,
        ),
        openai("gpt-4o", "GPT-4o", 128_000, 16_384, false, 2.50, 10.0, 1.25),
        openai(
            "gpt-4o-mini",
            "GPT-4o mini",
            128_000,
            16_384,
            false,
            0.15,
            0.60,
            0.075,
        ),
        openai("o3", "o3", 200_000, 100_000, true, 2.0, 8.0, 0.50),
        openai(
            "o4-mini", "o4-mini", 200_000, 100_000, true, 1.10, 4.40, 0.275,
        ),
        Entry {
            provider: OPENROUTER,
            id: "google/gemini-2.5-pro",
            name: "Gemini 2.5 Pro (OpenRouter)",
            caps: caps(1_048_576, 65_536, true, true, true),
            thinking: Effort,
            price: Price::with_cache(1.25, 10.0, 0.31, 1.25),
        },
        Entry {
            provider: OPENROUTER,
            id: "google/gemini-2.5-flash",
            name: "Gemini 2.5 Flash (OpenRouter)",
            caps: caps(1_048_576, 65_536, true, true, true),
            thinking: Effort,
            price: Price::with_cache(0.30, 2.50, 0.075, 0.30),
        },
    ];
    entries
        .into_iter()
        .map(|e| ModelInfo {
            reference: ModelRef::new(e.provider, e.id),
            display_name: e.name.to_owned(),
            capabilities: e.caps,
            thinking: e.thinking,
            price: Some(e.price),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refs_parse_provider_and_nested_model() {
        let r = ModelRef::parse("anthropic/claude-opus-5-5").unwrap();
        assert_eq!(
            (r.provider.as_str(), r.model.as_str()),
            ("anthropic", "claude-opus-5-5")
        );
        let r = ModelRef::parse("openrouter/google/gemini-2.5-pro").unwrap();
        assert_eq!(r.provider, "openrouter");
        assert_eq!(r.model, "google/gemini-2.5-pro");
        assert_eq!(r.to_string(), "openrouter/google/gemini-2.5-pro");
        for bad in ["opus", "/x", "anthropic/", "a b/c", "a/b/"] {
            assert!(ModelRef::parse(bad).is_err(), "{bad}");
        }
        let json = serde_json::to_string(&r).unwrap();
        assert_eq!(json, "\"openrouter/google/gemini-2.5-pro\"");
        assert_eq!(serde_json::from_str::<ModelRef>(&json).unwrap(), r);
    }

    #[test]
    fn builtin_table_covers_claude_openai_and_openrouter() {
        let reg = Registry::builtin();
        for name in [
            "anthropic/claude-opus-5-5",
            "anthropic/claude-sonnet-5-5",
            "anthropic/claude-haiku-5-5",
            "anthropic/claude-fable-5-1",
            "openai/gpt-5",
            "openai/gpt-4.1",
            "openai/o3",
            "openrouter/google/gemini-2.5-pro",
            "openrouter/anthropic/claude-sonnet-5.5",
            "openrouter/openai/gpt-5",
        ] {
            let r = reg.resolve(name).unwrap();
            assert!(r.known, "{name}");
            assert!(r.cost_known(), "{name}");
            assert!(r.warnings.is_empty(), "{name}: {:?}", r.warnings);
            assert!(r.capabilities().context_window >= 128_000, "{name}");
            assert!(r.capabilities().max_output_tokens > 0, "{name}");
        }
        let opus = reg.resolve("anthropic/claude-opus-5-5").unwrap();
        assert_eq!(opus.info.thinking, ThinkingStyle::Adaptive);
        assert_eq!(opus.price.unwrap(), Price::with_cache(4.0, 20.0, 0.2, 5.0));
        assert_eq!(opus.capabilities().context_window, 1_000_000);
        let haiku45 = reg.resolve("anthropic/claude-haiku-4-5").unwrap();
        assert_eq!(haiku45.info.thinking, ThinkingStyle::Budget);
        assert_eq!(haiku45.capabilities().context_window, 200_000);
        let via_or = reg
            .resolve("openrouter/anthropic/claude-sonnet-5.5")
            .unwrap();
        assert_eq!(via_or.reference.provider, "openrouter");
        assert_eq!(via_or.price, reg.resolve("sonnet").unwrap().price);
    }

    #[test]
    fn aliases_resolve_to_refs_and_user_aliases_win() {
        let reg = Registry::builtin()
            .with_aliases([
                ("sonnet", "openrouter/anthropic/claude-sonnet-5.5"),
                ("fast", "openai/gpt-5-mini"),
            ])
            .unwrap();
        let r = reg.resolve("fast").unwrap();
        assert_eq!(r.alias.as_deref(), Some("fast"));
        assert_eq!(r.reference, ModelRef::new("openai", "gpt-5-mini"));
        let s = reg.resolve("sonnet").unwrap();
        assert_eq!(s.reference.provider, "openrouter");
        assert_eq!(
            reg.resolve("opus").unwrap().reference.to_string(),
            "anthropic/claude-opus-5-5"
        );
        assert!(matches!(
            Registry::builtin().with_aliases([("x", "nope")]),
            Err(RegistryError::InvalidRef { .. })
        ));
        assert!(matches!(
            Registry::builtin().with_aliases([("a/b", "openai/gpt-5")]),
            Err(RegistryError::InvalidAlias(_))
        ));
    }

    #[test]
    fn bare_ids_resolve_when_unique_and_unknown_bare_names_fail() {
        let reg = Registry::builtin();
        assert_eq!(
            reg.resolve("gpt-5").unwrap().reference,
            ModelRef::new("openai", "gpt-5")
        );
        let err = reg.resolve("mystery").unwrap_err();
        assert!(err.to_string().contains("aliases: "), "{err}");
    }

    #[test]
    fn pricing_overrides_replace_builtin_prices() {
        let reg = Registry::builtin()
            .with_aliases([("cheap", "openai/gpt-5-nano")])
            .unwrap()
            .with_price_overrides([
                (
                    "anthropic/claude-sonnet-5-5",
                    Price::with_cache(3.0, 15.0, 0.3, 3.75),
                ),
                ("cheap", Price::new(1.0, 2.0)),
            ]);
        let s = reg.resolve("sonnet").unwrap();
        assert_eq!(s.price_source, PriceSource::Override);
        assert_eq!(s.price.unwrap(), Price::with_cache(3.0, 15.0, 0.3, 3.75));
        let usage = Usage {
            input_tokens: 1_000_000,
            output_tokens: 1_000_000,
            ..Usage::default()
        };
        assert!((s.cost(&usage).unwrap() - 18.0).abs() < 1e-9);
        let c = reg.resolve("cheap").unwrap();
        assert_eq!(c.price_source, PriceSource::Override);
        assert_eq!(c.price.unwrap(), Price::new(1.0, 2.0));
        let o = reg.resolve("opus").unwrap();
        assert_eq!(o.price_source, PriceSource::Builtin);
    }

    #[test]
    fn unknown_models_get_conservative_defaults_and_cost_warning() {
        let reg = Registry::builtin();
        let r = reg.resolve("openai/gpt-99-experimental").unwrap();
        assert!(!r.known);
        assert_eq!(r.capabilities(), CONSERVATIVE_CAPABILITIES);
        assert_eq!(r.info.thinking, ThinkingStyle::None);
        assert_eq!(r.price_source, PriceSource::Unknown);
        assert_eq!(r.cost(&Usage::default()), None);
        assert_eq!(r.warnings.len(), 2);
        assert!(
            r.warnings
                .iter()
                .any(|w| w.contains("cost of `openai/gpt-99-experimental` is unknown"))
        );
        assert_eq!(
            reg.capabilities("openai", "gpt-99-experimental"),
            CONSERVATIVE_CAPABILITIES
        );

        let priced = Registry::builtin()
            .with_price_overrides([("openai/gpt-99-experimental", Price::new(1.0, 1.0))])
            .resolve("openai/gpt-99-experimental")
            .unwrap();
        assert!(priced.cost_known());
        assert_eq!(priced.warnings.len(), 1, "{:?}", priced.warnings);
    }

    #[test]
    fn shared_registry_answers_capability_queries() {
        let c = Registry::shared().capabilities("anthropic", "claude-opus-5-5");
        assert!(c.thinking && c.caching && c.images && c.parallel_tool_calls);
        assert!(Registry::empty().models().next().is_none());
    }
}
