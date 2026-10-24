//! The CI harness-overhead guard (TASK-161.6): `scripts/bench-guard.sh`
//! runs `harness bench --fake` and fails when the best overhead per call or
//! session start of an attempt passes its limit in every attempt. These
//! tests drive the script with a fake `harness` that writes a chosen report
//! (so pass, retry and fail are deterministic), once with the real binary
//! and limits no machine can miss (so a renamed report field fails here,
//! not in CI), and check that CI runs it on both OSes and that the limits
//! in the script and docs/performance.md agree. No timing is asserted.
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

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

/// A fake `harness` whose attempt `k` (1-based) reports line `k` of
/// `reports` as `<overhead> <session start>`, and records its environment.
fn fake_harness(tmp: &Path, reports: &[&str]) -> PathBuf {
    std::fs::write(tmp.join("reports"), reports.join("\n") + "\n").unwrap();
    let state = tmp.display();
    let script = format!(
        r#"#!/bin/sh
n=$(cat "{state}/count" 2>/dev/null || echo 0)
n=$((n + 1))
echo "$n" > "{state}/count"
env > "{state}/env"
echo "$@" > "{state}/args"
pwd > "{state}/pwd"
set -- $(sed -n "${{n}}p" "{state}/reports") "$@"
o=$1; s=$2; shift 2
while [ $# -gt 0 ]; do
    if [ "$1" = --json ]; then out=$2; fi
    shift
done
if [ "$o" = fail ]; then exit 4; fi
if [ "$o" = none ]; then echo '{{"summary":{{}}}}' > "$out"; exit 0; fi
printf '{{"summary":{{"overhead_per_call_ms":{{"min":%s}},"session_start_ms":{{"min":%s}}}}}}\n' "$o" "$s" > "$out"
"#
    );
    let src = tmp.join("harness.src");
    std::fs::write(&src, script).unwrap();
    let bin = tmp.join("harness");
    // Installed by a child process, never written by this multi-threaded
    // process (an inherited write descriptor makes exec fail with ETXTBSY).
    let st = Command::new("install")
        .args(["-m", "755"])
        .arg(&src)
        .arg(&bin)
        .status()
        .unwrap();
    assert!(st.success());
    assert!(bin.metadata().unwrap().permissions().mode() & 0o100 != 0);
    bin
}

fn guard(tmp: &Path, bin: &Path, env: &[(&str, &str)]) -> Output {
    let mut c = Command::new(root().join("scripts/bench-guard.sh"));
    c.arg(bin)
        .env("TMPDIR", tmp)
        .env("HARNESS_CONFIG", "/real/harness.ini")
        .env("HARNESS_DATA_DIR", "/real/data")
        .env_remove("BENCH_GUARD_MAX_OVERHEAD_MS")
        .env_remove("BENCH_GUARD_MAX_SESSION_START_MS")
        .env_remove("BENCH_GUARD_RUNS")
        .env_remove("BENCH_GUARD_ATTEMPTS");
    for (k, v) in env {
        c.env(k, v);
    }
    c.output().unwrap()
}

fn attempts(tmp: &Path) -> u32 {
    std::fs::read_to_string(tmp.join("count")).map_or(0, |s| s.trim().parse().unwrap())
}

/// Temp dirs the guard left behind under `tmp`.
fn leftovers(tmp: &Path) -> Vec<String> {
    std::fs::read_dir(tmp)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with("harness-bench-guard"))
        .collect()
}

#[test]
fn guard_passes_under_the_limits_and_isolates_the_run() {
    let tmp = tempfile::tempdir().unwrap();
    let bin = fake_harness(tmp.path(), &["3.2 11.0"]);
    let o = guard(tmp.path(), &bin, &[]);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
    let out = text(&o.stdout);
    assert!(out.contains("overhead per call 3.200 ms"), "{out}");
    assert!(out.contains("session start 11.000 ms"), "{out}");
    assert!(out.contains("bench-guard: ok"), "{out}");
    assert_eq!(attempts(tmp.path()), 1);
    let args = std::fs::read_to_string(tmp.path().join("args")).unwrap();
    assert!(
        args.starts_with("bench --fake --runs 7 --no-render --json "),
        "{args}"
    );
    // Fresh HOME/XDG/TMPDIR under the guard's temp dir; the user's config
    // and data dir are not passed through.
    let env = std::fs::read_to_string(tmp.path().join("env")).unwrap();
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
        "TMPDIR",
    ] {
        let v = var(k).unwrap_or_else(|| panic!("{k} not set: {env}"));
        assert!(v.contains("harness-bench-guard"), "{k}={v}");
    }
    assert_eq!(var("HARNESS_CONFIG"), None, "{env}");
    assert_eq!(var("HARNESS_DATA_DIR"), None, "{env}");
    let pwd = std::fs::read_to_string(tmp.path().join("pwd")).unwrap();
    assert!(pwd.contains("harness-bench-guard"), "{pwd}");
    assert!(leftovers(tmp.path()).is_empty());
}

#[test]
fn guard_retries_a_noisy_attempt_and_fails_only_when_every_attempt_is_over() {
    // Over on the first attempt (runner noise), under on the second.
    let tmp = tempfile::tempdir().unwrap();
    let bin = fake_harness(tmp.path(), &["40 11", "3.3 11"]);
    let o = guard(tmp.path(), &bin, &[]);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
    assert!(
        text(&o.stdout).contains("attempt 1/3"),
        "{}",
        text(&o.stdout)
    );
    assert_eq!(attempts(tmp.path()), 2);

    // Session start over in all three attempts.
    let tmp = tempfile::tempdir().unwrap();
    let bin = fake_harness(tmp.path(), &["3 90", "3 80", "3 70"]);
    let o = guard(tmp.path(), &bin, &[]);
    assert_eq!(o.status.code(), Some(1), "{}", text(&o.stdout));
    assert_eq!(attempts(tmp.path()), 3);
    let err = text(&o.stderr);
    assert!(
        err.contains("regressed past the threshold in all 3 attempts"),
        "{err}"
    );
    assert!(err.contains("docs/performance.md"), "{err}");
    assert!(leftovers(tmp.path()).is_empty());

    // Limits, runs and attempts come from the environment.
    let tmp = tempfile::tempdir().unwrap();
    let bin = fake_harness(tmp.path(), &["3 11"]);
    let o = guard(
        tmp.path(),
        &bin,
        &[
            ("BENCH_GUARD_MAX_OVERHEAD_MS", "2.5"),
            ("BENCH_GUARD_ATTEMPTS", "1"),
            ("BENCH_GUARD_RUNS", "3"),
        ],
    );
    assert_eq!(o.status.code(), Some(1), "{}", text(&o.stdout));
    assert!(text(&o.stdout).contains("(max 2.5)"), "{}", text(&o.stdout));
    let args = std::fs::read_to_string(tmp.path().join("args")).unwrap();
    assert!(args.contains("--runs 3 "), "{args}");
}

#[test]
fn guard_fails_on_a_failed_bench_a_missing_metric_or_a_missing_binary() {
    let tmp = tempfile::tempdir().unwrap();
    let bin = fake_harness(tmp.path(), &["fail 0"]);
    let o = guard(tmp.path(), &bin, &[]);
    assert_eq!(o.status.code(), Some(1));
    assert!(text(&o.stderr).contains("harness bench --fake failed"));
    assert_eq!(attempts(tmp.path()), 1, "a broken bench is not retried");

    let tmp = tempfile::tempdir().unwrap();
    let bin = fake_harness(tmp.path(), &["none 0"]);
    let o = guard(tmp.path(), &bin, &[]);
    assert_eq!(o.status.code(), Some(1));
    assert!(
        text(&o.stderr).contains("no summary.overhead_per_call_ms.min"),
        "{}",
        text(&o.stderr)
    );

    let tmp = tempfile::tempdir().unwrap();
    let o = guard(tmp.path(), &tmp.path().join("missing"), &[]);
    assert_eq!(o.status.code(), Some(2));
    assert!(text(&o.stderr).contains("cargo build -p harness"));
}

/// The real binary's report has the fields the guard reads. The limits are
/// unreachable, so this checks wiring, not speed.
#[test]
fn guard_reads_the_real_bench_report() {
    let tmp = tempfile::tempdir().unwrap();
    let home = common::hermetic_root();
    let mut c = Command::new(root().join("scripts/bench-guard.sh"));
    common::hermetic(&mut c, home.path());
    let o = c
        .arg(env!("CARGO_BIN_EXE_harness"))
        .env("TMPDIR", tmp.path())
        .env("BENCH_GUARD_MAX_OVERHEAD_MS", "1000000")
        .env("BENCH_GUARD_MAX_SESSION_START_MS", "1000000")
        .env("BENCH_GUARD_RUNS", "2")
        .env("BENCH_GUARD_ATTEMPTS", "1")
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
    let out = text(&o.stdout);
    assert!(out.contains("best of 2 runs"), "{out}");
    assert!(out.contains("bench-guard: ok"), "{out}");
    assert!(leftovers(tmp.path()).is_empty());
}

/// CI calls the guard with a relative binary path (`target/debug/harness`
/// from the repo root); the guard runs the binary from inside its temp dir,
/// so the path must be resolved against the caller's cwd first.
#[test]
fn guard_accepts_a_relative_binary_path() {
    let tmp = tempfile::tempdir().unwrap();
    let fake = fake_harness(tmp.path(), &["1 2"]);
    // `<dir>/harness` relative to the guard's cwd, and a bare name (no
    // slash, which `env` would otherwise look up on PATH).
    let dir = fake.parent().unwrap();
    for (cwd, rel) in [
        (
            dir.parent().unwrap(),
            Path::new(dir.file_name().unwrap()).join("harness"),
        ),
        (dir, PathBuf::from("harness")),
    ] {
        std::fs::remove_file(tmp.path().join("count")).ok();
        let mut c = Command::new(root().join("scripts/bench-guard.sh"));
        c.current_dir(cwd);
        let home = common::hermetic_root();
        common::hermetic(&mut c, home.path());
        let o = c
            .arg(&rel)
            .env("TMPDIR", tmp.path())
            .env_remove("BENCH_GUARD_MAX_OVERHEAD_MS")
            .env_remove("BENCH_GUARD_MAX_SESSION_START_MS")
            .env_remove("BENCH_GUARD_RUNS")
            .env_remove("BENCH_GUARD_ATTEMPTS")
            .output()
            .unwrap();
        assert_eq!(o.status.code(), Some(0), "{rel:?}: {}", text(&o.stderr));
        assert!(text(&o.stdout).contains("bench-guard: ok"));
        assert_eq!(attempts(tmp.path()), 1, "{rel:?}");
    }
    // A missing relative path still fails with the usage error, naming it.
    let mut c = Command::new(root().join("scripts/bench-guard.sh"));
    let o = c
        .current_dir(tmp.path())
        .arg("no-such-dir/harness")
        .env("TMPDIR", tmp.path())
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(2));
    assert!(
        text(&o.stderr).contains("no-such-dir/harness not found"),
        "{}",
        text(&o.stderr)
    );
    assert!(leftovers(tmp.path()).is_empty());
}

/// The real binary, invoked the way CI does: `<profile dir>/harness`
/// relative to the target dir (CI: `target/debug/harness` from the root).
#[test]
fn guard_runs_the_real_binary_from_a_relative_path() {
    let tmp = tempfile::tempdir().unwrap();
    let home = common::hermetic_root();
    let bin = Path::new(env!("CARGO_BIN_EXE_harness"));
    let profile = bin.parent().unwrap();
    let rel = Path::new(profile.file_name().unwrap()).join(bin.file_name().unwrap());
    let mut c = Command::new(root().join("scripts/bench-guard.sh"));
    common::hermetic(&mut c, home.path());
    let o = c
        .current_dir(profile.parent().unwrap())
        .arg(&rel)
        .env("TMPDIR", tmp.path())
        .env("BENCH_GUARD_MAX_OVERHEAD_MS", "1000000")
        .env("BENCH_GUARD_MAX_SESSION_START_MS", "1000000")
        .env("BENCH_GUARD_RUNS", "1")
        .env("BENCH_GUARD_ATTEMPTS", "1")
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(0), "{rel:?}: {}", text(&o.stderr));
    assert!(
        text(&o.stdout).contains("bench-guard: ok"),
        "{}",
        text(&o.stdout)
    );
    assert!(leftovers(tmp.path()).is_empty());
}

/// Default limit `name` in scripts/bench-guard.sh (`${NAME:-value}`).
fn script_default(script: &str, name: &str) -> String {
    let pat = format!("${{{name}:-");
    let i = script
        .find(&pat)
        .unwrap_or_else(|| panic!("{name} default"))
        + pat.len();
    script[i..i + script[i..].find('}').unwrap()].to_string()
}

#[test]
fn ci_runs_the_guard_on_both_oses_and_docs_state_the_limits() {
    let ci: serde_json::Value =
        harness_config::yaml::parse(&read(".github/workflows/ci.yml"), None).unwrap();
    let test = &ci["jobs"]["test"];
    let oses: Vec<&str> = test["strategy"]["matrix"]["os"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|v| v.as_str())
        .collect();
    assert_eq!(oses, ["ubuntu-latest", "macos-latest"]);
    let step = test["steps"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| {
            s["run"]
                .as_str()
                .is_some_and(|r| r.contains("scripts/bench-guard.sh"))
        })
        .expect("the test job runs scripts/bench-guard.sh");
    let run = step["run"].as_str().unwrap();
    assert!(run.contains("cargo build --locked -p harness"), "{run}");
    assert!(run.contains("target/debug/harness"), "{run}");
    assert!(step["timeout-minutes"].as_u64().is_some(), "{step}");

    let script = read("scripts/bench-guard.sh");
    let perf = read("docs/performance.md");
    let overhead = script_default(&script, "BENCH_GUARD_MAX_OVERHEAD_MS");
    let start = script_default(&script, "BENCH_GUARD_MAX_SESSION_START_MS");
    let runs = script_default(&script, "BENCH_GUARD_RUNS");
    let tries = script_default(&script, "BENCH_GUARD_ATTEMPTS");
    for needle in [
        "## CI overhead guard".to_string(),
        "scripts/bench-guard.sh".to_string(),
        format!("| Overhead per call | {overhead} ms |"),
        format!("| Session start | {start} ms |"),
        format!("best of {runs} runs"),
        format!("{tries} attempts"),
    ] {
        assert!(
            perf.contains(&needle),
            "docs/performance.md missing {needle:?}"
        );
    }
    let ci_doc = read("docs/ci.md");
    assert!(ci_doc.contains("scripts/bench-guard.sh"), "docs/ci.md");
    assert!(ci_doc.contains("docs/performance.md"), "docs/ci.md");
}
