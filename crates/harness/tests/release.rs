//! Release automation (TASK-138): the scripts in `scripts/release-*.sh`
//! run against temp fixtures (and a fake `gh`), the release workflows are
//! parsed to prove the release is gated on CI, and `harness --version`
//! reports the version and commit.
#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_norway::Value;

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .unwrap()
        .to_path_buf()
}

fn script(name: &str) -> PathBuf {
    root().join("scripts").join(name)
}

/// Runs `scripts/<name> args...` in `dir` with extra environment.
fn run(dir: &Path, name: &str, args: &[&str], env: &[(&str, &str)]) -> Output {
    let mut c = Command::new(script(name));
    c.args(args).current_dir(dir);
    for (k, v) in env {
        c.env(k, v);
    }
    c.output().unwrap()
}

fn out(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn err(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

fn read(p: &Path) -> String {
    std::fs::read_to_string(p).unwrap()
}

// ---------------------------------------------------------------- AC #5

#[test]
fn version_flag_reports_version_and_commit() {
    let o = Command::new(env!("CARGO_BIN_EXE_harness"))
        .arg("--version")
        .output()
        .unwrap();
    assert!(o.status.success());
    let commit = env!("HARNESS_GIT_COMMIT");
    assert_eq!(
        out(&o).trim_end(),
        format!("harness {} ({commit})", env!("CARGO_PKG_VERSION"))
    );
    assert!(
        commit == "unknown"
            || (commit.len() == 12 && commit.chars().all(|c| c.is_ascii_hexdigit())),
        "commit must be 12 hex digits or `unknown`, got {commit:?}"
    );
    // The release smoke test accepts exactly this output.
    let tmp = tempfile::tempdir().unwrap();
    let ok = run(
        tmp.path(),
        "release-smoke.sh",
        &[
            env!("CARGO_PKG_VERSION"),
            commit,
            env!("CARGO_BIN_EXE_harness"),
        ],
        &[],
    );
    assert!(ok.status.success(), "{}", err(&ok));
    let bad = run(
        tmp.path(),
        "release-smoke.sh",
        &["9.9.9", commit, env!("CARGO_BIN_EXE_harness")],
        &[],
    );
    assert!(!bad.status.success());
    assert!(err(&bad).contains("want 'harness 9.9.9"), "{}", err(&bad));
}

// ---------------------------------------------------------------- AC #4

const CHANGELOG: &str = "# Changelog\n\nIntro.\n\n## Unreleased\n\n### Added\n\n- New thing.\n\n## [0.2.0] - 2026-10-01\n\n### Fixed\n\n- A bug.\n  Second line.\n\n### Added\n\n- B.\n\n## 0.1.0\n\n- First.\n\n## Defaults\n\n- x. defaults-fingerprint: 000000000000\n";

#[test]
fn release_notes_are_the_changelog_section_for_the_version() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("CHANGELOG.md"), CHANGELOG).unwrap();
    let o = run(tmp.path(), "release-notes.sh", &["0.2.0"], &[]);
    assert!(o.status.success(), "{}", err(&o));
    assert_eq!(
        out(&o),
        "### Fixed\n\n- A bug.\n  Second line.\n\n### Added\n\n- B.\n"
    );
    // A tag name works too, and so does a heading without brackets or date.
    let o = run(tmp.path(), "release-notes.sh", &["v0.1.0"], &[]);
    assert_eq!(out(&o), "- First.\n");
    // Missing section, or an empty one, fails with a fix.
    for v in ["0.3.0", "not-a-version"] {
        let o = run(tmp.path(), "release-notes.sh", &[v], &[]);
        assert!(!o.status.success(), "{v} should fail");
    }
    std::fs::write(
        tmp.path().join("CHANGELOG.md"),
        "## [1.0.0] - 2026-01-01\n\n## Defaults\n",
    )
    .unwrap();
    let o = run(tmp.path(), "release-notes.sh", &["1.0.0"], &[]);
    assert!(!o.status.success());
    assert!(err(&o).contains("no notes for 1.0.0"), "{}", err(&o));
}

#[test]
fn repository_changelog_has_an_unreleased_section_and_keeps_defaults() {
    let text = read(&root().join("CHANGELOG.md"));
    assert!(
        text.lines().any(|l| l == "## Unreleased"),
        "CHANGELOG.md needs a `## Unreleased` section for the next release"
    );
    // The defaults gate (harness-eval) reads the `## Defaults` section.
    assert!(text.contains("\n## Defaults\n"));
}

// ---------------------------------------------------------------- AC #2

const CARGO_TOML: &str = "[workspace]\nmembers = [\"a\"]\n\n[workspace.package]\nversion = \"0.1.0\"\nedition = \"2024\"\n\n[workspace.dependencies]\nfoo = { version = \"0.1.0\" }\n";
const CARGO_LOCK: &str = "version = 4\n\n[[package]]\nname = \"a\"\nversion = \"0.1.0\"\ndependencies = [\n \"foo\",\n]\n\n[[package]]\nname = \"foo\"\nversion = \"0.1.0\"\nsource = \"registry+https://github.com/rust-lang/crates.io-index\"\nchecksum = \"00\"\n\n[[package]]\nname = \"b\"\nversion = \"0.1.0\"\n";

fn bump_fixture(changelog: &str) -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("Cargo.toml"), CARGO_TOML).unwrap();
    std::fs::write(tmp.path().join("Cargo.lock"), CARGO_LOCK).unwrap();
    std::fs::write(tmp.path().join("CHANGELOG.md"), changelog).unwrap();
    tmp
}

#[test]
fn bump_sets_workspace_version_lockfile_and_changelog() {
    let tmp = bump_fixture(CHANGELOG);
    let d = tmp.path();
    let o = run(d, "release-bump.sh", &["v0.3.0", "2026-10-22"], &[]);
    assert!(o.status.success(), "{}", err(&o));
    assert!(out(&o).contains("0.1.0 -> 0.3.0"));
    // Only [workspace.package] changes, not a dependency's version.
    assert_eq!(
        read(&d.join("Cargo.toml")),
        CARGO_TOML.replacen(
            "version = \"0.1.0\"\nedition",
            "version = \"0.3.0\"\nedition",
            1
        )
    );
    // Path packages move, registry packages keep their version.
    let lock = read(&d.join("Cargo.lock"));
    assert!(lock.contains("name = \"a\"\nversion = \"0.3.0\""));
    assert!(lock.contains("name = \"b\"\nversion = \"0.3.0\""));
    assert!(lock.contains("name = \"foo\"\nversion = \"0.1.0\""));
    // Unreleased becomes the version section, with a fresh Unreleased above.
    let cl = read(&d.join("CHANGELOG.md"));
    assert!(cl.contains("## Unreleased\n\n## [0.3.0] - 2026-10-22\n\n### Added\n\n- New thing.\n"));
    let notes = run(d, "release-notes.sh", &["0.3.0"], &[]);
    assert_eq!(out(&notes), "### Added\n\n- New thing.\n");
    // The workspace version is what a tag must match.
    let v = run(d, "release-version.sh", &["v0.3.0"], &[]);
    assert_eq!(out(&v), "0.3.0\n");
    let v = run(d, "release-version.sh", &["v0.2.0"], &[]);
    assert!(!v.status.success());
    assert!(err(&v).contains("does not match"));
}

#[test]
fn bump_refuses_bad_input_and_changes_nothing() {
    let empty_unreleased =
        CHANGELOG.replace("### Added\n\n- New thing.\n\n## [0.2.0]", "## [0.2.0]");
    let no_unreleased = CHANGELOG.replace("## Unreleased\n", "");
    let cases: &[(&str, &str, &str)] = &[
        (CHANGELOG, "0.0.9", "older than the current"),
        (CHANGELOG, "0.2.0", "already has a section for 0.2.0"),
        (CHANGELOG, "1.0", "not a version"),
        (&empty_unreleased, "0.3.0", "has no entries"),
        (&no_unreleased, "0.3.0", "no '## Unreleased' section"),
    ];
    for (changelog, version, why) in cases {
        let tmp = bump_fixture(changelog);
        let d = tmp.path();
        let o = run(d, "release-bump.sh", &[version, "2026-10-22"], &[]);
        assert!(!o.status.success(), "{version}: should fail ({why})");
        assert!(err(&o).contains(why), "{version}: {}", err(&o));
        assert_eq!(read(&d.join("Cargo.toml")), CARGO_TOML);
        assert_eq!(read(&d.join("Cargo.lock")), CARGO_LOCK);
        assert_eq!(read(&d.join("CHANGELOG.md")), *changelog);
    }
    // Releasing the current version (the first release) is allowed.
    let tmp = bump_fixture("## Unreleased\n\n- First release.\n\n## Defaults\n");
    let o = run(tmp.path(), "release-bump.sh", &["0.1.0", "2026-10-22"], &[]);
    assert!(o.status.success(), "{}", err(&o));
}

/// Writes a fake `gh` that prints the next response file on each call
/// (the last one repeats) and logs its arguments.
fn fake_gh(dir: &Path, responses: &[&str]) -> PathBuf {
    let bin = dir.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    for (i, r) in responses.iter().enumerate() {
        std::fs::write(dir.join(format!("resp{i}")), r).unwrap();
    }
    let gh = bin.join("gh");
    std::fs::write(
        &gh,
        format!(
            "#!/bin/sh\nd='{}'\necho \"$*\" >> \"$d/calls\"\nn=$(wc -l < \"$d/calls\" | tr -d ' ')\ni=$((n - 1))\n[ \"$i\" -lt {} ] || i={}\ncat \"$d/resp$i\"\n",
            dir.display(),
            responses.len(),
            responses.len() - 1
        ),
    )
    .unwrap();
    std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755)).unwrap();
    bin
}

fn gate(responses: &[&str]) -> (Output, String) {
    let tmp = tempfile::tempdir().unwrap();
    let bin = fake_gh(tmp.path(), responses);
    let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap());
    let o = run(
        tmp.path(),
        "release-ci-gate.sh",
        &["0123456789abcdef0123456789abcdef01234567"],
        &[
            ("PATH", &path),
            ("GITHUB_REPOSITORY", "owner/repo"),
            ("RELEASE_CI_POLL", "0"),
            ("RELEASE_CI_WAIT", "3"),
        ],
    );
    let calls = std::fs::read_to_string(tmp.path().join("calls")).unwrap_or_default();
    (o, calls)
}

#[test]
fn ci_gate_requires_a_green_ci_run_on_the_commit() {
    let (o, calls) = gate(&["completed success\n"]);
    assert!(o.status.success(), "{}", err(&o));
    assert!(calls.contains(
        "api repos/owner/repo/actions/workflows/ci.yml/runs?head_sha=0123456789abcdef0123456789abcdef01234567"
    ));
    // A cancelled run next to a green one does not block.
    let (o, _) = gate(&["completed cancelled\ncompleted success\n"]);
    assert!(o.status.success(), "{}", err(&o));

    let (o, _) = gate(&["completed success\ncompleted failure\n"]);
    assert!(!o.status.success());
    assert!(err(&o).contains("failed on"), "{}", err(&o));

    let (o, _) = gate(&[""]);
    assert!(!o.status.success());
    assert!(err(&o).contains("no successful run"), "{}", err(&o));
}

#[test]
fn ci_gate_waits_for_running_ci_and_gives_up_after_the_limit() {
    let (o, calls) = gate(&["in_progress null\n", "queued null\n", "completed success\n"]);
    assert!(o.status.success(), "{}", err(&o));
    assert_eq!(calls.lines().count(), 3);

    let (o, _) = gate(&["in_progress null\n"]);
    assert!(!o.status.success());
    assert!(err(&o).contains("still running"), "{}", err(&o));
}

// ---------------------------------------------------------------- AC #3

#[test]
fn package_writes_an_archive_and_a_matching_sha256_file() {
    let tmp = tempfile::tempdir().unwrap();
    let d = tmp.path();
    std::fs::write(d.join("harness"), "#!/bin/sh\necho hi\n").unwrap();
    std::fs::set_permissions(d.join("harness"), std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::write(d.join("README.md"), "readme").unwrap();
    let o = run(
        d,
        "release-package.sh",
        &[
            "dist",
            "harness-1.2.3-x86_64-unknown-linux-musl",
            "harness",
            "README.md",
        ],
        &[],
    );
    assert!(o.status.success(), "{}", err(&o));
    let sha = read(&d.join("dist/harness-1.2.3-x86_64-unknown-linux-musl.tar.gz.sha256"));
    let (digest, name) = sha.trim_end().split_once("  ").unwrap();
    assert_eq!(name, "harness-1.2.3-x86_64-unknown-linux-musl.tar.gz");
    assert_eq!(digest.len(), 64);
    // The sidecar verifies with the platform's checker.
    let check = Command::new("sh")
        .arg("-c")
        .arg("if command -v sha256sum >/dev/null; then sha256sum -c ./*.sha256; else shasum -a 256 -c ./*.sha256; fi")
        .current_dir(d.join("dist"))
        .output()
        .unwrap();
    assert!(check.status.success(), "{}", err(&check));
    // The archive holds NAME/ with the files, executable bit kept.
    let list = Command::new("tar")
        .args([
            "-tvzf",
            "dist/harness-1.2.3-x86_64-unknown-linux-musl.tar.gz",
        ])
        .current_dir(d)
        .output()
        .unwrap();
    let list = out(&list);
    assert!(
        list.contains("harness-1.2.3-x86_64-unknown-linux-musl/harness"),
        "{list}"
    );
    assert!(
        list.contains("harness-1.2.3-x86_64-unknown-linux-musl/README.md"),
        "{list}"
    );
    assert!(
        list.lines()
            .any(|l| l.ends_with("/harness") && l.starts_with("-rwx")),
        "{list}"
    );
    let o = run(d, "release-package.sh", &["dist", "x", "missing"], &[]);
    assert!(!o.status.success());
}

// ---------------------------------------------------------- workflows (AC #1-#3)

fn workflow(name: &str) -> Value {
    let p = root().join(".github/workflows").join(name);
    serde_norway::from_str(&read(&p)).unwrap()
}

fn needs(job: &Value) -> Vec<String> {
    match &job["needs"] {
        Value::String(s) => vec![s.clone()],
        Value::Sequence(v) => v.iter().map(|n| n.as_str().unwrap().to_string()).collect(),
        _ => vec![],
    }
}

/// Every job that `job` transitively needs.
fn ancestors(jobs: &Value, job: &str) -> Vec<String> {
    let mut seen = vec![];
    let mut todo = needs(&jobs[job]);
    while let Some(n) = todo.pop() {
        if !seen.contains(&n) {
            todo.extend(needs(&jobs[n.as_str()]));
            seen.push(n);
        }
    }
    seen
}

fn run_lines(job: &Value) -> Vec<String> {
    job["steps"]
        .as_sequence()
        .unwrap()
        .iter()
        .filter_map(|s| s["run"].as_str().map(str::to_string))
        .collect()
}

#[test]
fn release_workflow_runs_full_ci_first_and_publishes_only_after_it() {
    let wf = workflow("release-harness.yml");
    // serde_norway maps the YAML key `on` to the string "on".
    let on = &wf["on"];
    let tags = on["push"]["tags"].as_sequence().unwrap();
    assert!(tags.iter().any(|t| t.as_str() == Some("v*")));
    assert!(on.get("workflow_dispatch").is_some());

    let jobs = &wf["jobs"];
    assert_eq!(
        jobs["ci"]["uses"].as_str(),
        Some("./.github/workflows/ci.yml")
    );
    // The called workflow must be reusable and contain the whole suite.
    let ci = workflow("ci.yml");
    assert!(
        ci["on"].get("workflow_call").is_some(),
        "ci.yml needs `on: workflow_call`"
    );
    for j in ["fmt", "clippy", "test", "deny"] {
        assert!(ci["jobs"].get(j).is_some(), "ci.yml lost the {j} job");
    }
    // Every job that builds or publishes depends on CI.
    for (name, _) in jobs.as_mapping().unwrap() {
        let name = name.as_str().unwrap();
        if name != "ci" && name != "meta" {
            assert!(
                ancestors(jobs, name).contains(&"ci".to_string()),
                "{name} must need ci"
            );
        }
    }
    let publish = ancestors(jobs, "publish");
    for j in ["ci", "meta", "build", "helpers"] {
        assert!(publish.contains(&j.to_string()), "publish must need {j}");
    }
    // Only the publish job can write releases.
    assert_eq!(wf["permissions"]["contents"].as_str(), Some("read"));
    assert_eq!(
        jobs["publish"]["permissions"]["contents"].as_str(),
        Some("write")
    );
    let publish_runs = run_lines(&jobs["publish"]).join("\n");
    assert!(publish_runs.contains("scripts/release-notes.sh"));
    assert!(publish_runs.contains("gh release create"));
    assert!(publish_runs.contains("sha256sum -c"));
}

#[test]
fn release_workflow_builds_every_target_and_the_helpers() {
    let wf = workflow("release-harness.yml");
    let build = &wf["jobs"]["build"];
    let include = build["strategy"]["matrix"]["include"]
        .as_sequence()
        .unwrap();
    let mut targets: Vec<(String, String, bool)> = include
        .iter()
        .map(|m| {
            (
                m["target"].as_str().unwrap().to_string(),
                m["os"].as_str().unwrap().to_string(),
                m["zig"].as_bool().unwrap(),
            )
        })
        .collect();
    targets.sort();
    let names: Vec<&str> = targets.iter().map(|t| t.0.as_str()).collect();
    assert_eq!(
        names,
        [
            "aarch64-apple-darwin",
            "aarch64-unknown-linux-musl",
            "x86_64-apple-darwin",
            "x86_64-unknown-linux-musl"
        ]
    );
    for (target, os, zig) in &targets {
        if target.ends_with("-linux-musl") {
            assert!(
                os.starts_with("ubuntu") && *zig,
                "{target}: musl via cargo-zigbuild"
            );
        } else {
            assert!(
                os.starts_with("macos") && !*zig,
                "{target}: native build on macOS"
            );
        }
    }
    assert!(
        build["env"]["HARNESS_GIT_COMMIT"]
            .as_str()
            .unwrap()
            .contains("github.sha")
    );
    let runs = run_lines(build).join("\n");
    assert!(runs.contains("cargo zigbuild --release --locked"));
    assert!(runs.contains("scripts/release-smoke.sh"));
    assert!(runs.contains("scripts/release-package.sh"));
    let helpers = run_lines(&wf["jobs"]["helpers"]).join("\n");
    assert!(helpers.contains("scripts/build-remote-helpers.sh"));
    assert!(helpers.contains("scripts/release-package.sh"));
}

#[test]
fn cut_release_gates_on_ci_then_bumps_tags_and_triggers_the_release() {
    let wf = workflow("cut-release.yml");
    let input = &wf["on"]["workflow_dispatch"]["inputs"]["version"];
    assert_eq!(input["required"].as_bool(), Some(true));
    let runs = run_lines(&wf["jobs"]["cut"]);
    let pos = |needle: &str| {
        runs.iter()
            .position(|r| r.contains(needle))
            .unwrap_or_else(|| panic!("cut-release.yml has no step running {needle}"))
    };
    let order = [
        pos("valid_version"),
        pos("refs/tags/v$VERSION"),
        pos("scripts/release-ci-gate.sh"),
        pos("scripts/release-bump.sh"),
        pos("git tag -a"),
        pos("git push --atomic"),
        pos("gh workflow run release-harness.yml"),
    ];
    // Tag and push share a step; everything else is a later step.
    assert!(
        order.windows(2).all(|w| w[0] <= w[1]),
        "steps out of order: {order:?}"
    );
    assert!(
        order[0] < order[1]
            && order[1] < order[2]
            && order[2] < order[3]
            && order[3] < order[4]
            && order[5] < order[6]
    );
    // The validation step normalises the input into $VERSION for every later
    // step, so a "v0.1.0" input can never produce a "vv0.1.0" tag.
    let validate = &runs[order[0]];
    assert!(validate.contains("${VERSION#v}"), "{validate}");
    assert!(validate.contains("GITHUB_ENV"), "{validate}");
    for (i, r) in runs.iter().enumerate().skip(order[0] + 1) {
        assert!(
            !r.contains("inputs.version"),
            "step {i} reads the raw input instead of $VERSION: {r}"
        );
    }
    assert_eq!(wf["permissions"]["contents"].as_str(), Some("write"));
    assert_eq!(wf["permissions"]["actions"].as_str(), Some("write"));
}

#[test]
fn cut_release_normalises_a_leading_v_and_rejects_bad_versions() {
    let wf = workflow("cut-release.yml");
    let runs = run_lines(&wf["jobs"]["cut"]);
    let step = runs
        .iter()
        .find(|r| r.contains("valid_version"))
        .expect("cut-release.yml has no version validation step");
    let tmp = tempfile::tempdir().unwrap();
    let check = |version: &str| {
        let env_file = tmp.path().join("github_env");
        std::fs::write(&env_file, "").unwrap();
        let o = Command::new("bash")
            .args(["-e", "-c", step])
            .current_dir(root())
            .env("VERSION", version)
            .env("GITHUB_ENV", &env_file)
            .output()
            .unwrap();
        (o.status.success(), read(&env_file))
    };
    for (input, want) in [
        ("v0.1.0", "0.1.0"),
        ("0.1.0", "0.1.0"),
        ("v1.2.3-rc.1", "1.2.3-rc.1"),
    ] {
        let (ok, env) = check(input);
        assert!(ok, "{input} should be accepted");
        assert_eq!(env, format!("VERSION={want}\n"), "{input}");
    }
    for bad in ["vv0.1.0", "", "0.1", "01.2.3", "1.2.3 ", "x1.2.3"] {
        let (ok, env) = check(bad);
        assert!(!ok, "{bad:?} should be rejected");
        assert_eq!(env, "", "{bad:?} must not reach GITHUB_ENV");
    }
}
