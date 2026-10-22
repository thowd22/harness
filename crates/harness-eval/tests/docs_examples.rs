//! Every example in the user guide (and the area docs written with it) is
//! validated (TASK-116 AC#2). Fenced blocks are checked by their info
//! string:
//!
//! | Fence | Check |
//! |---|---|
//! | `ini` | loads as harness.ini |
//! | `yaml prompt` / `yaml agent` / `yaml workflow` | loads in the global scope over the built-ins, validates and passes prompt lint |
//! | `markdown skill` | loads as a skill and passes prompt lint |
//! | `yaml eval-suite` | parses as an eval suite |
//! | `sh` | every `harness ...` line uses a known command |
//! | `text` | sample output, not an example to copy |
//!
//! Any other fence fails the test, so new examples must be checkable.

use std::path::{Path, PathBuf};

use harness_config::agents::ValidationContext;
use harness_config::discovery::{Catalog, Roots};
use harness_config::ini::Config;
use harness_config::lint::{self, LintOptions};

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn docs() -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(root().join("docs/guide"))
        .unwrap()
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "md"))
        .collect();
    for f in [
        "defaults.md",
        "evals.md",
        "dreaming.md",
        "memory-ui.md",
        "golden-snapshots.md",
        "effective-config.md",
        "doctor.md",
    ] {
        v.push(root().join("docs").join(f));
    }
    v.sort();
    v
}

/// (info string, body, line) of every fenced block.
fn blocks(text: &str) -> Vec<(String, String, usize)> {
    let mut out = Vec::new();
    let mut cur: Option<(String, String, usize)> = None;
    for (i, line) in text.lines().enumerate() {
        let trimmed = line.trim_start();
        if let Some(info) = trimmed.strip_prefix("```") {
            match cur.take() {
                Some(b) => out.push(b),
                None => cur = Some((info.trim().to_owned(), String::new(), i + 1)),
            }
        } else if let Some((_, body, _)) = cur.as_mut() {
            body.push_str(line);
            body.push('\n');
        }
    }
    assert!(cur.is_none(), "unterminated code fence");
    out
}

const KNOWN_COMMANDS: &[&str] = &[
    "--version",
    "run",
    "sessions",
    "config check",
    "config show",
    "schema",
    "lint",
    "doctor",
    "login",
    "logout",
    "eval run",
    "eval compare",
    "wf",
];

fn check_sh(body: &str) -> Result<(), String> {
    for line in body.lines().map(str::trim) {
        let Some(rest) = line.strip_prefix("harness ") else {
            continue;
        };
        if !KNOWN_COMMANDS.iter().any(|c| rest.starts_with(c)) {
            return Err(format!("unknown harness command: `{line}`"));
        }
    }
    Ok(())
}

fn context() -> ValidationContext {
    // The examples refer to the `sonnet` alias and a host labelled `gpu`.
    let cfg = harness_config::ini::load_str(
        "[models]\nsonnet = anthropic/claude-sonnet-5-5\n[remote.gpu-box]\nhost = 10.0.0.5\nlabels = gpu, cuda\n",
        None,
    )
    .unwrap();
    ValidationContext::from_config(&cfg)
}

fn yaml_name(body: &str) -> String {
    let v: serde_json::Value = harness_config::yaml::parse(body, None).unwrap();
    v["name"].as_str().unwrap_or("unnamed").to_owned()
}

#[test]
fn every_documented_example_is_valid() {
    let mut checked = 0;
    for doc in docs() {
        let text = std::fs::read_to_string(&doc).unwrap();
        let global = tempfile::tempdir().unwrap();
        let mut definitions = 0;
        for (info, body, line) in blocks(&text) {
            let at = format!("{}:{line}", doc.display());
            let mut words = info.split_whitespace();
            let (lang, kind) = (words.next().unwrap_or(""), words.next());
            match (lang, kind) {
                ("ini", None) => {
                    let r: Result<Config, _> = harness_config::ini::load_str(&body, None);
                    if let Err(e) = r {
                        panic!("{at}: invalid harness.ini example:\n{e}");
                    }
                }
                ("yaml", Some(k @ ("prompt" | "agent" | "workflow"))) => {
                    let dir = global.path().join(format!("{k}s"));
                    std::fs::create_dir_all(&dir).unwrap();
                    std::fs::write(dir.join(format!("{}.yaml", yaml_name(&body))), &body).unwrap();
                    definitions += 1;
                    if k == "workflow" && yaml_name(&body) == "dream" {
                        let v: serde_json::Value =
                            harness_config::yaml::parse(&body, None).unwrap();
                        harness_memory::dream::DreamSettings::from_workflow(&v)
                            .unwrap_or_else(|e| panic!("{at}: {e}"));
                    }
                }
                ("markdown", Some("skill")) => {
                    let dir = global.path().join("skills");
                    std::fs::create_dir_all(&dir).unwrap();
                    let name = body
                        .lines()
                        .find_map(|l| l.strip_prefix("name: "))
                        .unwrap_or("skill")
                        .trim()
                        .to_owned();
                    std::fs::write(dir.join(format!("{name}.md")), &body).unwrap();
                    definitions += 1;
                }
                ("yaml", Some("eval-suite")) => {
                    harness_eval::suite::Suite::parse(&body, None, PathBuf::from("."))
                        .unwrap_or_else(|e| panic!("{at}: invalid eval suite: {e}"));
                }
                ("sh", None) => check_sh(&body).unwrap_or_else(|e| panic!("{at}: {e}")),
                ("text", None) => continue,
                other => panic!(
                    "{at}: example with fence `{info}` ({other:?}) has no validator; tag it (see this test)"
                ),
            }
            checked += 1;
        }
        if definitions > 0 {
            let catalog = Catalog::load(
                &Roots::new(harness_config::builtins::BUILTINS).with_global(global.path()),
            );
            assert!(
                catalog.errors().is_empty(),
                "{}: {:?}",
                doc.display(),
                catalog.errors()
            );
            let ctx = context();
            let invalid = catalog.validate_agents(&ctx);
            assert!(invalid.is_empty(), "{}: {invalid:?}", doc.display());
            let report = lint::lint_catalog(&catalog, Some(&ctx), &[], &LintOptions::default());
            let errors: Vec<String> = report.errors().map(ToString::to_string).collect();
            assert!(errors.is_empty(), "{}: {errors:#?}", doc.display());
        }
    }
    assert!(checked >= 20, "only {checked} examples found");
}

#[test]
fn the_guide_covers_every_topic() {
    let guide = std::fs::read_to_string(root().join("docs/guide/README.md")).unwrap();
    for page in [
        "install.md",
        "harness-ini.md",
        "yaml.md",
        "skills.md",
        "workflows.md",
        "memory.md",
        "remotes.md",
        "palette.md",
    ] {
        assert!(guide.contains(page), "README.md must link {page}");
        assert!(
            root().join("docs/guide").join(page).is_file(),
            "{page} missing"
        );
    }
}
