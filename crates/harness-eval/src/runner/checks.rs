//! Post-run checks and workspace hygiene analysis.

use std::path::Path;
use std::time::Duration;

use globset::{Glob, GlobSet, GlobSetBuilder};
use harness_events::schema::FileChangeKind;
use harness_hygiene::snapshot::{Snapshot, SnapshotOptions};
use serde::{Deserialize, Serialize};

use super::workspace::run_shell;
use crate::suite::{Check, Expect, FileCheck, HygieneCheck};

/// Longest a check command may run.
pub const CHECK_TIMEOUT: Duration = Duration::from_secs(600);

/// The result of one check.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckResult {
    /// What was checked.
    pub check: String,
    /// Whether it passed.
    pub passed: bool,
    /// Why it failed (or command output), empty when passed.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub detail: String,
}

/// Files the agent left behind that hurt hygiene.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HygieneFindings {
    /// Created or modified temporary/debug files (with the reason).
    pub temp_files: Vec<String>,
    /// Changed files outside the task scope (only when a scope is given).
    pub out_of_scope: Vec<String>,
    /// Every changed path.
    pub changed: Vec<String>,
}

impl HygieneFindings {
    /// Number of violations (temporary files + out-of-scope changes).
    pub fn violations(&self) -> u32 {
        (self.temp_files.len() + self.out_of_scope.len()) as u32
    }
}

fn globset(globs: &[String]) -> GlobSet {
    let mut b = GlobSetBuilder::new();
    for g in globs {
        if let Ok(g) = Glob::new(g) {
            b.add(g);
        }
    }
    b.build().unwrap_or_else(|_| GlobSet::empty())
}

/// Snapshot of a workspace (gitignore-aware, `.git` skipped).
pub fn snapshot(root: &Path) -> Snapshot {
    Snapshot::take(root, &SnapshotOptions::default()).unwrap_or_default()
}

/// Compares the workspace before and after the agent ran. `scope` is the
/// union of hygiene-check scopes (empty = no scope).
pub fn hygiene(before: &Snapshot, after: &Snapshot, scope: &[String]) -> HygieneFindings {
    let scope_set = (!scope.is_empty()).then(|| globset(scope));
    let mut out = HygieneFindings::default();
    for change in before.diff(after) {
        out.changed.push(change.path.clone());
        if change.kind != FileChangeKind::Deleted
            && let Some(reason) = harness_hygiene::report::temp_or_debug(&change.path)
        {
            out.temp_files.push(format!("{} ({reason})", change.path));
            continue;
        }
        if let Some(set) = &scope_set
            && !set.is_match(&change.path)
        {
            out.out_of_scope.push(change.path.clone());
        }
    }
    out
}

/// Runs every check in `root`. `before` is the workspace snapshot taken after
/// setup, `after` the one taken after the agent.
pub async fn run_checks(
    checks: &[Check],
    root: &Path,
    before: &Snapshot,
    after: &Snapshot,
) -> Vec<CheckResult> {
    let mut out = Vec::with_capacity(checks.len());
    for c in checks {
        let label = c.to_string();
        let (passed, detail) = match c {
            Check::Command(cmd) => match run_shell(root, &cmd.command, CHECK_TIMEOUT).await {
                Ok(o) => {
                    let ok = !o.timed_out
                        && match cmd.expect {
                            Expect::Pass => o.code == Some(0),
                            Expect::Fail => o.code.is_some_and(|c| c != 0),
                        };
                    let detail = if ok {
                        String::new()
                    } else {
                        format!("exit {:?}: {}", o.code, o.output.trim())
                    };
                    (ok, detail)
                }
                Err(e) => (false, e),
            },
            Check::File { file } => file_check(root, file),
            Check::Unchanged { unchanged } => unchanged_check(before, after, unchanged),
            Check::Hygiene { hygiene: h } => hygiene_check(before, after, h),
        };
        out.push(CheckResult {
            check: label,
            passed,
            detail,
        });
    }
    out
}

fn file_check(root: &Path, f: &FileCheck) -> (bool, String) {
    let path = root.join(&f.path);
    let want_exists = f.exists.unwrap_or(true);
    let text = std::fs::read(&path).ok();
    match (want_exists, &text) {
        (true, None) => return (false, format!("{} does not exist", f.path)),
        (false, Some(_)) => return (false, format!("{} exists", f.path)),
        (false, None) => return (true, String::new()),
        (true, Some(_)) => {}
    }
    let text = String::from_utf8_lossy(text.as_deref().unwrap_or_default());
    let mut problems = Vec::new();
    for c in &f.contains {
        if !text.contains(c.as_str()) {
            problems.push(format!("missing `{c}`"));
        }
    }
    for c in &f.not_contains {
        if text.contains(c.as_str()) {
            problems.push(format!("contains `{c}`"));
        }
    }
    (problems.is_empty(), problems.join("; "))
}

fn unchanged_check(before: &Snapshot, after: &Snapshot, globs: &[String]) -> (bool, String) {
    let set = globset(globs);
    let changed: Vec<String> = before
        .diff(after)
        .into_iter()
        .filter(|c| set.is_match(&c.path))
        .map(|c| c.path)
        .collect();
    if changed.is_empty() {
        (true, String::new())
    } else {
        (false, format!("changed: {}", changed.join(", ")))
    }
}

fn hygiene_check(before: &Snapshot, after: &Snapshot, h: &HygieneCheck) -> (bool, String) {
    let found = hygiene(before, after, &h.scope);
    let mut problems = Vec::new();
    let max_temp = h.max_temp_files.unwrap_or(0) as usize;
    if found.temp_files.len() > max_temp {
        problems.push(format!(
            "{} temporary file(s) left (max {max_temp}): {}",
            found.temp_files.len(),
            found.temp_files.join(", ")
        ));
    }
    if !h.scope.is_empty() {
        let max = h.max_out_of_scope.unwrap_or(0) as usize;
        if found.out_of_scope.len() > max {
            problems.push(format!(
                "{} out-of-scope change(s) (max {max}): {}",
                found.out_of_scope.len(),
                found.out_of_scope.join(", ")
            ));
        }
    }
    (problems.is_empty(), problems.join("; "))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::suite::CommandCheck;

    fn write(root: &Path, rel: &str, text: &str) {
        let p = root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    }

    #[tokio::test]
    async fn checks_commands_files_unchanged_and_hygiene() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(root, "src/lib.rs", "fn a() {}\n");
        write(root, "tests/t.rs", "#[test] fn t() {}\n");
        let before = snapshot(root);
        write(root, "src/lib.rs", "fn a() {}\nfn double() {}\n");
        write(root, "tests/t.rs", "// weakened\n");
        write(root, "debug.log", "noise");
        write(root, "README.md", "changed");
        let after = snapshot(root);

        let checks = vec![
            Check::Command(CommandCheck {
                command: "true".into(),
                expect: Expect::Pass,
            }),
            Check::Command(CommandCheck {
                command: "false".into(),
                expect: Expect::Fail,
            }),
            Check::Command(CommandCheck {
                command: "exit 2".into(),
                expect: Expect::Pass,
            }),
            Check::File {
                file: FileCheck {
                    path: "src/lib.rs".into(),
                    exists: None,
                    contains: vec!["fn double".into()],
                    not_contains: vec!["todo!".into()],
                },
            },
            Check::File {
                file: FileCheck {
                    path: "debug.log".into(),
                    exists: Some(false),
                    contains: vec![],
                    not_contains: vec![],
                },
            },
            Check::Unchanged {
                unchanged: vec!["tests/**".into()],
            },
            Check::Hygiene {
                hygiene: HygieneCheck {
                    max_temp_files: Some(0),
                    scope: vec!["src/**".into()],
                    max_out_of_scope: None,
                },
            },
        ];
        let r = run_checks(&checks, root, &before, &after).await;
        let passed: Vec<bool> = r.iter().map(|c| c.passed).collect();
        assert_eq!(passed, [true, true, false, true, false, false, false]);
        assert!(r[2].detail.contains("exit Some(2)"));
        assert!(r[4].detail.contains("exists"));
        assert!(r[5].detail.contains("tests/t.rs"));
        assert!(r[6].detail.contains("debug.log"), "{}", r[6].detail);
        assert!(r[6].detail.contains("README.md"), "{}", r[6].detail);

        let h = hygiene(&before, &after, &["src/**".into()]);
        assert_eq!(h.temp_files.len(), 1);
        assert_eq!(h.out_of_scope, ["README.md", "tests/t.rs"]);
        assert_eq!(h.violations(), 3);
        assert_eq!(h.changed.len(), 4);
        // Without a scope only temp files count.
        assert_eq!(hygiene(&before, &after, &[]).violations(), 1);
    }
}
