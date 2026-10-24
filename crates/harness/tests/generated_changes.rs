//! A real bash run of a small python test suite through the `harness`
//! binary: the bytecode it writes (`__pycache__/*.pyc`) is recorded as
//! generated, so the turn does not end "unverified" and an auto-detected
//! verify command (`verify_when = on_changes`) does not run. A bash command
//! that changes a source file still counts.

mod common;

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;

use common::*;
use harness_events::schema::RunEvent;
use harness_tui::chat::transcript::{EntryKind, Transcript, ViewSettings};
use serde_json::Value;

/// A git repository with a tiny python package, its unittest suite, a
/// `run_tests.sh` and a `Makefile` (so `make test` is the detected verify
/// command). No `.gitignore`: the built-in artifact list must apply.
fn python_project(env: &Env) {
    let p = &env.project;
    std::fs::remove_file(p.join("secret.txt")).unwrap();
    std::fs::create_dir_all(p.join("calc")).unwrap();
    std::fs::create_dir_all(p.join("tests")).unwrap();
    std::fs::write(
        p.join("calc/__init__.py"),
        "def add(a, b):\n    return a + b\n",
    )
    .unwrap();
    std::fs::write(p.join("tests/__init__.py"), "").unwrap();
    std::fs::write(
        p.join("tests/test_calc.py"),
        "import unittest\nfrom calc import add\n\n\nclass T(unittest.TestCase):\n    def test_add(self):\n        self.assertEqual(add(1, 2), 3)\n",
    )
    .unwrap();
    std::fs::write(
        p.join("run_tests.sh"),
        "#!/bin/sh\nexec python3 -m unittest discover -s tests -t .\n",
    )
    .unwrap();
    std::fs::set_permissions(
        p.join("run_tests.sh"),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    std::fs::write(p.join("Makefile"), "test:\n\t@echo verify-ran\n").unwrap();
    git(p, &["init", "-q"]);
    git(p, &["add", "."]);
    git(p, &["commit", "-q", "-m", "init"]);
}

fn git(dir: &Path, args: &[&str]) {
    let mut c = Command::new("git");
    for (k, _) in std::env::vars_os() {
        if k.to_string_lossy().starts_with("GIT_") {
            c.env_remove(&k);
        }
    }
    let out = c
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .args([
            "-c",
            "user.name=u",
            "-c",
            "user.email=u@x",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn bash_then_done(command: &str) -> String {
    format!(
        "responses:\n  - content: [ {{ tool_use: {{ name: bash, input: {{ command: {command:?} }} }} }} ]\n  - content: [ {{ text: \"All tests pass.\" }} ]\n"
    )
}

/// The notices the TUI transcript shows for the run's events.
fn notices(evs: &[Value]) -> Vec<String> {
    let mut t = Transcript::new(ViewSettings::default());
    for e in evs {
        let ev: RunEvent = serde_json::from_value(e.clone()).unwrap();
        t.apply(&ev);
    }
    t.entries()
        .iter()
        .filter_map(|e| match &e.kind {
            EntryKind::Notice(n) => Some(n.text.clone()),
            _ => None,
        })
        .collect()
}

fn file_changes(evs: &[Value]) -> Vec<(String, bool)> {
    evs.iter()
        .filter(|e| e["type"] == "file_changed")
        .map(|e| {
            (
                e["path"].as_str().unwrap().to_owned(),
                e["generated"].as_bool().unwrap_or(false),
            )
        })
        .collect()
}

#[test]
fn a_python_test_run_only_writes_generated_files_and_ends_without_an_unverified_notice() {
    let env = Env::new("");
    python_project(&env);
    let script = env.script("s.yaml", &bash_then_done("./run_tests.sh"));
    let o = env.run(&script, &["--json", "--mode", "auto", "run the tests"]);
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
    let evs = events(&o);
    // The suite really ran and wrote bytecode.
    let output: String = evs
        .iter()
        .filter(|e| e["type"] == "tool_output")
        .filter_map(|e| e["text"].as_str())
        .collect();
    assert!(output.contains("OK"), "{output}");
    assert!(env.project.join("calc/__pycache__").is_dir());

    // Every change is a generated artifact, still in the log.
    let changes = file_changes(&evs);
    assert!(!changes.is_empty(), "the .pyc files are recorded");
    assert!(
        changes
            .iter()
            .all(|(p, g)| *g && p.contains("__pycache__/") && p.ends_with(".pyc")),
        "{changes:?}"
    );
    // `make test` is detected, so verification runs only on changes: none.
    assert!(
        !evs.iter().any(|e| e["type"] == "verify_started"),
        "verification ran for generated files only"
    );
    let n = notices(&evs);
    assert!(
        !n.iter().any(|t| t.starts_with("unverified")),
        "notices: {n:?}"
    );
}

#[test]
fn a_bash_command_that_changes_a_source_file_still_counts() {
    let env = Env::new("");
    python_project(&env);
    // A formatter-like in-place edit, then the suite.
    let script = env.script(
        "s.yaml",
        &bash_then_done("printf '\\n# formatted\\n' >> calc/__init__.py && ./run_tests.sh"),
    );
    let o = env.run(
        &script,
        &["--json", "--mode", "auto", "--no-verify", "format and test"],
    );
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
    let evs = events(&o);
    let changes = file_changes(&evs);
    assert!(
        changes.contains(&("calc/__init__.py".to_owned(), false)),
        "{changes:?}"
    );
    let counted = changes.iter().filter(|(_, g)| !g).count();
    assert_eq!(counted, 1, "{changes:?}");
    assert!(
        changes.iter().any(|(p, g)| *g && p.ends_with(".pyc")),
        "the suite ran and its bytecode is labelled generated: {changes:?}"
    );
    let n = notices(&evs);
    assert_eq!(
        n.last().map(String::as_str),
        Some("unverified: 1 file change and no verify command ran"),
        "notices: {n:?}"
    );

    // With the detected verify command, the change is verified.
    let env = Env::new("");
    python_project(&env);
    let script = env.script(
        "s.yaml",
        &bash_then_done("printf '\\n# formatted\\n' >> calc/__init__.py && ./run_tests.sh"),
    );
    let o = env.run(&script, &["--json", "--mode", "auto", "format and test"]);
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
    let evs = events(&o);
    assert!(
        evs.iter()
            .any(|e| e["type"] == "verify_started" && e["command"] == "make test"),
        "on_changes verification ran"
    );
}

#[test]
fn a_tracked_file_under_a_target_directory_still_counts_and_is_verified() {
    let env = Env::new("");
    python_project(&env);
    let p = &env.project;
    std::fs::create_dir_all(p.join("infra/target")).unwrap();
    std::fs::write(p.join("infra/target/config.txt"), "port = 1\n").unwrap();
    git(p, &["add", "infra/target/config.txt"]);
    git(p, &["commit", "-q", "-m", "config"]);
    let script = env.script(
        "s.yaml",
        &bash_then_done("printf 'port = 2\\n' > infra/target/config.txt"),
    );
    let o = env.run(&script, &["--json", "--mode", "auto", "bump the port"]);
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
    let evs = events(&o);
    assert_eq!(
        file_changes(&evs),
        vec![("infra/target/config.txt".to_owned(), false)]
    );
    assert!(
        evs.iter()
            .any(|e| e["type"] == "verify_started" && e["command"] == "make test"),
        "on_changes verification ran for the tracked file"
    );
}

#[test]
fn a_gitignore_above_the_repository_does_not_hide_bash_changes() {
    // A dotfiles home: the directory around the project is a repository
    // whose .gitignore ignores everything; the project is its own
    // repository.
    let env = Env::new("");
    python_project(&env);
    git(&env.root, &["init", "-q"]);
    std::fs::write(env.root.join(".gitignore"), "*\n!.bashrc\n").unwrap();
    let script = env.script(
        "s.yaml",
        &bash_then_done("echo '# b' >> calc/__init__.py; echo n > new.py"),
    );
    let o = env.run(
        &script,
        &["--json", "--mode", "auto", "--no-verify", "edit it"],
    );
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
    let evs = events(&o);
    assert_eq!(
        file_changes(&evs),
        vec![
            ("calc/__init__.py".to_owned(), false),
            ("new.py".to_owned(), false),
        ]
    );
    let n = notices(&evs);
    assert_eq!(
        n.last().map(String::as_str),
        Some("unverified: 2 file changes and no verify command ran"),
        "notices: {n:?}"
    );
}

#[test]
fn bash_changes_in_a_directory_a_dotfiles_home_ignores_still_count() {
    // A dotfiles home whose .gitignore ignores everything; the project is
    // a plain directory below it (not its own repository) with one file
    // force-added to the home repository.
    let env = Env::new("");
    let p = &env.project;
    std::fs::remove_file(p.join("secret.txt")).unwrap();
    std::fs::write(p.join("tracked.txt"), "one\n").unwrap();
    std::fs::write(p.join("main.py"), "print(1)\n").unwrap();
    git(&env.root, &["init", "-q"]);
    std::fs::write(env.root.join(".gitignore"), "*\n").unwrap();
    git(&env.root, &["add", "-f", "project/tracked.txt"]);
    let script = env.script(
        "s.yaml",
        &bash_then_done("echo two >> tracked.txt; echo '# x' >> main.py"),
    );
    let o = env.run(
        &script,
        &["--json", "--mode", "auto", "--no-verify", "edit it"],
    );
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
    let evs = events(&o);
    assert_eq!(
        file_changes(&evs),
        vec![
            ("main.py".to_owned(), false),
            ("tracked.txt".to_owned(), false),
        ]
    );
    let n = notices(&evs);
    assert_eq!(
        n.last().map(String::as_str),
        Some("unverified: 2 file changes and no verify command ran"),
        "notices: {n:?}"
    );
}
