//! Structure of the GitHub Actions CI workflow (TASK-137): what runs, where,
//! on which events, with warnings denied and caching, and the README badge.

use std::path::{Path, PathBuf};

use serde_json::Value;

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn read(rel: &str) -> String {
    std::fs::read_to_string(root().join(rel)).unwrap_or_else(|e| panic!("reading {rel}: {e}"))
}

fn ci() -> Value {
    harness_config::yaml::parse(&read(".github/workflows/ci.yml"), None).expect("ci.yml parses")
}

fn job<'a>(ci: &'a Value, id: &str) -> &'a Value {
    ci.get("jobs")
        .and_then(|j| j.get(id))
        .unwrap_or_else(|| panic!("ci.yml has no `{id}` job"))
}

fn steps(job: &Value) -> Vec<&Value> {
    job["steps"].as_array().expect("steps").iter().collect()
}

/// Every `run:` command of a job, joined.
fn runs(job: &Value) -> String {
    steps(job)
        .iter()
        .filter_map(|s| s.get("run").and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join("\n")
}

fn uses(job: &Value, action: &str) -> bool {
    steps(job).iter().any(|s| {
        s.get("uses")
            .and_then(Value::as_str)
            .is_some_and(|u| u.starts_with(action))
    })
}

fn oses(job: &Value) -> Vec<&str> {
    job["strategy"]["matrix"]["os"]
        .as_array()
        .map(|a| a.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default()
}

#[test]
fn runs_on_push_and_pull_request() {
    let ci = ci();
    let on = ci
        .get("on")
        .expect("`on` is a string key, not YAML 1.1 `true`");
    assert!(on.get("push").is_some(), "{on}");
    assert!(on.get("pull_request").is_some(), "{on}");
}

#[test]
fn warnings_are_denied_everywhere() {
    let ci = ci();
    assert_eq!(ci["env"]["RUSTFLAGS"], "-D warnings");
    assert!(runs(job(&ci, "clippy")).contains("-- -D warnings"));
}

#[test]
fn fmt_clippy_tests_and_deny_run() {
    let ci = ci();
    assert!(runs(job(&ci, "fmt")).contains("cargo fmt --all -- --check"));
    assert!(
        runs(job(&ci, "clippy")).contains("cargo clippy --workspace --all-targets -- -D warnings")
    );
    assert!(runs(job(&ci, "test")).contains("cargo test --workspace --locked"));
    assert!(uses(job(&ci, "deny"), "EmbarkStudios/cargo-deny-action@"));
}

#[test]
fn clippy_and_tests_run_on_linux_and_macos() {
    let ci = ci();
    for id in ["clippy", "test"] {
        let j = job(&ci, id);
        assert_eq!(oses(j), ["ubuntu-latest", "macos-latest"], "{id}");
        assert_eq!(j["runs-on"], "${{ matrix.os }}", "{id}");
        assert_eq!(j["strategy"]["fail-fast"], false, "{id}: report both OSes");
    }
}

#[test]
fn compiling_jobs_are_cached_and_bounded() {
    let ci = ci();
    for id in ["clippy", "test"] {
        let j = job(&ci, id);
        assert!(uses(j, "Swatinem/rust-cache@"), "{id} must cache target/");
        assert!(
            j["timeout-minutes"].as_u64().is_some(),
            "{id} needs a timeout"
        );
    }
}

#[test]
fn test_job_logs_host_features_and_skip_reasons() {
    let ci = ci();
    let r = runs(job(&ci, "test"));
    assert!(r.contains("harness-sandbox -- --probe"), "{r}");
    assert!(r.contains("--test sandbox -- --nocapture"), "{r}");
}

/// The channel pinned in `rust-toolchain.toml`.
fn pinned_toolchain() -> String {
    read("rust-toolchain.toml")
        .lines()
        .find_map(|l| l.trim().strip_prefix("channel = "))
        .expect("rust-toolchain.toml has a channel")
        .trim_matches('"')
        .to_string()
}

#[test]
fn workflows_install_the_pinned_toolchain() {
    let pinned = pinned_toolchain();
    let dir = root().join(".github/workflows");
    let mut seen = 0;
    for entry in std::fs::read_dir(&dir).expect("workflows dir") {
        let path = entry.unwrap().path();
        if path.extension().and_then(|e| e.to_str()) != Some("yml") {
            continue;
        }
        let text = std::fs::read_to_string(&path).unwrap();
        for tag in text
            .lines()
            .filter_map(|l| l.split("dtolnay/rust-toolchain@").nth(1))
        {
            seen += 1;
            assert_eq!(
                tag.trim(),
                pinned,
                "{}: dtolnay/rust-toolchain@{} differs from rust-toolchain.toml",
                path.display(),
                tag.trim()
            );
        }
    }
    assert!(seen > 0, "no workflow installs a Rust toolchain");
}

#[test]
fn dependabot_leaves_the_toolchain_pin_alone() {
    let cfg: Value = harness_config::yaml::parse(&read(".github/dependabot.yml"), None)
        .expect("dependabot.yml parses");
    let actions = cfg["updates"]
        .as_array()
        .expect("updates")
        .iter()
        .find(|u| u["package-ecosystem"] == "github-actions")
        .expect("github-actions updates");
    let ignored: Vec<&str> = actions["ignore"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|i| i["dependency-name"].as_str())
                .collect()
        })
        .unwrap_or_default();
    assert!(
        ignored.contains(&"dtolnay/rust-toolchain"),
        "its tag is the Rust version; bump it with rust-toolchain.toml, not via Dependabot"
    );
    assert!(read("docs/ci.md").contains("### Upgrading the Rust toolchain"));
}

#[test]
fn readme_shows_ci_badge_and_docs_exist() {
    let readme = read("README.md");
    assert!(
        readme.contains("actions/workflows/ci.yml/badge.svg"),
        "README has no CI badge"
    );
    let doc = read("docs/ci.md");
    for needle in [
        "Host-dependent tests",
        "SKIP enforcement check",
        "macos-latest",
    ] {
        assert!(doc.contains(needle), "docs/ci.md missing {needle:?}");
    }
}
