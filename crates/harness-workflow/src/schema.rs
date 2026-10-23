//! Workflow YAML schema: parsing and load-time validation.
//!
//! A workflow is a GitHub-Actions-like DAG of steps (doc-3). [`parse`]
//! turns YAML text into a [`Workflow`] and rejects, with `file:line:col`
//! errors:
//!
//! - unknown keys, wrong types, invalid step types and keys that do not
//!   belong to the step's type;
//! - duplicate or malformed step ids, unknown `needs`, cycles (reported
//!   with the cycle path), a `loop.retry_step` that is not upstream;
//! - `tasks` that point at unknown steps;
//! - template references that do not resolve ([`crate::template`]);
//! - with a [`CheckContext`]: unknown agents, unknown remote hosts in
//!   `runs_on`, and unknown nested workflows in `uses`.
//!
//! Step types: `agent` (default when `agent:` is present), `shell`
//! (`run:`), `verify` (`verify:`), `bugfix-proof` (`test:`), `approval`
//! (`message:`) and `workflow` (`uses:`).

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

use harness_config::discovery::{Entry, RawWorkflow};
use harness_config::ini::parse_duration;
use harness_config::yaml::{self, YamlError};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::template::{self, RefScope, StepRefs};

/// Default `loop.max_iterations`.
pub const DEFAULT_MAX_ITERATIONS: u32 = 3;
/// Default `budget.warn_at` (fraction of the limit).
pub const DEFAULT_WARN_AT: f64 = 0.8;
/// Step types accepted in `type:`.
pub const STEP_TYPES: &[&str] = &[
    "agent",
    "shell",
    "verify",
    "bugfix-proof",
    "approval",
    "workflow",
    "dream",
];

/// Outputs every dream step has.
pub const DREAM_OUTPUTS: &[&str] = &["summary", "merged", "pruned", "added", "commit"];

/// Outputs every shell step has.
pub const SHELL_OUTPUTS: &[&str] = &["stdout", "exit_code"];
/// Outputs every verify step has.
pub const VERIFY_OUTPUTS: &[&str] = &["passed", "exit_code", "output"];
/// Outputs every bugfix-proof step has.
pub const PROOF_OUTPUTS: &[&str] = &["proven", "phases"];
/// Outputs every approval step has.
pub const APPROVAL_OUTPUTS: &[&str] = &["approved", "by", "reason"];
/// Outputs every agent step has.
pub const AGENT_OUTPUTS: &[&str] = &["summary"];

/// Type of a workflow input.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InputType {
    /// Text.
    #[default]
    String,
    /// List of strings.
    List,
    /// Number.
    Number,
    /// Boolean.
    Bool,
}

impl InputType {
    /// The YAML name.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::String => "string",
            Self::List => "list",
            Self::Number => "number",
            Self::Bool => "bool",
        }
    }

    /// True when `v` has this type.
    pub fn accepts(self, v: &Value) -> bool {
        match self {
            Self::String => v.is_string(),
            Self::List => v.as_array().is_some_and(|a| a.iter().all(Value::is_string)),
            Self::Number => v.is_number(),
            Self::Bool => v.is_boolean(),
        }
    }
}

/// One declared input.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Input {
    /// Name.
    pub name: String,
    /// Type.
    pub kind: InputType,
    /// Must be given (and has no default).
    pub required: bool,
    /// Default value.
    pub default: Option<Value>,
    /// Position when given as a slash-command argument.
    pub positional: Option<usize>,
    /// Help text.
    pub description: Option<String>,
}

/// What happens when a budget runs out.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OnExceed {
    /// Stop the workflow or step.
    #[default]
    Stop,
    /// Only warn.
    Warn,
}

/// A spend cap.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct Budget {
    /// USD cap.
    pub usd: Option<f64>,
    /// Token cap.
    pub tokens: Option<u64>,
    /// Hard stop or warning at the cap.
    pub on_exceed: OnExceed,
    /// Fraction of the cap at which a soft warning is emitted.
    pub warn_at: f64,
}

/// A goal-bar task declared by the workflow.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TaskSpec {
    /// Id.
    pub id: String,
    /// Title.
    pub title: String,
    /// Linked step (status and owner follow it).
    pub step: Option<String>,
}

/// `verify.expect`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Expect {
    /// The command must pass.
    #[default]
    Pass,
    /// The command must fail (fail-before proof).
    Fail,
}

/// What a step does.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum StepKind {
    /// Runs an agent.
    Agent {
        /// Agent name.
        agent: String,
        /// Model override.
        model: Option<String>,
        /// Prompt template (the brief).
        prompt: Option<String>,
        /// MCP tools (`tools: [mcp:<server>/<tool>]`) added to the
        /// agent's own for this step.
        #[serde(skip_serializing_if = "Vec::is_empty")]
        tools: Vec<harness_config::mcp_tools::McpSelector>,
    },
    /// Runs a shell command.
    Shell {
        /// Command template.
        run: String,
    },
    /// Runs a verify command.
    Verify {
        /// Command template.
        command: String,
        /// Pass or fail expected.
        expect: Expect,
    },
    /// Fail-before / pass-after bug-fix proof.
    BugfixProof {
        /// Test command template.
        test: String,
        /// Paths kept when the fix is reverted (default `tests/**`).
        keep: Vec<String>,
    },
    /// Waits for a human decision.
    Approval {
        /// Question template.
        message: String,
    },
    /// Runs another workflow.
    Workflow {
        /// Workflow name.
        uses: String,
        /// Inputs (templated).
        with: serde_json::Map<String, Value>,
    },
    /// Consolidates long-term memory (`harness_memory::dream`).
    Dream {
        /// Pass settings (`grace_days`, `max_new_lessons`, ...).
        with: serde_json::Map<String, Value>,
    },
}

impl StepKind {
    /// The `type:` name.
    pub fn type_name(&self) -> &'static str {
        match self {
            Self::Agent { .. } => "agent",
            Self::Shell { .. } => "shell",
            Self::Verify { .. } => "verify",
            Self::BugfixProof { .. } => "bugfix-proof",
            Self::Approval { .. } => "approval",
            Self::Workflow { .. } => "workflow",
            Self::Dream { .. } => "dream",
        }
    }
}

/// Matrix or `for_each` expansion.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Expand {
    /// One instance per element (`item`). A list or a `{{ expr }}`.
    ForEach(Value),
    /// One instance per combination (`matrix.<key>`).
    Matrix(BTreeMap<String, Value>),
}

/// `loop:`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LoopSpec {
    /// Condition that ends the loop (evaluated after the step).
    pub until: String,
    /// Maximum runs of the step.
    pub max_iterations: u32,
    /// Re-run from this upstream step on each iteration.
    pub retry_step: Option<String>,
}

/// `on_failure:`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OnFailure {
    /// Fail the workflow; steps not started are skipped.
    #[default]
    Abort,
    /// Record the failure and keep going; dependents still run.
    Continue,
}

/// `scope:` of a step.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(untagged)]
pub enum ScopeSpec {
    /// Literal globs.
    Globs(Vec<String>),
    /// A template that renders to a list (e.g. `{{ inputs.scope }}`).
    Template(String),
}

/// Where a remote-capable step falls back to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Fallback {
    /// Run locally.
    Local,
    /// Run on this host.
    Host(String),
}

/// A host selector.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct HostSelector {
    /// Labels the host must have.
    pub labels: Vec<String>,
    /// Minimum memory in GB.
    pub min_mem_gb: Option<u32>,
    /// Minimum logical CPUs.
    pub min_cpus: Option<u32>,
    /// Minimum GPUs.
    pub min_gpus: Option<u32>,
    /// Used when no host matches.
    pub fallback: Option<Fallback>,
}

/// `runs_on:`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RunsOn {
    /// This machine.
    #[default]
    Local,
    /// A named `[remote.<name>]`.
    Host(String),
    /// Any host matching the selector.
    Select(HostSelector),
}

/// One step.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Step {
    /// Id (unique, `[A-Za-z0-9_-]`).
    pub id: String,
    /// What it does.
    pub kind: StepKind,
    /// Steps that must finish first.
    pub needs: Vec<String>,
    /// Condition (`if:`).
    pub condition: Option<String>,
    /// Matrix / for_each.
    pub expand: Option<Expand>,
    /// Loop.
    pub loop_spec: Option<LoopSpec>,
    /// Extra attempts after a failure.
    pub retries: u32,
    /// Time limit per attempt.
    pub timeout: Option<Duration>,
    /// Failure handling.
    pub on_failure: OnFailure,
    /// Placement.
    pub runs_on: Option<RunsOn>,
    /// Edit scope.
    pub scope: Option<ScopeSpec>,
    /// Step budget.
    pub budget: Option<Budget>,
    /// Declared outputs.
    pub outputs: Vec<String>,
    /// Declared artifacts (approval steps: artifacts to review).
    pub artifacts: Vec<String>,
}

impl Step {
    /// The names templates may read from `steps.<id>.outputs`; `None`
    /// means any (agents without declared outputs, nested workflows).
    pub fn output_names(&self) -> Option<BTreeSet<String>> {
        let builtin: &[&str] = match &self.kind {
            StepKind::Agent { .. } if self.outputs.is_empty() => return None,
            StepKind::Workflow { .. } => return None,
            StepKind::Agent { .. } => AGENT_OUTPUTS,
            StepKind::Shell { .. } => SHELL_OUTPUTS,
            StepKind::Verify { .. } => VERIFY_OUTPUTS,
            StepKind::BugfixProof { .. } => PROOF_OUTPUTS,
            StepKind::Approval { .. } => APPROVAL_OUTPUTS,
            StepKind::Dream { .. } => DREAM_OUTPUTS,
        };
        Some(
            builtin
                .iter()
                .map(|s| (*s).to_owned())
                .chain(self.outputs.iter().cloned())
                .collect(),
        )
    }

    /// Artifacts this step produces (approval steps only review).
    pub fn produced_artifacts(&self) -> &[String] {
        match self.kind {
            StepKind::Approval { .. } => &[],
            _ => &self.artifacts,
        }
    }
}

/// A parsed, validated workflow.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Workflow {
    /// Name.
    pub name: String,
    /// Description.
    pub description: Option<String>,
    /// Slash command (defaults to name).
    pub command: String,
    /// Inputs in positional order, then by name.
    pub inputs: Vec<Input>,
    /// Workflow budget.
    pub budget: Option<Budget>,
    /// Goal template.
    pub goal: Option<String>,
    /// Goal-bar tasks.
    pub tasks: Vec<TaskSpec>,
    /// The most memory access any agent step gets (`memory: off` turns
    /// memory off for the whole workflow); `None` leaves it to each agent.
    pub memory: Option<harness_config::agents::MemoryAccess>,
    /// Steps in file order.
    pub steps: Vec<Step>,
    /// Source file.
    #[serde(skip)]
    pub source: Option<PathBuf>,
    #[serde(skip)]
    ancestors: BTreeMap<String, BTreeSet<String>>,
    #[serde(skip)]
    order: Vec<usize>,
}

impl Workflow {
    /// A step by id.
    pub fn step(&self, id: &str) -> Option<&Step> {
        self.steps.iter().find(|s| s.id == id)
    }

    /// An input by name.
    pub fn input(&self, name: &str) -> Option<&Input> {
        self.inputs.iter().find(|i| i.name == name)
    }

    /// Steps in a dependency order (needs first, file order otherwise).
    pub fn topo_order(&self) -> impl Iterator<Item = &Step> {
        self.order.iter().map(|&i| &self.steps[i])
    }

    /// Every step `id` depends on, directly or not.
    pub fn upstream(&self, id: &str) -> BTreeSet<String> {
        self.ancestors.get(id).cloned().unwrap_or_default()
    }

    /// Every step that depends on `id`, directly or not.
    pub fn downstream(&self, id: &str) -> BTreeSet<String> {
        self.ancestors
            .iter()
            .filter(|(_, a)| a.contains(id))
            .map(|(s, _)| s.clone())
            .collect()
    }

    /// The steps a loop on `step` re-runs when it retries from `from`:
    /// `from`, the steps between them, and `step` itself.
    pub fn loop_chain(&self, from: &str, step: &str) -> BTreeSet<String> {
        let up = self.upstream(step);
        let mut chain: BTreeSet<String> = self
            .downstream(from)
            .into_iter()
            .filter(|d| up.contains(d))
            .collect();
        chain.insert(from.to_owned());
        chain.insert(step.to_owned());
        chain
    }

    /// Nested workflow names used by this workflow.
    pub fn uses(&self) -> Vec<&str> {
        self.steps
            .iter()
            .filter_map(|s| match &s.kind {
                StepKind::Workflow { uses, .. } => Some(uses.as_str()),
                _ => None,
            })
            .collect()
    }
}

/// Names outside the workflow file that it may refer to. `None` fields are
/// not checked.
#[derive(Debug, Clone, Default)]
pub struct CheckContext {
    /// Known agent names.
    pub agents: Option<BTreeSet<String>>,
    /// `[remote.<name>]` hosts and their labels.
    pub remotes: Option<BTreeMap<String, Vec<String>>>,
    /// Known workflow names.
    pub workflows: Option<BTreeSet<String>>,
    /// Agents that declare a non-local `runs_on`: an agent step without
    /// its own `runs_on` is placed like its agent.
    pub agent_runs_on: BTreeMap<String, RunsOn>,
    /// `[mcp.<name>]` servers, the only ones an agent step's `tools:` may
    /// name.
    pub mcp_servers: Option<BTreeSet<String>>,
}

impl CheckContext {
    /// Names from a discovered catalog and the ini remotes.
    pub fn from_catalog(
        catalog: &harness_config::discovery::Catalog,
        cfg: Option<&harness_config::ini::Config>,
    ) -> Self {
        Self {
            agents: Some(
                catalog
                    .agents
                    .active()
                    .iter()
                    .map(|e| e.item.name.clone())
                    .collect(),
            ),
            remotes: cfg.map(|c| {
                c.remotes
                    .iter()
                    .map(|(n, r)| (n.clone(), r.labels.clone()))
                    .collect()
            }),
            workflows: Some(
                catalog
                    .workflows
                    .active()
                    .iter()
                    .map(|e| e.item.name.clone())
                    .collect(),
            ),
            agent_runs_on: catalog
                .agents
                .active()
                .iter()
                .filter(|e| e.item.runs_on != harness_config::agents::RunsOn::Local)
                .map(|e| {
                    (
                        e.item.name.clone(),
                        crate::placement::from_agent(&e.item.runs_on),
                    )
                })
                .collect(),
            mcp_servers: cfg.map(|c| c.mcp.keys().cloned().collect()),
        }
    }
}

// ---------------------------------------------------------------- docs

#[derive(Deserialize)]
#[serde(untagged)]
enum OneOrMany {
    One(String),
    Many(Vec<String>),
}

impl OneOrMany {
    fn into_vec(self) -> Vec<String> {
        match self {
            Self::One(s) => vec![s],
            Self::Many(v) => v,
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkflowDoc {
    name: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    command: Option<String>,
    #[serde(default)]
    inputs: BTreeMap<String, InputDoc>,
    #[serde(default)]
    budget: Option<BudgetDoc>,
    #[serde(default)]
    goal: Option<String>,
    #[serde(default)]
    tasks: Vec<TaskDoc>,
    #[serde(default)]
    memory: Option<harness_config::agents::MemoryAccess>,
    steps: Vec<StepDoc>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct InputDoc {
    #[serde(rename = "type", default)]
    kind: InputType,
    #[serde(default)]
    required: bool,
    #[serde(default)]
    default: Option<Value>,
    #[serde(default)]
    positional: Option<usize>,
    #[serde(default)]
    description: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BudgetDoc {
    #[serde(default)]
    usd: Option<f64>,
    #[serde(
        default,
        deserialize_with = "harness_config::yaml::de::lenient_opt_u64"
    )]
    tokens: Option<u64>,
    #[serde(default)]
    on_exceed: Option<OnExceed>,
    #[serde(default)]
    warn_at: Option<f64>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TaskDoc {
    id: String,
    title: String,
    #[serde(default)]
    step: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LoopDoc {
    until: String,
    #[serde(default)]
    max_iterations: Option<u32>,
    #[serde(default)]
    retry_step: Option<String>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum VerifyDoc {
    Command(String),
    Full(VerifyFull),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct VerifyFull {
    command: String,
    #[serde(default)]
    expect: Expect,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum Seconds {
    Int(u64),
    Text(String),
}

#[derive(Deserialize)]
#[serde(untagged)]
enum RunsOnDoc {
    Name(String),
    Select(SelectDoc),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SelectDoc {
    #[serde(default)]
    labels: Vec<String>,
    #[serde(default)]
    min_mem_gb: Option<u32>,
    #[serde(default)]
    min_cpus: Option<u32>,
    #[serde(default)]
    min_gpus: Option<u32>,
    #[serde(default)]
    fallback: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StepDoc {
    id: String,
    #[serde(rename = "type", default)]
    kind: Option<String>,
    #[serde(default)]
    needs: Option<OneOrMany>,
    #[serde(rename = "if", default)]
    condition: Option<String>,
    #[serde(default)]
    for_each: Option<Value>,
    #[serde(default)]
    matrix: Option<BTreeMap<String, Value>>,
    #[serde(rename = "loop", default)]
    loop_spec: Option<LoopDoc>,
    #[serde(default)]
    retries: Option<u32>,
    #[serde(default)]
    timeout: Option<Seconds>,
    #[serde(default)]
    on_failure: Option<OnFailure>,
    #[serde(default)]
    runs_on: Option<RunsOnDoc>,
    #[serde(default)]
    scope: Option<OneOrMany>,
    #[serde(default)]
    budget: Option<BudgetDoc>,
    #[serde(default)]
    outputs: Vec<String>,
    #[serde(default)]
    artifacts: Vec<String>,
    #[serde(default)]
    agent: Option<String>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    prompt: Option<String>,
    #[serde(default)]
    tools: Option<Vec<String>>,
    #[serde(default)]
    run: Option<String>,
    #[serde(default)]
    verify: Option<VerifyDoc>,
    #[serde(default)]
    test: Option<String>,
    #[serde(default)]
    keep: Option<Vec<String>>,
    #[serde(default)]
    message: Option<String>,
    #[serde(default)]
    uses: Option<String>,
    #[serde(default)]
    with: Option<serde_json::Map<String, Value>>,
}

// ---------------------------------------------------------------- errors

struct Errs<'a> {
    file: Option<&'a Path>,
    text: &'a str,
    list: Vec<YamlError>,
}

impl<'a> Errs<'a> {
    fn top(&mut self, key: &str, msg: impl Into<String>) {
        let (line, col) = yaml::locate_key(self.text, &[key]).unwrap_or((0, 0));
        self.list.push(YamlError::at(self.file, line, col, msg));
    }

    fn step(&mut self, id: &str, key: Option<&str>, msg: impl Into<String>) {
        let (line, col) = locate_step(self.text, id, key);
        self.list.push(YamlError::at(
            self.file,
            line,
            col,
            format!("step `{id}`: {}", msg.into()),
        ));
    }
}

/// Line and column (1-based) of step `id`'s `key` (or its `id:` line).
fn locate_step(text: &str, id: &str, key: Option<&str>) -> (usize, usize) {
    let lines: Vec<&str> = text.lines().collect();
    let is_id_line = |l: &str| {
        let t = l.trim_start().trim_start_matches("- ").trim_start();
        t.strip_prefix("id:")
            .map(|v| v.trim().trim_matches(['"', '\'']) == id)
            .unwrap_or(false)
            || l.contains(&format!("id: {id},"))
            || l.contains(&format!("id: {id} "))
    };
    let Some(start) = lines.iter().position(|l| is_id_line(l)) else {
        return yaml::locate_key(text, &["steps"]).unwrap_or((0, 0));
    };
    let id_col = lines[start].find("id:").map_or(1, |c| c + 1);
    if let Some(key) = key {
        let pat = format!("{key}:");
        for (i, l) in lines.iter().enumerate().skip(start) {
            if i > start && l.trim_start().starts_with("- id:") {
                break;
            }
            if let Some(c) = l.find(&pat) {
                let before = &l[..c];
                if before.trim().is_empty()
                    || before.trim_end().ends_with('-')
                    || before.trim_end().ends_with('{')
                    || before.trim_end().ends_with(',')
                {
                    return (i + 1, c + 1);
                }
            }
        }
    }
    (start + 1, id_col)
}

fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

fn budget(doc: BudgetDoc, errs: &mut Vec<String>) -> Budget {
    let warn_at = doc.warn_at.unwrap_or(DEFAULT_WARN_AT);
    if !(0.0..=1.0).contains(&warn_at) {
        errs.push(format!(
            "budget.warn_at must be between 0 and 1, got {warn_at}"
        ));
    }
    if doc.usd.is_some_and(|u| u < 0.0) {
        errs.push("budget.usd must not be negative".into());
    }
    if doc.usd.is_none() && doc.tokens.is_none() {
        errs.push("budget needs `usd` or `tokens`".into());
    }
    Budget {
        usd: doc.usd,
        tokens: doc.tokens,
        on_exceed: doc.on_exceed.unwrap_or_default(),
        warn_at,
    }
}

// ---------------------------------------------------------------- parse

/// Parses and validates workflow YAML. `file` is used in error locations.
pub fn parse(text: &str, file: Option<&Path>) -> Result<Workflow, Vec<YamlError>> {
    parse_with(text, file, &CheckContext::default())
}

/// Parses a discovered workflow entry.
pub fn from_entry(
    entry: &Entry<RawWorkflow>,
    ctx: &CheckContext,
) -> Result<Workflow, Vec<YamlError>> {
    parse_with(&entry.text, entry.origin.path.as_deref(), ctx).map_err(|errs| {
        errs.into_iter()
            .map(|e| {
                if e.file.is_none() {
                    e.with_origin(entry.origin.display.clone())
                } else {
                    e
                }
            })
            .collect()
    })
}

/// Parses and validates, also checking names against `ctx`.
pub fn parse_with(
    text: &str,
    file: Option<&Path>,
    ctx: &CheckContext,
) -> Result<Workflow, Vec<YamlError>> {
    let doc: WorkflowDoc = yaml::parse(text, file).map_err(|e| vec![e])?;
    let mut e = Errs {
        file,
        text,
        list: Vec::new(),
    };
    if doc.name.trim().is_empty() {
        e.top("name", "`name` must not be empty");
    }

    // Inputs.
    let mut inputs = Vec::new();
    let mut positions = BTreeMap::new();
    for (name, d) in doc.inputs {
        if !valid_id(&name) || name.contains('-') {
            e.top(
                "inputs",
                format!("input `{name}`: names use letters, digits and `_`"),
            );
        }
        if let Some(def) = &d.default
            && !d.kind.accepts(def)
        {
            e.top(
                "inputs",
                format!(
                    "input `{name}`: default does not match type `{}`",
                    d.kind.as_str()
                ),
            );
        }
        if let Some(p) = d.positional
            && let Some(other) = positions.insert(p, name.clone())
        {
            e.top(
                "inputs",
                format!("inputs `{other}` and `{name}` both use positional {p}"),
            );
        }
        inputs.push(Input {
            required: d.required && d.default.is_none(),
            name,
            kind: d.kind,
            default: d.default,
            positional: d.positional,
            description: d.description,
        });
    }
    inputs.sort_by(|a, b| {
        (a.positional.is_none(), a.positional, &a.name).cmp(&(
            b.positional.is_none(),
            b.positional,
            &b.name,
        ))
    });

    let wf_budget = doc.budget.map(|b| {
        let mut errs = Vec::new();
        let b = budget(b, &mut errs);
        for m in errs {
            e.top("budget", m);
        }
        b
    });

    // Steps.
    let mut steps = Vec::new();
    let mut seen = BTreeSet::new();
    for s in doc.steps {
        if !valid_id(&s.id) {
            e.step(&s.id, None, "ids use letters, digits, `_` and `-`");
        }
        if !seen.insert(s.id.clone()) {
            e.step(&s.id, None, "duplicate step id");
        }
        if let Some(step) = step_from_doc(s, &mut e, ctx) {
            steps.push(step);
        }
    }
    if steps.is_empty() && e.list.is_empty() {
        e.top("steps", "a workflow needs at least one step");
    }

    // needs and the DAG.
    let ids: BTreeSet<String> = steps.iter().map(|s| s.id.clone()).collect();
    for s in &steps {
        for n in &s.needs {
            if n == &s.id {
                e.step(&s.id, Some("needs"), "a step cannot need itself");
            } else if !ids.contains(n) {
                e.step(&s.id, Some("needs"), format!("unknown step `{n}` in needs"));
            }
        }
    }
    let (order, ancestors) = match toposort(&steps) {
        Ok(v) => v,
        Err(cycle) => {
            let first = cycle.first().cloned().unwrap_or_default();
            e.step(
                &first,
                Some("needs"),
                format!("dependency cycle: {}", cycle.join(" -> ")),
            );
            (Vec::new(), BTreeMap::new())
        }
    };

    // Loops.
    for s in &steps {
        if let Some(l) = &s.loop_spec
            && let Some(r) = &l.retry_step
            && r != &s.id
            && !ancestors.get(&s.id).is_some_and(|a| a.contains(r))
        {
            e.step(
                &s.id,
                Some("retry_step"),
                format!("loop.retry_step `{r}` must be this step or one of its upstream steps"),
            );
        }
    }

    // Tasks.
    let mut task_ids = BTreeSet::new();
    let mut tasks = Vec::new();
    for t in doc.tasks {
        if !task_ids.insert(t.id.clone()) {
            e.top("tasks", format!("duplicate task id `{}`", t.id));
        }
        if t.title.trim().is_empty() {
            e.top("tasks", format!("task `{}` needs a title", t.id));
        }
        if let Some(st) = &t.step
            && !ids.contains(st)
        {
            e.top(
                "tasks",
                format!("task `{}` refers to unknown step `{st}`", t.id),
            );
        }
        tasks.push(TaskSpec {
            id: t.id,
            title: t.title,
            step: t.step,
        });
    }

    let mut wf = Workflow {
        command: doc.command.unwrap_or_else(|| doc.name.clone()),
        name: doc.name,
        description: doc.description,
        inputs,
        budget: wf_budget,
        goal: doc.goal,
        tasks,
        memory: doc.memory,
        steps,
        source: file.map(Path::to_path_buf),
        ancestors,
        order,
    };
    if !wf.order.is_empty() || wf.steps.is_empty() {
        check_templates(&wf, &mut e);
    }
    if e.list.is_empty() {
        if wf.order.is_empty() {
            wf.order = (0..wf.steps.len()).collect();
        }
        Ok(wf)
    } else {
        Err(e.list)
    }
}

fn step_from_doc(s: StepDoc, e: &mut Errs<'_>, ctx: &CheckContext) -> Option<Step> {
    let id = s.id.clone();
    let explicit = s.kind.as_deref();
    if let Some(t) = explicit
        && !STEP_TYPES.contains(&t)
    {
        e.step(
            &id,
            Some("type"),
            format!("invalid step type `{t}` (use {})", STEP_TYPES.join(", ")),
        );
        return None;
    }
    let inferred = explicit.or_else(|| {
        if s.agent.is_some() {
            Some("agent")
        } else if s.run.is_some() {
            Some("shell")
        } else if s.verify.is_some() {
            Some("verify")
        } else if s.uses.is_some() {
            Some("workflow")
        } else if s.message.is_some() {
            Some("approval")
        } else if s.test.is_some() {
            Some("bugfix-proof")
        } else {
            None
        }
    });
    let Some(ty) = inferred else {
        e.step(
            &id,
            None,
            "cannot tell the step type: add `type:` or one of `agent`, `run`, `verify`, `uses`, `message`, `test`",
        );
        return None;
    };

    // Keys that belong to each type.
    let present: Vec<(&str, bool)> = vec![
        ("agent", s.agent.is_some()),
        ("model", s.model.is_some()),
        ("prompt", s.prompt.is_some()),
        ("tools", s.tools.is_some()),
        ("run", s.run.is_some()),
        ("verify", s.verify.is_some()),
        ("test", s.test.is_some()),
        ("keep", s.keep.is_some()),
        ("message", s.message.is_some()),
        ("uses", s.uses.is_some()),
        ("with", s.with.is_some()),
    ];
    let allowed: &[&str] = match ty {
        "agent" => &["agent", "model", "prompt", "tools"],
        "shell" => &["run"],
        "verify" => &["verify"],
        "bugfix-proof" => &["test", "keep"],
        "approval" => &["message"],
        "dream" => &["with"],
        _ => &["uses", "with"],
    };
    let mut bad = false;
    for (k, on) in present {
        if on && !allowed.contains(&k) {
            e.step(&id, Some(k), format!("`{k}` is not valid in a `{ty}` step"));
            bad = true;
        }
    }
    let missing = |e: &mut Errs<'_>, key: &str| {
        e.step(&id, None, format!("a `{ty}` step needs `{key}`"));
    };
    let kind = match ty {
        "agent" => match s.agent {
            Some(agent) => {
                if let Some(known) = &ctx.agents
                    && !known.contains(&agent)
                {
                    e.step(&id, Some("agent"), format!("unknown agent `{agent}`"));
                }
                let mut tools = Vec::new();
                for t in s.tools.unwrap_or_default() {
                    if !t.starts_with(harness_config::mcp_tools::MCP_TOOL_PREFIX) {
                        e.step(
                            &id,
                            Some("tools"),
                            format!(
                                "`{t}`: an agent step's `tools` can only add MCP tools (mcp:<server>/<tool>); built-in tools come from the agent's YAML"
                            ),
                        );
                        continue;
                    }
                    match harness_config::mcp_tools::McpSelector::parse(&t) {
                        Ok(sel) => {
                            if let Some(known) = &ctx.mcp_servers
                                && !known.contains(&sel.server)
                            {
                                e.step(
                                    &id,
                                    Some("tools"),
                                    format!(
                                        "`{t}` names MCP server `{}`, which harness.ini does not define; add a [mcp.{}] section",
                                        sel.server, sel.server
                                    ),
                                );
                            }
                            if !tools.contains(&sel) {
                                tools.push(sel);
                            }
                        }
                        Err(m) => e.step(&id, Some("tools"), m),
                    }
                }
                StepKind::Agent {
                    agent,
                    model: s.model,
                    prompt: s.prompt,
                    tools,
                }
            }
            None => {
                missing(e, "agent");
                return None;
            }
        },
        "shell" => match s.run {
            Some(run) => StepKind::Shell { run },
            None => {
                missing(e, "run");
                return None;
            }
        },
        "verify" => match s.verify {
            Some(VerifyDoc::Command(command)) => StepKind::Verify {
                command,
                expect: Expect::Pass,
            },
            Some(VerifyDoc::Full(v)) => StepKind::Verify {
                command: v.command,
                expect: v.expect,
            },
            None => {
                missing(e, "verify");
                return None;
            }
        },
        "bugfix-proof" => match s.test {
            Some(test) => StepKind::BugfixProof {
                test,
                keep: s.keep.unwrap_or_else(|| vec!["tests/**".to_owned()]),
            },
            None => {
                missing(e, "test");
                return None;
            }
        },
        "approval" => match s.message {
            Some(message) => StepKind::Approval { message },
            None => {
                missing(e, "message");
                return None;
            }
        },
        "dream" => StepKind::Dream {
            with: s.with.unwrap_or_default(),
        },
        _ => match s.uses {
            Some(uses) => {
                if let Some(known) = &ctx.workflows
                    && !known.contains(&uses)
                {
                    e.step(&id, Some("uses"), format!("unknown workflow `{uses}`"));
                }
                StepKind::Workflow {
                    uses,
                    with: s.with.unwrap_or_default(),
                }
            }
            None => {
                missing(e, "uses");
                return None;
            }
        },
    };
    if bad {
        return None;
    }

    let expand = match (s.for_each, s.matrix) {
        (Some(_), Some(_)) => {
            e.step(
                &id,
                Some("matrix"),
                "use either `for_each` or `matrix`, not both",
            );
            None
        }
        (Some(v), None) => {
            if !(v.is_array()
                || v.as_str()
                    .is_some_and(|t| template::sole_expression(t).is_some()))
            {
                e.step(
                    &id,
                    Some("for_each"),
                    "for_each must be a list or a `{{ expression }}`",
                );
            }
            Some(Expand::ForEach(v))
        }
        (None, Some(m)) => {
            for (k, v) in &m {
                if !(v.is_array()
                    || v.as_str()
                        .is_some_and(|t| template::sole_expression(t).is_some()))
                {
                    e.step(
                        &id,
                        Some("matrix"),
                        format!("matrix `{k}` must be a list or a `{{{{ expression }}}}`"),
                    );
                }
            }
            if m.is_empty() {
                e.step(&id, Some("matrix"), "matrix must have at least one key");
            }
            Some(Expand::Matrix(m))
        }
        (None, None) => None,
    };
    let loop_spec = s.loop_spec.map(|l| {
        let max = l.max_iterations.unwrap_or(DEFAULT_MAX_ITERATIONS);
        if max == 0 {
            e.step(
                &id,
                Some("max_iterations"),
                "loop.max_iterations must be at least 1",
            );
        }
        LoopSpec {
            until: l.until,
            max_iterations: max.max(1),
            retry_step: l.retry_step,
        }
    });
    let timeout = match s.timeout {
        None => None,
        Some(Seconds::Int(n)) => Some(Duration::from_secs(n)),
        Some(Seconds::Text(t)) => match parse_duration(&t) {
            Ok(d) => Some(d),
            Err(msg) => {
                e.step(
                    &id,
                    Some("timeout"),
                    format!("invalid timeout `{t}`: {msg}"),
                );
                None
            }
        },
    };
    if timeout.is_some_and(|t| t.is_zero()) {
        e.step(&id, Some("timeout"), "timeout must be greater than zero");
    }
    let runs_on = s
        .runs_on
        .map(|r| runs_on(r, &id, e, ctx))
        .or_else(|| match &kind {
            StepKind::Agent { agent, .. } => ctx.agent_runs_on.get(agent).cloned(),
            _ => None,
        });
    if runs_on.as_ref().is_some_and(|r| *r != RunsOn::Local)
        && matches!(kind, StepKind::Approval { .. } | StepKind::Workflow { .. })
    {
        e.step(
            &id,
            Some("runs_on"),
            format!("`runs_on` is not valid in a `{ty}` step"),
        );
    }
    let scope = s.scope.map(|sc| match sc {
        OneOrMany::One(t) if template::is_template(&t) => ScopeSpec::Template(t),
        other => ScopeSpec::Globs(other.into_vec()),
    });
    let step_budget = s.budget.map(|b| {
        let mut errs = Vec::new();
        let b = budget(b, &mut errs);
        for m in errs {
            e.step(&id, Some("budget"), m);
        }
        b
    });
    let mut names = BTreeSet::new();
    for o in s.outputs.iter().chain(&s.artifacts) {
        if !valid_id(o) || o.contains('-') {
            e.step(
                &id,
                Some("outputs"),
                format!("`{o}`: output and artifact names use letters, digits and `_`"),
            );
        }
        if !names.insert(o) {
            e.step(&id, Some("outputs"), format!("`{o}` is declared twice"));
        }
    }
    Some(Step {
        id,
        kind,
        needs: s.needs.map(OneOrMany::into_vec).unwrap_or_default(),
        condition: s.condition,
        expand,
        loop_spec,
        retries: s.retries.unwrap_or(0),
        timeout,
        on_failure: s.on_failure.unwrap_or_default(),
        runs_on,
        scope,
        budget: step_budget,
        outputs: s.outputs,
        artifacts: s.artifacts,
    })
}

fn known_host(name: &str, id: &str, e: &mut Errs<'_>, ctx: &CheckContext) {
    if let Some(remotes) = &ctx.remotes
        && !remotes.contains_key(name)
    {
        let known: Vec<&str> = remotes.keys().map(String::as_str).collect();
        e.step(
            id,
            Some("runs_on"),
            format!(
                "unknown host `{name}`: hosts must be declared as [remote.{name}] in harness.ini (known: {})",
                if known.is_empty() { "none".into() } else { known.join(", ") }
            ),
        );
    }
}

fn runs_on(doc: RunsOnDoc, id: &str, e: &mut Errs<'_>, ctx: &CheckContext) -> RunsOn {
    match doc {
        RunsOnDoc::Name(n) if n == "local" => RunsOn::Local,
        RunsOnDoc::Name(n) => {
            known_host(&n, id, e, ctx);
            RunsOn::Host(n)
        }
        RunsOnDoc::Select(s) => {
            let fallback = s.fallback.map(|f| {
                if f == "local" {
                    Fallback::Local
                } else {
                    known_host(&f, id, e, ctx);
                    Fallback::Host(f)
                }
            });
            if let Some(remotes) = &ctx.remotes
                && fallback.is_none()
                && !remotes
                    .values()
                    .any(|labels| s.labels.iter().all(|l| labels.contains(l)))
            {
                e.step(
                    id,
                    Some("runs_on"),
                    format!(
                        "no [remote.*] host has labels [{}] and there is no `fallback`",
                        s.labels.join(", ")
                    ),
                );
            }
            RunsOn::Select(HostSelector {
                labels: s.labels,
                min_mem_gb: s.min_mem_gb,
                min_cpus: s.min_cpus,
                min_gpus: s.min_gpus,
                fallback,
            })
        }
    }
}

type Order = (Vec<usize>, BTreeMap<String, BTreeSet<String>>);

/// Kahn's algorithm in file order. On a cycle returns its path.
fn toposort(steps: &[Step]) -> Result<Order, Vec<String>> {
    let index: BTreeMap<&str, usize> = steps
        .iter()
        .enumerate()
        .map(|(i, s)| (s.id.as_str(), i))
        .collect();
    let mut indeg: Vec<usize> = steps
        .iter()
        .map(|s| {
            s.needs
                .iter()
                .filter(|n| index.contains_key(n.as_str()) && **n != s.id)
                .count()
        })
        .collect();
    let mut done = vec![false; steps.len()];
    let mut order = Vec::new();
    let mut ancestors: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    loop {
        let Some(i) = (0..steps.len()).find(|&i| !done[i] && indeg[i] == 0) else {
            break;
        };
        done[i] = true;
        order.push(i);
        let mut anc = BTreeSet::new();
        for n in &steps[i].needs {
            if let Some(a) = ancestors.get(n) {
                anc.insert(n.clone());
                anc.extend(a.iter().cloned());
            }
        }
        ancestors.insert(steps[i].id.clone(), anc);
        for (j, s) in steps.iter().enumerate() {
            if !done[j] && s.needs.iter().any(|n| *n == steps[i].id) {
                indeg[j] -= 1;
            }
        }
    }
    if order.len() == steps.len() {
        return Ok((order, ancestors));
    }
    // Walk unfinished needs until a step repeats.
    let start = (0..steps.len()).find(|&i| !done[i]).unwrap_or(0);
    let mut path = vec![start];
    let mut cur = start;
    loop {
        let next = steps[cur]
            .needs
            .iter()
            .filter_map(|n| index.get(n.as_str()).copied())
            .find(|&j| !done[j] && j != cur);
        let Some(next) = next else { break };
        if let Some(pos) = path.iter().position(|&p| p == next) {
            let mut cycle: Vec<String> = path[pos..].iter().map(|&p| steps[p].id.clone()).collect();
            cycle.push(steps[next].id.clone());
            return Err(cycle);
        }
        path.push(next);
        cur = next;
    }
    Err(path.iter().map(|&p| steps[p].id.clone()).collect())
}

fn check_templates(wf: &Workflow, e: &mut Errs<'_>) {
    let inputs: BTreeSet<String> = wf.inputs.iter().map(|i| i.name.clone()).collect();
    let infos: BTreeMap<String, StepRefs> = wf
        .steps
        .iter()
        .map(|s| {
            (
                s.id.clone(),
                StepRefs {
                    outputs: s.output_names(),
                    artifacts: s.produced_artifacts().iter().cloned().collect(),
                },
            )
        })
        .collect();
    let base = RefScope {
        inputs: Some(&inputs),
        steps: Some(&infos),
        ..RefScope::default()
    };
    if let Some(goal) = &wf.goal {
        match template::references(goal) {
            Ok(refs) => {
                for m in template::check_refs(&refs, &base) {
                    e.top("goal", format!("goal: {m}"));
                }
            }
            Err(err) => e.top("goal", format!("goal: {}", err.message)),
        }
    }
    // Steps re-run by a loop may read the looping step's last result.
    let mut loop_visible: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for s in &wf.steps {
        if let Some(r) = s.loop_spec.as_ref().and_then(|l| l.retry_step.as_ref()) {
            for x in wf.loop_chain(r, &s.id) {
                if x != s.id {
                    loop_visible.entry(x).or_default().insert(s.id.clone());
                }
            }
        }
    }
    for s in &wf.steps {
        let mut scope = base.clone();
        scope.visible = wf.upstream(&s.id);
        if let Some(extra) = loop_visible.get(&s.id) {
            scope.visible.extend(extra.iter().cloned());
        }
        match &s.expand {
            Some(Expand::ForEach(_)) => scope.item = true,
            Some(Expand::Matrix(m)) => scope.matrix = Some(m.keys().cloned().collect()),
            None => {}
        }
        let id = s.id.as_str();
        let mut check = |key: &str, text: &str, expr: bool, scope: &RefScope<'_>| {
            check_one(e, id, key, text, expr, scope);
        };
        // Expansion sources are evaluated before the step's own matrix and
        // item exist.
        let outer = RefScope {
            item: false,
            matrix: None,
            ..scope.clone()
        };
        match &s.expand {
            Some(Expand::ForEach(Value::String(t))) => check("for_each", t, true, &outer),
            Some(Expand::Matrix(m)) => {
                for v in m.values() {
                    if let Value::String(t) = v {
                        check("matrix", t, true, &outer);
                    }
                }
            }
            _ => {}
        }
        if let Some(c) = &s.condition {
            check("if", c, true, &scope);
        }
        match &s.kind {
            StepKind::Agent { prompt, model, .. } => {
                if let Some(p) = prompt {
                    check("prompt", p, false, &scope);
                }
                if let Some(m) = model {
                    check("model", m, false, &scope);
                }
            }
            StepKind::Shell { run } => check("run", run, false, &scope),
            StepKind::Verify { command, .. } => check("verify", command, false, &scope),
            StepKind::BugfixProof { test, .. } => check("test", test, false, &scope),
            StepKind::Approval { message } => {
                check("message", message, false, &scope);
            }
            StepKind::Workflow { with, .. } => {
                for v in with.values() {
                    for t in strings(v) {
                        check("with", t, false, &scope);
                    }
                }
            }
            StepKind::Dream { .. } => {}
        }
        if let Some(ScopeSpec::Template(t)) = &s.scope {
            check("scope", t, false, &scope);
        }
        if let Some(l) = &s.loop_spec {
            let mut own = scope.clone();
            own.visible.insert(s.id.clone());
            check("until", &l.until, true, &own);
        }
        if let StepKind::Approval { .. } = &s.kind {
            for a in &s.artifacts {
                let known = scope
                    .visible
                    .iter()
                    .any(|up| infos.get(up).is_some_and(|i| i.artifacts.contains(a)));
                if !known {
                    e.step(
                        &s.id,
                        Some("artifacts"),
                        format!("artifact `{a}` to review is not produced by an upstream step"),
                    );
                }
            }
        }
    }
}

fn check_one(e: &mut Errs<'_>, id: &str, key: &str, text: &str, expr: bool, scope: &RefScope<'_>) {
    let refs = if expr {
        template::expr_references(text)
    } else {
        template::references(text)
    };
    match refs {
        Ok(refs) => {
            for m in template::check_refs(&refs, scope) {
                e.step(id, Some(key), format!("{key}: {m}"));
            }
        }
        Err(err) => e.step(id, Some(key), format!("{key}: {}", err.message)),
    }
}

fn strings(v: &Value) -> Vec<&str> {
    match v {
        Value::String(s) => vec![s.as_str()],
        Value::Array(a) => a.iter().flat_map(strings).collect(),
        Value::Object(m) => m.values().flat_map(strings).collect(),
        _ => Vec::new(),
    }
}

/// Finds `uses:` cycles across workflows (`a` uses `b` uses `a`).
pub fn check_uses_cycles(workflows: &BTreeMap<String, Workflow>) -> Vec<String> {
    fn visit<'a>(
        name: &'a str,
        all: &'a BTreeMap<String, Workflow>,
        stack: &mut Vec<&'a str>,
        out: &mut BTreeSet<String>,
    ) {
        if let Some(pos) = stack.iter().position(|s| *s == name) {
            let mut c: Vec<&str> = stack[pos..].to_vec();
            c.push(name);
            out.insert(format!("workflow `uses` cycle: {}", c.join(" -> ")));
            return;
        }
        let Some(wf) = all.get(name) else { return };
        stack.push(name);
        for u in wf.uses() {
            visit(u, all, stack, out);
        }
        stack.pop();
    }
    let mut out = BTreeSet::new();
    for name in workflows.keys() {
        visit(name, workflows, &mut Vec::new(), &mut out);
    }
    out.into_iter().collect()
}

#[cfg(test)]
mod tests;
