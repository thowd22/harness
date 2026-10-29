//! Scope declaration and out-of-scope detection.
//!
//! A [`Scope`] is a positive list of path globs an agent is expected to
//! edit (`src/**`, `tests/**`). It comes from, most specific first:
//!
//! 1. the session (`/scope src/parser/**`),
//! 2. the workflow step (`scope:` on a step),
//! 3. the agent (`scope:` in agent YAML).
//!
//! [`Scope::resolve`] picks the first one that is set. Globs are relative to
//! the workspace root; `*` does not cross `/`, `**` does.
//!
//! Edits outside the scope are not refused. [`Scope::check_write`] returns
//! [`ScopeDecision::Ask`] in `ask` and `auto-edit` mode, so the user
//! decides, and [`ScopeDecision::Flag`] in `auto` mode, so the change goes
//! ahead and shows up in the hygiene report. Changes made by bash are
//! checked after the fact with [`Scope::contains`].
//!
//! The model sees the scope only as allowed areas
//! ([`Scope::prompt_section`]), never as a prohibition.

use std::path::{Path, PathBuf};

use globset::{Glob, GlobBuilder, GlobSet, GlobSetBuilder};
use harness_policy::rules::Mode;
use serde::Serialize;

/// Where a scope came from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", content = "name", rename_all = "snake_case")]
pub enum ScopeSource {
    /// Set for the session by the user.
    Session,
    /// A workflow step (`steps[].scope`), by step id.
    WorkflowStep(String),
    /// An agent's `scope:`, by agent name.
    Agent(String),
}

impl std::fmt::Display for ScopeSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Session => f.write_str("session scope"),
            Self::WorkflowStep(id) => write!(f, "workflow step `{id}`"),
            Self::Agent(name) => write!(f, "agent `{name}`"),
        }
    }
}

/// An invalid scope glob.
#[derive(Debug, thiserror::Error)]
#[error("invalid scope glob `{glob}` ({source_name}): {error}")]
pub struct ScopeError {
    /// The glob.
    pub glob: String,
    /// Where it came from.
    pub source_name: String,
    /// Why it is invalid.
    #[source]
    pub error: globset::Error,
}

/// A compiled scope.
#[derive(Debug, Clone)]
pub struct Scope {
    globs: Vec<String>,
    source: ScopeSource,
    set: GlobSet,
}

/// The result of checking a write against a scope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ScopeDecision {
    /// Inside the scope (or no scope is set).
    InScope,
    /// Outside the scope: ask the user before writing.
    Ask {
        /// Message for the approval prompt and the model.
        reason: String,
    },
    /// Outside the scope: allowed, but flagged in the hygiene report.
    Flag {
        /// Message for the report.
        reason: String,
    },
}

fn compile(glob: &str) -> Result<Glob, globset::Error> {
    let g = glob.trim().trim_start_matches("./");
    // `src/` means everything below src.
    let g = if g.ends_with('/') {
        format!("{g}**")
    } else {
        g.to_string()
    };
    GlobBuilder::new(&g).literal_separator(true).build()
}

impl Scope {
    /// Compile `globs` (empty entries are ignored).
    pub fn new(
        source: ScopeSource,
        globs: impl IntoIterator<Item = impl Into<String>>,
    ) -> Result<Self, ScopeError> {
        let globs: Vec<String> = globs
            .into_iter()
            .map(Into::into)
            .filter(|g: &String| !g.trim().is_empty())
            .collect();
        let mut b = GlobSetBuilder::new();
        for g in &globs {
            let glob = compile(g).map_err(|error| ScopeError {
                glob: g.clone(),
                source_name: source.to_string(),
                error,
            })?;
            b.add(glob);
        }
        let set = b.build().map_err(|error| ScopeError {
            glob: globs.join(", "),
            source_name: source.to_string(),
            error,
        })?;
        Ok(Self { globs, source, set })
    }

    /// The effective scope: session, else workflow step, else agent. Each
    /// argument is `(source name, globs)`; an empty glob list counts as
    /// not set. Returns `None` when no scope applies.
    pub fn resolve(
        session: Option<&[String]>,
        step: Option<(&str, &[String])>,
        agent: Option<(&str, &[String])>,
    ) -> Result<Option<Scope>, ScopeError> {
        let nonempty = |g: &[String]| g.iter().any(|s| !s.trim().is_empty());
        if let Some(g) = session.filter(|g| nonempty(g)) {
            return Self::new(ScopeSource::Session, g.iter().cloned()).map(Some);
        }
        if let Some((id, g)) = step.filter(|(_, g)| nonempty(g)) {
            return Self::new(ScopeSource::WorkflowStep(id.into()), g.iter().cloned()).map(Some);
        }
        if let Some((name, g)) = agent.filter(|(_, g)| nonempty(g)) {
            return Self::new(ScopeSource::Agent(name.into()), g.iter().cloned()).map(Some);
        }
        Ok(None)
    }

    /// The globs as written.
    pub fn globs(&self) -> &[String] {
        &self.globs
    }

    /// Where the scope came from.
    pub fn source(&self) -> &ScopeSource {
        &self.source
    }

    /// True if the workspace-relative `rel` path is inside the scope.
    pub fn contains(&self, rel: &str) -> bool {
        self.set.is_match(rel.trim_start_matches("./"))
    }

    /// True if `path` (absolute, or relative to `cwd`) is inside the scope
    /// of the workspace at `root`. Paths outside the workspace are never in
    /// scope.
    pub fn contains_path(&self, root: &Path, cwd: &Path, path: &Path) -> bool {
        workspace_relative(root, cwd, path).is_some_and(|rel| self.contains(&rel))
    }

    /// Check a write by a file tool. Outside the scope this asks in `ask`
    /// and `auto-edit` mode and flags in `auto` mode.
    pub fn check_write(&self, root: &Path, cwd: &Path, path: &Path, mode: Mode) -> ScopeDecision {
        if self.contains_path(root, cwd, path) {
            return ScopeDecision::InScope;
        }
        let shown = workspace_relative(root, cwd, path)
            .unwrap_or_else(|| path.to_string_lossy().into_owned());
        let reason = format!(
            "`{shown}` is outside the task scope ({}: {}).",
            self.source,
            self.globs.join(", ")
        );
        match mode {
            Mode::Ask | Mode::AutoEdit => ScopeDecision::Ask { reason },
            Mode::Auto | Mode::Yolo => ScopeDecision::Flag { reason },
        }
    }

    /// The scope as a system-prompt section, phrased as allowed areas.
    pub fn prompt_section(&self) -> String {
        let mut s = String::from(
            "## Task scope\nThis task covers files matching these paths. Make your edits here, \
             and add or update tests within these paths:\n",
        );
        for g in &self.globs {
            s.push_str(&format!("- `{g}`\n"));
        }
        s.push_str(
            "If the task genuinely needs a change elsewhere, say which file and why; the user \
             will be asked to approve it.\n",
        );
        s
    }
}

/// `path` relative to `root` (resolving it against `cwd` first), with `/`
/// separators, or `None` when it is outside `root`.
pub fn workspace_relative(root: &Path, cwd: &Path, path: &Path) -> Option<String> {
    let root = harness_tools::workspace::normalize(root);
    let abs: PathBuf = harness_tools::workspace::normalize(&cwd.join(path));
    crate::snapshot::relative(&root, &abs)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn globs(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn resolution_prefers_session_then_step_then_agent() {
        let agent = globs(&["src/**"]);
        let step = globs(&["tests/**"]);
        let session = globs(&["docs/**"]);
        let s = Scope::resolve(
            Some(&session),
            Some(("repro", &step)),
            Some(("coder", &agent)),
        )
        .unwrap()
        .unwrap();
        assert_eq!(s.source(), &ScopeSource::Session);
        let s = Scope::resolve(None, Some(("repro", &step)), Some(("coder", &agent)))
            .unwrap()
            .unwrap();
        assert_eq!(s.source(), &ScopeSource::WorkflowStep("repro".into()));
        assert!(s.contains("tests/a.rs") && !s.contains("src/a.rs"));
        let s = Scope::resolve(Some(&[]), Some(("repro", &[])), Some(("coder", &agent)))
            .unwrap()
            .unwrap();
        assert_eq!(s.source(), &ScopeSource::Agent("coder".into()));
        assert!(Scope::resolve(None, None, None).unwrap().is_none());
    }

    #[test]
    fn glob_semantics() {
        let s = Scope::new(
            ScopeSource::Session,
            ["src/**", "Cargo.toml", "docs/", "*.md"],
        )
        .unwrap();
        assert!(s.contains("src/a/b.rs"));
        assert!(s.contains("./src/a.rs"));
        assert!(s.contains("Cargo.toml"));
        assert!(s.contains("docs/x/y.md"));
        assert!(s.contains("README.md"));
        assert!(!s.contains("sub/README.md"), "* does not cross /");
        assert!(!s.contains("crates/x/Cargo.toml"));
        assert!(Scope::new(ScopeSource::Session, ["src/[**"]).is_err());
    }

    #[test]
    fn out_of_scope_writes_ask_or_flag_by_mode() {
        let root = Path::new("/w");
        let s = Scope::new(ScopeSource::Agent("coder".into()), ["src/**"]).unwrap();
        let cwd = Path::new("/w/src");
        assert_eq!(
            s.check_write(root, cwd, Path::new("lib.rs"), Mode::AutoEdit),
            ScopeDecision::InScope
        );
        for mode in [Mode::Ask, Mode::AutoEdit] {
            let d = s.check_write(root, cwd, Path::new("../build.rs"), mode);
            assert!(
                matches!(&d, ScopeDecision::Ask { reason } if reason.contains("`build.rs` is outside the task scope (agent `coder`: src/**)")),
                "{d:?}"
            );
        }
        assert!(matches!(
            s.check_write(root, cwd, Path::new("/w/build.rs"), Mode::Auto),
            ScopeDecision::Flag { .. }
        ));
        // Outside the workspace is never in scope.
        assert!(!s.contains_path(root, cwd, Path::new("/etc/passwd")));
    }

    #[test]
    fn prompt_section_lists_allowed_areas_without_prohibitions() {
        let s = Scope::new(ScopeSource::Session, ["src/**", "tests/**"]).unwrap();
        let text = s.prompt_section();
        assert!(text.contains("- `src/**`") && text.contains("- `tests/**`"));
        let src = harness_config::lint::LintSource {
            kind: harness_config::lint::SourceKind::AgentPrompt,
            label: "scope".into(),
            file: "scope".into(),
            text: text.clone(),
            file_text: None,
        };
        assert!(
            harness_config::lint::blanket_prohibitions(&src).is_empty(),
            "{text}"
        );
        let lower = text.to_lowercase();
        for banned in [
            "do not",
            "don't",
            "never",
            "must not",
            "forbidden",
            "only edit",
        ] {
            assert!(!lower.contains(banned), "found `{banned}` in {text}");
        }
    }
}
