//! Tests never touch the user's real config or data (TASK-147).
//!
//! `scripts/hermetic-guard.sh` runs the harness and harness-eval suites
//! with HOME and the XDG base directories pointed at a sentinel directory
//! and fails if anything appears there; CI runs it after the test step.
//! These tests check the guard itself (with a fake cargo), that CI runs it,
//! and that every test spawning the `harness` binary isolates it.
#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

mod common;

fn root() -> PathBuf {
    common::workspace_root()
}

fn read(rel: &str) -> String {
    std::fs::read_to_string(root().join(rel)).unwrap_or_else(|e| panic!("reading {rel}: {e}"))
}

/// Runs the guard with a fake cargo whose body is `script`.
fn guard(tmp: &Path, script: &str) -> Output {
    let src = tmp.join("cargo.src");
    std::fs::write(&src, format!("#!/bin/sh\n{script}")).unwrap();
    let cargo = tmp.join("cargo");
    // Created by a child process, never written by this multi-threaded
    // process (an inherited write descriptor makes exec fail with ETXTBSY).
    let st = Command::new("install")
        .args(["-m", "755"])
        .arg(&src)
        .arg(&cargo)
        .status()
        .unwrap();
    assert!(st.success());
    assert!(cargo.metadata().unwrap().permissions().mode() & 0o100 != 0);
    Command::new(root().join("scripts/hermetic-guard.sh"))
        .args(["test", "-p", "harness"])
        .env("HARNESS_GUARD_CARGO", &cargo)
        .env("TMPDIR", tmp)
        .env("HARNESS_CONFIG", "/real/harness.ini")
        .env("HARNESS_DATA_DIR", "/real/data")
        .output()
        .unwrap()
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

#[test]
fn guard_fails_when_a_test_writes_to_home_or_xdg_dirs() {
    let tmp = tempfile::tempdir().unwrap();
    let o = guard(
        tmp.path(),
        "mkdir -p \"$XDG_DATA_HOME/harness/sessions\" \"$HOME/.config/harness\"\n\
         echo x > \"$XDG_DATA_HOME/harness/sessions/leak.json\"\n",
    );
    assert_eq!(o.status.code(), Some(1), "{}", text(&o.stderr));
    let err = text(&o.stderr);
    assert!(err.contains("tests wrote to HOME or XDG"), "{err}");
    assert!(err.contains("data/harness/sessions/leak.json"), "{err}");
    assert!(err.contains("home/.config/harness"), "{err}");
    // The sentinel is removed afterwards.
    let left: Vec<_> = std::fs::read_dir(tmp.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.file_name()
                .to_string_lossy()
                .starts_with("harness-hermetic")
        })
        .collect();
    assert!(left.is_empty(), "sentinel left behind");
}

#[test]
fn guard_passes_clean_runs_and_forwards_test_failures() {
    let tmp = tempfile::tempdir().unwrap();
    let env_dump = tmp.path().join("env.txt");
    // A clean run: sockets in the runtime dir are fine; the fake cargo
    // records the environment it got.
    let o = guard(
        tmp.path(),
        &format!(
            "mkdir -p \"$XDG_RUNTIME_DIR/harness\"\n\
             echo \"args=$*\" > {d}\n\
             env >> {d}\n",
            d = env_dump.display()
        ),
    );
    assert!(o.status.success(), "{}", text(&o.stderr));
    assert!(text(&o.stdout).contains("hermetic-guard: ok"));
    let env = std::fs::read_to_string(&env_dump).unwrap();
    assert!(env.contains("args=test -p harness"), "{env}");
    let var = |k: &str| {
        env.lines()
            .find_map(|l| l.strip_prefix(&format!("{k}=")))
            .map(str::to_owned)
    };
    for k in [
        "HOME",
        "XDG_CONFIG_HOME",
        "XDG_DATA_HOME",
        "XDG_CACHE_HOME",
        "XDG_STATE_HOME",
    ] {
        let v = var(k).unwrap_or_else(|| panic!("{k} not set: {env}"));
        assert!(v.contains("harness-hermetic"), "{k}={v}");
    }
    assert_eq!(var("HARNESS_CONFIG"), None, "{env}");
    assert_eq!(var("HARNESS_DATA_DIR"), None, "{env}");
    let cargo_home = var("CARGO_HOME").expect("CARGO_HOME kept");
    assert!(!cargo_home.contains("harness-hermetic"), "{cargo_home}");

    let o = guard(tmp.path(), "exit 3\n");
    assert_eq!(o.status.code(), Some(3), "{}", text(&o.stderr));
    assert!(text(&o.stderr).contains("tests failed"));
}

#[test]
fn ci_runs_the_guard_on_the_harness_and_eval_suites() {
    let ci = read(".github/workflows/ci.yml");
    let line = ci
        .lines()
        .find(|l| l.contains("scripts/hermetic-guard.sh"))
        .expect("ci.yml runs scripts/hermetic-guard.sh");
    assert!(
        line.contains("-p harness ") && line.contains("-p harness-eval"),
        "{line}"
    );
    let script = read("scripts/hermetic-guard.sh");
    assert!(script.contains("-p harness -p harness-eval"), "default run");
}

#[test]
fn hermetic_commands_point_every_location_at_the_temp_root() {
    let root = common::hermetic_root();
    let mut c = Command::new("env");
    common::hermetic(&mut c, root.path());
    let env = text(&c.output().unwrap().stdout);
    let r = root.path().display().to_string();
    for k in [
        "HARNESS_CONFIG",
        "HOME",
        "XDG_CONFIG_HOME",
        "XDG_DATA_HOME",
        "XDG_CACHE_HOME",
        "XDG_STATE_HOME",
    ] {
        let v = env
            .lines()
            .find_map(|l| l.strip_prefix(&format!("{k}=")))
            .unwrap_or_else(|| panic!("{k} not set"));
        assert!(v.starts_with(&r), "{k}={v} is outside {r}");
    }
    assert!(!env.contains("HARNESS_DATA_DIR="), "{env}");
    assert!(!env.contains("HARNESS_LOG="), "{env}");
}

/// Every test file that starts the `harness` binary isolates each command
/// with `common::hermetic` (directly or through `Env::cmd`), and every
/// eval test that runs it through `ProcessExecutor` passes a hermetic env.
#[test]
fn every_test_that_runs_the_binary_is_hermetic() {
    let spawn = "Command::new(env!(\"CARGO_BIN_EXE_harness\"))";
    for (dir, rel) in [
        ("crates/harness/tests", "harness"),
        ("crates/harness/tests/common", "harness/common"),
    ] {
        for e in std::fs::read_dir(root().join(dir)).unwrap() {
            let p = e.unwrap().path();
            if p.extension().is_none_or(|x| x != "rs") {
                continue;
            }
            let src = std::fs::read_to_string(&p).unwrap();
            let spawns = src.matches(spawn).count();
            let isolated = src.matches("hermetic(&mut").count();
            assert!(
                isolated >= spawns,
                "{rel}/{}: {spawns} spawns of the binary but {isolated} `hermetic` calls",
                p.file_name().unwrap().to_string_lossy()
            );
        }
    }
    let eval = root().join("crates/harness-eval/tests");
    for e in std::fs::read_dir(eval).unwrap() {
        let p = e.unwrap().path();
        let src = std::fs::read_to_string(&p).unwrap_or_default();
        for (i, _) in src.match_indices("ProcessExecutor::new(") {
            if src[..i].ends_with(|c: char| c.is_alphanumeric()) {
                continue; // InProcessExecutor
            }
            let call = &src[i..src[i..].find(';').map_or(src.len(), |j| i + j)];
            assert!(
                call.contains(".with_env("),
                "{}: ProcessExecutor without a hermetic env: {call}",
                p.display()
            );
        }
    }
}
