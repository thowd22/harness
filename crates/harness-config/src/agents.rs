//! Agent YAML (`agents/*.yaml`): model, prompt, tools, limits, scope,
//! tighten-only policy and placement.
//!
//! ```yaml
//! name: implementer
//! display_name: Implementer     # shown in the TUI and CLI; default from the name
//! description: Writes code for a scoped task
//! model: sonnet                 # alias or provider/model
//! effort: medium                # minimal | low | medium | high | xhigh | max | ultra
//! prompt: coder                 # prompt name, or inline `prompt_text:`
//! tools: [read, write, edit, bash, todo, mcp:github/create_issue, mcp:db/*]
//! subagents: [researcher]       # enables spawn_agent restricted to these
//! memory: read-write            # off | read | read-write
//! max_turns: 60
//! budget: { usd: 2.00, tokens: 2_000_000 }
//! scope: ["src/**", "tests/**"]
//! policy:                       # tighten-only
//!   bash.deny: ["git push*"]
//! verify: cargo test -p core
//! runs_on: local                # local | <remote name> | { labels: [gpu], min_mem_gb: 32, min_cpus: 8, min_gpus: 1, fallback: local }
//! output_schema: { type: object }
//! ```
//!
//! [`Agent::load`](crate::discovery::Artifact::load) performs file-local
//! checks (syntax, known tools, glob syntax, policy keys). [`validate`]
//! performs cross-file checks against the [`Catalog`] and
//! [`ValidationContext`]: model aliases, prompt, subagent and remote
//! references, budget caps, and tighten-only rules for a project agent that
//! shadows a global or built-in one.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::discovery::{Artifact, Catalog, Entry, Kind, Scope, SourceFile};
use crate::ini::Config;
use crate::ini::config::PROVIDER_IDS;
use crate::mcp_tools::{MCP_TOOL_PREFIX, McpSelector};
use crate::yaml::{YamlError, de};

/// Tools an agent may list in `tools:`.
pub const TOOL_NAMES: &[&str] = &["read", "write", "edit", "bash", "grep", "glob", "todo"];

/// Tool added to requests when `subagents:` is non-empty.
pub const SPAWN_AGENT_TOOL: &str = "spawn_agent";

/// Tool added to requests when `memory:` is not `off`.
pub const MEMORY_TOOL: &str = "memory";

/// `policy:` keys an agent may set. All of them only add restrictions.
pub const POLICY_KEYS: &[&str] = &["bash.deny", "bash.ask", "paths.protected", "outward.ask"];

/// Reasoning effort, least to most, mapped to each provider's own
/// setting. A model that does not accept a level gets the nearest one it
/// does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Effort {
    /// Minimal.
    Minimal,
    /// Low.
    Low,
    /// Medium.
    Medium,
    /// High.
    High,
    /// Extra high.
    XHigh,
    /// Max.
    Max,
    /// Ultra.
    Ultra,
}

impl Effort {
    /// Every level, least to most.
    pub const ALL: [Effort; 7] = [
        Self::Minimal,
        Self::Low,
        Self::Medium,
        Self::High,
        Self::XHigh,
        Self::Max,
        Self::Ultra,
    ];

    /// The YAML name: `minimal`, `low`, `medium`, `high`, `xhigh`, `max`
    /// or `ultra`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Minimal => "minimal",
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::XHigh => "xhigh",
            Self::Max => "max",
            Self::Ultra => "ultra",
        }
    }

    /// Parses a name (case-insensitive).
    pub fn parse(s: &str) -> Option<Self> {
        let s = s.trim();
        Self::ALL
            .into_iter()
            .find(|e| e.as_str().eq_ignore_ascii_case(s))
    }
}

/// Memory access level. Ordered from least to most access.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MemoryAccess {
    /// No memory tool, no memory index.
    #[default]
    Off,
    /// Read-only.
    Read,
    /// Read and write.
    ReadWrite,
}

/// The agent's system prompt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PromptRef {
    /// `prompt: <name>`: a prompt from `prompts/`.
    Named(String),
    /// `prompt_text: ...`: inline text (not templated).
    Inline(String),
}

/// Per-agent budget. Both caps are optional.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentBudget {
    /// USD cap.
    #[serde(default)]
    pub usd: Option<f64>,
    /// Token cap (input + output). `2_000_000` style separators are allowed.
    #[serde(default, deserialize_with = "de::lenient_opt_u64")]
    pub tokens: Option<u64>,
}

/// Restrictions an agent adds on top of `harness.ini` `[policy]`.
///
/// This is harness-config's own representation; the policy engine merges it
/// as "ini plus these extra rules". It has no allow lists by construction.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct PolicyFragment {
    /// `bash.deny` globs.
    pub bash_deny: Vec<String>,
    /// `bash.ask` globs.
    pub bash_ask: Vec<String>,
    /// `paths.protected` globs.
    pub paths_protected: Vec<String>,
    /// `outward.ask` globs.
    pub outward_ask: Vec<String>,
}

impl PolicyFragment {
    fn lists(&self) -> [(&'static str, &Vec<String>); 4] {
        [
            ("bash.deny", &self.bash_deny),
            ("bash.ask", &self.bash_ask),
            ("paths.protected", &self.paths_protected),
            ("outward.ask", &self.outward_ask),
        ]
    }

    /// True if no rule is set.
    pub fn is_empty(&self) -> bool {
        self.lists().iter().all(|(_, v)| v.is_empty())
    }

    /// Rules in `base` that are missing here, as `key: rule` (dropping them
    /// would loosen policy).
    pub fn missing_from(&self, base: &PolicyFragment) -> Vec<String> {
        base.lists()
            .iter()
            .zip(self.lists())
            .flat_map(|((k, b), (_, mine))| {
                b.iter()
                    .filter(|r| !mine.contains(r))
                    .map(move |r| format!("{k}: {r}"))
            })
            .collect()
    }
}

/// Where an agent runs.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RunsOn {
    /// This machine.
    #[default]
    Local,
    /// A `[remote.<name>]` from harness.ini.
    Remote(String),
    /// Any remote having all `labels` and the resource minimums.
    Labels {
        /// Required labels.
        labels: Vec<String>,
        /// Minimum memory in GB.
        min_mem_gb: Option<u32>,
        /// Minimum logical CPUs.
        min_cpus: Option<u32>,
        /// Minimum GPUs.
        min_gpus: Option<u32>,
        /// Where to run when no host matches: `local` or a remote name
        /// (`None`: wait for a matching host).
        fallback: Option<String>,
    },
}

#[derive(Deserialize)]
#[serde(untagged)]
enum RunsOnDoc {
    Name(String),
    Labels(LabelsDoc),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LabelsDoc {
    #[serde(default)]
    labels: Vec<String>,
    #[serde(default, deserialize_with = "de::lenient_opt_u32")]
    min_mem_gb: Option<u32>,
    #[serde(default, deserialize_with = "de::lenient_opt_u32")]
    min_cpus: Option<u32>,
    #[serde(default, deserialize_with = "de::lenient_opt_u32")]
    min_gpus: Option<u32>,
    #[serde(default)]
    fallback: Option<String>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum OneOrMany {
    One(String),
    Many(Vec<String>),
}

impl OneOrMany {
    fn into_vec(self) -> Vec<String> {
        match self {
            OneOrMany::One(s) => vec![s],
            OneOrMany::Many(v) => v,
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AgentDoc {
    name: String,
    #[serde(default)]
    display_name: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    effort: Option<Effort>,
    #[serde(default)]
    prompt: Option<String>,
    #[serde(default)]
    prompt_text: Option<String>,
    #[serde(default)]
    tools: Vec<String>,
    #[serde(default)]
    subagents: Vec<String>,
    #[serde(default)]
    memory: MemoryAccess,
    #[serde(default)]
    mode: Option<String>,
    #[serde(default, deserialize_with = "de::lenient_opt_u32")]
    max_turns: Option<u32>,
    #[serde(default)]
    budget: AgentBudget,
    #[serde(default)]
    scope: Vec<String>,
    #[serde(default)]
    policy: BTreeMap<String, OneOrMany>,
    #[serde(default)]
    verify: Option<String>,
    #[serde(default)]
    runs_on: Option<RunsOnDoc>,
    #[serde(default)]
    output_schema: Option<serde_json::Value>,
}

/// A loaded agent definition.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Agent {
    /// Name.
    pub name: String,
    /// Human-readable name shown in the palette, map, history and CLI
    /// (`display_name:`); [`Agent::display_name`] falls back to the name.
    pub display_name: Option<String>,
    /// Description (shown to parents choosing a subagent).
    pub description: Option<String>,
    /// Model alias or `provider/model`; `None` uses `[general] default_model`.
    pub model: Option<String>,
    /// Reasoning effort.
    pub effort: Option<Effort>,
    /// System prompt.
    pub prompt: PromptRef,
    /// Tools in declared order (subset of [`TOOL_NAMES`]).
    pub tools: Vec<String>,
    /// MCP tools selected in `tools:` (`mcp:<server>/<tool>` entries), in
    /// declared order. Nothing from an MCP server is offered unless an
    /// entry here selects it.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub mcp_tools: Vec<McpSelector>,
    /// Agents this one may spawn.
    pub subagents: Vec<String>,
    /// Memory access.
    pub memory: MemoryAccess,
    /// Autonomy mode for this agent's tool calls. It can only make the
    /// session's mode stricter (`ask` < `auto-edit` < `auto`).
    pub mode: Option<crate::ini::config::Mode>,
    /// Turn limit.
    pub max_turns: Option<u32>,
    /// Budget caps.
    pub budget: AgentBudget,
    /// Edit scope globs (empty = no scope declared).
    pub scope: Vec<String>,
    /// Extra policy restrictions.
    pub policy: PolicyFragment,
    /// Verify command override.
    pub verify: Option<String>,
    /// Placement.
    pub runs_on: RunsOn,
    /// JSON Schema for the structured result.
    pub output_schema: Option<serde_json::Value>,
}

/// Display names by agent name ([`Agent::display_name`]), as recorded in
/// `run_started.agent_names` so every view names agents the same way.
pub fn display_names<'a>(agents: impl IntoIterator<Item = &'a Agent>) -> BTreeMap<String, String> {
    agents
        .into_iter()
        .map(|a| (a.name.clone(), a.display_name()))
        .collect()
}

impl Agent {
    /// The name people see: `display_name:` when set, else the name with
    /// `-`/`_` as spaces and a capital first letter
    /// ([`harness_events::names::default_display_name`]).
    pub fn display_name(&self) -> String {
        self.display_name
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .unwrap_or_else(|| harness_events::names::default_display_name(&self.name))
    }

    /// A one-line description: the first line of `description:`, else
    /// `Run the <display name> agent`.
    pub fn summary(&self) -> String {
        self.description
            .as_deref()
            .and_then(|d| d.lines().map(str::trim).find(|l| !l.is_empty()))
            .map(str::to_owned)
            .unwrap_or_else(|| format!("Run the {} agent", self.display_name()))
    }

    /// The tool names that appear in this agent's model requests, in a
    /// stable order: `tools:` as declared, then [`SPAWN_AGENT_TOOL`] if
    /// `subagents:` is non-empty, then [`MEMORY_TOOL`] if memory is not
    /// `off`. Nothing else is ever offered.
    pub fn request_tools(&self) -> Vec<&str> {
        let mut v: Vec<&str> = self.tools.iter().map(String::as_str).collect();
        if !self.subagents.is_empty() {
            v.push(SPAWN_AGENT_TOOL);
        }
        if self.memory != MemoryAccess::Off {
            v.push(MEMORY_TOOL);
        }
        v
    }

    /// Picks from `available` exactly the tools in [`Agent::request_tools`],
    /// in that order. Tools the agent lists but `available` lacks are
    /// returned in the second vector so the caller can report them.
    pub fn select_tools<'t, T>(
        &self,
        available: &'t [T],
        name: impl Fn(&T) -> &str,
    ) -> (Vec<&'t T>, Vec<String>) {
        let mut picked = Vec::new();
        let mut missing = Vec::new();
        for want in self.request_tools() {
            match available.iter().find(|t| name(t) == want) {
                Some(t) => picked.push(t),
                None => missing.push(want.to_owned()),
            }
        }
        (picked, missing)
    }

    /// The JSON Schema for agent files.
    pub fn json_schema() -> serde_json::Value {
        let globs = serde_json::json!({ "type": "array", "items": { "type": "string" } });
        let rules = serde_json::json!({ "oneOf": [ { "type": "string" }, globs ] });
        let count = serde_json::json!({
            "oneOf": [ { "type": "integer", "minimum": 0 }, { "type": "string", "pattern": "^[0-9][0-9_]*$" } ]
        });
        serde_json::json!({
            "$schema": "https://json-schema.org/draft/2020-12/schema",
            "$id": "https://harness.dev/schema/agent.json",
            "title": "harness agent",
            "type": "object",
            "additionalProperties": false,
            "required": ["name"],
            "oneOf": [ { "required": ["prompt"] }, { "required": ["prompt_text"] } ],
            "properties": {
                "name": { "type": "string", "minLength": 1 },
                "display_name": { "type": "string", "minLength": 1, "description": "Human-readable name shown in the TUI and CLI. Defaults to the name with a capital first letter." },
                "description": { "type": "string", "description": "One line shown in the palette and to parents choosing a subagent." },
                "model": { "type": "string", "description": "Alias from [models] or provider/model. Defaults to [general] default_model." },
                "effort": { "enum": ["minimal", "low", "medium", "high", "xhigh", "max", "ultra"],
                            "description": "Reasoning effort. A level the model does not accept is sent as the nearest one it does." },
                "prompt": { "type": "string", "description": "Prompt name from prompts/." },
                "prompt_text": { "type": "string", "description": "Inline system prompt (not templated)." },
                "tools": { "type": "array", "uniqueItems": true, "items": { "anyOf": [
                    { "enum": TOOL_NAMES },
                    { "type": "string", "pattern": "^mcp:[A-Za-z0-9_-]+/[A-Za-z0-9_.*-]+$",
                      "description": "MCP tools: mcp:<server>/<tool>, `*` wildcards allowed; <server> is a [mcp.<server>] in harness.ini." }
                ] } },
                "subagents": { "type": "array", "uniqueItems": true, "items": { "type": "string" },
                               "description": "Enables spawn_agent restricted to these agents." },
                "memory": { "enum": ["off", "read", "read-write"] },
                "mode": { "enum": ["ask", "auto-edit", "auto"] },
                "max_turns": count.clone(),
                "budget": {
                    "type": "object", "additionalProperties": false,
                    "properties": { "usd": { "type": "number", "minimum": 0 }, "tokens": count.clone() }
                },
                "scope": globs,
                "policy": {
                    "type": "object", "additionalProperties": false,
                    "description": "Tighten-only additions to [policy] in harness.ini.",
                    "properties": {
                        "bash.deny": rules.clone(), "bash.ask": rules.clone(),
                        "paths.protected": rules.clone(), "outward.ask": rules
                    }
                },
                "verify": { "type": "string" },
                "runs_on": {
                    "oneOf": [
                        { "type": "string", "description": "`local` or a [remote.<name>] from harness.ini." },
                        {
                            "type": "object", "additionalProperties": false,
                            "properties": {
                                "labels": { "type": "array", "items": { "type": "string" } },
                                "min_mem_gb": count.clone(),
                                "min_cpus": count.clone(),
                                "min_gpus": count.clone(),
                                "fallback": { "type": "string", "description": "`local` or a [remote.<name>] used when no host matches." }
                            }
                        }
                    ]
                },
                "output_schema": { "type": "object", "description": "JSON Schema of the structured result." }
            }
        })
    }
}

fn check_model_syntax(m: &str) -> Result<(), String> {
    if m.trim().is_empty() {
        return Err("`model` must not be empty".into());
    }
    if let Some((p, rest)) = m.split_once('/')
        && (p.is_empty() || rest.is_empty())
    {
        return Err(format!("model `{m}` must be an alias or `provider/model`"));
    }
    Ok(())
}

impl Artifact for Agent {
    const KIND: Kind = Kind::Agent;

    fn load(src: &SourceFile) -> Result<Self, YamlError> {
        let d: AgentDoc = src.parse_yaml(&src.text)?;
        let err = |k: &[&str], m: String| src.error_at(k, m);
        if d.name.trim().is_empty() {
            return Err(err(&["name"], "`name` must not be empty".into()));
        }
        if let Some(m) = &d.model {
            check_model_syntax(m).map_err(|e| err(&["model"], e))?;
        }
        let prompt = match (d.prompt, d.prompt_text) {
            (Some(p), None) => PromptRef::Named(p),
            (None, Some(t)) => PromptRef::Inline(t),
            (Some(_), Some(_)) => {
                return Err(err(
                    &["prompt_text"],
                    "set either `prompt` or `prompt_text`, not both".into(),
                ));
            }
            (None, None) => {
                return Err(err(
                    &["name"],
                    "missing `prompt` (a prompt name) or `prompt_text` (inline text)".into(),
                ));
            }
        };
        let mut seen = BTreeSet::new();
        let mut tools = Vec::new();
        let mut mcp_tools = Vec::new();
        for t in &d.tools {
            if t.starts_with(MCP_TOOL_PREFIX) {
                let sel = McpSelector::parse(t).map_err(|e| err(&["tools"], e))?;
                if !seen.insert(t) {
                    return Err(err(&["tools"], format!("tool `{t}` is listed twice")));
                }
                mcp_tools.push(sel);
                continue;
            }
            let hint = match t.as_str() {
                SPAWN_AGENT_TOOL => " (spawn_agent is enabled by listing `subagents:`)".to_owned(),
                MEMORY_TOOL => {
                    " (the memory tool is enabled by `memory: read` or `memory: read-write`)"
                        .to_owned()
                }
                _ => format!(
                    "; known tools: {}, or mcp:<server>/<tool> for MCP tools",
                    TOOL_NAMES.join(", ")
                ),
            };
            if !TOOL_NAMES.contains(&t.as_str()) {
                return Err(err(&["tools"], format!("unknown tool `{t}`{hint}")));
            }
            if !seen.insert(t) {
                return Err(err(&["tools"], format!("tool `{t}` is listed twice")));
            }
            tools.push(t.clone());
        }
        let mode = match d.mode.as_deref() {
            None => None,
            Some(m) => Some(
                <crate::ini::config::Mode as crate::ini::ConfigEnum>::from_ini(m).ok_or_else(
                    || {
                        err(
                            &["mode"],
                            format!("unknown mode `{m}`; use ask, auto-edit or auto"),
                        )
                    },
                )?,
            ),
        };
        if d.max_turns == Some(0) {
            return Err(err(&["max_turns"], "`max_turns` must be at least 1".into()));
        }
        if let Some(u) = d.budget.usd
            && (!u.is_finite() || u < 0.0)
        {
            return Err(err(
                &["budget", "usd"],
                "`budget.usd` must be a non-negative number".into(),
            ));
        }
        for g in &d.scope {
            globset::Glob::new(g)
                .map_err(|e| err(&["scope"], format!("invalid scope glob `{g}`: {e}")))?;
        }
        let mut policy = PolicyFragment::default();
        for (k, v) in d.policy {
            let v = v.into_vec();
            let slot = match k.as_str() {
                "bash.deny" => &mut policy.bash_deny,
                "bash.ask" => &mut policy.bash_ask,
                "paths.protected" => &mut policy.paths_protected,
                "outward.ask" => &mut policy.outward_ask,
                "bash.allow" => {
                    return Err(err(
                        &["policy", "bash.allow"],
                        "`policy` in agent YAML can only tighten; `bash.allow` would loosen it. \
                         Add allow rules to [policy] in harness.ini instead"
                            .into(),
                    ));
                }
                _ => {
                    return Err(err(
                        &["policy", &k],
                        format!(
                            "unknown policy key `{k}`; agents may set {}",
                            POLICY_KEYS.join(", ")
                        ),
                    ));
                }
            };
            for rule in &v {
                if rule.trim().is_empty() {
                    return Err(err(&["policy", &k], format!("empty rule in `{k}`")));
                }
            }
            *slot = v;
        }
        let runs_on = match d.runs_on {
            None => RunsOn::Local,
            Some(RunsOnDoc::Name(n)) if n == "local" => RunsOn::Local,
            Some(RunsOnDoc::Name(n)) if n.trim().is_empty() => {
                return Err(err(
                    &["runs_on"],
                    "`runs_on` must be `local`, a remote name or `{ labels: [...] }`".into(),
                ));
            }
            Some(RunsOnDoc::Name(n)) => RunsOn::Remote(n),
            Some(RunsOnDoc::Labels(l)) => {
                if l.labels.is_empty()
                    && l.min_mem_gb.is_none()
                    && l.min_cpus.is_none()
                    && l.min_gpus.is_none()
                {
                    return Err(err(
                        &["runs_on"],
                        "`runs_on` needs `labels`, `min_mem_gb`, `min_cpus` or `min_gpus`".into(),
                    ));
                }
                if l.fallback.as_deref().is_some_and(|f| f.trim().is_empty()) {
                    return Err(err(
                        &["runs_on", "fallback"],
                        "`fallback` must be `local` or a remote name".into(),
                    ));
                }
                RunsOn::Labels {
                    labels: l.labels,
                    min_mem_gb: l.min_mem_gb,
                    min_cpus: l.min_cpus,
                    min_gpus: l.min_gpus,
                    fallback: l.fallback,
                }
            }
        };
        if let Some(s) = &d.output_schema
            && !s.is_object()
        {
            return Err(err(
                &["output_schema"],
                "`output_schema` must be a JSON Schema object".into(),
            ));
        }
        if d.display_name
            .as_deref()
            .is_some_and(|n| n.trim().is_empty())
        {
            return Err(err(
                &["display_name"],
                "`display_name` must not be empty; remove it to use the default".into(),
            ));
        }
        Ok(Agent {
            name: d.name,
            display_name: d.display_name,
            description: d.description,
            model: d.model,
            effort: d.effort,
            prompt,
            tools,
            mcp_tools,
            subagents: d.subagents,
            memory: d.memory,
            mode,
            max_turns: d.max_turns,
            budget: d.budget,
            scope: d.scope,
            policy,
            verify: d.verify,
            runs_on,
            output_schema: d.output_schema,
        })
    }

    fn name(&self) -> &str {
        &self.name
    }
}

/// Known model aliases and provider ids, for validating `model:`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ModelRefs {
    /// Aliases (e.g. from `[models]` and the built-in model registry).
    pub aliases: BTreeSet<String>,
    /// Provider ids accepted in `provider/model`.
    pub providers: BTreeSet<String>,
}

impl ModelRefs {
    /// Aliases from `[models]` and all built-in provider ids.
    pub fn from_config(config: &Config) -> Self {
        ModelRefs {
            aliases: config.models.keys().cloned().collect(),
            providers: PROVIDER_IDS.iter().map(|s| (*s).to_owned()).collect(),
        }
    }

    /// Adds more aliases (e.g. built-in ones from the model registry).
    pub fn with_aliases<I: IntoIterator<Item = S>, S: Into<String>>(mut self, aliases: I) -> Self {
        self.aliases.extend(aliases.into_iter().map(Into::into));
        self
    }

    /// Checks a model reference.
    pub fn check(&self, model: &str) -> Result<(), String> {
        check_model_syntax(model)?;
        match model.split_once('/') {
            Some((p, _)) if self.providers.contains(p) => Ok(()),
            Some((p, _)) => Err(format!(
                "unknown provider `{p}` in model `{model}`; known providers: {}",
                self.providers
                    .iter()
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", ")
            )),
            None if self.aliases.contains(model) => Ok(()),
            None => Err(format!(
                "unknown model alias `{model}`; define it in [models] in harness.ini or use provider/model{}",
                if self.aliases.is_empty() {
                    String::new()
                } else {
                    format!(
                        " (known aliases: {})",
                        self.aliases.iter().cloned().collect::<Vec<_>>().join(", ")
                    )
                }
            )),
        }
    }
}

/// What agent references are validated against.
#[derive(Debug, Clone, Default)]
pub struct ValidationContext {
    /// Model aliases and providers.
    pub models: ModelRefs,
    /// Remote name → labels, from `[remote.<name>]`.
    pub remotes: BTreeMap<String, Vec<String>>,
    /// `[budget] run_usd`: no agent budget may exceed it.
    pub max_budget_usd: Option<f64>,
    /// `[mcp.<name>]` servers: the only servers `mcp:` tools may name.
    pub mcp_servers: BTreeSet<String>,
}

impl ValidationContext {
    /// Builds the context from harness.ini.
    pub fn from_config(config: &Config) -> Self {
        ValidationContext {
            models: ModelRefs::from_config(config),
            remotes: config
                .remotes
                .iter()
                .map(|(k, r)| (k.clone(), r.labels.clone()))
                .collect(),
            max_budget_usd: config.budget.run_usd,
            mcp_servers: config.mcp.keys().cloned().collect(),
        }
    }
}

fn entry_error<T>(e: &Entry<T>, key_path: &[&str], message: String) -> YamlError {
    let err = YamlError::at_key(e.origin.path.as_deref(), &e.text, key_path, message);
    if e.origin.path.is_none() {
        err.with_origin(e.origin.display.clone())
    } else {
        err
    }
}

/// Cross-file validation of one agent entry. Returns every problem found.
pub fn validate(
    entry: &Entry<Agent>,
    catalog: &Catalog,
    ctx: &ValidationContext,
) -> Vec<YamlError> {
    let a = &entry.item;
    let mut errs = Vec::new();
    let mut push = |k: &[&str], m: String| errs.push(entry_error(entry, k, m));
    if let Some(m) = &a.model
        && let Err(e) = ctx.models.check(m)
    {
        push(&["model"], e);
    }
    if let PromptRef::Named(p) = &a.prompt
        && catalog.prompts.get(p).is_none()
    {
        push(
            &["prompt"],
            format!("unknown prompt `{p}`; no prompts/*.yaml has `name: {p}`"),
        );
    }
    for sel in &a.mcp_tools {
        if !ctx.mcp_servers.contains(&sel.server) {
            push(
                &["tools"],
                format!(
                    "`{sel}` names MCP server `{}`, which harness.ini does not define; add a [mcp.{}] section (only harness.ini can grant MCP servers)",
                    sel.server, sel.server
                ),
            );
        }
    }
    for s in &a.subagents {
        if catalog.agents.get(s).is_none() {
            push(
                &["subagents"],
                format!("unknown subagent `{s}`; no agents/*.yaml has `name: {s}`"),
            );
        }
    }
    match &a.runs_on {
        RunsOn::Local => {}
        RunsOn::Remote(r) => {
            if !ctx.remotes.contains_key(r) {
                push(
                    &["runs_on"],
                    format!("unknown remote `{r}`; define [remote.{r}] in harness.ini"),
                );
            }
        }
        RunsOn::Labels {
            labels, fallback, ..
        } => {
            if let Some(f) = fallback.as_deref().filter(|f| *f != "local")
                && !ctx.remotes.contains_key(f)
            {
                push(
                    &["runs_on", "fallback"],
                    format!("unknown fallback remote `{f}`; define [remote.{f}] in harness.ini"),
                );
            }
            let any = ctx
                .remotes
                .values()
                .any(|have| labels.iter().all(|l| have.contains(l)));
            // With a fallback the step still runs when no host matches.
            if !any && fallback.is_none() {
                push(
                    &["runs_on"],
                    format!(
                        "no [remote.<name>] in harness.ini has all labels [{}]",
                        labels.join(", ")
                    ),
                );
            }
        }
    }
    if let (Some(cap), Some(usd)) = (ctx.max_budget_usd, a.budget.usd)
        && usd > cap
    {
        push(
            &["budget", "usd"],
            format!(
                "budget.usd {usd} exceeds [budget] run_usd {cap} in harness.ini; agent budgets can only tighten"
            ),
        );
    }
    if entry.origin.scope == Scope::Project
        && let Some(base) = catalog.agents.shadowed_by(&a.name)
    {
        for e in tighten_violations(a, &base.item) {
            push(
                &[e.0],
                format!(
                    "{} (project agents may only tighten `{}` from {})",
                    e.1, a.name, base.origin
                ),
            );
        }
    }
    errs
}

/// Ways in which `agent` is looser than `base`, as `(key, message)`.
pub fn tighten_violations(agent: &Agent, base: &Agent) -> Vec<(&'static str, String)> {
    let mut v = Vec::new();
    let extra: Vec<&String> = agent
        .tools
        .iter()
        .filter(|t| !base.tools.contains(t))
        .collect();
    if !extra.is_empty() {
        v.push(("tools", format!("adds tools {extra:?}")));
    }
    let extra: Vec<String> = agent
        .mcp_tools
        .iter()
        .filter(|t| !base.mcp_tools.contains(t))
        .map(ToString::to_string)
        .collect();
    if !extra.is_empty() {
        v.push(("tools", format!("adds MCP tools {extra:?}")));
    }
    let extra: Vec<&String> = agent
        .subagents
        .iter()
        .filter(|t| !base.subagents.contains(t))
        .collect();
    if !extra.is_empty() {
        v.push(("subagents", format!("adds subagents {extra:?}")));
    }
    if agent.memory > base.memory {
        v.push((
            "memory",
            format!(
                "raises memory access from {:?} to {:?}",
                base.memory, agent.memory
            ),
        ));
    }
    // Strictness order: ask < auto-edit < auto.
    let rank = |m: Option<crate::ini::config::Mode>| {
        m.map(|m| {
            <crate::ini::config::Mode as crate::ini::ConfigEnum>::VARIANTS
                .iter()
                .position(|v| *v == crate::ini::ConfigEnum::as_ini(&m))
                .unwrap_or(0)
        })
    };
    if let Some(b) = rank(base.mode)
        && rank(agent.mode).is_none_or(|a| a > b)
    {
        v.push(("mode", "loosens the agent's mode".to_owned()));
    }
    if let Some(b) = base.max_turns
        && agent.max_turns.is_none_or(|m| m > b)
    {
        v.push(("max_turns", format!("raises max_turns above {b}")));
    }
    if let Some(b) = base.budget.usd
        && agent.budget.usd.is_none_or(|u| u > b)
    {
        v.push(("budget", format!("raises budget.usd above {b}")));
    }
    if let Some(b) = base.budget.tokens
        && agent.budget.tokens.is_none_or(|u| u > b)
    {
        v.push(("budget", format!("raises budget.tokens above {b}")));
    }
    if !base.scope.is_empty()
        && let Some(wide) = scope_widening(&agent.scope, &base.scope)
    {
        v.push(("scope", wide));
    }
    let missing = agent.policy.missing_from(&base.policy);
    if !missing.is_empty() {
        v.push((
            "policy",
            format!("drops policy rules [{}]", missing.join(", ")),
        ));
    }
    v
}

/// `None` if every pattern in `scope` lies within `base` (checked by
/// matching the pattern text against `base`'s globs), else a message.
fn scope_widening(scope: &[String], base: &[String]) -> Option<String> {
    if scope.is_empty() {
        return Some(format!("removes the scope restriction {base:?}"));
    }
    let mut b = globset::GlobSetBuilder::new();
    for g in base {
        b.add(globset::Glob::new(g).ok()?);
    }
    let set = b.build().ok()?;
    let wider: Vec<&String> = scope
        .iter()
        .filter(|p| !base.contains(p) && !set.is_match(p.as_str()))
        .collect();
    (!wider.is_empty()).then(|| format!("widens scope with {wider:?} beyond {base:?}"))
}

#[cfg(test)]
mod tests;
