//! The bundled bench task: a small Python library (`inventory`) whose
//! total-value computation ignores quantities, its unit tests, two user
//! prompts (fix the bug, then add a method with a test) and fake replies
//! that solve both with parallel reads, edits and test runs.

use std::io;
use std::path::Path;
use std::process::Command;

/// Project files, by relative path.
pub const FILES: &[(&str, &str)] = &[
    ("README.md", include_str!("../../bench/repo/README.md")),
    (
        "inventory/__init__.py",
        include_str!("../../bench/repo/inventory/__init__.py"),
    ),
    (
        "inventory/stock.py",
        include_str!("../../bench/repo/inventory/stock.py"),
    ),
    (
        "inventory/report.py",
        include_str!("../../bench/repo/inventory/report.py"),
    ),
    (
        "tests/__init__.py",
        include_str!("../../bench/repo/tests/__init__.py"),
    ),
    (
        "tests/test_stock.py",
        include_str!("../../bench/repo/tests/test_stock.py"),
    ),
    (
        "tests/test_report.py",
        include_str!("../../bench/repo/tests/test_report.py"),
    ),
];

/// The fake provider's replies (YAML, see `harness_providers::fake`).
pub const FAKE_SCRIPT: &str = include_str!("../../bench/fake.yaml");

/// Fake replies for the compaction variant (`harness bench --fake
/// --compaction`): the same task in a 20k-token window that compacts once,
/// with 1.5 s agent calls and a 2.5 s handoff summary.
pub const COMPACTION_SCRIPT: &str = include_str!("../../bench/compaction.yaml");

const PROMPTS: &str = include_str!("../../bench/prompts.txt");

/// The user prompts, one per turn.
pub fn prompts() -> Vec<String> {
    PROMPTS
        .split("\n---\n")
        .map(|p| p.trim().to_owned())
        .filter(|p| !p.is_empty())
        .collect()
}

/// Writes the project into `dir` and, when `git` is available, makes it a
/// repository with one commit (so diff and checkpoint paths are realistic).
pub fn write_repo(dir: &Path) -> io::Result<()> {
    for (path, text) in FILES {
        let p = dir.join(path);
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(p, text)?;
    }
    let git = |args: &[&str]| {
        Command::new("git")
            .args(args)
            .current_dir(dir)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    };
    if git(&["init", "-q"]) {
        git(&["add", "--", "."]);
        git(&[
            "-c",
            "user.name=bench",
            "-c",
            "user.email=bench@example.invalid",
            "commit",
            "-q",
            "-m",
            "initial",
        ]);
    }
    Ok(())
}
