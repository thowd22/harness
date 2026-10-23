//! The typed `harness.ini` schema and its built-in defaults.
//!
//! [`Config::from_document`] is the single source of truth for which sections
//! and keys exist: each section builder reads its known keys through a
//! [`SectionReader`] and every key it did not read becomes an unknown-key
//! error. See `backlog/docs/doc-2 - harness.ini-Reference.md`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Serialize;

use super::document::{Document, Section};
use super::error::{IniError, Location, closest};
use super::value::{SectionReader, config_enum, parse_list};
use crate::secrets::Secret;

/// Provider ids accepted in `[provider.<id>]`.
pub const PROVIDER_IDS: &[&str] = &["anthropic", "openai", "openrouter"];

/// Action names accepted in `[keys]`.
pub const KEY_ACTIONS: &[&str] = &[
    "palette",
    "send",
    "newline",
    "cancel",
    "interrupt",
    "switch_view",
    "quit",
    "copy",
    "select",
];

/// Fixed (non-dotted) section names.
pub const FIXED_SECTIONS: &[&str] = &[
    "general", "models", "pricing", "keys", "ui", "theme", "policy", "sandbox", "verify",
    "context", "budget", "memory",
];

config_enum!(
    /// Autonomy mode (`[general] mode`).
    Mode { /// Ask before every edit or command.
        Ask = "ask", /// Edit files freely, ask for commands.
        AutoEdit = "auto-edit", /// Act without asking (policy still applies).
        Auto = "auto" }
);
config_enum!(
    /// UI theme (`[ui] theme`).
    UiTheme { /// Dracula palette.
        Dracula = "dracula", /// No colours.
        None = "none" }
);
config_enum!(
    /// Background painting (`[ui] background`).
    Background { /// Paint the theme background.
        Theme = "theme", /// Leave the terminal background.
        Terminal = "terminal" }
);
config_enum!(
    /// Timestamp style (`[ui] timestamps`).
    Timestamps { /// Wall-clock time.
        Time = "time", /// Relative ("2m ago").
        Relative = "relative", /// Hidden.
        Off = "off" }
);
config_enum!(
    /// Reasoning display (`[ui] show_thinking`).
    ShowThinking { /// Never shown.
        Hidden = "hidden", /// Collapsed by default.
        Collapsed = "collapsed", /// Expanded by default.
        Expanded = "expanded" }
);
config_enum!(
    /// How copy actions reach the clipboard (`[ui] clipboard`).
    ClipboardMode { /// The OSC 52 escape sequence (works over SSH and tmux passthrough).
        Osc52 = "osc52", /// A system tool: wl-copy, xclip, xsel or pbcopy.
        System = "system", /// A system tool when one is usable locally, else OSC 52.
        Auto = "auto" }
);
config_enum!(
    /// Goal bar visibility (`[ui] goal_bar`).
    GoalBar { /// Shown only when tasks exist.
        Auto = "auto", /// Never shown.
        Off = "off" }
);
config_enum!(
    /// Sandbox network policy (`[sandbox] network`).
    NetworkMode { /// Network allowed.
        Allow = "allow", /// Network denied.
        Deny = "deny" }
);
config_enum!(
    /// Budget overrun behaviour (`[budget] on_exceed`).
    OnExceed { /// Ask the user.
        Ask = "ask", /// Stop the run.
        Stop = "stop" }
);
config_enum!(
    /// Memory consolidation schedule (`[memory] dream`).
    Dream { /// Never.
        Off = "off", /// Once a day.
        Daily = "daily", /// Only on request.
        Manual = "manual" }
);
config_enum!(
    /// Request-body gzip (`[provider.anthropic] gzip_requests`).
    GzipRequests { /// Compress large bodies when talking to the public API directly.
        Auto = "auto", /// Never compress.
        Off = "off" }
);
config_enum!(
    /// How the OpenAI provider authenticates (`[provider.openai] auth`).
    AuthMode { /// `api_key`, `api_key_cmd` or `api_key_env` (the default).
        ApiKey = "api_key", /// Sign in with ChatGPT (`harness login openai`); usage counts
        /// against the ChatGPT plan.
        ChatGpt = "chatgpt" }
);
config_enum!(
    /// Remote workspace sync method (`[remote.<name>] sync`).
    SyncMethod { /// rsync over SSH.
        Rsync = "rsync", /// git push/fetch.
        Git = "git" }
);
config_enum!(
    /// SSH channel compression for one host (`[remote.<name>] compression`).
    SshCompression { /// Compress only when the measured link is slower than
        /// `compression_below_mbps`.
        Auto = "auto", /// Always compress (`Compression=yes`).
        Yes = "yes", /// Never compress (`Compression=no`).
        No = "no" }
);

/// Default `[remote.<name>] compression_below_mbps`: with
/// `compression = auto`, links measured slower than this many Mbit/s get
/// SSH compression.
pub const DEFAULT_COMPRESSION_BELOW_MBPS: f64 = 20.0;

/// Serializes a [`Duration`] as fractional seconds.
fn ser_duration<S: serde::Serializer>(d: &Duration, s: S) -> Result<S::Ok, S::Error> {
    s.serialize_f64(d.as_secs_f64())
}

/// `[general]`.
#[derive(Debug, Clone, Serialize)]
pub struct General {
    /// Model alias or `provider/model` ref. Default `sonnet`.
    pub default_model: String,
    /// Autonomy mode. Default `auto-edit`.
    pub mode: Mode,
    /// Root of all harness state. Default `$XDG_DATA_HOME/harness`
    /// (`~/.local/share/harness`).
    pub data_dir: PathBuf,
    /// Maximum subagent nesting. Default 2.
    pub max_subagent_depth: u32,
    /// Maximum agents running at once. Default 4.
    pub max_concurrent_agents: u32,
    /// How long cached live model lists (for `/model`) stay fresh before
    /// they are refetched in the background. Default 24h.
    #[serde(serialize_with = "ser_duration")]
    pub model_list_ttl: Duration,
    /// Send the built-in identity prompt layer, which names harness and
    /// points the model to its docs (`identity_prompt`). Default true.
    pub identity_prompt: bool,
    /// Model alias or `provider/model` ref that names and describes
    /// sessions after each turn (`session_summary_model`). `None` (the
    /// default, also written `off`) derives them locally with no model
    /// call.
    pub session_summary_model: Option<String>,
}

/// `[provider.<id>]`. Use [`ProviderConfig::resolve_api_key`] to obtain the key.
#[derive(Debug, Clone, Default, Serialize)]
pub struct ProviderConfig {
    /// Inline key (`api_key`). Serialized as `[redacted]`.
    pub api_key: Option<Secret>,
    /// Shell command printing the key on stdout (`api_key_cmd`).
    pub api_key_cmd: Option<String>,
    /// Name of an environment variable holding the key (`api_key_env`).
    pub api_key_env: Option<String>,
    /// API base URL override.
    pub base_url: Option<String>,
    /// OpenRouter only: `X-Title` app name.
    pub app_name: Option<String>,
    /// OpenRouter only: upstream provider preference order.
    pub provider_order: Vec<String>,
    /// OpenRouter only: allow routing fallbacks.
    pub allow_fallbacks: Option<bool>,
    /// Anthropic only: request-body gzip (`gzip_requests`, default `auto`).
    pub gzip_requests: Option<GzipRequests>,
    /// Anthropic only: gzip level 1 to 9 (`gzip_level`, default 3).
    pub gzip_level: Option<u32>,
    /// OpenAI only: `api_key` (default) or `chatgpt` (Sign in with
    /// ChatGPT).
    pub auth: Option<AuthMode>,
    /// OpenAI only: allow `auth = chatgpt` in headless and eval runs
    /// (`chatgpt_headless`, default false). The ChatGPT credential is
    /// personal; CI is refused even when this is set.
    pub chatgpt_headless: bool,
}

impl ProviderConfig {
    /// The auth method in effect: `auth`, else [`AuthMode::ApiKey`].
    pub fn auth_mode(&self) -> AuthMode {
        self.auth.unwrap_or(AuthMode::ApiKey)
    }

    /// Resolves the API key: `api_key`, then `api_key_cmd`, then
    /// `api_key_env`; the first non-empty value wins. Returns `Ok(None)` when
    /// no source yields a key.
    pub fn resolve_api_key(&self) -> Result<Option<Secret>, crate::secrets::SecretError> {
        self.resolve_api_key_with_origin()
            .map(|r| r.map(|(secret, _)| secret))
    }

    /// Like [`resolve_api_key`](Self::resolve_api_key), also saying which
    /// key (`api_key`, `api_key_cmd` or `api_key_env`) supplied the value.
    pub fn resolve_api_key_with_origin(
        &self,
    ) -> Result<Option<(Secret, crate::secrets::SecretOrigin)>, crate::secrets::SecretError> {
        crate::secrets::resolve(
            &crate::secrets::SecretSources {
                inline: self.api_key.as_ref(),
                cmd: self.api_key_cmd.as_deref(),
                env: self.api_key_env.as_deref(),
            },
            &crate::secrets::ResolveOptions::default(),
        )
    }

    /// Describes the credential that is in effect, without the secret:
    /// `API key (api_key_env ANTHROPIC_API_KEY)`, `API key (api_key_cmd)` or
    /// `API key (api_key in harness.ini)`.
    pub fn auth_description(&self, origin: crate::secrets::SecretOrigin) -> String {
        use crate::secrets::SecretOrigin;
        match origin {
            SecretOrigin::Inline => "API key (api_key in harness.ini)".into(),
            SecretOrigin::Command => "API key (api_key_cmd)".into(),
            SecretOrigin::Env => format!(
                "API key (api_key_env {})",
                self.api_key_env.as_deref().unwrap_or("?")
            ),
        }
    }
}

/// One `[pricing]` entry, USD per million tokens.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct Pricing {
    /// Input tokens.
    pub input: f64,
    /// Output tokens.
    pub output: f64,
    /// Cache reads, if priced separately.
    pub cache_read: Option<f64>,
    /// Cache writes, if priced separately.
    pub cache_write: Option<f64>,
}

/// `[ui]`.
#[derive(Debug, Clone, Serialize)]
pub struct Ui {
    /// Theme. Default `dracula`.
    pub theme: UiTheme,
    /// Background painting. Default `theme`.
    pub background: Background,
    /// Animated spinners. Default true.
    pub animations: bool,
    /// Silence before an indicator turns orange. Default 20s.
    #[serde(serialize_with = "ser_duration")]
    pub stall_after: Duration,
    /// Timestamp style. Default `time`.
    pub timestamps: Timestamps,
    /// Lines of tool output previewed in cards. Default 8.
    pub tool_preview_lines: u32,
    /// Spinner and status in the terminal title. Default true.
    pub terminal_title: bool,
    /// Reasoning display. Default `collapsed`.
    pub show_thinking: ShowThinking,
    /// Render Markdown. Default true.
    pub markdown: bool,
    /// Mouse capture. Default true.
    pub mouse: bool,
    /// Clipboard used by copy actions. Default `osc52`.
    pub clipboard: ClipboardMode,
    /// Goal bar visibility. Default `auto`.
    pub goal_bar: GoalBar,
    /// Goal bar width in columns. Default 36.
    pub goal_bar_width: u16,
    /// Live one-line previews on map nodes. Default true.
    pub live_preview: bool,
}

/// `[policy]`: glob patterns. Empty by default; the policy engine adds its
/// own built-in rules.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Policy {
    /// `bash.allow`.
    pub bash_allow: Vec<String>,
    /// `bash.ask`.
    pub bash_ask: Vec<String>,
    /// `bash.deny`.
    pub bash_deny: Vec<String>,
    /// `paths.protected` (globs, `~` not expanded).
    pub paths_protected: Vec<String>,
    /// `outward.ask`.
    pub outward_ask: Vec<String>,
}

/// `[sandbox]`.
#[derive(Debug, Clone, Serialize)]
pub struct Sandbox {
    /// Enable the filesystem sandbox. Default true.
    pub enabled: bool,
    /// Network policy. Default `allow`.
    pub network: NetworkMode,
}

/// `[verify]`.
#[derive(Debug, Clone, Serialize)]
pub struct Verify {
    /// Verification command; `None` means auto-detect.
    pub command: Option<String>,
    /// Record a baseline before changes. Default true.
    pub baseline: bool,
    /// Fix attempts before giving up. Default 3.
    pub max_fix_attempts: u32,
}

/// `[context]`.
#[derive(Debug, Clone, Serialize)]
pub struct Context {
    /// Compact when this fraction of the window is used. Default 0.8.
    pub compact_at: f64,
    /// Turns kept verbatim on compaction. Default 6.
    pub keep_recent_turns: u32,
    /// Elide tool results larger than this many tokens. Default 4000.
    pub elide_tool_results_over: u64,
}

/// `[budget]`: spend caps in USD; `None` means no cap.
#[derive(Debug, Clone, Serialize)]
pub struct Budget {
    /// Per session.
    pub session_usd: Option<f64>,
    /// Per run.
    pub run_usd: Option<f64>,
    /// Per day.
    pub daily_usd: Option<f64>,
    /// What to do on overrun. Default `ask`.
    pub on_exceed: OnExceed,
}

/// `[memory]`.
#[derive(Debug, Clone, Serialize)]
pub struct Memory {
    /// Memory enabled. Default true.
    pub enabled: bool,
    /// Drive location; `None` means `<data_dir>/memory`.
    pub drive: Option<PathBuf>,
    /// Consolidation schedule. Default `daily`.
    pub dream: Dream,
    /// Model for consolidation. Default `haiku`.
    pub dream_model: String,
    /// Token cap for the memory index. Default 1500.
    pub index_max_tokens: u32,
    /// Headless runs may use memory. Default false.
    pub headless_access: bool,
}

/// `[remote.<name>]`.
#[derive(Debug, Clone, Serialize)]
pub struct Remote {
    /// Host name or address (required).
    pub host: String,
    /// SSH user.
    pub user: Option<String>,
    /// SSH port. Default 22.
    pub port: u16,
    /// Private key file (local, `~` expanded).
    pub identity_file: Option<PathBuf>,
    /// Labels matched by `runs_on`.
    pub labels: Vec<String>,
    /// Remote working directory (remote path, not expanded locally).
    pub workdir: Option<String>,
    /// Concurrent jobs on this host. Default 1.
    pub max_jobs: u32,
    /// Sync method. Default `rsync`.
    pub sync: SyncMethod,
    /// Extra `policy.bash.deny` patterns for this host (tighten only).
    pub policy_bash_deny: Vec<String>,
    /// Extra `policy.bash.ask` patterns for this host (tighten only).
    pub policy_bash_ask: Vec<String>,
    /// SSH channel compression. Default `auto`.
    pub compression: SshCompression,
    /// With `compression = auto`: compress when the measured link
    /// throughput is below this many Mbit/s. Default
    /// [`DEFAULT_COMPRESSION_BELOW_MBPS`].
    pub compression_below_mbps: f64,
}

/// The whole effective `harness.ini`: built-in defaults overlaid by the file.
///
/// `Debug` and `Serialize` never reveal secrets.
#[derive(Debug, Clone, Serialize)]
pub struct Config {
    /// File the config was loaded from; `None` when only defaults apply.
    pub source: Option<PathBuf>,
    /// `[general]`.
    pub general: General,
    /// `[provider.<id>]`, keyed by id.
    pub providers: BTreeMap<String, ProviderConfig>,
    /// `[models]`: alias → `provider/model`.
    pub models: BTreeMap<String, String>,
    /// `[pricing]`: model ref → prices.
    pub pricing: BTreeMap<String, Pricing>,
    /// `[keys]`: action → key chords as written (parsed by the TUI).
    pub keys: BTreeMap<String, Vec<String>>,
    /// `[ui]`.
    pub ui: Ui,
    /// `[theme]`: token → `#rrggbb`.
    pub theme: BTreeMap<String, String>,
    /// `[policy]`.
    pub policy: Policy,
    /// `[sandbox]`.
    pub sandbox: Sandbox,
    /// `[verify]`.
    pub verify: Verify,
    /// `[context]`.
    pub context: Context,
    /// `[budget]`.
    pub budget: Budget,
    /// `[memory]`.
    pub memory: Memory,
    /// `[remote.<name>]`, keyed by name.
    pub remotes: BTreeMap<String, Remote>,
    /// Where each set value came from, keyed `section.key` (for
    /// `harness config show`).
    #[serde(skip)]
    pub origins: BTreeMap<String, Location>,
}

impl Config {
    /// Built-in defaults, with `data_dir` derived from `home` /
    /// `XDG_DATA_HOME` (see [`crate::paths::default_data_dir`]).
    pub fn defaults() -> Self {
        Self::defaults_with_data_dir(crate::paths::default_data_dir())
    }

    /// Built-in defaults with an explicit default `data_dir`.
    pub fn defaults_with_data_dir(data_dir: PathBuf) -> Self {
        Config {
            source: None,
            general: General {
                default_model: "sonnet".into(),
                mode: Mode::AutoEdit,
                data_dir,
                max_subagent_depth: 2,
                max_concurrent_agents: 4,
                model_list_ttl: Duration::from_secs(24 * 3600),
                identity_prompt: true,
                session_summary_model: None,
            },
            providers: BTreeMap::new(),
            models: BTreeMap::new(),
            pricing: BTreeMap::new(),
            keys: BTreeMap::new(),
            ui: Ui {
                theme: UiTheme::Dracula,
                background: Background::Theme,
                animations: true,
                stall_after: Duration::from_secs(20),
                timestamps: Timestamps::Time,
                tool_preview_lines: 8,
                terminal_title: true,
                show_thinking: ShowThinking::Collapsed,
                markdown: true,
                mouse: true,
                clipboard: ClipboardMode::Osc52,
                goal_bar: GoalBar::Auto,
                goal_bar_width: 36,
                live_preview: true,
            },
            theme: BTreeMap::new(),
            policy: Policy::default(),
            sandbox: Sandbox {
                enabled: true,
                network: NetworkMode::Allow,
            },
            verify: Verify {
                command: None,
                baseline: true,
                max_fix_attempts: 3,
            },
            context: Context {
                compact_at: 0.8,
                keep_recent_turns: 6,
                elide_tool_results_over: 4000,
            },
            budget: Budget {
                session_usd: None,
                run_usd: None,
                daily_usd: None,
                on_exceed: OnExceed::Ask,
            },
            memory: Memory {
                enabled: true,
                drive: None,
                dream: Dream::Daily,
                dream_model: "haiku".into(),
                index_max_tokens: 1500,
                headless_access: false,
            },
            remotes: BTreeMap::new(),
            origins: BTreeMap::new(),
        }
    }

    /// Overlays a parsed document onto `defaults`, validating every section
    /// and key. All problems are returned together, sorted by line.
    pub fn from_document(
        doc: &Document,
        defaults: Config,
        home: Option<&Path>,
    ) -> Result<Config, Vec<IniError>> {
        let mut cfg = defaults;
        cfg.source = doc.file.as_deref().map(Path::to_path_buf);
        let mut errors = Vec::new();
        for section in &doc.sections {
            for e in &section.entries {
                cfg.origins
                    .insert(format!("{}.{}", section.name, e.key), e.location.clone());
            }
            let mut r = SectionReader::new(section, home, &mut errors);
            match section.name.as_str() {
                "general" => read_general(&mut r, &mut cfg.general),
                "models" => read_models(&mut r, &mut cfg.models),
                "pricing" => read_pricing(&mut r, &mut cfg.pricing),
                "keys" => read_keys(&mut r, &mut cfg.keys),
                "ui" => read_ui(&mut r, &mut cfg.ui),
                "theme" => read_theme(&mut r, &mut cfg.theme),
                "policy" => read_policy(&mut r, &mut cfg.policy),
                "sandbox" => read_sandbox(&mut r, &mut cfg.sandbox),
                "verify" => read_verify(&mut r, &mut cfg.verify),
                "context" => read_context(&mut r, &mut cfg.context),
                "budget" => read_budget(&mut r, &mut cfg.budget),
                "memory" => read_memory(&mut r, &mut cfg.memory),
                name => {
                    if let Some(id) = name
                        .strip_prefix("provider.")
                        .filter(|id| PROVIDER_IDS.contains(id))
                    {
                        let p = read_provider(&mut r, id);
                        cfg.providers.insert(id.to_owned(), p);
                    } else if let Some(remote) =
                        name.strip_prefix("remote.").filter(|n| !n.is_empty())
                    {
                        if let Some(rm) = read_remote(&mut r) {
                            cfg.remotes.insert(remote.to_owned(), rm);
                        }
                    } else {
                        drop(r);
                        errors.push(unknown_section(section));
                        continue;
                    }
                }
            }
            r.finish();
        }
        if errors.is_empty() {
            Ok(cfg)
        } else {
            errors.sort_by_key(|e| e.location().map_or(0, |l| l.line));
            Err(errors)
        }
    }

    /// Names of environment variables configured as `api_key_env`. Callers
    /// that spawn subprocesses (tools, sandbox) must remove these from the
    /// child environment.
    pub fn secret_env_vars(&self) -> Vec<String> {
        let mut v: Vec<String> = self
            .providers
            .values()
            .filter_map(|p| p.api_key_env.clone())
            .collect();
        v.sort();
        v.dedup();
        v
    }

    /// True when any provider has an inline `api_key`.
    pub fn has_inline_secrets(&self) -> bool {
        self.providers.values().any(|p| p.api_key.is_some())
    }
}

fn unknown_section(section: &Section) -> IniError {
    let name = section.name.as_str();
    let hint = if let Some(id) = name.strip_prefix("provider.") {
        closest(id, PROVIDER_IDS.iter().copied()).map(|h| format!("provider.{h}"))
    } else {
        closest(
            name,
            FIXED_SECTIONS
                .iter()
                .copied()
                .chain(["provider.<id>", "remote.<name>"]),
        )
    };
    IniError::UnknownSection {
        location: section.location.clone(),
        name: name.to_owned(),
        hint,
    }
}

macro_rules! set {
    ($target:expr, $value:expr) => {
        if let Some(v) = $value {
            $target = v;
        }
    };
}

fn read_general(r: &mut SectionReader<'_>, g: &mut General) {
    set!(g.default_model, r.nonempty_string("default_model"));
    set!(g.mode, r.enumerated("mode"));
    set!(g.data_dir, r.path("data_dir"));
    set!(g.max_subagent_depth, r.uint("max_subagent_depth"));
    set!(
        g.max_concurrent_agents,
        positive(r, "max_concurrent_agents")
    );
    set!(g.model_list_ttl, r.duration("model_list_ttl"));
    set!(g.identity_prompt, on_off(r, "identity_prompt"));
    if let Some(v) = r.string("session_summary_model") {
        let v = v.trim();
        g.session_summary_model = (!v.is_empty() && v != "off").then(|| v.to_owned());
    }
}

/// Reads a switch written `on` / `off` (or `true` / `false`).
fn on_off(r: &mut SectionReader<'_>, key: &'static str) -> Option<bool> {
    let v = r.string(key)?;
    match v.as_str() {
        "on" | "true" => Some(true),
        "off" | "false" => Some(false),
        _ => {
            let entry = r.section().get(key).expect("key was just read");
            r.invalid(entry, "expected `on` or `off`");
            None
        }
    }
}

/// Reads a `u32` that must be at least 1.
fn positive(r: &mut SectionReader<'_>, key: &'static str) -> Option<u32> {
    let v = r.uint::<u32>(key)?;
    if v == 0 {
        let entry = r.section().get(key).expect("key was just read");
        r.invalid(entry, "must be at least 1");
        return None;
    }
    Some(v)
}

/// Reads an `f64` that must be greater than 0.
fn positive_float(r: &mut SectionReader<'_>, key: &'static str) -> Option<f64> {
    let v = r.float(key)?;
    if v <= 0.0 {
        let entry = r.section().get(key).expect("key was just read");
        r.invalid(entry, "must be greater than 0");
        return None;
    }
    Some(v)
}

fn read_provider(r: &mut SectionReader<'_>, id: &str) -> ProviderConfig {
    let mut p = ProviderConfig {
        api_key: r.secret("api_key"),
        api_key_cmd: r.string("api_key_cmd").filter(|s| !s.is_empty()),
        api_key_env: r.string("api_key_env").filter(|s| !s.is_empty()),
        base_url: r.nonempty_string("base_url"),
        ..ProviderConfig::default()
    };
    if id == "anthropic" {
        p.gzip_requests = r.enumerated("gzip_requests");
        p.gzip_level = r.uint::<u32>("gzip_level");
        if p.gzip_level.is_some_and(|l| !(1..=9).contains(&l)) {
            let entry = r.section().get("gzip_level").expect("key was just read");
            r.invalid(entry, "must be between 1 and 9");
            p.gzip_level = None;
        }
    }
    if id == "openai" {
        p.auth = r.enumerated("auth");
        p.chatgpt_headless = r.bool("chatgpt_headless").unwrap_or(false);
    }
    if id == "openrouter" {
        p.app_name = r.nonempty_string("app_name");
        p.provider_order = r.list("provider_order").unwrap_or_default();
        p.allow_fallbacks = r.bool("allow_fallbacks");
    }
    p
}

fn read_models(r: &mut SectionReader<'_>, models: &mut BTreeMap<String, String>) {
    for e in r.section().entries.iter() {
        if e.value.contains('/') && !e.value.starts_with('/') && !e.value.ends_with('/') {
            models.insert(e.key.clone(), e.value.clone());
        } else {
            r.invalid(e, format!("expected `provider/model`, got `{}`", e.value));
        }
    }
    r.mark_all_used();
}

fn read_pricing(r: &mut SectionReader<'_>, pricing: &mut BTreeMap<String, Pricing>) {
    for e in r.section().entries.iter() {
        let nums: Result<Vec<f64>, _> = parse_list(&e.value)
            .iter()
            .map(|s| s.parse::<f64>())
            .collect();
        match nums {
            Ok(n) if (2..=4).contains(&n.len()) && n.iter().all(|x| x.is_finite() && *x >= 0.0) => {
                pricing.insert(
                    e.key.clone(),
                    Pricing {
                        input: n[0],
                        output: n[1],
                        cache_read: n.get(2).copied(),
                        cache_write: n.get(3).copied(),
                    },
                );
            }
            _ => r.invalid(
                e,
                "expected `input,output[,cache_read[,cache_write]]` in USD per million tokens",
            ),
        }
    }
    r.mark_all_used();
}

fn read_keys(r: &mut SectionReader<'_>, keys: &mut BTreeMap<String, Vec<String>>) {
    for action in KEY_ACTIONS {
        if let Some(chords) = r.list(action) {
            if chords.is_empty() {
                let entry = r.section().get(action).expect("key was just read");
                r.invalid(entry, "expected at least one key chord, e.g. `ctrl+p`");
            } else {
                keys.insert((*action).to_owned(), chords);
            }
        }
    }
}

fn read_ui(r: &mut SectionReader<'_>, ui: &mut Ui) {
    set!(ui.theme, r.enumerated("theme"));
    set!(ui.background, r.enumerated("background"));
    set!(ui.animations, r.bool("animations"));
    set!(ui.stall_after, r.duration("stall_after"));
    set!(ui.timestamps, r.enumerated("timestamps"));
    set!(ui.tool_preview_lines, r.uint("tool_preview_lines"));
    set!(ui.terminal_title, r.bool("terminal_title"));
    set!(ui.show_thinking, r.enumerated("show_thinking"));
    set!(ui.markdown, r.bool("markdown"));
    set!(ui.mouse, r.bool("mouse"));
    set!(ui.clipboard, r.enumerated("clipboard"));
    set!(ui.goal_bar, r.enumerated("goal_bar"));
    set!(ui.goal_bar_width, r.uint("goal_bar_width"));
    set!(ui.live_preview, r.bool("live_preview"));
}

fn read_theme(r: &mut SectionReader<'_>, theme: &mut BTreeMap<String, String>) {
    for e in r.section().entries.iter() {
        let v = &e.value;
        if v.len() == 7 && v.starts_with('#') && v[1..].chars().all(|c| c.is_ascii_hexdigit()) {
            theme.insert(e.key.clone(), v.to_ascii_lowercase());
        } else {
            r.invalid(
                e,
                format!("expected a hex colour like `#bd93f9`, got `{v}`"),
            );
        }
    }
    r.mark_all_used();
}

fn read_policy(r: &mut SectionReader<'_>, p: &mut Policy) {
    set!(p.bash_allow, r.list("bash.allow"));
    set!(p.bash_ask, r.list("bash.ask"));
    set!(p.bash_deny, r.list("bash.deny"));
    set!(p.paths_protected, r.list("paths.protected"));
    set!(p.outward_ask, r.list("outward.ask"));
}

fn read_sandbox(r: &mut SectionReader<'_>, s: &mut Sandbox) {
    set!(s.enabled, r.bool("enabled"));
    set!(s.network, r.enumerated("network"));
}

fn read_verify(r: &mut SectionReader<'_>, v: &mut Verify) {
    if let Some(cmd) = r.string("command") {
        v.command = (!cmd.is_empty()).then_some(cmd);
    }
    set!(v.baseline, r.bool("baseline"));
    set!(v.max_fix_attempts, r.uint("max_fix_attempts"));
}

fn read_context(r: &mut SectionReader<'_>, c: &mut Context) {
    if let Some(f) = r.float("compact_at") {
        if f > 0.0 && f <= 1.0 {
            c.compact_at = f;
        } else {
            let entry = r.section().get("compact_at").expect("key was just read");
            r.invalid(entry, "expected a fraction greater than 0 and at most 1");
        }
    }
    set!(c.keep_recent_turns, r.uint("keep_recent_turns"));
    set!(c.elide_tool_results_over, r.uint("elide_tool_results_over"));
}

fn read_budget(r: &mut SectionReader<'_>, b: &mut Budget) {
    for (key, slot) in [
        ("session_usd", &mut b.session_usd),
        ("run_usd", &mut b.run_usd),
        ("daily_usd", &mut b.daily_usd),
    ] {
        if let Some(v) = r.float(key) {
            if v >= 0.0 {
                *slot = Some(v);
            } else {
                let entry = r.section().get(key).expect("key was just read");
                r.invalid(entry, "must not be negative");
            }
        }
    }
    set!(b.on_exceed, r.enumerated("on_exceed"));
}

fn read_memory(r: &mut SectionReader<'_>, m: &mut Memory) {
    set!(m.enabled, r.bool("enabled"));
    if let Some(p) = r.path("drive") {
        m.drive = Some(p);
    }
    set!(m.dream, r.enumerated("dream"));
    set!(m.dream_model, r.nonempty_string("dream_model"));
    set!(m.index_max_tokens, r.uint("index_max_tokens"));
    set!(m.headless_access, r.bool("headless_access"));
}

fn read_remote(r: &mut SectionReader<'_>) -> Option<Remote> {
    let host = r.nonempty_string("host");
    let user = r.nonempty_string("user");
    let port = r.uint("port").unwrap_or(22);
    let identity_file = r.path("identity_file");
    let labels = r.list("labels").unwrap_or_default();
    let workdir = r.nonempty_string("workdir");
    let max_jobs = positive(r, "max_jobs").unwrap_or(1);
    let sync = r.enumerated("sync").unwrap_or(SyncMethod::Rsync);
    let policy_bash_deny = r.list("policy.bash.deny").unwrap_or_default();
    let policy_bash_ask = r.list("policy.bash.ask").unwrap_or_default();
    let compression = r.enumerated("compression").unwrap_or(SshCompression::Auto);
    let compression_below_mbps =
        positive_float(r, "compression_below_mbps").unwrap_or(DEFAULT_COMPRESSION_BELOW_MBPS);
    let Some(host) = host else {
        if r.section().get("host").is_none() {
            r.missing("host");
        }
        return None;
    };
    Some(Remote {
        host,
        user,
        port,
        identity_file,
        labels,
        workdir,
        max_jobs,
        sync,
        policy_bash_deny,
        policy_bash_ask,
        compression,
        compression_below_mbps,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ini::document::parse;

    fn load(text: &str) -> Result<Config, Vec<IniError>> {
        let doc = parse(text, Some(Path::new("/cfg/harness.ini")))?;
        Config::from_document(
            &doc,
            Config::defaults_with_data_dir("/data".into()),
            Some(Path::new("/home/me")),
        )
    }

    #[test]
    fn empty_file_gives_defaults() {
        let c = load("").unwrap();
        assert_eq!(c.general.default_model, "sonnet");
        assert_eq!(c.general.mode, Mode::AutoEdit);
        assert_eq!(c.general.data_dir, PathBuf::from("/data"));
        assert_eq!(c.ui.stall_after, Duration::from_secs(20));
        assert_eq!(c.ui.goal_bar_width, 36);
        assert!(c.sandbox.enabled);
        assert_eq!(c.context.compact_at, 0.8);
        assert_eq!(c.memory.dream_model, "haiku");
        assert!(c.providers.is_empty());
        assert_eq!(c.general.session_summary_model, None);
    }

    #[test]
    fn session_summary_model_is_off_unless_set() {
        let c = load("[general]\nsession_summary_model = haiku\n").unwrap();
        assert_eq!(c.general.session_summary_model.as_deref(), Some("haiku"));
        for off in ["off", ""] {
            let c = load(&format!("[general]\nsession_summary_model = {off}\n")).unwrap();
            assert_eq!(c.general.session_summary_model, None, "{off:?}");
        }
    }

    #[test]
    fn full_reference_example_parses() {
        let text = include_str!("../../tests/fixtures/reference.ini");
        let c = load(text).unwrap_or_else(|e| panic!("{e:#?}"));
        assert_eq!(c.general.mode, Mode::AutoEdit);
        assert_eq!(
            c.general.data_dir,
            PathBuf::from("/home/me/.local/share/harness")
        );
        let or = &c.providers["openrouter"];
        assert_eq!(or.provider_order, vec!["anthropic", "openai"]);
        assert_eq!(or.allow_fallbacks, Some(true));
        assert_eq!(
            c.providers["openai"].api_key_env.as_deref(),
            Some("OPENAI_API_KEY")
        );
        assert_eq!(c.models["opus"], "anthropic/claude-opus-5-5");
        let p = c.pricing["anthropic/claude-sonnet-5-5"];
        assert_eq!(
            (p.input, p.output, p.cache_read, p.cache_write),
            (3.0, 15.0, Some(0.3), Some(3.75))
        );
        assert_eq!(c.keys["newline"], vec!["shift+enter", "alt+enter"]);
        assert_eq!(c.policy.bash_allow.len(), 6);
        assert_eq!(
            c.policy.paths_protected,
            vec!["~/.ssh/**", "**/.env*", "~/.aws/**"]
        );
        assert_eq!(c.budget.daily_usd, Some(30.0));
        assert_eq!(
            c.memory.drive,
            Some(PathBuf::from("/home/me/.local/share/harness/memory"))
        );
        let gpu = &c.remotes["gpu-box"];
        assert_eq!(gpu.host, "10.0.0.12");
        assert_eq!(gpu.labels, vec!["gpu", "bigmem"]);
        assert_eq!(
            gpu.identity_file,
            Some(PathBuf::from("/home/me/.ssh/id_ed25519"))
        );
        assert_eq!(gpu.workdir.as_deref(), Some("~/harness-work"));
        assert_eq!(gpu.policy_bash_deny, vec!["sudo*"]);
        assert_eq!(gpu.compression, SshCompression::Auto);
        assert_eq!(gpu.compression_below_mbps, 20.0);
        assert!(
            c.theme.is_empty(),
            "theme overrides are commented out in the example"
        );
        assert_eq!(c.origins["general.mode"].line, 3);
    }

    #[test]
    fn remote_compression_settings() {
        let r = |extra: &str| load(&format!("[remote.x]\nhost = h\n{extra}"));
        let d = r("").unwrap();
        assert_eq!(d.remotes["x"].compression, SshCompression::Auto);
        assert_eq!(
            d.remotes["x"].compression_below_mbps,
            DEFAULT_COMPRESSION_BELOW_MBPS
        );
        for (v, want) in [
            ("auto", SshCompression::Auto),
            ("yes", SshCompression::Yes),
            ("no", SshCompression::No),
        ] {
            let c = r(&format!("compression = {v}\n")).unwrap();
            assert_eq!(c.remotes["x"].compression, want);
        }
        let c = r("compression_below_mbps = 2.5\n").unwrap();
        assert_eq!(c.remotes["x"].compression_below_mbps, 2.5);
        let e = r("compression = maybe\n").unwrap_err();
        assert!(
            e.iter()
                .any(|m| m.to_string().contains("expected one of auto | yes | no")),
            "{e:?}"
        );
        for bad in ["0", "-1", "fast"] {
            assert!(
                r(&format!("compression_below_mbps = {bad}\n")).is_err(),
                "{bad}"
            );
        }
    }

    #[test]
    fn identity_prompt_defaults_on_and_can_be_turned_off() {
        assert!(load("").unwrap().general.identity_prompt);
        assert!(
            !load("[general]\nidentity_prompt = off\n")
                .unwrap()
                .general
                .identity_prompt
        );
        assert!(
            load("[general]\nidentity_prompt = on\n")
                .unwrap()
                .general
                .identity_prompt
        );
        assert!(
            !load("[general]\nidentity_prompt = false\n")
                .unwrap()
                .general
                .identity_prompt
        );
        let e = format!(
            "{:?}",
            load("[general]\nidentity_prompt = maybe\n").unwrap_err()
        );
        assert!(e.contains("expected `on` or `off`"), "{e}");
    }

    #[test]
    fn model_list_ttl_defaults_to_a_day_and_parses_durations() {
        assert_eq!(
            load("").unwrap().general.model_list_ttl,
            Duration::from_secs(86_400)
        );
        let c = load("[general]\nmodel_list_ttl = 6h\n").unwrap();
        assert_eq!(c.general.model_list_ttl, Duration::from_secs(6 * 3600));
        assert!(load("[general]\nmodel_list_ttl = soon\n").is_err());
    }

    #[test]
    fn overrides_replace_defaults() {
        let c = load("[ui]\nanimations = false\nstall_after = 5s\n[general]\nmax_concurrent_agents = 9\n[theme]\naccent = #BD93F9\n").unwrap();
        assert_eq!(c.theme["accent"], "#bd93f9");
        assert!(!c.ui.animations);
        assert_eq!(c.ui.stall_after, Duration::from_secs(5));
        assert_eq!(c.general.max_concurrent_agents, 9);
        assert!(c.ui.mouse, "untouched keys keep defaults");
    }

    #[test]
    fn clipboard_mode_defaults_to_osc52_and_parses() {
        assert_eq!(load("").unwrap().ui.clipboard, ClipboardMode::Osc52);
        for (v, want) in [
            ("osc52", ClipboardMode::Osc52),
            ("system", ClipboardMode::System),
            ("auto", ClipboardMode::Auto),
        ] {
            let c = load(&format!("[ui]\nclipboard = {v}\n")).unwrap();
            assert_eq!(c.ui.clipboard, want, "{v}");
        }
        let errs = load("[ui]\nclipboard = xclip\n").unwrap_err();
        let msg = errs[0].to_string();
        assert!(msg.contains("clipboard"), "{msg}");
        let keys = load("[keys]\ncopy = ctrl+y\nselect = ctrl+t\n").unwrap();
        assert_eq!(keys.keys.len(), 2);
    }

    #[test]
    fn anthropic_gzip_keys() {
        let c = load("[provider.anthropic]\ngzip_requests = off\ngzip_level = 6\n").unwrap();
        let a = &c.providers["anthropic"];
        assert_eq!(a.gzip_requests, Some(GzipRequests::Off));
        assert_eq!(a.gzip_level, Some(6));
        let c = load("[provider.anthropic]\ngzip_requests = auto\n").unwrap();
        assert_eq!(
            c.providers["anthropic"].gzip_requests,
            Some(GzipRequests::Auto)
        );
        assert_eq!(c.providers["anthropic"].gzip_level, None);
        let msgs: Vec<String> =
            load("[provider.anthropic]\ngzip_requests = zstd\ngzip_level = 0\n")
                .unwrap_err()
                .iter()
                .map(ToString::to_string)
                .collect();
        assert_eq!(msgs.len(), 2, "{msgs:#?}");
        assert!(
            msgs[0].contains("expected one of auto | off, got `zstd`"),
            "{}",
            msgs[0]
        );
        assert!(msgs[1].contains("must be between 1 and 9"), "{}", msgs[1]);
        // Only the Anthropic provider takes them.
        let errs = load("[provider.openai]\ngzip_requests = auto\n").unwrap_err();
        assert!(errs[0].to_string().contains("unknown key `gzip_requests`"));
    }

    #[test]
    fn openai_auth_selects_api_key_or_chatgpt() {
        let c = load("[provider.openai]\nauth = chatgpt\nchatgpt_headless = true\n").unwrap();
        let o = &c.providers["openai"];
        assert_eq!(o.auth, Some(AuthMode::ChatGpt));
        assert_eq!(o.auth_mode(), AuthMode::ChatGpt);
        assert!(o.chatgpt_headless);
        let c = load("[provider.openai]\nauth = api_key\n").unwrap();
        assert_eq!(c.providers["openai"].auth_mode(), AuthMode::ApiKey);
        assert!(!c.providers["openai"].chatgpt_headless);
        let c = load("[provider.openai]\nbase_url = http://x\n").unwrap();
        assert_eq!(
            c.providers["openai"].auth_mode(),
            AuthMode::ApiKey,
            "default"
        );
        let errs = load("[provider.openai]\nauth = oauth\n").unwrap_err();
        assert!(errs[0].to_string().contains("auth"), "{}", errs[0]);
        let errs = load("[provider.anthropic]\nauth = chatgpt\n").unwrap_err();
        assert!(
            errs[0].to_string().contains("unknown key `auth`"),
            "{}",
            errs[0]
        );
    }

    #[test]
    fn unknown_sections_and_keys_have_locations() {
        let errs = load("[general]\ndefualt_model = x\n[provider.antropic]\napi_key = sk\n[uii]\n[provider.openai]\napp_name = x\n").unwrap_err();
        let msgs: Vec<String> = errs.iter().map(ToString::to_string).collect();
        assert_eq!(msgs.len(), 4, "{msgs:#?}");
        assert_eq!(
            msgs[0],
            "/cfg/harness.ini:2: unknown key `defualt_model` in [general] (did you mean `default_model`?)"
        );
        assert_eq!(
            msgs[1],
            "/cfg/harness.ini:3: unknown section [provider.antropic] (did you mean `provider.anthropic`?)"
        );
        assert_eq!(
            msgs[2],
            "/cfg/harness.ini:5: unknown section [uii] (did you mean `ui`?)"
        );
        assert!(
            msgs[3].starts_with("/cfg/harness.ini:7: unknown key `app_name` in [provider.openai]")
        );
    }

    #[test]
    fn invalid_values_report_line_and_expectation() {
        let errs =
            load("[ui]\ntheme = solarized\n[context]\ncompact_at = 2\n[remote.x]\nport = 99999\n")
                .unwrap_err();
        let msgs: Vec<String> = errs.iter().map(ToString::to_string).collect();
        assert!(
            msgs[0].starts_with("/cfg/harness.ini:2: invalid value for `theme`"),
            "{msgs:?}"
        );
        assert!(msgs[0].contains("dracula | none"));
        assert!(msgs[1].starts_with("/cfg/harness.ini:4:"));
        assert!(
            msgs.iter()
                .any(|m| m.contains(":5: [remote.x] is missing required key `host`"))
        );
        assert!(
            msgs.iter()
                .any(|m| m.starts_with("/cfg/harness.ini:6: invalid value for `port`"))
        );
    }

    #[test]
    fn free_form_sections_validate_values() {
        let errs = load("[models]\nfoo = bar\n[theme]\naccent = purple\n[pricing]\nm/x = 1\n[keys]\nlaunch = ctrl+l\n").unwrap_err();
        assert_eq!(errs.len(), 4);
        assert!(matches!(&errs[3], IniError::UnknownKey { key, .. } if key == "launch"));
    }

    #[test]
    fn secret_env_vars_are_listed() {
        let c = load("[provider.openai]\napi_key_env = OPENAI_API_KEY\n[provider.anthropic]\napi_key_env = ANTHROPIC_API_KEY\n").unwrap();
        assert_eq!(
            c.secret_env_vars(),
            vec!["ANTHROPIC_API_KEY", "OPENAI_API_KEY"]
        );
    }
}
