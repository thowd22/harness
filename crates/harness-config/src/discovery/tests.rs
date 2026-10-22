use super::*;
use std::fs;

static BUILTINS: &[BuiltinFile] = &[
    BuiltinFile {
        path: "prompts/coder.yaml",
        contents: "name: coder\nlayers: [{text: builtin coder}]\n",
    },
    BuiltinFile {
        path: "prompts/notes.md",
        contents: "not yaml, ignored",
    },
    BuiltinFile {
        path: "agents/implementer.yaml",
        contents: "name: implementer\nprompt: coder\ntools: [read, write, edit, bash]\nmax_turns: 60\n",
    },
    BuiltinFile {
        path: "skills/review/SKILL.md",
        contents: "Review the diff.\n",
    },
    BuiltinFile {
        path: "workflows/fix.yaml",
        contents: "name: fix-bug\nsteps: [{id: a}]\n",
    },
];

fn write(path: &Path, text: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

struct Env {
    _tmp: tempfile::TempDir,
    global: PathBuf,
    project: PathBuf,
    trust: TrustStore,
}

fn env() -> Env {
    let tmp = tempfile::tempdir().unwrap();
    let global = tmp.path().join("config/harness");
    let project = tmp.path().join("repo");
    fs::create_dir_all(&global).unwrap();
    fs::create_dir_all(&project).unwrap();
    let trust = TrustStore::new(tmp.path().join("data/trust"));
    Env {
        global,
        project,
        trust,
        _tmp: tmp,
    }
}

fn trust_project(e: &Env) {
    let crate::trust::TrustStatus::Untrusted { current } = e.trust.status(&e.project).unwrap()
    else {
        panic!("expected untrusted");
    };
    e.trust.trust(&e.project, &current).unwrap();
}

fn roots(e: &Env) -> Roots {
    Roots::resolve_with(BUILTINS, Some(e.global.clone()), &e.trust, Some(&e.project))
}

#[test]
fn discovers_all_scopes_and_kinds() {
    let e = env();
    write(
        &e.global.join("prompts/global.yaml"),
        "name: g\nlayers: [{text: g}]\n",
    );
    write(
        &e.global.join("prompts/also.yml"),
        "name: g2\nlayers: [{text: g}]\n",
    );
    write(&e.global.join("prompts/readme.txt"), "ignored");
    write(&e.global.join("skills/deploy.md"), "Deploy it.\n");
    write(&e.global.join("skills/nested/SKILL.md"), "Nested skill.\n");
    write(&e.global.join("skills/nested/extra.md"), "not a skill");
    write(
        &e.project.join(".harness/agents/local.yaml"),
        "name: local\nprompt_text: hi\n",
    );
    trust_project(&e);
    let r = roots(&e);
    assert!(r.project.is_some(), "{:?}", r.notes);
    let c = Catalog::load(&r);
    assert!(c.errors().is_empty(), "{:?}", c.errors());
    let names = |v: Vec<&Entry<Prompt>>| v.iter().map(|e| e.item.name.clone()).collect::<Vec<_>>();
    assert_eq!(names(c.prompts.active()), ["coder", "g", "g2"]);
    let skills: Vec<_> = c
        .skills
        .active()
        .iter()
        .map(|e| (e.item.name.clone(), e.origin.scope))
        .collect();
    assert_eq!(
        skills,
        [
            ("deploy".into(), Scope::Global),
            ("nested".into(), Scope::Global),
            ("review".into(), Scope::Builtin)
        ]
    );
    assert_eq!(
        c.agents.get_entry("local").unwrap().origin.scope,
        Scope::Project
    );
    assert_eq!(
        c.agents.get_entry("implementer").unwrap().origin.display,
        "builtin:agents/implementer.yaml"
    );
    assert_eq!(c.workflows.get("fix-bug").unwrap().command, "fix-bug");
}

#[test]
fn untrusted_or_changed_project_is_not_loaded() {
    let e = env();
    write(
        &e.project.join(".harness/agents/local.yaml"),
        "name: local\nprompt_text: hi\n",
    );
    let r = roots(&e);
    assert!(r.project.is_none());
    assert!(r.notes[0].contains("not trusted"), "{:?}", r.notes);
    let c = Catalog::load(&r);
    assert!(c.agents.get("local").is_none());
    assert!(c.notes().iter().any(|n| n.contains("not trusted")));

    trust_project(&e);
    let mut r = roots(&e);
    assert!(Catalog::load(&r).agents.get("local").is_some());
    // An edit makes it untrusted again until reviewed.
    write(
        &e.project.join(".harness/agents/local.yaml"),
        "name: local\nprompt_text: changed\n",
    );
    r.recheck_trust(&e.trust);
    assert!(r.project.is_none());
    assert!(Catalog::load(&r).agents.get("local").is_none());
}

#[test]
fn later_scopes_shadow_and_shadowing_is_reported() {
    let e = env();
    write(
        &e.global.join("prompts/coder.yaml"),
        "name: coder\nlayers: [{text: global coder}]\n",
    );
    write(
        &e.project.join(".harness/prompts/mine.yaml"),
        "name: coder\nlayers: [{text: project coder}]\n",
    );
    write(&e.global.join("skills/x.md"), "Global x\n");
    write(&e.project.join(".harness/skills/x.md"), "Project x\n");
    trust_project(&e);
    let c = Catalog::load(&roots(&e));
    let coder = c.prompts.get_entry("coder").unwrap();
    assert_eq!(coder.origin.scope, Scope::Project);
    assert_eq!(coder.item.layers[0].template, "project coder");
    assert_eq!(
        c.prompts
            .get_in(Scope::Global, "coder")
            .unwrap()
            .item
            .layers[0]
            .template,
        "global coder"
    );
    assert_eq!(
        c.prompts.shadowed_by("coder").unwrap().origin.scope,
        Scope::Global
    );
    let sh = c.shadowed();
    assert_eq!(sh.len(), 2, "{sh:?}");
    let p = sh.iter().find(|s| s.kind == Kind::Prompt).unwrap();
    assert_eq!(p.name, "coder");
    assert_eq!(
        p.hidden.iter().map(|o| o.scope).collect::<Vec<_>>(),
        [Scope::Builtin, Scope::Global]
    );
    let text = p.to_string();
    assert!(text.starts_with("prompt `coder` from "), "{text}");
    assert!(
        text.contains("(project) shadows builtin:prompts/coder.yaml (builtin), "),
        "{text}"
    );
    // Slash collisions: both stay reachable.
    assert_eq!(
        crate::skills::resolve_command(&c.skills, "x")
            .unwrap()
            .description,
        "Project x"
    );
    assert_eq!(
        crate::skills::resolve_command(&c.skills, "global:x")
            .unwrap()
            .description,
        "Global x"
    );
    assert_eq!(
        crate::skills::resolve_command(&c.skills, "project:x")
            .unwrap()
            .description,
        "Project x"
    );
    assert!(crate::skills::resolve_command(&c.skills, "nope:x").is_none());
}

#[test]
fn duplicate_names_in_one_scope_are_errors() {
    let e = env();
    write(
        &e.global.join("prompts/a.yaml"),
        "name: dup\nlayers: [{text: a}]\n",
    );
    write(
        &e.global.join("prompts/b.yaml"),
        "# comment\nname: dup\nlayers: [{text: b}]\n",
    );
    let c = Catalog::load(&Roots::new(&[]).with_global(&e.global));
    assert_eq!(c.prompts.get("dup").unwrap().layers[0].template, "a");
    let err = &c.errors()[0].error;
    assert_eq!(err.line, 2);
    assert!(
        err.to_string()
            .contains("b.yaml:2:1: duplicate prompt name `dup`"),
        "{err}"
    );
}

#[test]
fn hot_reload_keeps_previous_version_on_invalid_edit() {
    let e = env();
    let f = e.global.join("agents/a.yaml");
    write(&f, "name: a\nprompt_text: one\n");
    let r = Roots::new(&[]).with_global(&e.global);
    let mut c = Catalog::load(&r);
    assert!(!c.reload(&r), "nothing changed");
    // Invalid edit: unknown key on line 3.
    write(&f, "name: a\nprompt_text: two\nmax_turnz: 3\n");
    c.reload(&r);
    assert_eq!(
        c.agents.get("a").unwrap().prompt,
        crate::agents::PromptRef::Inline("one".into())
    );
    let errs = c.errors();
    assert_eq!(errs.len(), 1);
    assert!(errs[0].kept_previous);
    let shown = errs[0].to_string();
    assert!(
        shown.starts_with(&format!("{}:3:1: unknown field `max_turnz`", f.display())),
        "{shown}"
    );
    assert!(shown.ends_with("(keeping the previous version)"));
    // Still broken on the next reload: error persists, old version kept.
    c.reload(&r);
    assert_eq!(c.errors().len(), 1);
    // Fix it.
    write(&f, "name: a\nprompt_text: three\n");
    assert!(c.reload(&r));
    assert!(c.errors().is_empty());
    assert_eq!(
        c.agents.get("a").unwrap().prompt,
        crate::agents::PromptRef::Inline("three".into())
    );
    // New broken file has no previous version.
    write(&e.global.join("agents/b.yaml"), "name: [\n");
    c.reload(&r);
    assert!(!c.errors()[0].kept_previous);
    assert!(c.agents.get("b").is_none());
    // Deletion removes the definition.
    fs::remove_file(&f).unwrap();
    assert!(c.reload(&r));
    assert!(c.agents.get("a").is_none());
}

#[test]
fn symlinks_are_not_traversed() {
    let e = env();
    let outside = e.global.parent().unwrap().join("outside");
    write(&outside.join("evil.yaml"), "name: evil\nprompt_text: x\n");
    write(&outside.join("SKILL.md"), "evil\n");
    fs::create_dir_all(e.project.join(".harness/skills")).unwrap();
    std::os::unix::fs::symlink(&outside, e.project.join(".harness/agents")).unwrap();
    std::os::unix::fs::symlink(&outside, e.project.join(".harness/skills/linked")).unwrap();
    std::os::unix::fs::symlink(
        outside.join("SKILL.md"),
        e.project.join(".harness/skills/file.md"),
    )
    .unwrap();
    // Global: symlinked file followed, symlinked dir not.
    write(
        &e.global.join("agents/real.yaml"),
        "name: real\nprompt_text: x\n",
    );
    std::os::unix::fs::symlink(outside.join("evil.yaml"), e.global.join("agents/link.yaml"))
        .unwrap();
    std::os::unix::fs::symlink(&outside, e.global.join("prompts")).unwrap();
    trust_project(&e);
    let r = roots(&e);
    assert!(r.project.is_some(), "{:?}", r.notes);
    let c = Catalog::load(&r);
    assert!(c.agents.get_in(Scope::Project, "evil").is_none());
    assert_eq!(
        c.agents.get_entry("evil").unwrap().origin.scope,
        Scope::Global
    );
    assert!(c.skills.get("linked").is_none());
    assert!(c.skills.get("file").is_none());
    assert!(
        c.prompts
            .active()
            .iter()
            .all(|p| p.origin.scope == Scope::Builtin)
    );
    let notes = c.notes();
    assert!(
        notes
            .iter()
            .any(|n| n.contains("symlinked directory skipped")),
        "{notes:?}"
    );
    assert!(
        notes.iter().any(|n| n.contains("symlink skipped")),
        "{notes:?}"
    );
}

#[test]
fn workflow_requires_name() {
    let src = SourceFile::inline(
        Scope::Global,
        Kind::Workflow,
        "workflows/w.yaml",
        "command: x\nsteps: []\n",
    );
    let e = RawWorkflow::load(&src).unwrap_err();
    assert!(
        e.to_string()
            .starts_with("global:workflows/w.yaml:1:1: missing required field `name`"),
        "{e}"
    );
    let src = src.with_text("name: w\ncommand: fix\nsteps: []\n");
    let w = RawWorkflow::load(&src).unwrap();
    assert_eq!((w.name.as_str(), w.command.as_str()), ("w", "fix"));
    assert!(w.document["steps"].is_array());
}

#[test]
fn global_dir_ignores_harness_config_override() {
    let env = |k: &str| match k {
        "HARNESS_CONFIG" => Some("/tmp/other.ini".into()),
        "XDG_CONFIG_HOME" => Some("/xdg".into()),
        _ => None,
    };
    assert_eq!(
        global_config_dir(env, Some(Path::new("/home/u"))),
        PathBuf::from("/xdg/harness")
    );
    assert_eq!(
        global_config_dir(|_| None, Some(Path::new("/home/u"))),
        PathBuf::from("/home/u/.config/harness")
    );
}

#[test]
fn watcher_reports_changes() {
    let e = env();
    fs::create_dir_all(e.global.join("agents")).unwrap();
    let r = Roots::new(&[]).with_global(&e.global);
    let w = Watcher::new(&r).unwrap();
    assert!(!w.take_changed());
    write(
        &e.global.join("agents/new.yaml"),
        "name: new\nprompt_text: x\n",
    );
    assert!(
        w.wait_changed(Duration::from_secs(10)),
        "no filesystem event within 10s"
    );
    let c = Catalog::load(&r);
    assert!(c.agents.get("new").is_some());
}
