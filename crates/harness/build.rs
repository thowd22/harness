//! Embeds the git commit the binary was built from as `HARNESS_GIT_COMMIT`,
//! so `harness --version` can report it (TASK-138).
//!
//! Order of precedence:
//! 1. the `HARNESS_GIT_COMMIT` environment variable (release CI sets it to
//!    the full commit SHA; only the first 12 characters are kept),
//! 2. `git rev-parse HEAD` in the source tree,
//! 3. `unknown` (a source tarball, or a repository with no commits yet).

use std::path::{Path, PathBuf};
use std::process::Command;

/// Length of the abbreviated commit printed by `--version`.
const SHORT: usize = 12;

fn main() {
    println!("cargo:rerun-if-env-changed=HARNESS_GIT_COMMIT");
    let commit = std::env::var("HARNESS_GIT_COMMIT")
        .ok()
        .map(|c| c.trim().to_string())
        .filter(|c| !c.is_empty())
        .or_else(|| {
            let dir = git_dir()?;
            watch(&dir);
            git(&["rev-parse", "HEAD"])
        })
        .map(|c| c.chars().take(SHORT).collect::<String>())
        .unwrap_or_else(|| "unknown".to_string());
    println!("cargo:rustc-env=HARNESS_GIT_COMMIT={commit}");
}

/// Runs git in the crate directory and returns trimmed stdout on success.
fn git(args: &[&str]) -> Option<String> {
    let manifest = std::env::var_os("CARGO_MANIFEST_DIR")?;
    let out = Command::new("git")
        .args(args)
        .current_dir(manifest)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8(out.stdout).ok()?.trim().to_string();
    (!s.is_empty()).then_some(s)
}

fn git_dir() -> Option<PathBuf> {
    git(&["rev-parse", "--absolute-git-dir"]).map(PathBuf::from)
}

/// Re-runs this script when HEAD moves: HEAD itself, the branch ref it points
/// at (or the refs directory while that ref does not exist yet, so a missing
/// file does not force a rebuild every time) and packed-refs.
fn watch(dir: &Path) {
    let head = dir.join("HEAD");
    println!("cargo:rerun-if-changed={}", head.display());
    if let Some(r) = std::fs::read_to_string(&head)
        .ok()
        .and_then(|h| h.strip_prefix("ref: ").map(|r| r.trim().to_string()))
    {
        let file = dir.join(&r);
        if file.exists() {
            println!("cargo:rerun-if-changed={}", file.display());
        } else if dir.join("refs/heads").is_dir() {
            println!(
                "cargo:rerun-if-changed={}",
                dir.join("refs/heads").display()
            );
        }
    }
    let packed = dir.join("packed-refs");
    if packed.exists() {
        println!("cargo:rerun-if-changed={}", packed.display());
    }
}
