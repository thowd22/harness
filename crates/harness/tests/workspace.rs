//! Checks for the workspace skeleton (TASK-1): crates, lints, toolchain, CI
//! and README stay in place.

use std::fs;
use std::path::{Path, PathBuf};

const CRATES: &[&str] = &[
    "harness-config",
    "harness-events",
    "harness-providers",
    "harness-tools",
    "harness-policy",
    "harness-agent",
    "harness-workflow",
    "harness-hygiene",
    "harness-memory",
    "harness-remote",
    "harness-tui",
    "harness-eval",
    "harness",
];

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn read(rel: &str) -> String {
    fs::read_to_string(root().join(rel)).unwrap_or_else(|e| panic!("reading {rel}: {e}"))
}

#[test]
fn every_crate_is_a_member_and_uses_workspace_lints() {
    let manifest = read("Cargo.toml");
    for name in CRATES {
        assert!(
            manifest.contains(&format!("\"crates/{name}\"")),
            "{name} missing from workspace members"
        );
        let krate = read(&format!("crates/{name}/Cargo.toml"));
        assert!(
            krate.contains("[lints]\nworkspace = true"),
            "{name} does not inherit workspace lints"
        );
        assert!(krate.contains("edition.workspace = true"), "{name} edition");
    }
}

#[test]
fn workspace_lints_are_defined() {
    let manifest = read("Cargo.toml");
    assert!(manifest.contains("[workspace.lints.rust]"));
    assert!(manifest.contains("unsafe_code = \"deny\""));
    assert!(manifest.contains("[workspace.lints.clippy]"));
}

#[test]
fn toolchain_is_pinned() {
    let toolchain = read("rust-toolchain.toml");
    let channel = toolchain
        .lines()
        .find_map(|l| l.trim().strip_prefix("channel = "))
        .expect("channel line");
    let version = channel.trim_matches('"');
    assert_eq!(
        version.split('.').count(),
        3,
        "toolchain must pin an exact x.y.z version, got {version}"
    );
    assert!(toolchain.contains("clippy") && toolchain.contains("rustfmt"));
}

#[test]
fn ci_runs_fmt_clippy_test_and_deny_on_push() {
    let ci = read(".github/workflows/ci.yml");
    for needle in [
        "push:",
        "cargo fmt --all -- --check",
        "cargo clippy --workspace --all-targets -- -D warnings",
        "cargo test --workspace",
        "cargo-deny",
    ] {
        assert!(ci.contains(needle), "ci.yml missing {needle:?}");
    }
    assert!(root().join("deny.toml").is_file());
}

#[test]
fn readme_states_build_and_test_commands() {
    let readme = read("README.md");
    for needle in ["cargo build --workspace", "cargo test --workspace"] {
        assert!(readme.contains(needle), "README missing {needle:?}");
    }
}
