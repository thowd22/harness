//! Policy rules engine (allow / ask / deny) and precedence.
//!
//! Every tool call becomes an [`Action`] and gets a [`Verdict`] from
//! [`PolicyEngine::decide`]. Layers are evaluated in this order (ADR-6):
//!
//! 1. **Built-in hard denies**: protected paths (harness.ini, the trust
//!    store, data dir internals, `.git/hooks`, writes to `.harness/`).
//! 2. **harness.ini** `[policy]`: `bash.deny`, `paths.protected`, outward
//!    guard, `bash.ask`, `bash.allow`.
//! 3. **Agent / workflow YAML** overlays ([`PolicyOverlay`]): deny, ask,
//!    outward and protected paths only (tighten-only).
//! 4. **Session approvals** ([`crate::approvals::SessionGrants`]): can turn an
//!    `ask` into `allow`, never a `deny`.
//!
//! MCP tool calls (`Action::Tool` named `mcp:<server>/<tool>`) are decided
//! by `[policy] mcp.<server>.*` and the outward default ([`crate::mcp`]).
//!
//! Anything no rule decides falls back to the autonomy [`Mode`].
//! Every decision is reported to the engine's [`DecisionSink`] together with
//! the rule that produced it.

mod overlay;
mod paths;
mod pattern;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub use overlay::{OVERLAY_KEYS, OneOrMany, OverlayError, PolicyOverlay};
pub use paths::{Access, BuiltInHit, BuiltInPaths, MAX_GLOB_EXPANSION, PathGlob};
pub use pattern::CommandPattern;

use crate::approvals::SessionGrants;
use crate::mcp::{McpRules, McpVerdict};
use crate::outward::{DEFAULT_OUTWARD, canonical_forms};
use crate::shell::{self, SimpleCommand};

/// A policy decision. Ordered by strictness: `Allow < Ask < Deny`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Decision {
    /// Run without asking.
    Allow,
    /// Ask the user first.
    Ask,
    /// Refuse.
    Deny,
}

/// Autonomy mode. Ordered from most to least supervised.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Serialize, Deserialize,
)]
#[serde(rename_all = "kebab-case")]
pub enum Mode {
    /// Ask before every edit and command.
    Ask,
    /// Edit files freely; ask before commands.
    #[default]
    AutoEdit,
    /// Act without asking (rules and the outward guard still apply).
    Auto,
}

impl Mode {
    /// All modes, most supervised first.
    pub const ALL: [Mode; 3] = [Mode::Ask, Mode::AutoEdit, Mode::Auto];

    /// The ini / palette spelling (`ask`, `auto-edit`, `auto`).
    pub fn as_str(self) -> &'static str {
        match self {
            Mode::Ask => "ask",
            Mode::AutoEdit => "auto-edit",
            Mode::Auto => "auto",
        }
    }

    /// Parse the ini / palette spelling.
    pub fn parse(s: &str) -> Option<Mode> {
        Self::ALL.into_iter().find(|m| m.as_str() == s.trim())
    }
}

impl std::fmt::Display for Mode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// What a tool call wants to do.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Action {
    /// Read a file or directory.
    Read {
        /// Tool name.
        tool: String,
        /// Path as given (relative to the working directory).
        path: PathBuf,
    },
    /// Create or change a file.
    Write {
        /// Tool name.
        tool: String,
        /// Path as given.
        path: PathBuf,
    },
    /// Run a shell command.
    Bash {
        /// The command line.
        command: String,
    },
    /// Any other tool.
    Tool {
        /// Tool name.
        name: String,
    },
}

impl Action {
    /// Map a tool call to an action: `read`, `grep` and `glob` → read,
    /// `write`/`edit` → write, `bash` → bash (argument names as in `harness-tools`).
    pub fn from_tool_call(tool: &str, args: &Value) -> Action {
        let s = |k: &str| args.get(k).and_then(Value::as_str).map(str::to_string);
        match (tool, s("path"), s("command")) {
            ("read", Some(p), _) => Action::Read {
                tool: tool.into(),
                path: p.into(),
            },
            // Search tools read under `path` (default: the working directory).
            ("grep" | "glob", p, _) => Action::Read {
                tool: tool.into(),
                path: p.unwrap_or_else(|| ".".into()).into(),
            },
            ("write" | "edit", Some(p), _) => Action::Write {
                tool: tool.into(),
                path: p.into(),
            },
            ("bash", _, Some(c)) => Action::Bash { command: c },
            _ => Action::Tool { name: tool.into() },
        }
    }

    /// The tool behind the action.
    pub fn tool(&self) -> &str {
        match self {
            Action::Read { tool, .. } | Action::Write { tool, .. } => tool,
            Action::Bash { .. } => "bash",
            Action::Tool { name } => name,
        }
    }

    /// One-line description (`bash: git push`, `write: src/a.rs`).
    pub fn summary(&self) -> String {
        match self {
            Action::Read { tool, path } | Action::Write { tool, path } => {
                format!("{tool}: {}", path.display())
            }
            Action::Bash { command } => format!("bash: {command}"),
            Action::Tool { name } => name.clone(),
        }
    }
}

/// Which precedence layer produced a decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Layer {
    /// Built-in rules.
    BuiltIn,
    /// `[policy]` in harness.ini.
    Ini,
    /// Agent or workflow YAML overlay.
    Yaml,
    /// A session approval.
    Session,
    /// The autonomy mode default.
    Mode,
}

/// The rule behind a decision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MatchedRule {
    /// Layer.
    pub layer: Layer,
    /// Where the rule lives (`harness.ini [policy] bash.deny`, `built-in`,
    /// the YAML origin, `session`, `mode auto-edit`).
    pub origin: String,
    /// The pattern or rule name.
    pub rule: String,
    /// Human-readable reason, written for the model and the user.
    pub reason: String,
}

/// The outcome of [`PolicyEngine::decide`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Verdict {
    /// The decision.
    pub decision: Decision,
    /// The rule that decided it.
    pub rule: MatchedRule,
    /// For bash: the simple command that decided it.
    pub command: Option<String>,
    /// True if the outward-action guard matched.
    pub outward: bool,
}

/// A decision as reported to a [`DecisionSink`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DecisionRecord {
    /// Tool name.
    pub tool: String,
    /// Action summary.
    pub action: String,
    /// Mode in effect.
    pub mode: Mode,
    /// The verdict.
    #[serde(flatten)]
    pub verdict: Verdict,
}

/// Receives every policy decision (for the event log and the UI).
pub trait DecisionSink: Send + Sync {
    /// Record one decision.
    fn record(&self, record: &DecisionRecord);
}

/// Discards decisions.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoDecisions;

impl DecisionSink for NoDecisions {
    fn record(&self, _: &DecisionRecord) {}
}

/// Keeps decisions in memory (tests, metrics).
#[derive(Debug, Default)]
pub struct RecordingDecisions(std::sync::Mutex<Vec<DecisionRecord>>);

impl RecordingDecisions {
    /// Everything recorded so far.
    pub fn records(&self) -> Vec<DecisionRecord> {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

impl DecisionSink for RecordingDecisions {
    fn record(&self, record: &DecisionRecord) {
        self.0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(record.clone());
    }
}

/// The user-owned policy inputs (harness.ini plus built-ins).
#[derive(Debug, Clone, Default)]
pub struct PolicyConfig {
    /// `[policy] bash.allow`.
    pub bash_allow: Vec<String>,
    /// `[policy] bash.ask`.
    pub bash_ask: Vec<String>,
    /// `[policy] bash.deny`.
    pub bash_deny: Vec<String>,
    /// `[policy] paths.protected`.
    pub paths_protected: Vec<String>,
    /// `[policy] outward.ask` (added to [`DEFAULT_OUTWARD`]).
    pub outward_ask: Vec<String>,
    /// Built-in protected roots: harness.ini, the data dir (trust store,
    /// runs, memory internals). Usually
    /// `DataDir::protected_paths(Some(config_file))`.
    pub protected_roots: Vec<PathBuf>,
    /// Home directory for `~` expansion.
    pub home: Option<PathBuf>,
}

impl PolicyConfig {
    /// Build from a loaded harness.ini: `[policy]` lists, plus the built-in
    /// protected roots `DataDir::protected_paths(config file)` (the config
    /// file is `config.source`, or the default location when none was read).
    pub fn from_config(config: &harness_config::ini::Config) -> Self {
        let config_file = config.source.clone().unwrap_or_else(|| {
            harness_config::paths::config_location(|k| std::env::var_os(k))
                .path()
                .to_path_buf()
        });
        let roots =
            harness_config::paths::DataDir::from_config(config).protected_paths(Some(&config_file));
        Self::from_ini(config, roots, harness_config::paths::home_dir())
    }

    /// Build from harness.ini with explicit built-in roots and home.
    pub fn from_ini(
        config: &harness_config::ini::Config,
        protected_roots: Vec<PathBuf>,
        home: Option<PathBuf>,
    ) -> Self {
        let p = &config.policy;
        Self {
            bash_allow: p.bash_allow.clone(),
            bash_ask: p.bash_ask.clone(),
            bash_deny: p.bash_deny.clone(),
            paths_protected: p.paths_protected.clone(),
            outward_ask: p.outward_ask.clone(),
            protected_roots,
            home,
        }
    }
}

impl From<harness_config::ini::config::Mode> for Mode {
    fn from(m: harness_config::ini::config::Mode) -> Self {
        use harness_config::ini::config::Mode as M;
        match m {
            M::Ask => Mode::Ask,
            M::AutoEdit => Mode::AutoEdit,
            M::Auto => Mode::Auto,
        }
    }
}

impl From<&harness_config::agents::PolicyFragment> for PolicyOverlay {
    fn from(f: &harness_config::agents::PolicyFragment) -> Self {
        Self {
            bash_deny: f.bash_deny.clone(),
            bash_ask: f.bash_ask.clone(),
            paths_protected: f.paths_protected.clone(),
            outward_ask: f.outward_ask.clone(),
        }
    }
}

/// An invalid rule.
#[derive(Debug, thiserror::Error)]
#[error("invalid protected path pattern `{pattern}` ({origin}): {source}")]
pub struct RuleError {
    /// The pattern.
    pub pattern: String,
    /// Where it came from.
    pub origin: String,
    /// Glob error.
    #[source]
    pub source: globset::Error,
}

#[derive(Debug, Clone)]
struct Rules {
    origin: String,
    layer: Layer,
    deny: Vec<CommandPattern>,
    ask: Vec<CommandPattern>,
    outward: Vec<CommandPattern>,
    paths: Vec<PathGlob>,
}

impl Rules {
    fn key(&self, key: &str) -> String {
        match self.layer {
            Layer::Ini => format!("{} {key}", self.origin),
            _ => format!("{} policy.{key}", self.origin),
        }
    }
}

const INI_ORIGIN: &str = "harness.ini [policy]";
const BUILT_IN: &str = "built-in";

/// Programs whose path arguments are treated as writes.
const WRITE_PROGRAMS: &[&str] = &[
    "rm", "rmdir", "mv", "cp", "tee", "touch", "mkdir", "chmod", "chown", "chgrp", "ln",
    "truncate", "dd", "install", "rsync", "sed", "perl", "shred", "unlink", "patch", "tar",
    "unzip",
];

/// Programs that change whole directory trees; their arguments must not be
/// an ancestor of a built-in protected root (`rm -rf ~/.local`).
const TREE_WRITERS: &[&str] = &[
    "rm", "rmdir", "mv", "chmod", "chown", "chgrp", "rsync", "shred",
];

/// The policy engine.
#[derive(Clone)]
pub struct PolicyEngine {
    root: PathBuf,
    home: Option<PathBuf>,
    builtin: BuiltInPaths,
    ini: Rules,
    allow: Vec<CommandPattern>,
    overlays: Vec<Rules>,
    grants: Arc<SessionGrants>,
    sink: Arc<dyn DecisionSink>,
    mcp: McpRules,
}

impl std::fmt::Debug for PolicyEngine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PolicyEngine")
            .field("root", &self.root)
            .field("builtin", &self.builtin)
            .field("overlays", &self.overlays.len())
            .finish_non_exhaustive()
    }
}

fn patterns(v: &[String]) -> Vec<CommandPattern> {
    v.iter()
        .filter(|p| !p.trim().is_empty())
        .map(|p| CommandPattern::new(p))
        .collect()
}

fn globs(
    v: &[String],
    home: Option<&Path>,
    root: &Path,
    origin: &str,
) -> Result<Vec<PathGlob>, RuleError> {
    v.iter()
        .filter(|p| !p.trim().is_empty())
        .map(|p| {
            PathGlob::new(p, home, root).map_err(|source| RuleError {
                pattern: p.clone(),
                origin: origin.into(),
                source,
            })
        })
        .collect()
}

impl PolicyEngine {
    /// Build an engine for the workspace at `root`.
    pub fn new(config: &PolicyConfig, root: impl Into<PathBuf>) -> Result<Self, RuleError> {
        let root = harness_tools::workspace::normalize(&root.into());
        let home = config.home.clone();
        let mut outward: Vec<CommandPattern> = DEFAULT_OUTWARD
            .iter()
            .map(|p| CommandPattern::new(p))
            .collect();
        outward.extend(patterns(&config.outward_ask));
        let ini = Rules {
            origin: INI_ORIGIN.into(),
            layer: Layer::Ini,
            deny: patterns(&config.bash_deny),
            ask: patterns(&config.bash_ask),
            outward,
            paths: globs(&config.paths_protected, home.as_deref(), &root, INI_ORIGIN)?,
        };
        Ok(Self {
            builtin: BuiltInPaths::new(config.protected_roots.iter().cloned()),
            allow: patterns(&config.bash_allow),
            ini,
            overlays: Vec::new(),
            grants: Arc::new(SessionGrants::default()),
            sink: Arc::new(NoDecisions),
            mcp: McpRules::default(),
            root,
            home,
        })
    }

    /// Decide MCP tool calls with `rules` (`[policy] mcp.*`). Without it
    /// every MCP tool is an outward action that asks.
    pub fn with_mcp(mut self, rules: McpRules) -> Self {
        self.mcp = rules;
        self
    }

    /// The MCP rules.
    pub fn mcp(&self) -> &McpRules {
        &self.mcp
    }

    /// Report every decision to `sink`.
    pub fn with_sink(mut self, sink: Arc<dyn DecisionSink>) -> Self {
        self.sink = sink;
        self
    }

    /// Share session approvals with an approval broker.
    pub fn with_grants(mut self, grants: Arc<SessionGrants>) -> Self {
        self.grants = grants;
        self
    }

    /// The session approvals consulted for `ask` decisions.
    pub fn grants(&self) -> &Arc<SessionGrants> {
        &self.grants
    }

    /// Add a tighten-only overlay from agent or workflow YAML. `origin`
    /// names it in decisions (e.g. `agent implementer`).
    pub fn with_overlay(
        mut self,
        origin: impl Into<String>,
        overlay: &PolicyOverlay,
    ) -> Result<Self, RuleError> {
        let origin = origin.into();
        let rules = Rules {
            deny: patterns(&overlay.bash_deny),
            ask: patterns(&overlay.bash_ask),
            outward: patterns(&overlay.outward_ask),
            paths: globs(
                &overlay.paths_protected,
                self.home.as_deref(),
                &self.root,
                &origin,
            )?,
            origin,
            layer: Layer::Yaml,
        };
        self.overlays.push(rules);
        Ok(self)
    }

    /// The workspace root.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Decide `action` under `mode`, with relative paths resolved against
    /// `cwd`, and report the decision to the sink.
    pub fn decide(&self, action: &Action, mode: Mode, cwd: &Path) -> Verdict {
        let verdict = match action {
            Action::Read { path, .. } => self.decide_path(path, Access::Read, mode, cwd),
            Action::Write { path, .. } => self.decide_path(path, Access::Write, mode, cwd),
            Action::Bash { command } => self.decide_bash(command, mode, cwd),
            Action::Tool { name } if crate::mcp::parse_action_name(name).is_some() => {
                self.decide_mcp(name)
            }
            Action::Tool { name } => Verdict {
                decision: Decision::Allow,
                rule: mode_rule(mode, &format!("`{name}` has no policy rules")),
                command: None,
                outward: false,
            },
        };
        self.sink.record(&DecisionRecord {
            tool: action.tool().to_string(),
            action: action.summary(),
            mode,
            verdict: verdict.clone(),
        });
        verdict
    }

    /// Decide without reporting (for previews and tests).
    pub fn check(&self, action: &Action, mode: Mode, cwd: &Path) -> Verdict {
        self.clone()
            .with_sink(Arc::new(NoDecisions))
            .decide(action, mode, cwd)
    }

    fn layers(&self) -> impl Iterator<Item = &Rules> {
        std::iter::once(&self.ini).chain(self.overlays.iter())
    }

    /// True when reading `path` (a concrete path, not a pattern) is denied
    /// as a protected path. Search tools use it to drop protected files
    /// from their results.
    pub fn read_protected(&self, path: &Path, cwd: &Path) -> bool {
        let raw = path.to_string_lossy();
        let cands = paths::candidates(&raw, cwd, self.home.as_deref());
        self.protected_candidates(&raw, &cands, Access::Read)
            .is_some()
    }

    /// A deny rule if `raw` names a protected path.
    fn protected(&self, raw: &str, access: Access, cwd: &Path) -> Option<MatchedRule> {
        let mut cands = paths::candidates(raw, cwd, self.home.as_deref());
        if paths::has_wildcards(raw) {
            let expanded: Vec<PathBuf> = cands
                .iter()
                .flat_map(|c| paths::expand_wildcards(c))
                .collect();
            for e in expanded {
                cands.extend(paths::candidates(&e.to_string_lossy(), cwd, None));
            }
        }
        self.protected_candidates(raw, &cands, access)
    }

    fn protected_candidates(
        &self,
        raw: &str,
        cands: &[PathBuf],
        access: Access,
    ) -> Option<MatchedRule> {
        for c in cands {
            if let Some(hit) = self.builtin.check(c, access) {
                let (rule, what) = match hit {
                    BuiltInHit::Root(r) => (
                        r.display().to_string(),
                        "harness configuration and internal data",
                    ),
                    BuiltInHit::GitHooks => (".git/hooks".into(), "git hooks"),
                    BuiltInHit::ProjectConfig => {
                        (".harness/".into(), "project harness configuration")
                    }
                };
                return Some(MatchedRule {
                    layer: Layer::BuiltIn,
                    origin: BUILT_IN.into(),
                    rule,
                    reason: format!(
                        "`{raw}` is a protected path ({what}); agents cannot read or change it."
                    ),
                });
            }
        }
        for layer in self.layers() {
            for g in &layer.paths {
                if cands.iter().any(|c| g.matches(c)) {
                    return Some(MatchedRule {
                        layer: layer.layer,
                        origin: layer.key("paths.protected"),
                        rule: g.as_str().into(),
                        reason: format!("`{raw}` matches protected path `{}`.", g.as_str()),
                    });
                }
            }
        }
        None
    }

    /// A deny rule if an argument is a directory containing a built-in
    /// protected root.
    fn contains_protected(&self, cmd: &SimpleCommand, cwd: &Path) -> Option<MatchedRule> {
        for w in cmd.args() {
            for c in paths::candidates(&w.text, cwd, self.home.as_deref()) {
                if let Some(r) = self
                    .builtin
                    .roots()
                    .iter()
                    .find(|r| r.starts_with(&c) && **r != c)
                {
                    return Some(MatchedRule {
                        layer: Layer::BuiltIn,
                        origin: BUILT_IN.into(),
                        rule: r.display().to_string(),
                        reason: format!(
                            "`{}` contains the protected path {}; agents cannot change it.",
                            w.text,
                            r.display()
                        ),
                    });
                }
            }
        }
        None
    }

    fn decide_path(&self, path: &Path, access: Access, mode: Mode, cwd: &Path) -> Verdict {
        let raw = path.to_string_lossy();
        if let Some(rule) = self.protected(&raw, access, cwd) {
            return verdict(Decision::Deny, rule, None, false);
        }
        if access == Access::Read || mode >= Mode::AutoEdit {
            return verdict(
                Decision::Allow,
                mode_rule(mode, "file access allowed by mode"),
                None,
                false,
            );
        }
        let abs = harness_tools::workspace::normalize(&cwd.join(path));
        if let Some(g) = self.grants.matching_path(&abs, &self.root) {
            return verdict(Decision::Allow, session_rule(&g), None, false);
        }
        verdict(
            Decision::Ask,
            mode_rule(mode, "edits need approval in ask mode"),
            None,
            false,
        )
    }

    fn decide_bash(&self, command: &str, mode: Mode, cwd: &Path) -> Verdict {
        let script = match shell::parse(command) {
            Ok(s) => s,
            Err(e) => return self.unparseable(command, &e),
        };
        let mut best: Option<Verdict> = None;
        for cmd in &script.commands {
            let v = self.decide_command(cmd, mode, cwd);
            if best.as_ref().is_none_or(|b| v.decision > b.decision) {
                best = Some(v);
            }
            if best.as_ref().is_some_and(|b| b.decision == Decision::Deny) {
                break;
            }
        }
        best.unwrap_or_else(|| {
            verdict(
                Decision::Allow,
                MatchedRule {
                    layer: Layer::BuiltIn,
                    origin: BUILT_IN.into(),
                    rule: "empty command".into(),
                    reason: "the command runs nothing".into(),
                },
                None,
                false,
            )
        })
    }

    fn unparseable(&self, command: &str, e: &shell::ParseError) -> Verdict {
        for layer in self.layers() {
            if let Some(p) = layer.deny.iter().find(|p| p.matches(command.trim())) {
                return verdict(
                    Decision::Deny,
                    deny_rule(layer, p),
                    Some(command.into()),
                    false,
                );
            }
        }
        verdict(
            Decision::Ask,
            MatchedRule {
                layer: Layer::BuiltIn,
                origin: BUILT_IN.into(),
                rule: "unparseable command".into(),
                reason: format!(
                    "policy cannot tell what this command runs ({e}), so it needs approval."
                ),
            },
            Some(command.into()),
            false,
        )
    }

    fn decide_command(&self, cmd: &SimpleCommand, mode: Mode, cwd: &Path) -> Verdict {
        let line = cmd.line();
        let some_line = || Some(line.clone());
        // 1. Built-in and configured protected paths in arguments.
        let writer = cmd
            .program_name()
            .is_some_and(|n| WRITE_PROGRAMS.contains(&n));
        for (raw, write) in path_args(cmd, writer) {
            let access = if write { Access::Write } else { Access::Read };
            if let Some(rule) = self.protected(raw, access, cwd) {
                return verdict(Decision::Deny, rule, some_line(), false);
            }
        }
        if cmd
            .program_name()
            .is_some_and(|n| TREE_WRITERS.contains(&n))
            && let Some(rule) = self.contains_protected(cmd, cwd)
        {
            return verdict(Decision::Deny, rule, some_line(), false);
        }
        let forms = canonical_forms(cmd);
        // 2. Deny rules (ini, then YAML).
        for layer in self.layers() {
            if let Some(p) = layer.deny.iter().find(|p| p.matches_any(&forms)) {
                return verdict(Decision::Deny, deny_rule(layer, p), some_line(), false);
            }
        }
        // 3. Outward guard: asks in every mode unless explicitly allowed.
        let outward = self.layers().find_map(|l| {
            l.outward
                .iter()
                .find(|p| p.matches_any(&forms))
                .map(|p| (l, p))
        });
        if let Some((layer, p)) = outward {
            let explicit = self.allow.iter().find(|a| {
                !cmd.is_dynamic()
                    && a.matches_any(&forms)
                    && p.matches(a.literal_prefix().trim_end())
            });
            if let Some(a) = explicit {
                return verdict(
                    Decision::Allow,
                    MatchedRule {
                        layer: Layer::Ini,
                        origin: format!("{INI_ORIGIN} bash.allow"),
                        rule: a.as_str().into(),
                        reason: format!(
                            "outward action explicitly allowed by `{}` in harness.ini",
                            a.as_str()
                        ),
                    },
                    some_line(),
                    true,
                );
            }
            let builtin = layer.layer == Layer::Ini && DEFAULT_OUTWARD.contains(&p.as_str());
            let rule = MatchedRule {
                layer: if builtin { Layer::BuiltIn } else { layer.layer },
                origin: if builtin {
                    "built-in outward guard".into()
                } else {
                    layer.key("outward.ask")
                },
                rule: p.as_str().into(),
                reason: format!(
                    "`{line}` is an outward-facing action (`{}`); it always needs approval.",
                    p.as_str()
                ),
            };
            return self.ask_or_grant(rule, &forms, some_line(), true);
        }
        // 4. Ask rules.
        for layer in self.layers() {
            if let Some(p) = layer.ask.iter().find(|p| p.matches_any(&forms)) {
                let rule = MatchedRule {
                    layer: layer.layer,
                    origin: layer.key("bash.ask"),
                    rule: p.as_str().into(),
                    reason: format!("`{line}` matches ask rule `{}`.", p.as_str()),
                };
                return self.ask_or_grant(rule, &forms, some_line(), false);
            }
        }
        // 5. Wrappers are decided by the command they run.
        if cmd.wrapper {
            return verdict(
                Decision::Allow,
                MatchedRule {
                    layer: Layer::BuiltIn,
                    origin: BUILT_IN.into(),
                    rule: "wrapper".into(),
                    reason: "the wrapped command is checked separately".into(),
                },
                some_line(),
                false,
            );
        }
        // 6. Allow rules (only for commands fully known before they run).
        if !cmd.is_dynamic()
            && let Some(p) = self.allow.iter().find(|p| p.matches_any(&forms))
        {
            return verdict(
                Decision::Allow,
                MatchedRule {
                    layer: Layer::Ini,
                    origin: format!("{INI_ORIGIN} bash.allow"),
                    rule: p.as_str().into(),
                    reason: format!("`{line}` matches allow rule `{}`.", p.as_str()),
                },
                some_line(),
                false,
            );
        }
        // 7. Mode default.
        if mode == Mode::Auto {
            return verdict(
                Decision::Allow,
                mode_rule(mode, "commands run without asking in auto mode"),
                some_line(),
                false,
            );
        }
        self.ask_or_grant(
            mode_rule(mode, "commands need approval in this mode"),
            &forms,
            some_line(),
            false,
        )
    }

    fn decide_mcp(&self, name: &str) -> Verdict {
        let (server, tool) = crate::mcp::parse_action_name(name).expect("checked by the caller");
        let ini = |key: &str, pattern: String, reason: String| MatchedRule {
            layer: Layer::Ini,
            origin: format!("{INI_ORIGIN} mcp.{server}.{key}"),
            rule: pattern,
            reason,
        };
        let grant = || self.grants.matching_tool(name);
        match self.mcp.decide(server, tool) {
            McpVerdict::Deny(key, p) => verdict(
                Decision::Deny,
                ini(
                    key,
                    p.clone(),
                    format!("MCP tool `{server}/{tool}` matches deny rule `{p}`."),
                ),
                None,
                false,
            ),
            McpVerdict::Ask(key, p) => {
                let rule = ini(
                    key,
                    p.clone(),
                    format!("MCP tool `{server}/{tool}` matches ask rule `{p}`."),
                );
                match grant() {
                    Some(g) => verdict(Decision::Allow, session_rule(&g), None, false),
                    None => verdict(Decision::Ask, rule, None, false),
                }
            }
            McpVerdict::Allow(key, p) => verdict(
                Decision::Allow,
                ini(
                    key,
                    p.clone(),
                    format!("MCP tool `{server}/{tool}` matches allow rule `{p}`."),
                ),
                None,
                false,
            ),
            McpVerdict::ReadOnly => verdict(
                Decision::Allow,
                ini(
                    "trust_annotations",
                    "readOnlyHint".into(),
                    format!(
                        "the server marks `{tool}` read-only and harness.ini trusts its annotations"
                    ),
                ),
                None,
                false,
            ),
            McpVerdict::Outward => match grant() {
                Some(g) => verdict(Decision::Allow, session_rule(&g), None, true),
                None => verdict(
                    Decision::Ask,
                    MatchedRule {
                        layer: Layer::BuiltIn,
                        origin: "built-in outward guard".into(),
                        rule: "mcp tools".into(),
                        reason: format!(
                            "MCP tool `{server}/{tool}` can act outside the workspace, so it always needs approval; allow it with `[policy] mcp.{server}.allow` in harness.ini."
                        ),
                    },
                    None,
                    true,
                ),
            },
        }
    }

    fn ask_or_grant(
        &self,
        rule: MatchedRule,
        forms: &[String],
        command: Option<String>,
        outward: bool,
    ) -> Verdict {
        match self.grants.matching_command(forms) {
            Some(g) => verdict(Decision::Allow, session_rule(&g), command, outward),
            None => verdict(Decision::Ask, rule, command, outward),
        }
    }
}

/// Path-like arguments of `cmd` and whether each is written.
fn path_args(cmd: &SimpleCommand, writer: bool) -> Vec<(&str, bool)> {
    let mut out: Vec<(&str, bool)> = Vec::new();
    for w in cmd.args() {
        out.push((w.text.as_str(), writer));
        if let Some((_, v)) = w.text.split_once('=')
            && !v.is_empty()
        {
            out.push((v, writer));
        }
    }
    out.extend(
        cmd.assignments
            .iter()
            .map(|(_, w)| (w.text.as_str(), false)),
    );
    for r in cmd.redirects.iter().filter(|r| r.op.targets_path()) {
        out.push((r.target.text.as_str(), r.op.writes()));
    }
    out
}

fn verdict(
    decision: Decision,
    rule: MatchedRule,
    command: Option<String>,
    outward: bool,
) -> Verdict {
    Verdict {
        decision,
        rule,
        command,
        outward,
    }
}

fn deny_rule(layer: &Rules, p: &CommandPattern) -> MatchedRule {
    MatchedRule {
        layer: layer.layer,
        origin: layer.key("bash.deny"),
        rule: p.as_str().into(),
        reason: format!("the command matches deny rule `{}`.", p.as_str()),
    }
}

fn mode_rule(mode: Mode, reason: &str) -> MatchedRule {
    MatchedRule {
        layer: Layer::Mode,
        origin: format!("mode {mode}"),
        rule: mode.as_str().into(),
        reason: reason.into(),
    }
}

fn session_rule(grant: &str) -> MatchedRule {
    MatchedRule {
        layer: Layer::Session,
        origin: "session approval".into(),
        rule: grant.into(),
        reason: format!("approved for this session (`{grant}`)"),
    }
}

#[cfg(test)]
mod tests;
