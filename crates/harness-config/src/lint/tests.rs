use super::*;
use crate::discovery::{BuiltinFile, Roots};

fn src(text: &str) -> LintSource {
    LintSource {
        kind: SourceKind::PromptLayer,
        label: "prompt `p` layer 1".into(),
        file: "p.md".into(),
        text: text.into(),
        file_text: None,
    }
}

fn catalog(files: &[(&str, &str)]) -> (tempfile::TempDir, Catalog) {
    let tmp = tempfile::tempdir().unwrap();
    for (rel, text) in files {
        let p = tmp.path().join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    }
    let c = Catalog::load(&Roots::new(&[]).with_global(tmp.path()));
    (tmp, c)
}

#[test]
fn flags_blanket_prohibitions_with_positive_rewording() {
    let text = "# Rules\nBe careful.\nDo not modify the tests.\nNever change any configuration files!\nDO NOT MODIFY.\n";
    let f = blanket_prohibitions(&src(text));
    assert_eq!(f.len(), 3, "{f:#?}");
    assert!(
        f.iter()
            .all(|x| x.rule == Rule::BlanketProhibition && x.severity == Severity::Error)
    );
    assert_eq!((f[0].loc.line, f[0].loc.column), (3, 1));
    assert!(
        f[0].message.contains("\"Do not modify the tests\""),
        "{}",
        f[0].message
    );
    assert!(
        f[0].suggestion
            .as_ref()
            .unwrap()
            .contains("You may add or update tests")
    );
    assert_eq!(f[1].loc.line, 4);
    assert!(
        f[1].suggestion
            .as_ref()
            .unwrap()
            .contains("Change configuration only when")
    );
    assert_eq!(f[2].loc.line, 5);
    assert!(
        f[0].to_string()
            .starts_with("p.md:3:1: error [blanket-prohibition]: "),
        "{}",
        f[0]
    );
    assert!(
        f[0].to_string()
            .contains("\n  help: say what is allowed instead")
    );

    // Positive scope and narrow prohibitions are fine.
    for ok in [
        "You may edit files matching the task scope and add or update tests.",
        "Do not modify generated/schema.rs by hand; regenerate it.",
        "Never push to main.",
    ] {
        assert!(
            blanket_prohibitions(&src(ok)).is_empty(),
            "false positive: {ok}"
        );
    }
    // Memory text is data: warnings only.
    let m = LintSource::memory("memory `m1`", "memory:m1", "don't touch tests");
    assert_eq!(blanket_prohibitions(&m)[0].severity, Severity::Warning);
}

#[test]
fn flags_contradictions_between_sources() {
    let a = src("Never run git commit yourself.");
    let b = LintSource {
        label: "skill `ship`".into(),
        file: "ship.md".into(),
        kind: SourceKind::Skill,
        ..src("Always run git commit when done.")
    };
    let f = contradictions(&[a.clone(), b.clone()]);
    assert_eq!(f.len(), 1, "{f:#?}");
    assert_eq!(f[0].rule, Rule::Contradiction);
    assert!(f[0].message.contains("prompt `p` layer 1 says \"Never run git commit\" but skill `ship` (ship.md:1:1) says \"Always run git commit\""), "{}", f[0].message);
    let m = LintSource::memory("memory `m`", "memory:m", "You may run git commit freely.");
    let f = contradictions(&[a.clone(), m]);
    assert_eq!(f[0].severity, Severity::Warning);
    assert!(contradictions(&[a, src("Always run the tests.")]).is_empty());
}

#[test]
fn flags_contradictions_with_agent_config() {
    let (_t, c) = catalog(&[
        (
            "prompts/p.yaml",
            "name: p\nlayers:\n  - text: |\n      Read code carefully.\n      Never use bash for anything.\n      Use the memory tool to save notes.\n",
        ),
        ("agents/a.yaml", "name: a\nprompt: p\ntools: [read, bash]\n"),
        (
            "agents/b.yaml",
            "name: b\nprompt_text: Do not run shell commands.\ntools: [read]\n",
        ),
    ]);
    let r = lint_catalog(&c, None, &[], &LintOptions::default());
    let c: Vec<&Finding> = r
        .findings
        .iter()
        .filter(|f| f.rule == Rule::Contradiction)
        .collect();
    assert_eq!(c.len(), 2, "{:#?}", r.findings);
    let bash = c.iter().find(|f| f.message.contains("`bash`")).unwrap();
    assert!(
        bash.message
            .contains("says \"Never use bash\" but agent `a` is given the `bash` tool"),
        "{}",
        bash.message
    );
    assert_eq!(bash.loc.line, 5, "located inside the YAML: {}", bash.loc);
    let mem = c.iter().find(|f| f.message.contains("`memory`")).unwrap();
    assert!(mem.message.contains("does not have the `memory` tool"));
    assert!(mem.suggestion.as_ref().unwrap().contains("memory: read"));
}

#[test]
fn contradictions_with_skills_and_memory_per_agent() {
    let (_t, c) = catalog(&[
        (
            "prompts/p.yaml",
            "name: p\nlayers: [{text: Never edit the lockfile directly.}]\n",
        ),
        ("agents/a.yaml", "name: a\nprompt: p\nmemory: read\n"),
        (
            "skills/deps.md",
            "---\nmodel_invocable: true\n---\nAlways edit the lockfile directly after upgrades.\n",
        ),
    ]);
    let mem = [LintSource::memory(
        "memory `x`",
        "memory:x",
        "You should edit the lockfile directly.",
    )];
    let r = lint_catalog(&c, None, &mem, &LintOptions::default());
    let found: Vec<String> = r
        .findings
        .iter()
        .filter(|f| f.rule == Rule::Contradiction)
        .map(|f| f.message.clone())
        .collect();
    assert_eq!(found.len(), 2, "{found:#?}");
    assert!(found.iter().any(|m| m.contains("skill `deps`")));
    assert!(found.iter().any(|m| m.contains("memory `x`")));
    assert!(r.has_errors(), "skill contradiction is an error");
}

#[test]
fn flags_prompts_over_token_budget() {
    let big = "word ".repeat(1000);
    let (_t, c) = catalog(&[(
        "prompts/big.yaml",
        &format!("name: big\nlayers: [{{text: \"{big}\"}}]\n"),
    )]);
    let opts = LintOptions {
        prompt_token_budget: 100,
    };
    let r = lint_catalog(&c, None, &[], &opts);
    let f: Vec<&Finding> = r
        .findings
        .iter()
        .filter(|f| f.rule == Rule::TokenBudget)
        .collect();
    assert_eq!(f.len(), 1);
    assert_eq!(f[0].severity, Severity::Warning);
    assert!(
        f[0].message
            .contains("prompt `big` is about 1250 tokens, over the budget of 100"),
        "{}",
        f[0].message
    );
    assert!(
        lint_catalog(&c, None, &[], &LintOptions::default())
            .findings
            .is_empty()
    );
    assert_eq!(estimate_tokens("abcde"), 2);
}

static CLEAN: &[BuiltinFile] = &[
    BuiltinFile {
        path: "prompts/coder.yaml",
        contents: "name: coder\nlayers: [{text: You may edit files matching the task scope and add or update tests.}]\n",
    },
    BuiltinFile {
        path: "agents/impl.yaml",
        contents: "name: impl\nprompt: coder\ntools: [read, edit]\n",
    },
];

#[test]
fn run_lint_exit_codes_and_doctor_warnings() {
    let c = Catalog::load(&Roots::new(CLEAN));
    let ctx = ValidationContext::default();
    let r = lint_catalog(&c, Some(&ctx), &[], &LintOptions::default());
    let mut out = Vec::new();
    assert_eq!(
        run_lint(&r, &mut out),
        0,
        "{}",
        String::from_utf8_lossy(&out)
    );
    assert_eq!(String::from_utf8(out).unwrap(), "0 errors, 0 warnings\n");

    // Warnings only: exit 0, but doctor shows them.
    let r = lint_catalog(
        &c,
        Some(&ctx),
        &[],
        &LintOptions {
            prompt_token_budget: 1,
        },
    );
    let mut out = Vec::new();
    assert_eq!(run_lint(&r, &mut out), 0);
    assert!(
        String::from_utf8(out)
            .unwrap()
            .ends_with("0 errors, 1 warning\n")
    );
    assert_eq!(r.warnings().count(), 1);
    assert_eq!(doctor_lines(&r).len(), 1);
    assert!(doctor_lines(&r)[0].contains("warning [token-budget]"));

    // Errors (lint finding, broken file, bad reference): exit 1.
    let (_t, bad) = catalog(&[
        (
            "prompts/p.yaml",
            "name: p\nlayers: [{text: DO NOT MODIFY tests.}]\n",
        ),
        ("agents/x.yaml", "name: x\nprompt: missing\n"),
        ("agents/y.yaml", "name: y\nprompt_text: hi\nbogus: 1\n"),
    ]);
    let r = lint_catalog(&bad, Some(&ctx), &[], &LintOptions::default());
    let mut out = Vec::new();
    assert_eq!(run_lint(&r, &mut out), 1);
    let text = String::from_utf8(out).unwrap();
    assert!(text.contains("error [blanket-prohibition]"), "{text}");
    assert!(
        text.contains("x.yaml:2:1: error [invalid]: unknown prompt `missing`"),
        "{text}"
    );
    assert!(
        text.contains("y.yaml:3:1: error [invalid]: unknown field `bogus`"),
        "{text}"
    );
    assert!(text.ends_with("3 errors, 0 warnings\n"), "{text}");
}
