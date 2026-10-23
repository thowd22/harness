//! The command registry.

use std::collections::BTreeMap;

use harness_config::discovery::{Catalog, Scope};
use serde_json::Value;

use super::builtin::Builtin;
use super::parse::{self, ArgKind, ArgSpec, CommandError, Invocation};

/// Where a command comes from. Earlier kinds win a bare name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CommandKind {
    /// Built into harness.
    Builtin(Builtin),
    /// A workflow (`command:` of `workflows/*.yaml`).
    Workflow,
    /// A skill (`skills/*.md`): the body is injected as a user turn.
    Skill,
    /// Switches the session to an agent.
    Agent,
}

impl CommandKind {
    /// `builtin`, `workflow`, `skill` or `agent`.
    pub fn label(self) -> &'static str {
        match self {
            Self::Builtin(_) => "builtin",
            Self::Workflow => "workflow",
            Self::Skill => "skill",
            Self::Agent => "agent",
        }
    }

    fn rank(self) -> u8 {
        match self {
            Self::Builtin(_) => 0,
            Self::Workflow => 1,
            Self::Skill => 2,
            Self::Agent => 3,
        }
    }
}

/// A slash command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandSpec {
    /// Name without `/` (the bare name, even when reached by an alias).
    pub name: String,
    /// One-line description.
    pub description: String,
    /// Kind.
    pub kind: CommandKind,
    /// Config scope of an authored command (`None` for built-ins).
    pub scope: Option<Scope>,
    /// Arguments.
    pub args: Vec<ArgSpec>,
    /// Refuse unknown keys and extra words. Skills are lenient: their whole
    /// argument string goes to `$ARGUMENTS`.
    pub strict: bool,
    /// The artifact name (workflow/skill/agent name; differs from `name`
    /// for workflows with a `command:`).
    pub target: String,
}

impl CommandSpec {
    /// The usage line, e.g. `/mode [ask|auto-edit|auto]`.
    pub fn usage(&self) -> String {
        let mut out = format!("/{}", self.name);
        for a in &self.args {
            out.push(' ');
            out.push_str(&a.usage());
        }
        out
    }

    /// The argument hint (usage without the name), `None` without
    /// arguments. This is what completion and the palette show.
    pub fn arg_hint(&self) -> Option<String> {
        if self.args.is_empty() {
            return None;
        }
        Some(
            self.args
                .iter()
                .map(ArgSpec::usage)
                .collect::<Vec<_>>()
                .join(" "),
        )
    }

    /// One help line per argument.
    pub fn arg_help(&self) -> Vec<String> {
        self.args.iter().map(ArgSpec::help_line).collect()
    }

    /// Scope label for display: `builtin`, `global` or `project`.
    pub fn scope_label(&self) -> &'static str {
        self.scope.map_or("builtin", Scope::label)
    }

    fn builtin(b: Builtin) -> Self {
        Self {
            name: b.name().to_owned(),
            description: b.description().to_owned(),
            kind: CommandKind::Builtin(b),
            scope: None,
            args: b.args(),
            strict: true,
            target: b.name().to_owned(),
        }
    }
}

/// A name claimed by several commands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Collision {
    /// The bare name.
    pub name: String,
    /// Who gets the bare name (`kind`/`scope`).
    pub winner: String,
    /// Qualified names that reach every contender.
    pub reachable_as: Vec<String>,
}

/// Every slash command, by invocation name.
#[derive(Debug, Clone, PartialEq)]
pub struct CommandRegistry {
    by_name: BTreeMap<String, CommandSpec>,
    collisions: Vec<Collision>,
}

impl Default for CommandRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// Workflow arguments from its `inputs:` map.
fn workflow_args(document: &Value) -> Vec<ArgSpec> {
    let Some(inputs) = document.get("inputs").and_then(Value::as_object) else {
        return Vec::new();
    };
    let mut positional: Vec<(u64, ArgSpec)> = Vec::new();
    let mut named = Vec::new();
    for (name, def) in inputs {
        let ty = def.get("type").and_then(Value::as_str);
        let mut spec = ArgSpec {
            name: name.clone(),
            required: def
                .get("required")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            help: def
                .get("description")
                .and_then(Value::as_str)
                .map(str::to_owned)
                .or_else(|| ty.map(|t| format!("{t} input"))),
            kind: ArgKind::Named,
            choices: def
                .get("enum")
                .and_then(Value::as_array)
                .map(|v| {
                    v.iter()
                        .filter_map(Value::as_str)
                        .map(str::to_owned)
                        .collect()
                })
                .unwrap_or_default(),
            default: def.get("default").map(|d| match d {
                Value::String(s) => s.clone(),
                Value::Array(items) => items
                    .iter()
                    .map(|i| i.as_str().map_or_else(|| i.to_string(), str::to_owned))
                    .collect::<Vec<_>>()
                    .join(","),
                other => other.to_string(),
            }),
        };
        if spec.default.is_some() {
            spec.required = false;
        }
        match def.get("positional").and_then(Value::as_u64) {
            Some(i) => {
                spec.kind = ArgKind::Positional;
                positional.push((i, spec));
            }
            None => named.push(spec),
        }
    }
    positional.sort_by_key(|(i, _)| *i);
    positional
        .into_iter()
        .map(|(_, s)| s)
        .chain(named)
        .collect()
}

/// Skill arguments from its `args:` hint (`<a> [b]`); always lenient with
/// a trailing rest argument.
fn skill_args(hint: Option<&str>) -> Vec<ArgSpec> {
    let mut out = Vec::new();
    for word in hint.unwrap_or("").split_whitespace() {
        let (inner, required) = if let Some(w) = word.strip_prefix('<') {
            (w.trim_end_matches('>'), true)
        } else {
            (word.trim_start_matches('[').trim_end_matches(']'), false)
        };
        let inner = inner.trim_end_matches("...");
        if inner.is_empty() {
            continue;
        }
        out.push(if required {
            ArgSpec::required(inner)
        } else {
            ArgSpec::optional(inner)
        });
    }
    out
}

impl CommandRegistry {
    /// Built-ins only.
    pub fn new() -> Self {
        let mut reg = Self {
            by_name: BTreeMap::new(),
            collisions: Vec::new(),
        };
        reg.build(Vec::new());
        reg
    }

    /// Built-ins plus every workflow, skill and agent of `catalog`.
    pub fn from_catalog(catalog: &Catalog) -> Self {
        let mut reg = Self::new();
        reg.reload(catalog);
        reg
    }

    /// Rebuilds from a (hot-reloaded) catalog. True when anything changed.
    pub fn reload(&mut self, catalog: &Catalog) -> bool {
        let before = (self.by_name.clone(), self.collisions.clone());
        let mut authored = Vec::new();
        for e in catalog.workflows.all() {
            authored.push(CommandSpec {
                name: e.item.command.clone(),
                description: e
                    .item
                    .description
                    .clone()
                    .unwrap_or_else(|| format!("Run the {} workflow", e.item.name)),
                kind: CommandKind::Workflow,
                scope: Some(e.origin.scope),
                args: workflow_args(&e.item.document),
                strict: true,
                target: e.item.name.clone(),
            });
        }
        for e in catalog.skills.all() {
            let mut args = skill_args(e.item.args.as_deref());
            args.push(ArgSpec::rest("arguments").with_help("passed to the skill as $ARGUMENTS"));
            authored.push(CommandSpec {
                name: e.item.name.clone(),
                description: e.item.description.clone(),
                kind: CommandKind::Skill,
                scope: Some(e.origin.scope),
                args,
                strict: false,
                target: e.item.name.clone(),
            });
        }
        for e in catalog.agents.all() {
            // People see the display name and one-line summary; the
            // command itself stays the agent's name.
            let shown = e.item.display_name();
            let description = if e.item.description.is_some() {
                format!("Switch to {shown}: {}", e.item.summary())
            } else {
                format!("Switch to {shown}")
            };
            authored.push(CommandSpec {
                name: e.item.name.clone(),
                description,
                kind: CommandKind::Agent,
                scope: Some(e.origin.scope),
                args: Vec::new(),
                strict: true,
                target: e.item.name.clone(),
            });
        }
        self.build(authored);
        (self.by_name.clone(), self.collisions.clone()) != before
    }

    fn build(&mut self, authored: Vec<CommandSpec>) {
        let mut groups: BTreeMap<String, Vec<CommandSpec>> = BTreeMap::new();
        for b in Builtin::ALL {
            groups
                .entry(b.name().to_owned())
                .or_default()
                .push(CommandSpec::builtin(b));
        }
        for spec in authored {
            groups.entry(spec.name.clone()).or_default().push(spec);
        }
        let mut by_name = BTreeMap::new();
        let mut collisions = Vec::new();
        for (name, mut specs) in groups {
            // Winner: kind rank, then the highest scope.
            specs.sort_by(|a, b| {
                a.kind
                    .rank()
                    .cmp(&b.kind.rank())
                    .then(b.scope.cmp(&a.scope))
            });
            if specs.len() > 1 {
                let mut reachable = Vec::new();
                for (i, s) in specs.iter().enumerate() {
                    let Some(scope) = s.scope else {
                        continue;
                    };
                    let same_scope_earlier = specs[..i].iter().any(|o| o.scope == Some(scope));
                    let alias = if same_scope_earlier {
                        format!("{}:{name}", s.kind.label())
                    } else {
                        format!("{}:{name}", scope.label())
                    };
                    by_name.entry(alias.clone()).or_insert_with(|| s.clone());
                    reachable.push(alias);
                }
                let w = &specs[0];
                collisions.push(Collision {
                    name: name.clone(),
                    winner: format!("{} ({})", w.kind.label(), w.scope_label()),
                    reachable_as: reachable,
                });
            }
            by_name.insert(name, specs.swap_remove(0));
        }
        self.by_name = by_name;
        self.collisions = collisions;
    }

    /// The command reached by `name` (bare or qualified, without `/`).
    pub fn get(&self, name: &str) -> Option<&CommandSpec> {
        self.by_name.get(name)
    }

    /// Every invocation name with its command, sorted by name.
    pub fn entries(&self) -> impl Iterator<Item = (&str, &CommandSpec)> {
        self.by_name.iter().map(|(k, v)| (k.as_str(), v))
    }

    /// Commands reached by a bare name (one per command name), sorted.
    pub fn commands(&self) -> Vec<&CommandSpec> {
        self.by_name
            .iter()
            .filter(|(k, _)| !k.contains(':'))
            .map(|(_, v)| v)
            .collect()
    }

    /// Names that more than one command claims (for `harness doctor`).
    pub fn collisions(&self) -> &[Collision] {
        &self.collisions
    }

    /// Invocation names starting with `prefix` (for completion).
    pub fn completions(&self, prefix: &str) -> Vec<(&str, &CommandSpec)> {
        self.entries()
            .filter(|(k, _)| k.starts_with(prefix))
            .collect()
    }

    /// Parses a `/name args` line.
    pub fn parse(&self, line: &str) -> Result<Invocation, CommandError> {
        let (name, raw) = parse::split_line(line).ok_or(CommandError::NotACommand)?;
        let Some(spec) = self.get(name) else {
            let names: Vec<&str> = self.by_name.keys().map(String::as_str).collect();
            return Err(CommandError::Unknown {
                name: name.to_owned(),
                suggestion: crate::robustness::closest(name, &names).map(str::to_owned),
            });
        };
        parse::bind(name, spec, raw)
    }

    /// The `/help` text: one line per command, or the details of one.
    pub fn help(&self, command: Option<&str>) -> Vec<String> {
        if let Some(name) = command {
            let name = name.trim_start_matches('/');
            return match self.get(name) {
                Some(spec) => {
                    let mut out = vec![
                        format!("{} — {}", spec.usage(), spec.description),
                        format!("source: {} ({})", spec.kind.label(), spec.scope_label()),
                    ];
                    out.extend(spec.arg_help().into_iter().map(|l| format!("  {l}")));
                    out
                }
                None => vec![format!("unknown command `/{name}`")],
            };
        }
        self.commands()
            .into_iter()
            .map(|s| format!("{:<28} {}", s.usage(), s.description))
            .collect()
    }
}
