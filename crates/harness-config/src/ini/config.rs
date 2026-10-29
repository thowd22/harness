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
    "cycle_mode",
    "focus_transcript",
];

/// Fixed (non-dotted) section names.
pub const FIXED_SECTIONS: &[&str] = &[
    "general",
    "models",
    "pricing",
    "keys",
    "ui",
    "theme",
    "policy",
    "sandbox",
    "verify",
    "context",
    "budget",
    "memory",
    "experimental",
];

config_enum!(
    /// Autonomy mode (`[general] mode`).
    Mode { /// Ask before every edit or command.
        Ask = "ask", /// Edit files freely, ask for commands.
        AutoEdit = "auto-edit", /// Act without asking (policy still applies).
        Auto = "auto", /// Never ask (deny rules still apply).
        Yolo = "yolo" }
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
    /// Prompt-cache lifetime (`[provider.anthropic] cache_ttl`, and for
    /// Anthropic models `[provider.openrouter] cache_ttl`, which accepts
    /// only `5m` and `1h`, see [`OpenRouterCacheTtl`]).
    CacheTtl { /// Five minutes on every breakpoint (the default).
        FiveMinutes = "5m", /// One hour on every breakpoint; cache writes cost 2x
        /// instead of 1.25x.
        OneHour = "1h", /// Anthropic only: one hour on the tool and system
        /// breakpoints, five minutes on the message breakpoints.
        Mixed = "mixed" }
);
config_enum!(
    /// The lifetimes `[provider.openrouter] cache_ttl` accepts.
    OpenRouterCacheTtl { /// Five minutes (the default).
        FiveMinutes = "5m", /// One hour; cache writes cost more.
        OneHour = "1h" }
);

impl From<OpenRouterCacheTtl> for CacheTtl {
    fn from(t: OpenRouterCacheTtl) -> Self {
        match t {
            OpenRouterCacheTtl::FiveMinutes => Self::FiveMinutes,
            OpenRouterCacheTtl::OneHour => Self::OneHour,
        }
    }
}
config_enum!(
    /// How the Anthropic provider returns thinking
    /// (`[provider.anthropic] thinking_display`).
    ThinkingDisplay { /// Readable reasoning summaries (the default).
        Summarized = "summarized", /// Empty thinking blocks (signatures only).
        Omitted = "omitted", /// Only the between-tool progress notes (beta).
        Updates = "updates" }
);
config_enum!(
    /// OpenRouter service tier (`[provider.openrouter] service_tier`).
    OpenRouterServiceTier { /// Send `service_tier: "default"`: standard endpoints only (also
        /// stops a `:nitro` model from admitting priority endpoints).
        Default = "default", /// Send `service_tier: "fast"`: the priority tier where the model
        /// has one (Anthropic fast mode on Claude Opus), at its higher
        /// price; OpenRouter falls back to standard endpoints at the
        /// standard price when none is available.
        Fast = "fast" }
);
config_enum!(
    /// How the OpenAI provider authenticates (`[provider.openai] auth`).
    AuthMode { /// `api_key`, `api_key_cmd` or `api_key_env` (the default).
        ApiKey = "api_key", /// Sign in with ChatGPT (`harness login openai`); usage counts
        /// against the ChatGPT plan.
        ChatGpt = "chatgpt" }
);
config_enum!(
    /// OpenAI prompt-cache retention (`[provider.openai]
    /// prompt_cache_retention`).
    OpenAiCacheRetention { /// Send nothing, so the organisation's default applies (the
        /// default; `24h` for organisations without Zero Data Retention,
        /// `in_memory` for ZDR organisations).
        Default = "default", /// Send `prompt_cache_retention: "24h"` for the models that
        /// support extended caching (opt-in).
        TwentyFourHours = "24h" }
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
    /// Model calls an agent with the `todo` tool may make in one turn with
    /// an empty task list before harness reminds it to plan with `todo`
    /// (`todo_nudge_after`). The reminder repeats at most this often and
    /// stops once a task exists. Default 3; 0 turns it off.
    pub todo_nudge_after: u32,
    /// Check GitHub for a newer harness release when the interactive TUI
    /// starts (`update_check`). Headless commands never check. Default
    /// true.
    pub update_check: bool,
    /// How long the last update check stays fresh: the TUI asks GitHub at
    /// most this often (`update_check_interval`). Default 24h.
    #[serde(serialize_with = "ser_duration")]
    pub update_check_interval: Duration,
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
    /// Anthropic, and OpenRouter for Anthropic models: prompt-cache
    /// lifetime (`cache_ttl`, default `5m`; `mixed` only for anthropic).
    pub cache_ttl: Option<CacheTtl>,
    /// OpenRouter only: send a per-session `session_id` so OpenRouter
    /// keeps routing the session to the same upstream (sticky routing,
    /// warm prompt cache) from the first request on (`session_id`,
    /// default on; `None` = default). Ignored by OpenRouter when
    /// `provider_order` is set.
    pub session_id: Option<bool>,
    /// Anthropic only: request-body gzip (`gzip_requests`, default `auto`).
    pub gzip_requests: Option<GzipRequests>,
    /// Anthropic only: gzip level 1 to 9 (`gzip_level`, default 3).
    pub gzip_level: Option<u32>,
    /// Anthropic only: thinking display (`thinking_display`, default
    /// `summarized`). Left out of the serialized form when unset.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking_display: Option<ThinkingDisplay>,
    /// Anthropic only: use the one-hour lifetime on every breakpoint while
    /// a long wait is expected (a background job runs, or the last tool
    /// round was long) (`cache_ttl_auto_1h`, default false). Left out of
    /// the serialized form when unset.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_ttl_auto_1h: Option<bool>,
    /// OpenRouter only: ask for reasoning to be left out of responses
    /// (`reasoning_exclude`, default false). Left out of the serialized
    /// form when unset.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_exclude: Option<bool>,
    /// OpenRouter only: the service tier to request (`service_tier`,
    /// default unset: the field is not sent). Left out of the serialized
    /// form when unset.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub service_tier: Option<OpenRouterServiceTier>,
    /// OpenAI only: `api_key` (default) or `chatgpt` (Sign in with
    /// ChatGPT).
    pub auth: Option<AuthMode>,
    /// OpenAI only: allow `auth = chatgpt` in headless and eval runs
    /// (`chatgpt_headless`, default false). The ChatGPT credential is
    /// personal; CI is refused even when this is set.
    pub chatgpt_headless: bool,
    /// OpenAI only: prompt-cache retention to request
    /// (`prompt_cache_retention`, default unset = `default`: the field is
    /// not sent). Left out of the serialized form when unset.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_cache_retention: Option<OpenAiCacheRetention>,
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
    /// Provider-stream silence before the activity line warns, and the
    /// quiet time (no output, no CPU) before a running tool gets a soft
    /// hint. Default 20s; 0 disables both.
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
    /// `mcp.<server>.allow`, `mcp.<server>.ask`, `mcp.<server>.deny` and
    /// `mcp.<server>.trust_annotations`, by server.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub mcp: crate::mcp_tools::McpPolicy,
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
    /// Token budget for the schemas of an agent's selected MCP tools; over
    /// it they are offered through the `mcp_tools` search-and-call tool
    /// instead. Default [`DEFAULT_MCP_SCHEMA_BUDGET`].
    #[serde(skip_serializing_if = "is_default_mcp_budget")]
    pub mcp_schema_budget: u64,
    /// `background_compaction`: start the handoff summary in the background
    /// at `background_compact_at` and swap it in at `compact_at`, so the
    /// turn that crosses `compact_at` does not wait for the summary call.
    /// Default false (experimental, TASK-161.5). Left out of the serialized
    /// config when off, so config hashes stay unchanged.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub background_compaction: bool,
    /// `background_compact_at`: fraction of the window at which a
    /// background summary starts (capped at `compact_at`). Default
    /// [`DEFAULT_BACKGROUND_COMPACT_AT`].
    #[serde(skip_serializing_if = "is_default_background_compact_at")]
    pub background_compact_at: f64,
}

/// Default `[context] background_compact_at`.
pub const DEFAULT_BACKGROUND_COMPACT_AT: f64 = 0.7;

fn is_default_background_compact_at(v: &f64) -> bool {
    *v == DEFAULT_BACKGROUND_COMPACT_AT
}

/// Default `[context] mcp_schema_budget` (tokens).
pub const DEFAULT_MCP_SCHEMA_BUDGET: u64 = 2000;

fn is_default_mcp_budget(v: &u64) -> bool {
    *v == DEFAULT_MCP_SCHEMA_BUDGET
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

/// `[experimental]`: opt-in performance experiments (TASK-161,
/// `docs/performance.md`). Everything is off by default.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Experimental {
    /// `prewarm_connection`: open the model provider's connection (DNS,
    /// TCP, TLS) in the background when a session starts, so the first
    /// request does not pay for it.
    pub prewarm_connection: bool,
    /// `cache_prewarm`: write the prompt cache ahead of the first request
    /// with a non-generating call (Anthropic `max_tokens: 0`) at session
    /// start and resume, after `/compact`, and (with `stagger_subagents`)
    /// before a subagent fan-out. Costs a cache write per pre-warm.
    pub cache_prewarm: bool,
    /// `stagger_subagents`: when several subagent requests with the same
    /// prompt prefix start together, send the first one alone and start
    /// the rest once its response begins (its cache entry exists then).
    pub stagger_subagents: bool,
    /// `early_tool_start`: start a reply's leading read-only tool calls
    /// that policy allows without asking as soon as each call's arguments
    /// are complete in the stream, instead of after the whole message.
    /// Results and the event log are unchanged.
    pub early_tool_start: bool,
}

impl Experimental {
    /// True when nothing is turned on (the section is then left out of the
    /// serialized config, so config hashes stay unchanged).
    pub fn is_off(&self) -> bool {
        *self == Experimental::default()
    }
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
    /// `[mcp.<name>]`: MCP servers, keyed by name. Omitted from the
    /// serialized form when empty, so config hashes without MCP servers
    /// are unchanged.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub mcp: BTreeMap<String, super::mcp::McpServer>,
    /// `[experimental]`. Omitted from the serialized form when off.
    #[serde(skip_serializing_if = "Experimental::is_off")]
    pub experimental: Experimental,
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
                todo_nudge_after: 3,
                update_check: true,
                update_check_interval: Duration::from_secs(24 * 3600),
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
                mcp_schema_budget: DEFAULT_MCP_SCHEMA_BUDGET,
                background_compaction: false,
                background_compact_at: DEFAULT_BACKGROUND_COMPACT_AT,
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
            experimental: Experimental::default(),
            mcp: BTreeMap::new(),
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
                "experimental" => read_experimental(&mut r, &mut cfg.experimental),
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
                    } else if let Some(server) = name.strip_prefix("mcp.") {
                        if !super::mcp::valid_server_name(server) {
                            drop(r);
                            errors.push(IniError::Syntax {
                                location: section.location.clone(),
                                message: format!(
                                    "invalid MCP server name `{server}` in [{name}] (use letters, digits, `_` and `-`)"
                                ),
                            });
                            continue;
                        }
                        if let Some(m) = super::mcp::read_mcp(&mut r) {
                            cfg.mcp.insert(server.to_owned(), m);
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
            .chain(
                self.mcp
                    .values()
                    .flat_map(|m| m.secret_env_vars().map(str::to_owned)),
            )
            .collect();
        v.sort();
        v.dedup();
        v
    }

    /// True when any provider has an inline `api_key`, or any MCP server
    /// an inline `env.*` / `header.*` value.
    pub fn has_inline_secrets(&self) -> bool {
        self.providers.values().any(|p| p.api_key.is_some())
            || self.mcp.values().any(|m| m.has_inline_secrets())
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
                .chain(["provider.<id>", "remote.<name>", "mcp.<name>"]),
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
    set!(g.todo_nudge_after, r.uint("todo_nudge_after"));
    set!(g.update_check, on_off(r, "update_check"));
    set!(g.update_check_interval, r.duration("update_check_interval"));
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
        p.thinking_display = r.enumerated("thinking_display");
        p.cache_ttl = r.enumerated("cache_ttl");
        p.cache_ttl_auto_1h = r.bool("cache_ttl_auto_1h");
    }
    if id == "openai" {
        p.auth = r.enumerated("auth");
        p.chatgpt_headless = r.bool("chatgpt_headless").unwrap_or(false);
        p.prompt_cache_retention = r.enumerated("prompt_cache_retention");
    }
    if id == "openrouter" {
        p.app_name = r.nonempty_string("app_name");
        p.provider_order = r.list("provider_order").unwrap_or_default();
        p.allow_fallbacks = r.bool("allow_fallbacks");
        p.cache_ttl = r
            .enumerated::<OpenRouterCacheTtl>("cache_ttl")
            .map(Into::into);
        p.session_id = r.bool("session_id");
        p.reasoning_exclude = r.bool("reasoning_exclude");
        p.service_tier = r.enumerated("service_tier");
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
    read_mcp_policy(r, &mut p.mcp);
}

/// `[policy] mcp.<server>.allow|ask|deny|trust_annotations`.
fn read_mcp_policy<'a>(r: &mut SectionReader<'a>, rules: &mut crate::mcp_tools::McpPolicy) {
    let section = r.section();
    for e in &section.entries {
        let Some(rest) = e.key.strip_prefix("mcp.") else {
            continue;
        };
        let entry = r.entry(&e.key).expect("entry exists");
        let Some((server, what)) = rest.rsplit_once('.') else {
            r.invalid(
                entry,
                "expected `mcp.<server>.allow`, `.ask`, `.deny` or `.trust_annotations`",
            );
            continue;
        };
        if !super::mcp::valid_server_name(server) {
            r.invalid(
                entry,
                format!("`{server}` is not an MCP server name (letters, digits, `_` and `-`)"),
            );
            continue;
        }
        let slot = rules.entry(server.to_owned()).or_default();
        let list = match what {
            "allow" => &mut slot.allow,
            "ask" => &mut slot.ask,
            "deny" => &mut slot.deny,
            "trust_annotations" => {
                match super::value::parse_bool(&entry.value) {
                    Ok(b) => slot.trust_annotations = b,
                    Err(m) => r.invalid(entry, m),
                }
                continue;
            }
            other => {
                r.invalid(
                    entry,
                    format!(
                        "unknown MCP rule `{other}`; use allow, ask, deny or trust_annotations"
                    ),
                );
                continue;
            }
        };
        list.extend(parse_list(&entry.value));
    }
    rules.retain(|_, v| *v != crate::mcp_tools::McpToolRules::default());
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
    for (key, slot) in [
        ("compact_at", &mut c.compact_at),
        ("background_compact_at", &mut c.background_compact_at),
    ] {
        if let Some(f) = r.float(key) {
            if f > 0.0 && f <= 1.0 {
                *slot = f;
            } else {
                let entry = r.section().get(key).expect("key was just read");
                r.invalid(entry, "expected a fraction greater than 0 and at most 1");
            }
        }
    }
    set!(c.background_compaction, r.bool("background_compaction"));
    set!(c.keep_recent_turns, r.uint("keep_recent_turns"));
    set!(c.elide_tool_results_over, r.uint("elide_tool_results_over"));
    set!(c.mcp_schema_budget, r.uint("mcp_schema_budget"));
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

fn read_experimental(r: &mut SectionReader<'_>, x: &mut Experimental) {
    set!(x.prewarm_connection, r.bool("prewarm_connection"));
    set!(x.cache_prewarm, r.bool("cache_prewarm"));
    set!(x.stagger_subagents, r.bool("stagger_subagents"));
    set!(x.early_tool_start, r.bool("early_tool_start"));
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
        assert_eq!(or.cache_ttl, Some(CacheTtl::FiveMinutes));
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
    fn update_check_defaults_to_on_daily_and_can_be_turned_off() {
        let c = load("").unwrap();
        assert!(c.general.update_check);
        assert_eq!(c.general.update_check_interval, Duration::from_secs(86_400));
        for off in ["off", "false"] {
            let c = load(&format!("[general]\nupdate_check = {off}\n")).unwrap();
            assert!(!c.general.update_check, "{off}");
        }
        let c = load("[general]\nupdate_check = on\nupdate_check_interval = 7d\n");
        assert!(c.is_err(), "days are not a duration unit");
        let c = load("[general]\nupdate_check_interval = 168h\n").unwrap();
        assert_eq!(
            c.general.update_check_interval,
            Duration::from_secs(7 * 86_400)
        );
        assert!(load("[general]\nupdate_check = maybe\n").is_err());
    }

    #[test]
    fn todo_nudge_after_defaults_to_three_and_zero_turns_it_off() {
        assert_eq!(load("").unwrap().general.todo_nudge_after, 3);
        let c = load("[general]\ntodo_nudge_after = 0\n").unwrap();
        assert_eq!(c.general.todo_nudge_after, 0);
        assert!(load("[general]\ntodo_nudge_after = -1\n").is_err());
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
    fn openrouter_session_id_defaults_on_with_an_off_switch() {
        let c = load("[provider.openrouter]\napi_key = k\n").unwrap();
        assert_eq!(c.providers["openrouter"].session_id, None);
        let c = load("[provider.openrouter]\nsession_id = false\n").unwrap();
        assert_eq!(c.providers["openrouter"].session_id, Some(false));
        assert!(load("[provider.openai]\nsession_id = false\n").is_err());
        // The pre-release experimental key is gone.
        assert!(load("[experimental]\nopenrouter_session_id = true\n").is_err());
    }

    #[test]
    fn openrouter_cache_ttl() {
        let c = load("[provider.openrouter]\ncache_ttl = 1h\n").unwrap();
        assert_eq!(c.providers["openrouter"].cache_ttl, Some(CacheTtl::OneHour));
        let c = load("[provider.openrouter]\n").unwrap();
        assert_eq!(c.providers["openrouter"].cache_ttl, None);
        let errs = load("[provider.openrouter]\ncache_ttl = 2h\n").unwrap_err();
        assert!(errs[0].to_string().contains("5m | 1h"), "{errs:?}");
    }

    #[test]
    fn anthropic_request_options() {
        let c = load("[provider.anthropic]\n").unwrap();
        let a = &c.providers["anthropic"];
        assert_eq!(
            (a.thinking_display, a.cache_ttl, a.cache_ttl_auto_1h),
            (None, None, None)
        );
        let plain = serde_json::to_value(&c).unwrap();
        let p = &plain["providers"]["anthropic"];
        for k in ["thinking_display", "cache_ttl_auto_1h"] {
            assert!(p.get(k).is_none(), "{k} unset must not change the hash");
        }
        let c = load(
            "[provider.anthropic]\nthinking_display = omitted\ncache_ttl = mixed\ncache_ttl_auto_1h = true\n",
        )
        .unwrap();
        let a = &c.providers["anthropic"];
        assert_eq!(a.thinking_display, Some(ThinkingDisplay::Omitted));
        assert_eq!(a.cache_ttl, Some(CacheTtl::Mixed));
        assert_eq!(a.cache_ttl_auto_1h, Some(true));
        for (v, want) in [
            ("summarized", ThinkingDisplay::Summarized),
            ("updates", ThinkingDisplay::Updates),
        ] {
            let c = load(&format!("[provider.anthropic]\nthinking_display = {v}\n")).unwrap();
            assert_eq!(c.providers["anthropic"].thinking_display, Some(want));
        }
        let errs =
            load("[provider.anthropic]\nthinking_display = full\ncache_ttl = 2h\n").unwrap_err();
        let all = errs
            .iter()
            .map(|e| e.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(all.contains("summarized | omitted | updates"), "{all}");
        assert!(all.contains("5m | 1h | mixed"), "{all}");
        // OpenRouter keeps its own two lifetimes and the Anthropic-only keys
        // are unknown there.
        let errs = load("[provider.openrouter]\ncache_ttl = mixed\nthinking_display = omitted\n")
            .unwrap_err();
        let all = errs
            .iter()
            .map(|e| e.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(all.contains("5m | 1h"), "{all}");
        assert!(all.contains("unknown key `thinking_display`"), "{all}");
    }

    #[test]
    fn openrouter_service_tier_is_opt_in() {
        let c = load("[provider.openrouter]\n").unwrap();
        assert_eq!(c.providers["openrouter"].service_tier, None);
        let plain = serde_json::to_value(&c).unwrap();
        assert!(
            plain["providers"]["openrouter"]
                .get("service_tier")
                .is_none(),
            "unset must not change the hash"
        );
        for (v, want) in [
            ("fast", OpenRouterServiceTier::Fast),
            ("default", OpenRouterServiceTier::Default),
        ] {
            let c = load(&format!("[provider.openrouter]\nservice_tier = {v}\n")).unwrap();
            assert_eq!(c.providers["openrouter"].service_tier, Some(want));
        }
        let errs = load("[provider.openrouter]\nservice_tier = flex\n").unwrap_err();
        assert!(errs[0].to_string().contains("default | fast"), "{errs:?}");
        let errs = load("[provider.anthropic]\nservice_tier = fast\n").unwrap_err();
        assert!(
            errs[0].to_string().contains("unknown key `service_tier`"),
            "{errs:?}"
        );
    }

    #[test]
    fn openrouter_reasoning_exclude() {
        let c = load("[provider.openrouter]\nreasoning_exclude = true\n").unwrap();
        assert_eq!(c.providers["openrouter"].reasoning_exclude, Some(true));
        let c = load("[provider.openrouter]\n").unwrap();
        assert_eq!(c.providers["openrouter"].reasoning_exclude, None);
        let errs = load("[provider.anthropic]\nreasoning_exclude = true\n").unwrap_err();
        assert!(
            errs[0]
                .to_string()
                .contains("unknown key `reasoning_exclude`")
        );
    }

    #[test]
    fn cache_prewarm_and_stagger_are_experimental_and_off_by_default() {
        let c = load("").unwrap();
        assert!(!c.experimental.cache_prewarm && !c.experimental.stagger_subagents);
        let c = load("[experimental]\ncache_prewarm = true\nstagger_subagents = true\n").unwrap();
        assert!(c.experimental.cache_prewarm && c.experimental.stagger_subagents);
        assert!(!c.experimental.is_off());
    }

    #[test]
    fn early_tool_start_is_experimental_and_off_by_default() {
        assert!(!load("").unwrap().experimental.early_tool_start);
        let c = load("[experimental]\nearly_tool_start = true\n").unwrap();
        assert!(c.experimental.early_tool_start);
        assert!(!c.experimental.is_off());
    }

    #[test]
    fn experimental_section_is_off_by_default_and_hash_neutral() {
        let c = load("").unwrap();
        assert!(c.experimental.is_off());
        let plain = serde_json::to_value(&c).unwrap();
        assert!(plain.get("experimental").is_none());
        let c = load("[experimental]\nprewarm_connection = true\n").unwrap();
        assert!(c.experimental.prewarm_connection);
        assert!(
            serde_json::to_value(&c)
                .unwrap()
                .get("experimental")
                .is_some()
        );
        let errs = load("[experimental]\nwarp_speed = true\n").unwrap_err();
        assert!(errs[0].to_string().contains("warp_speed"), "{errs:?}");
    }

    #[test]
    fn background_compaction_is_off_by_default_and_hash_neutral() {
        let c = load("").unwrap();
        assert!(!c.context.background_compaction);
        assert_eq!(
            c.context.background_compact_at,
            DEFAULT_BACKGROUND_COMPACT_AT
        );
        let plain = serde_json::to_value(&c).unwrap();
        assert!(plain["context"].get("background_compaction").is_none());
        assert!(plain["context"].get("background_compact_at").is_none());
        let c = load("[context]\nbackground_compaction = true\nbackground_compact_at = 0.65\n")
            .unwrap();
        assert!(c.context.background_compaction);
        assert_eq!(c.context.background_compact_at, 0.65);
        let v = serde_json::to_value(&c).unwrap();
        assert_eq!(v["context"]["background_compaction"], true);
        let errs = load("[context]\nbackground_compact_at = 1.5\n").unwrap_err();
        assert!(
            errs[0].to_string().contains("background_compact_at"),
            "{errs:?}"
        );
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
    fn openai_prompt_cache_retention_is_opt_in() {
        let c = load("[provider.openai]\n").unwrap();
        assert_eq!(c.providers["openai"].prompt_cache_retention, None);
        let plain = serde_json::to_value(&c).unwrap();
        assert!(
            plain["providers"]["openai"]
                .get("prompt_cache_retention")
                .is_none(),
            "unset must not change the hash"
        );
        for (v, want) in [
            ("24h", OpenAiCacheRetention::TwentyFourHours),
            ("default", OpenAiCacheRetention::Default),
        ] {
            let c = load(&format!(
                "[provider.openai]\nprompt_cache_retention = {v}\n"
            ))
            .unwrap();
            assert_eq!(c.providers["openai"].prompt_cache_retention, Some(want));
        }
        let errs = load("[provider.openai]\nprompt_cache_retention = 1h\n").unwrap_err();
        assert!(errs[0].to_string().contains("default | 24h"), "{errs:?}");
        // Only the OpenAI provider takes it.
        let errs = load("[provider.openrouter]\nprompt_cache_retention = 24h\n").unwrap_err();
        assert!(
            errs[0]
                .to_string()
                .contains("unknown key `prompt_cache_retention`"),
            "{errs:?}"
        );
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
