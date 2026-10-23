//! Workflows that ship with harness.
//!
//! | Workflow | Command | What it encodes |
//! |---|---|---|
//! | `implement-verify-review` | `/ivr <task> --test <cmd>` | implement, run the tests, loop until an independent reviewer approves |
//! | `fix-bug` | `/fix <issue>` | failing test first (proved to fail), minimal fix, bug-fix proof (fails before, passes after), review loop |
//! | `plan-approve-implement` | `/plan <task> --test <cmd>` | a plan artifact you approve before any code changes, then implement and verify |
//!
//! They use the built-in agents (`coder`, `researcher`, `reviewer`) and
//! are ordinary [`BuiltinFile`]s, so a global or project workflow with the
//! same name overrides them. The files live in
//! `crates/harness-config/assets/workflows/` and are part of
//! `harness_config::builtins::BUILTINS` (and so of the defaults
//! fingerprint); [`BUNDLED`] names the same files. Each has an eval task in
//! `crates/harness-workflow/evals/workflows/suite.yaml`.

use std::sync::OnceLock;

use harness_config::discovery::BuiltinFile;

macro_rules! bundled {
    ($name:literal) => {
        BuiltinFile {
            path: concat!("workflows/", $name, ".yaml"),
            contents: include_str!(concat!(
                "../../harness-config/assets/workflows/",
                $name,
                ".yaml"
            )),
        }
    };
}

/// The bundled workflow files (paths relative to a scope root).
pub static BUNDLED: &[BuiltinFile] = &[
    bundled!("implement-verify-review"),
    bundled!("fix-bug"),
    bundled!("plan-approve-implement"),
];

/// Names of the bundled workflows.
pub const NAMES: &[&str] = &[
    "implement-verify-review",
    "fix-bug",
    "plan-approve-implement",
];

/// `base` (e.g. `harness_config::builtins::BUILTINS`) plus [`BUNDLED`], as
/// one static slice for [`harness_config::discovery::Roots::resolve`].
/// Files in `base` with the same path win. Computed once.
pub fn with_builtins(base: &'static [BuiltinFile]) -> &'static [BuiltinFile] {
    static ALL: OnceLock<&'static [BuiltinFile]> = OnceLock::new();
    ALL.get_or_init(|| {
        let mut v: Vec<BuiltinFile> = base.to_vec();
        for f in BUNDLED {
            if !v.iter().any(|b| b.path == f.path) {
                v.push(*f);
            }
        }
        Box::leak(v.into_boxed_slice())
    })
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use harness_config::builtins::{AGENTS, BUILTINS};
    use harness_config::discovery::{Catalog, Roots};

    use super::*;
    use crate::schema::{CheckContext, from_entry, parse_with};

    fn ctx() -> CheckContext {
        CheckContext {
            agents: Some(AGENTS.iter().map(|s| (*s).to_owned()).collect()),
            remotes: Some(Default::default()),
            workflows: Some(NAMES.iter().map(|s| (*s).to_owned()).collect()),
            agent_runs_on: Default::default(),
            mcp_servers: None,
        }
    }

    #[test]
    fn bundled_workflows_validate_against_the_builtin_agents() {
        assert_eq!(BUNDLED.len(), NAMES.len());
        for f in BUNDLED {
            let wf = parse_with(f.contents, None, &ctx()).unwrap_or_else(|e| {
                panic!(
                    "{}: {}",
                    f.path,
                    e.iter()
                        .map(ToString::to_string)
                        .collect::<Vec<_>>()
                        .join("\n")
                )
            });
            assert!(NAMES.contains(&wf.name.as_str()));
            assert!(wf.description.is_some());
            assert!(!wf.tasks.is_empty(), "{} seeds the goal bar", wf.name);
        }
    }

    #[test]
    fn they_are_discovered_alongside_the_builtins() {
        let all = with_builtins(BUILTINS);
        // The bundled files are part of BUILTINS, so nothing is added twice.
        assert_eq!(all.len(), BUILTINS.len());
        for f in BUNDLED {
            let b = BUILTINS.iter().find(|b| b.path == f.path);
            assert_eq!(b.map(|b| b.contents), Some(f.contents), "{}", f.path);
        }
        let catalog = Catalog::load(&Roots::new(all));
        let names: BTreeSet<&str> = catalog
            .workflows
            .active()
            .iter()
            .map(|e| e.item.name.as_str())
            .collect();
        for n in NAMES {
            assert!(names.contains(n), "{n} missing from {names:?}");
            let entry = catalog.workflows.get_entry(n).unwrap();
            from_entry(entry, &CheckContext::from_catalog(&catalog, None)).unwrap();
        }
        let commands: BTreeSet<&str> = catalog
            .workflows
            .active()
            .iter()
            .map(|e| e.item.command.as_str())
            .collect();
        assert!(commands.is_superset(&["fix", "ivr", "plan"].into()));
    }
}
