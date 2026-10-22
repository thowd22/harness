//! Eval suite format: YAML files that define tasks to run headless.
//!
//! ```yaml
//! name: starter
//! description: Trap tasks for the known failure modes
//! defaults:
//!   repeats: 1
//!   agent: coder
//!   timeout_secs: 600
//! tasks:
//!   - id: temp-file-hygiene
//!     problem: "problem-5: leftover temporary and debug files"
//!     description: The agent must remove the scratch files it creates.
//!     fixture: fixtures/rust-lib        # directory copied into the workspace
//!     files:                            # extra inline files (override the fixture)
//!       NOTES.md: "scratch"
//!     setup: ["git init -q", "git add -A", "git commit -qm init"]
//!     prompt: "Add a `double` function to src/lib.rs."
//!     provider: { fake: scripts/temp-file.yaml }   # or { model: sonnet }
//!     repeats: 3
//!     checks:
//!       - command: "grep -q 'fn double' src/lib.rs"
//!       - file: { path: src/lib.rs, contains: "fn double" }
//!       - unchanged: ["tests/**"]
//!       - hygiene: { max_temp_files: 0, scope: ["src/**"], max_out_of_scope: 0 }
//! ```
//!
//! A task runs either a `prompt` with an agent or a `workflow` (by name, with
//! inputs). `provider` picks a scripted fake provider (`fake:` path to a
//! [`FakeScript`](harness_providers::fake::FakeScript) YAML file) or a real
//! model (`model:` alias or `provider/model`); omitted, the agent's model or
//! `[general] default_model` is used. Relative paths resolve against the
//! suite file's directory.
//!
//! Checks run in the workspace after the agent finishes. A task attempt
//! passes when every check passes.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Default per-attempt timeout.
pub const DEFAULT_TIMEOUT_SECS: u64 = 600;

/// An error loading a suite.
#[derive(Debug, thiserror::Error)]
pub enum SuiteError {
    /// The file could not be read.
    #[error("{path}: {source}")]
    Io {
        /// The file.
        path: PathBuf,
        /// The error.
        #[source]
        source: std::io::Error,
    },
    /// The YAML is invalid (`file:line:col: message`).
    #[error("{0}")]
    Yaml(#[from] harness_config::yaml::YamlError),
    /// The suite parsed but is inconsistent.
    #[error("{file}: {message}")]
    Invalid {
        /// The suite file.
        file: String,
        /// What is wrong.
        message: String,
    },
}

/// A loaded suite.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Suite {
    /// Suite name (used in results).
    pub name: String,
    /// What the suite measures.
    #[serde(default)]
    pub description: Option<String>,
    /// Values tasks inherit.
    #[serde(default)]
    pub defaults: Defaults,
    /// The tasks.
    pub tasks: Vec<Task>,
    /// Directory relative paths resolve against (the suite file's directory).
    #[serde(skip)]
    pub base_dir: PathBuf,
}

/// Suite-wide defaults.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Defaults {
    /// Attempts per task.
    #[serde(default)]
    pub repeats: Option<u32>,
    /// Agent profile.
    #[serde(default)]
    pub agent: Option<String>,
    /// Provider selection.
    #[serde(default)]
    pub provider: Option<ProviderSpec>,
    /// Per-attempt timeout in seconds.
    #[serde(default)]
    pub timeout_secs: Option<u64>,
    /// Turn limit.
    #[serde(default)]
    pub max_turns: Option<u32>,
    /// Cost limit per attempt in USD.
    #[serde(default)]
    pub budget_usd: Option<f64>,
    /// Autonomy mode (`ask`, `auto-edit`, `auto`).
    #[serde(default)]
    pub mode: Option<String>,
    /// Command patterns approved up front; every other call that needs
    /// approval is denied (evals never prompt).
    #[serde(default)]
    pub allow: Vec<String>,
}

/// Which provider a task uses.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderSpec {
    /// Scripted fake provider: path of a fake script YAML file.
    #[serde(default)]
    pub fake: Option<PathBuf>,
    /// Real model: alias or `provider/model`.
    #[serde(default)]
    pub model: Option<String>,
}

/// One eval task.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Task {
    /// Unique id within the suite.
    pub id: String,
    /// The failure mode or problem this task targets (e.g. `problem-5: ...`).
    #[serde(default)]
    pub problem: Option<String>,
    /// Human description of the trap.
    #[serde(default)]
    pub description: Option<String>,
    /// Fixture directory copied into the workspace.
    #[serde(default)]
    pub fixture: Option<PathBuf>,
    /// Inline files written after the fixture (path -> contents).
    #[serde(default)]
    pub files: BTreeMap<String, String>,
    /// Shell commands run in the workspace before the agent (e.g. `git init`).
    #[serde(default)]
    pub setup: Vec<String>,
    /// The user prompt.
    #[serde(default)]
    pub prompt: Option<String>,
    /// A workflow to run instead of a prompt.
    #[serde(default)]
    pub workflow: Option<WorkflowSpec>,
    /// Agent profile (default: suite default, then `coder`).
    #[serde(default)]
    pub agent: Option<String>,
    /// Provider selection (default: suite default).
    #[serde(default)]
    pub provider: Option<ProviderSpec>,
    /// Attempts (default: suite default, then 1).
    #[serde(default)]
    pub repeats: Option<u32>,
    /// Per-attempt timeout in seconds.
    #[serde(default)]
    pub timeout_secs: Option<u64>,
    /// Turn limit.
    #[serde(default)]
    pub max_turns: Option<u32>,
    /// Cost limit per attempt in USD.
    #[serde(default)]
    pub budget_usd: Option<f64>,
    /// Autonomy mode (default: suite default, then `[general] mode`).
    #[serde(default)]
    pub mode: Option<String>,
    /// Extra command patterns approved up front (added to the suite's).
    #[serde(default)]
    pub allow: Vec<String>,
    /// Checks; all must pass.
    #[serde(default)]
    pub checks: Vec<Check>,
}

/// A workflow invocation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkflowSpec {
    /// Workflow name.
    pub name: String,
    /// Inputs.
    #[serde(default)]
    pub inputs: BTreeMap<String, serde_json::Value>,
}

/// Whether a command check expects success or failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Expect {
    /// Exit code 0.
    #[default]
    Pass,
    /// Non-zero exit code.
    Fail,
}

/// A check run after the agent finishes. Written as a single-key map.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Check {
    /// `command: "cargo test"` (optionally with `expect: fail`).
    Command(CommandCheck),
    /// `file: { path, exists, contains, not_contains }`.
    File {
        /// The expectation.
        file: FileCheck,
    },
    /// `unchanged: [globs]`: matching files are identical to the fixture.
    Unchanged {
        /// Globs relative to the workspace.
        unchanged: Vec<String>,
    },
    /// `hygiene: { ... }`: limits on temporary and out-of-scope files.
    Hygiene {
        /// The limits.
        hygiene: HygieneCheck,
    },
}

/// A shell command check.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandCheck {
    /// Run with `sh -c` in the workspace.
    pub command: String,
    /// Expected outcome.
    #[serde(default)]
    pub expect: Expect,
}

/// A file expectation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileCheck {
    /// Workspace-relative path.
    pub path: String,
    /// Whether the file must exist (default true).
    #[serde(default)]
    pub exists: Option<bool>,
    /// Substrings the file must contain.
    #[serde(default, deserialize_with = "one_or_many")]
    pub contains: Vec<String>,
    /// Substrings the file must not contain.
    #[serde(default, deserialize_with = "one_or_many")]
    pub not_contains: Vec<String>,
}

/// Hygiene limits.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HygieneCheck {
    /// Most temporary/debug files left behind (default 0).
    #[serde(default)]
    pub max_temp_files: Option<u32>,
    /// Paths the task may change.
    #[serde(default)]
    pub scope: Vec<String>,
    /// Most changed files outside `scope` (default 0 when `scope` is set).
    #[serde(default)]
    pub max_out_of_scope: Option<u32>,
}

fn one_or_many<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<String>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum OneOrMany {
        One(String),
        Many(Vec<String>),
    }
    Ok(match OneOrMany::deserialize(d)? {
        OneOrMany::One(s) => vec![s],
        OneOrMany::Many(v) => v,
    })
}

impl fmt::Display for Check {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Check::Command(c) => match c.expect {
                Expect::Pass => write!(f, "command `{}` passes", c.command),
                Expect::Fail => write!(f, "command `{}` fails", c.command),
            },
            Check::File { file } => write!(f, "file {}", file.path),
            Check::Unchanged { unchanged } => write!(f, "unchanged {}", unchanged.join(", ")),
            Check::Hygiene { .. } => f.write_str("hygiene"),
        }
    }
}

/// What an attempt asks the executor to run, with every default applied.
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedTask {
    /// Task id.
    pub id: String,
    /// Prompt (`None` for workflow tasks).
    pub prompt: Option<String>,
    /// Workflow (`None` for prompt tasks).
    pub workflow: Option<WorkflowSpec>,
    /// Agent profile.
    pub agent: Option<String>,
    /// Absolute path of a fake script, if the fake provider is used.
    pub fake_script: Option<PathBuf>,
    /// Model alias or reference for real providers.
    pub model: Option<String>,
    /// Attempts.
    pub repeats: u32,
    /// Per-attempt timeout in seconds.
    pub timeout_secs: u64,
    /// Turn limit.
    pub max_turns: Option<u32>,
    /// Cost limit in USD.
    pub budget_usd: Option<f64>,
    /// Autonomy mode.
    pub mode: Option<String>,
    /// Approved command patterns (suite defaults, then the task's).
    pub allow: Vec<String>,
}

impl Suite {
    /// Loads and validates a suite file.
    pub fn load(path: &Path) -> Result<Suite, SuiteError> {
        let text = std::fs::read_to_string(path).map_err(|source| SuiteError::Io {
            path: path.to_owned(),
            source,
        })?;
        let base = path.parent().unwrap_or(Path::new(".")).to_owned();
        Self::parse(&text, Some(path), base)
    }

    /// Parses suite YAML. `file` is used in error messages; `base_dir`
    /// resolves relative paths.
    pub fn parse(text: &str, file: Option<&Path>, base_dir: PathBuf) -> Result<Suite, SuiteError> {
        let mut suite: Suite = harness_config::yaml::parse(text, file)?;
        suite.base_dir = base_dir;
        let name = file.map_or_else(|| "<suite>".to_owned(), |p| p.display().to_string());
        suite.validate().map_err(|message| SuiteError::Invalid {
            file: name,
            message,
        })?;
        Ok(suite)
    }

    fn validate(&self) -> Result<(), String> {
        if self.tasks.is_empty() {
            return Err("a suite needs at least one task".into());
        }
        let mut seen = BTreeSet::new();
        for t in &self.tasks {
            if t.id.trim().is_empty() {
                return Err("every task needs a non-empty `id`".into());
            }
            if !seen.insert(t.id.as_str()) {
                return Err(format!("duplicate task id `{}`", t.id));
            }
            match (&t.prompt, &t.workflow) {
                (Some(_), Some(_)) => {
                    return Err(format!(
                        "task `{}`: set `prompt` or `workflow`, not both",
                        t.id
                    ));
                }
                (None, None) => {
                    return Err(format!("task `{}`: set `prompt` or `workflow`", t.id));
                }
                _ => {}
            }
            if let Some(m) = t.mode.as_ref().or(self.defaults.mode.as_ref())
                && harness_policy::rules::Mode::parse(m).is_none()
            {
                return Err(format!(
                    "task `{}`: mode `{m}` is not ask, auto-edit or auto",
                    t.id
                ));
            }
            if t.repeats == Some(0) {
                return Err(format!("task `{}`: `repeats` must be at least 1", t.id));
            }
            if let Some(p) = t.provider.as_ref().or(self.defaults.provider.as_ref())
                && p.fake.is_some() == p.model.is_some()
            {
                return Err(format!(
                    "task `{}`: `provider` needs exactly one of `fake` or `model`",
                    t.id
                ));
            }
            for f in t.files.keys() {
                safe_relative(f).map_err(|e| format!("task `{}`: files: {e}", t.id))?;
            }
            for c in &t.checks {
                match c {
                    Check::File { file } => safe_relative(&file.path)
                        .map_err(|e| format!("task `{}`: file check: {e}", t.id))?,
                    Check::Unchanged { unchanged } => {
                        for g in unchanged {
                            globset::Glob::new(g)
                                .map_err(|e| format!("task `{}`: unchanged `{g}`: {e}", t.id))?;
                        }
                    }
                    Check::Hygiene { hygiene } => {
                        for g in &hygiene.scope {
                            globset::Glob::new(g).map_err(|e| {
                                format!("task `{}`: hygiene scope `{g}`: {e}", t.id)
                            })?;
                        }
                    }
                    Check::Command(_) => {}
                }
            }
        }
        Ok(())
    }

    /// Resolves `path` against the suite directory.
    pub fn resolve_path(&self, path: &Path) -> PathBuf {
        if path.is_absolute() {
            path.to_owned()
        } else {
            self.base_dir.join(path)
        }
    }

    /// The task with every default applied.
    pub fn resolve(&self, task: &Task) -> ResolvedTask {
        let d = &self.defaults;
        let provider = task.provider.as_ref().or(d.provider.as_ref());
        ResolvedTask {
            id: task.id.clone(),
            prompt: task.prompt.clone(),
            workflow: task.workflow.clone(),
            agent: task.agent.clone().or_else(|| d.agent.clone()),
            fake_script: provider
                .and_then(|p| p.fake.as_deref())
                .map(|p| self.resolve_path(p)),
            model: provider.and_then(|p| p.model.clone()),
            repeats: task.repeats.or(d.repeats).unwrap_or(1).max(1),
            timeout_secs: task
                .timeout_secs
                .or(d.timeout_secs)
                .unwrap_or(DEFAULT_TIMEOUT_SECS),
            max_turns: task.max_turns.or(d.max_turns),
            budget_usd: task.budget_usd.or(d.budget_usd),
            mode: task.mode.clone().or_else(|| d.mode.clone()),
            allow: d.allow.iter().chain(&task.allow).cloned().collect(),
        }
    }

    /// Tasks whose id contains `filter` (all when `None`).
    pub fn select<'a>(&'a self, filter: Option<&'a str>) -> impl Iterator<Item = &'a Task> + 'a {
        self.tasks
            .iter()
            .filter(move |t| filter.is_none_or(|f| t.id.contains(f)))
    }
}

/// Rejects absolute paths and `..` components.
pub fn safe_relative(path: &str) -> Result<(), String> {
    let p = Path::new(path);
    if path.is_empty() || p.is_absolute() {
        return Err(format!("`{path}` must be a relative path"));
    }
    if p.components()
        .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return Err(format!("`{path}` must not contain `..`"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SUITE: &str = r#"
name: demo
defaults:
  repeats: 2
  provider: { fake: scripts/a.yaml }
tasks:
  - id: one
    problem: "problem-5: temp files"
    fixture: fixtures/lib
    files: { "src/x.rs": "fn x() {}" }
    setup: ["git init -q"]
    prompt: Fix it
    checks:
      - command: "cargo test"
      - command: "grep -q bug src/lib.rs"
        expect: fail
      - file: { path: src/lib.rs, contains: "fn double" }
      - file: { path: debug.log, exists: false }
      - unchanged: ["tests/**"]
      - hygiene: { max_temp_files: 0, scope: ["src/**"] }
  - id: two
    workflow: { name: fix-bug, inputs: { issue: "crash" } }
    provider: { model: sonnet }
    repeats: 1
"#;

    fn suite() -> Suite {
        Suite::parse(SUITE, None, PathBuf::from("/suites/demo")).unwrap()
    }

    #[test]
    fn parses_tasks_checks_and_defaults() {
        let s = suite();
        assert_eq!(s.tasks.len(), 2);
        let t = &s.tasks[0];
        assert_eq!(t.checks.len(), 6);
        assert!(matches!(&t.checks[0], Check::Command(c) if c.expect == Expect::Pass));
        assert!(matches!(&t.checks[1], Check::Command(c) if c.expect == Expect::Fail));
        assert!(matches!(&t.checks[2], Check::File { file } if file.contains == ["fn double"]));
        assert!(matches!(&t.checks[3], Check::File { file } if file.exists == Some(false)));
        assert!(
            matches!(&t.checks[4], Check::Unchanged { unchanged } if unchanged == &["tests/**"])
        );
        assert!(
            matches!(&t.checks[5], Check::Hygiene { hygiene } if hygiene.max_temp_files == Some(0))
        );

        let r = s.resolve(t);
        assert_eq!(r.repeats, 2);
        assert_eq!(
            r.fake_script,
            Some(PathBuf::from("/suites/demo/scripts/a.yaml"))
        );
        assert_eq!(r.model, None);
        assert_eq!(r.timeout_secs, DEFAULT_TIMEOUT_SECS);

        let r2 = s.resolve(&s.tasks[1]);
        assert_eq!(r2.repeats, 1);
        assert_eq!(r2.fake_script, None);
        assert_eq!(r2.model.as_deref(), Some("sonnet"));
        assert_eq!(r2.workflow.as_ref().unwrap().name, "fix-bug");
    }

    #[test]
    fn rejects_inconsistent_suites() {
        let bad = |yaml: &str| {
            Suite::parse(yaml, None, PathBuf::new())
                .unwrap_err()
                .to_string()
        };
        assert!(bad("name: x\ntasks: []\n").contains("at least one task"));
        assert!(bad("name: x\ntasks: [{id: a}]\n").contains("set `prompt` or `workflow`"));
        assert!(
            bad("name: x\ntasks: [{id: a, prompt: p}, {id: a, prompt: q}]\n")
                .contains("duplicate task id")
        );
        assert!(
            bad("name: x\ntasks: [{id: a, prompt: p, provider: {fake: f, model: m}}]\n")
                .contains("exactly one of")
        );
        assert!(bad("name: x\ntasks: [{id: a, prompt: p, files: {\"../x\": y}}]\n").contains(".."));
        assert!(bad("name: x\ntasks: [{id: a, prompt: p, bogus: 1}]\n").contains("bogus"));
    }

    #[test]
    fn yaml_errors_carry_locations() {
        let e = Suite::parse(
            "name: x\ntasks:\n  - id: [\n",
            Some(Path::new("s.yaml")),
            PathBuf::new(),
        )
        .unwrap_err()
        .to_string();
        assert!(e.starts_with("s.yaml:"), "{e}");
    }

    #[test]
    fn select_filters_by_id() {
        let s = suite();
        assert_eq!(s.select(Some("tw")).count(), 1);
        assert_eq!(s.select(None).count(), 2);
    }
}
