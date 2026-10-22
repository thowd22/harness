use std::path::Path;

use super::*;
use harness_config::discovery::{Catalog, Roots, Scope};

fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

struct Dirs {
    _tmp: tempfile::TempDir,
    global: std::path::PathBuf,
    project: std::path::PathBuf,
}

impl Dirs {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let global = tmp.path().join("global");
        let project = tmp.path().join("project");
        std::fs::create_dir_all(&global).unwrap();
        std::fs::create_dir_all(&project).unwrap();
        Self {
            _tmp: tmp,
            global,
            project,
        }
    }

    fn roots(&self) -> Roots {
        Roots::new(&[])
            .with_global(&self.global)
            .with_project(&self.project)
    }
}

const FIX_WORKFLOW: &str = "name: fix-bug\ndescription: Reproduce, fix and prove a bug fix\ncommand: fix\n\
inputs:\n  issue: { type: string, required: true, positional: 0 }\n  scope: { type: list, default: [\"src/**\", \"tests/**\"] }\n  \
level: { type: string, enum: [low, high] }\nsteps: [{id: a}]\n";

#[test]
fn tokenizer_handles_positional_key_value_and_quotes() {
    let t = tokenize(r#"one key=value "two words" k2="quoted \"v\"" 'it''s' a"b c"d"#).unwrap();
    let pairs: Vec<(Option<&str>, &str)> = t
        .iter()
        .map(|t| (t.key.as_deref(), t.value.as_str()))
        .collect();
    assert_eq!(
        pairs,
        [
            (None, "one"),
            (Some("key"), "value"),
            (None, "two words"),
            (Some("k2"), "quoted \"v\""),
            (None, "its"),
            (None, "ab cd"),
        ]
    );
    // `=` inside a value or after a quote is literal.
    let t = tokenize("url=a=b \"x=y\"").unwrap();
    assert_eq!(t[0].key.as_deref(), Some("url"));
    assert_eq!(t[0].value, "a=b");
    assert_eq!(t[1].key, None);
    let err = tokenize("fix \"unclosed").unwrap_err();
    assert_eq!(
        err.to_string(),
        "unterminated \" quote starting at column 5: close it with \""
    );
    assert_eq!(split_line("  /mode auto "), Some(("mode", "auto")));
    assert_eq!(split_line("hello"), None);
}

#[test]
fn every_builtin_has_a_description_and_argument_help() {
    let names: Vec<&str> = Builtin::ALL.iter().map(|b| b.name()).collect();
    assert_eq!(
        names,
        [
            "help",
            "new",
            "resume",
            "model",
            "mode",
            "agents",
            "workflows",
            "skills",
            "memory",
            "runs",
            "remote",
            "cost",
            "compact",
            "undo",
            "diff",
            "context",
            "config",
            "doctor",
            "login",
            "quit"
        ]
    );
    let reg = CommandRegistry::new();
    for b in Builtin::ALL {
        let spec = reg.get(b.name()).unwrap();
        assert_eq!(spec.kind, CommandKind::Builtin(b));
        assert!(!spec.description.is_empty());
        assert_eq!(Builtin::from_name(b.name()), Some(b));
        // Every argument has help text for the palette.
        for line in spec.arg_help() {
            assert!(line.contains(':'), "{} arg help: {line}", b.name());
        }
        assert_eq!(spec.arg_hint().is_some(), !spec.args.is_empty());
    }
    assert_eq!(
        reg.get("mode").unwrap().usage(),
        "/mode [ask|auto-edit|auto]"
    );
    assert_eq!(
        reg.get("compact").unwrap().arg_hint().unwrap(),
        "[focus...]"
    );
    assert_eq!(
        reg.get("mode").unwrap().arg_help(),
        ["mode: approval mode (one of ask, auto-edit, auto; optional)"]
    );
    let help = reg.help(None);
    assert_eq!(help.len(), Builtin::ALL.len());
    assert!(reg.help(Some("/cost"))[0].starts_with("/cost [model|agent|node] — "));
}

#[test]
fn builtin_arguments_are_validated_with_helpful_errors() {
    let reg = CommandRegistry::new();
    let inv = reg.parse("/mode auto").unwrap();
    assert_eq!(inv.get("mode"), Some("auto"));
    let inv = reg.parse("/cost by=agent").unwrap();
    assert_eq!(inv.get("by"), Some("agent"));
    let inv = reg
        .parse("/compact keep the parser decisions, drop logs")
        .unwrap();
    assert_eq!(
        inv.get("focus"),
        Some("keep the parser decisions, drop logs")
    );
    assert_eq!(reg.parse("/memory").unwrap().get("action"), Some("show"));
    let inv = reg.parse("/memory search build cache").unwrap();
    assert_eq!(inv.get("query"), Some("build cache"));

    let e = reg.parse("/mode fast").unwrap_err();
    assert_eq!(
        e.to_string(),
        "`fast` is not a valid mode for `/mode`: use one of ask, auto-edit, auto"
    );
    let e = reg.parse("/undo now").unwrap_err();
    assert_eq!(
        e.to_string(),
        "`/undo` takes at most 0 argument(s), got `now`. Usage: /undo"
    );
    let e = reg.parse("/cost group=model").unwrap_err();
    assert_eq!(
        e.to_string(),
        "`/cost` has no argument `group` (known: by). Usage: /cost [model|agent|node]"
    );
    let e = reg.parse("/cots").unwrap_err();
    assert_eq!(
        e.to_string(),
        "unknown command `/cots`. Did you mean `/cost`? Type /help to list commands."
    );
    assert_eq!(reg.parse("cost").unwrap_err(), CommandError::NotACommand);
    let e = reg.parse("/cost by=model by=node").unwrap_err();
    assert!(matches!(e, CommandError::Duplicate { .. }));
}

#[test]
fn registry_holds_workflows_skills_and_agents_with_argument_specs() {
    let d = Dirs::new();
    write(&d.global.join("workflows/fix.yaml"), FIX_WORKFLOW);
    write(
        &d.global.join("skills/release-notes.md"),
        "---\nname: release-notes\ndescription: Draft release notes\nargs: \"<since-tag> [format]\"\n---\nNotes since $1\n",
    );
    write(
        &d.project.join("agents/reviewer.yaml"),
        "name: reviewer\ndescription: Reviews diffs\nprompt_text: review\n",
    );
    let catalog = Catalog::load(&d.roots());
    assert!(catalog.errors().is_empty(), "{:?}", catalog.errors());
    let reg = CommandRegistry::from_catalog(&catalog);

    let fix = reg.get("fix").unwrap();
    assert_eq!(fix.kind, CommandKind::Workflow);
    assert_eq!(fix.target, "fix-bug");
    assert_eq!(fix.description, "Reproduce, fix and prove a bug fix");
    assert_eq!(fix.usage(), "/fix <issue> [level=…] [scope=…]");
    let inv = reg
        .parse(r#"/fix "panic on empty input" level=high"#)
        .unwrap();
    assert_eq!(inv.get("issue"), Some("panic on empty input"));
    assert_eq!(inv.get("scope"), Some("src/**,tests/**"));
    assert_eq!(inv.get("level"), Some("high"));
    let e = reg.parse("/fix").unwrap_err();
    assert_eq!(
        e.to_string(),
        "`/fix` needs <issue>. Usage: /fix <issue> [level=…] [scope=…]"
    );
    assert!(matches!(
        reg.parse("/fix x priority=1").unwrap_err(),
        CommandError::UnknownKey { .. }
    ));

    let skill = reg.get("release-notes").unwrap();
    assert_eq!(skill.kind, CommandKind::Skill);
    assert_eq!(skill.scope, Some(Scope::Global));
    assert_eq!(
        skill.arg_hint().unwrap(),
        "<since-tag> [format] [arguments...]"
    );
    let inv = reg.parse("/release-notes v1.2 md extra=1 words").unwrap();
    assert_eq!(inv.get("since-tag"), Some("v1.2"));
    assert_eq!(inv.raw, "v1.2 md extra=1 words");
    assert!(reg.parse("/release-notes").is_err(), "required skill arg");

    let agent = reg.get("reviewer").unwrap();
    assert_eq!(agent.kind, CommandKind::Agent);
    assert_eq!(
        agent.description,
        "Switch to the reviewer agent: Reviews diffs"
    );
    let names: Vec<&str> = reg.completions("re").into_iter().map(|(n, _)| n).collect();
    assert_eq!(names, ["release-notes", "remote", "resume", "reviewer"]);
}

#[test]
fn collisions_keep_both_reachable_by_scope() {
    let d = Dirs::new();
    write(
        &d.global.join("skills/deploy.md"),
        "---\ndescription: Global deploy\n---\nglobal body\n",
    );
    write(
        &d.project.join("skills/deploy.md"),
        "---\ndescription: Project deploy\n---\nproject body\n",
    );
    // A skill named like a built-in.
    write(
        &d.project.join("skills/cost.md"),
        "---\ndescription: Team cost\n---\nbody\n",
    );
    // A workflow and a skill with the same name in one scope.
    write(
        &d.global.join("workflows/review.yaml"),
        "name: review\nsteps: [{id: a}]\n",
    );
    write(
        &d.global.join("skills/review.md"),
        "---\ndescription: Review skill\n---\nbody\n",
    );
    let reg = CommandRegistry::from_catalog(&Catalog::load(&d.roots()));

    assert_eq!(reg.get("deploy").unwrap().description, "Project deploy");
    assert_eq!(
        reg.get("project:deploy").unwrap().description,
        "Project deploy"
    );
    assert_eq!(
        reg.get("global:deploy").unwrap().description,
        "Global deploy"
    );
    assert_eq!(
        reg.parse("/global:deploy now").unwrap().spec.scope,
        Some(Scope::Global)
    );

    assert_eq!(
        reg.get("cost").unwrap().kind,
        CommandKind::Builtin(Builtin::Cost)
    );
    assert_eq!(reg.get("project:cost").unwrap().description, "Team cost");

    assert_eq!(reg.get("review").unwrap().kind, CommandKind::Workflow);
    assert_eq!(
        reg.get("global:review").unwrap().kind,
        CommandKind::Workflow
    );
    assert_eq!(reg.get("skill:review").unwrap().kind, CommandKind::Skill);

    let c: Vec<&Collision> = reg.collisions().iter().collect();
    let deploy = c.iter().find(|c| c.name == "deploy").unwrap();
    assert_eq!(deploy.winner, "skill (project)");
    assert_eq!(deploy.reachable_as, ["project:deploy", "global:deploy"]);
    // Bare-name listing has one entry per name.
    let bare: Vec<&str> = reg.commands().iter().map(|s| s.name.as_str()).collect();
    assert_eq!(bare.iter().filter(|n| **n == "deploy").count(), 1);
}

#[test]
fn registry_updates_on_hot_reload() {
    let d = Dirs::new();
    let roots = d.roots();
    let mut catalog = Catalog::load(&roots);
    let mut reg = CommandRegistry::from_catalog(&catalog);
    assert!(reg.get("deploy").is_none());
    assert!(!reg.reload(&catalog), "nothing changed");

    write(
        &d.global.join("skills/deploy.md"),
        "---\ndescription: Deploy\n---\nbody\n",
    );
    catalog.reload(&roots);
    assert!(reg.reload(&catalog));
    assert_eq!(reg.get("deploy").unwrap().description, "Deploy");

    write(
        &d.project.join("skills/deploy.md"),
        "---\ndescription: Local\n---\nbody\n",
    );
    catalog.reload(&roots);
    assert!(reg.reload(&catalog));
    assert_eq!(reg.get("deploy").unwrap().description, "Local");
    assert!(reg.get("global:deploy").is_some());

    std::fs::remove_file(d.global.join("skills/deploy.md")).unwrap();
    std::fs::remove_file(d.project.join("skills/deploy.md")).unwrap();
    catalog.reload(&roots);
    assert!(reg.reload(&catalog));
    assert!(reg.get("deploy").is_none());
    assert!(reg.get("global:deploy").is_none());
}
