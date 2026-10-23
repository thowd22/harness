use super::*;
use serde_json::json;

struct Fx {
    _dir: tempfile::TempDir,
    root: PathBuf,
    home: PathBuf,
    data: PathBuf,
    ini: PathBuf,
}

fn fx() -> Fx {
    let dir = tempfile::tempdir().unwrap();
    let base = std::fs::canonicalize(dir.path()).unwrap();
    let root = base.join("ws");
    let home = base.join("home");
    let data = home.join(".local/share/harness");
    let ini = home.join(".config/harness/harness.ini");
    for d in [
        &root,
        &home.join(".ssh"),
        &data.join("trust"),
        &root.join(".git/hooks"),
    ] {
        std::fs::create_dir_all(d).unwrap();
    }
    std::fs::write(home.join(".ssh/id_rsa"), "key").unwrap();
    Fx {
        _dir: dir,
        root,
        home,
        data,
        ini,
    }
}

impl Fx {
    fn config(&self) -> PolicyConfig {
        PolicyConfig {
            bash_allow: vec![
                "cargo test*".into(),
                "ls*".into(),
                "cat*".into(),
                "git status*".into(),
            ],
            bash_ask: vec!["git commit*".into()],
            bash_deny: vec!["rm -rf /*".into(), "git push --force*".into()],
            paths_protected: vec!["~/.ssh/**".into(), "**/.env*".into()],
            outward_ask: vec!["deploy*".into()],
            protected_roots: vec![self.data.clone(), self.ini.clone()],
            home: Some(self.home.clone()),
        }
    }

    fn engine(&self) -> PolicyEngine {
        PolicyEngine::new(&self.config(), &self.root).unwrap()
    }
}

fn bash(c: &str) -> Action {
    Action::Bash { command: c.into() }
}

fn read(p: impl Into<PathBuf>) -> Action {
    Action::Read {
        tool: "read".into(),
        path: p.into(),
    }
}

fn write(p: impl Into<PathBuf>) -> Action {
    Action::Write {
        tool: "write".into(),
        path: p.into(),
    }
}

fn decide(e: &PolicyEngine, a: &Action, mode: Mode) -> Verdict {
    e.check(a, mode, e.root())
}

fn d(e: &PolicyEngine, a: &Action, mode: Mode) -> Decision {
    decide(e, a, mode).decision
}

// ---- TASK-25 AC#1: decisions and precedence ----------------------------

#[test]
fn every_action_gets_allow_ask_or_deny() {
    let f = fx();
    let e = f.engine();
    assert_eq!(d(&e, &bash("cargo test"), Mode::Ask), Decision::Allow);
    assert_eq!(d(&e, &bash("git commit -m x"), Mode::Auto), Decision::Ask);
    assert_eq!(d(&e, &bash("rm -rf /etc"), Mode::Auto), Decision::Deny);
    assert_eq!(
        d(
            &e,
            &Action::Tool {
                name: "todo".into()
            },
            Mode::Ask
        ),
        Decision::Allow
    );
}

#[test]
fn mode_defaults() {
    let f = fx();
    let e = f.engine();
    let unknown = bash("make all");
    assert_eq!(d(&e, &unknown, Mode::Ask), Decision::Ask);
    assert_eq!(d(&e, &unknown, Mode::AutoEdit), Decision::Ask);
    assert_eq!(d(&e, &unknown, Mode::Auto), Decision::Allow);
    let w = write("src/a.rs");
    assert_eq!(d(&e, &w, Mode::Ask), Decision::Ask);
    assert_eq!(d(&e, &w, Mode::AutoEdit), Decision::Allow);
    assert_eq!(d(&e, &w, Mode::Auto), Decision::Allow);
    assert_eq!(d(&e, &read("src/a.rs"), Mode::Ask), Decision::Allow);
    assert_eq!(decide(&e, &unknown, Mode::Ask).rule.layer, Layer::Mode);
}

#[test]
fn builtin_denies_beat_ini_allow() {
    let f = fx();
    let e = f.engine();
    // `cat*` is allowed in ini, but harness.ini is built-in protected.
    let v = decide(&e, &bash(&format!("cat {}", f.ini.display())), Mode::Auto);
    assert_eq!(v.decision, Decision::Deny);
    assert_eq!(v.rule.layer, Layer::BuiltIn);
}

#[test]
fn ini_deny_beats_ini_allow_and_yaml_tightens() {
    let f = fx();
    let mut cfg = f.config();
    cfg.bash_allow.push("rm*".into());
    let e = PolicyEngine::new(&cfg, &f.root).unwrap();
    assert_eq!(d(&e, &bash("rm -rf /tmp/x"), Mode::Auto), Decision::Deny);
    let overlay = PolicyOverlay {
        bash_deny: vec!["cargo test --ignored*".into()],
        bash_ask: vec!["ls -R*".into()],
        paths_protected: vec!["secrets/**".into()],
        outward_ask: vec![],
    };
    let e = e.with_overlay("agent implementer", &overlay).unwrap();
    let v = decide(&e, &bash("cargo test --ignored"), Mode::Auto);
    assert_eq!((v.decision, v.rule.layer), (Decision::Deny, Layer::Yaml));
    assert_eq!(v.rule.origin, "agent implementer policy.bash.deny");
    // ini allow `ls*` is tightened to ask by the overlay.
    let v = decide(&e, &bash("ls -R"), Mode::Auto);
    assert_eq!((v.decision, v.rule.layer), (Decision::Ask, Layer::Yaml));
    assert_eq!(d(&e, &read("secrets/k"), Mode::Auto), Decision::Deny);
    assert_eq!(d(&e, &bash("cargo test"), Mode::Ask), Decision::Allow);
}

#[test]
fn session_approvals_turn_ask_into_allow_but_never_deny() {
    let f = fx();
    let e = f.engine();
    e.grants().allow_command("git commit*");
    e.grants().allow_command("rm*");
    let v = decide(&e, &bash("git commit -m x"), Mode::Ask);
    assert_eq!(
        (v.decision, v.rule.layer),
        (Decision::Allow, Layer::Session)
    );
    assert_eq!(d(&e, &bash("rm -rf /etc"), Mode::Ask), Decision::Deny);
    e.grants().allow_path("src/**", &f.root).unwrap();
    let v = decide(&e, &write("src/a.rs"), Mode::Ask);
    assert_eq!(
        (v.decision, v.rule.layer),
        (Decision::Allow, Layer::Session)
    );
    e.grants().allow_path("**", &f.root).unwrap();
    assert_eq!(d(&e, &write(f.ini.clone()), Mode::Ask), Decision::Deny);
}

// ---- TASK-25 AC#2: built-in protected paths ----------------------------

#[test]
fn builtin_protected_paths_denied_for_read_and_write() {
    let f = fx();
    let e = f.engine();
    let cases = [
        f.ini.clone(),
        f.data.join("trust/trust.json"),
        f.data.join("runs/x/events.jsonl"),
        f.data.join("memory/index.md"),
        f.root.join(".git/hooks/pre-commit"),
        PathBuf::from(".git/hooks/post-checkout"),
    ];
    for p in cases {
        for a in [read(p.clone()), write(p.clone())] {
            let v = decide(&e, &a, Mode::Auto);
            assert_eq!(v.decision, Decision::Deny, "{a:?}");
            assert_eq!(v.rule.layer, Layer::BuiltIn, "{a:?}");
        }
        let v = decide(&e, &bash(&format!("cat {}", p.display())), Mode::Auto);
        assert_eq!(v.decision, Decision::Deny, "bash cat {}", p.display());
    }
    assert_eq!(
        d(&e, &write(".harness/agents/a.yaml"), Mode::Auto),
        Decision::Deny
    );
    assert_eq!(
        d(&e, &read(".harness/agents/a.yaml"), Mode::Auto),
        Decision::Allow
    );
}

#[test]
fn tree_writes_above_protected_roots_are_denied() {
    let f = fx();
    let e = f.engine();
    for c in [
        "rm -rf ~/.local",
        "rm -rf ~",
        "mv ~/.config /tmp/x",
        "chmod -R 777 ~/.config",
        "rm -rf /",
    ] {
        let v = decide(&e, &bash(c), Mode::Auto);
        assert_eq!(
            (v.decision, v.rule.layer),
            (Decision::Deny, Layer::BuiltIn),
            "{c}"
        );
    }
    assert_eq!(d(&e, &bash("rm -rf build"), Mode::Auto), Decision::Allow);
    assert_eq!(d(&e, &bash("ls ~"), Mode::Ask), Decision::Allow);
}

#[test]
fn symlinks_and_dotdot_do_not_bypass_protection() {
    let f = fx();
    std::os::unix::fs::symlink(f.data.join("trust"), f.root.join("t")).unwrap();
    let e = f.engine();
    assert_eq!(d(&e, &read("t/trust.json"), Mode::Auto), Decision::Deny);
    let sneaky = f.root.join("a/../../home/.config/harness/harness.ini");
    assert_eq!(d(&e, &read(sneaky), Mode::Auto), Decision::Deny);
}

// ---- TASK-25 AC#3: user protected paths --------------------------------

#[test]
fn user_protected_paths_apply_to_read_write_edit_and_bash() {
    let f = fx();
    let e = f.engine();
    let key = f.home.join(".ssh/id_rsa");
    for tool in ["read", "write", "edit"] {
        let a = Action::from_tool_call(tool, &json!({"path": key, "content": "x"}));
        let v = decide(&e, &a, Mode::Auto);
        assert_eq!(
            (v.decision, v.rule.layer),
            (Decision::Deny, Layer::Ini),
            "{tool}"
        );
        assert_eq!(v.rule.rule, "~/.ssh/**");
        assert_eq!(v.rule.origin, "harness.ini [policy] paths.protected");
    }
    for cmd in [
        "cat ~/.ssh/id_rsa",
        "cat $HOME/.ssh/id_rsa",
        "cat ${HOME}/.ssh/id_rsa",
        "cat ~/.ss*/id_*",
        "echo hi > ~/.ssh/authorized_keys",
        "cp x ~/.ssh/",
        "tool --key=~/.ssh/id_rsa",
        "ls && cat ~/.ssh/id_rsa",
        "echo $(cat ~/.ssh/id_rsa)",
        "cat app/.env.local",
        "grep x .env",
    ] {
        let v = decide(&e, &bash(cmd), Mode::Auto);
        assert_eq!(v.decision, Decision::Deny, "{cmd}");
    }
    assert_eq!(d(&e, &bash("cat README.md"), Mode::Ask), Decision::Allow);
    assert_eq!(d(&e, &read("~/.ssh"), Mode::Auto), Decision::Deny);
}

// ---- TASK-25 AC#5: events ----------------------------------------------

#[test]
fn every_decision_is_recorded_with_its_rule() {
    let f = fx();
    let sink = Arc::new(RecordingDecisions::default());
    let e = f.engine().with_sink(sink.clone());
    let actions = [
        bash("cargo test"),
        bash("git commit"),
        bash("rm -rf /x"),
        read("~/.ssh/id_rsa"),
        write("a.rs"),
        Action::Tool {
            name: "todo".into(),
        },
        bash("echo 'open"),
    ];
    let verdicts: Vec<Verdict> = actions
        .iter()
        .map(|a| e.decide(a, Mode::AutoEdit, &f.root))
        .collect();
    let recs = sink.records();
    assert_eq!(recs.len(), actions.len());
    for ((r, a), v) in recs.iter().zip(&actions).zip(&verdicts) {
        assert_eq!(&r.verdict, v);
        assert_eq!(r.action, a.summary());
        assert_eq!(r.mode, Mode::AutoEdit);
        assert!(!r.verdict.rule.rule.is_empty() && !r.verdict.rule.reason.is_empty());
    }
    assert_eq!(recs[0].verdict.rule.rule, "cargo test*");
    assert_eq!(recs[2].verdict.rule.rule, "rm -rf /*");
    let json = serde_json::to_value(&recs[2]).unwrap();
    assert_eq!(json["decision"], "deny");
    assert_eq!(json["rule"]["layer"], "ini");
}

// ---- TASK-26: shell parsing in policy -----------------------------------

#[test]
fn each_simple_command_is_checked() {
    let f = fx();
    let e = f.engine();
    let denied = [
        "ls && rm -rf /etc",
        "ls; rm -rf /etc",
        "ls | rm -rf /etc",
        "ls || rm -rf /etc",
        "ls & rm -rf /etc",
        "(rm -rf /etc)",
        "{ rm -rf /etc; }",
        "echo $(rm -rf /etc)",
        "echo `rm -rf /etc`",
        "cat <(rm -rf /etc)",
        "FOO=1 rm -rf /etc",
        "sudo rm -rf /etc",
        "env A=b rm -rf /etc",
        "bash -c 'rm -rf /etc'",
        "sh -c \"ls; rm -rf /etc\"",
        "eval rm -rf /etc",
        "xargs rm -rf /etc",
        "find . -exec rm -rf /etc \\;",
        "r''m -rf /etc",
        "\\rm -rf /etc",
        "/bin/rm -rf /etc",
        "$'\\x72m' -rf /etc",
        "if true; then rm -rf /etc; fi",
        "for x in a; do rm -rf /etc; done",
        "git -C . push --force",
        "ls\nrm -rf /etc",
    ];
    for c in denied {
        assert_eq!(d(&e, &bash(c), Mode::Auto), Decision::Deny, "{c}");
    }
    // An allowed program does not launder a chained one.
    assert_eq!(
        d(&e, &bash("cargo test && make deploy-all"), Mode::AutoEdit),
        Decision::Ask
    );
    assert_eq!(d(&e, &bash("ls && cargo test"), Mode::Ask), Decision::Allow);
}

#[test]
fn unparseable_commands_ask() {
    let f = fx();
    let e = f.engine();
    for c in [
        "echo 'open",
        "$CMD x",
        "f() { ls; }; f",
        "case a in a) ls;; esac",
        "eval \"$X\"",
    ] {
        let v = decide(&e, &bash(c), Mode::Auto);
        assert_eq!(v.decision, Decision::Ask, "{c}");
        assert_eq!(v.rule.rule, "unparseable command");
    }
    // Deny rules still match the raw text.
    assert_eq!(d(&e, &bash("rm -rf /x 'open"), Mode::Auto), Decision::Deny);
}

#[test]
fn allow_rules_do_not_cover_runtime_values() {
    let f = fx();
    let e = f.engine();
    assert_eq!(d(&e, &bash("cat $FILE"), Mode::AutoEdit), Decision::Ask);
    assert_eq!(d(&e, &bash("cat $FILE"), Mode::Auto), Decision::Allow);
}

// ---- TASK-28: outward guard ----------------------------------------------

#[test]
fn outward_actions_ask_even_in_auto() {
    let f = fx();
    let e = f.engine();
    for c in [
        "git push",
        "git push origin main",
        "gh pr create --fill",
        "cargo publish",
        "npm publish",
        "deploy prod",
    ] {
        let v = decide(&e, &bash(c), Mode::Auto);
        assert_eq!(v.decision, Decision::Ask, "{c}");
        assert!(v.outward, "{c}");
    }
    let v = decide(&e, &bash("deploy prod"), Mode::Auto);
    assert_eq!(
        (v.rule.layer, v.rule.origin.as_str()),
        (Layer::Ini, "harness.ini [policy] outward.ask")
    );
    let v = decide(&e, &bash("git push"), Mode::Auto);
    assert_eq!(v.rule.layer, Layer::BuiltIn);
}

#[test]
fn outward_chained_and_aliased_forms() {
    let f = fx();
    let e = f.engine();
    for c in [
        "cargo test && git push",
        "git status; git push",
        "ls | xargs git push",
        "echo $(git push)",
        "(cd sub && git push)",
        "bash -c 'git push'",
        "sh -lc \"cargo publish --dry-run=false\"",
        "sudo -u me git push",
        "env GIT_TRACE=1 git push",
        "command git push",
        "/usr/bin/git push",
        "\\git push",
        "g\"i\"t push",
        "git -C repo -c user.name=x --no-pager push",
        "git --git-dir=.git push",
        "gh pr new",
        "gh -R o/r pr new --fill",
        "npm --registry https://r.example publish",
        "cargo +nightly publish",
        "docker image push img",
        "nohup git push &",
        "timeout 30 git push",
        "eval git push",
        "if true; then git push; fi",
        "GIT_SSH_COMMAND=ssh git push",
    ] {
        let v = decide(&e, &bash(c), Mode::Auto);
        assert_eq!(v.decision, Decision::Ask, "{c}");
        assert!(v.outward, "{c}");
    }
    assert_eq!(d(&e, &bash("git status"), Mode::Auto), Decision::Allow);
    assert_eq!(
        d(&e, &bash("git pushx"), Mode::Auto),
        Decision::Ask,
        "conservative glob"
    );
}

#[test]
fn outward_explicit_ini_allow_and_precedence() {
    let f = fx();
    let mut cfg = f.config();
    cfg.bash_allow
        .extend(["git *".into(), "*".into(), "cargo pub*".into()]);
    let e = PolicyEngine::new(&cfg, &f.root).unwrap();
    // Broad allow rules do not count as explicitly allowing outward actions.
    assert_eq!(d(&e, &bash("git push"), Mode::Auto), Decision::Ask);
    assert_eq!(d(&e, &bash("cargo publish"), Mode::Auto), Decision::Ask);
    assert_eq!(d(&e, &bash("git log"), Mode::Ask), Decision::Allow);
    // An allow rule that names the outward command does.
    cfg.bash_allow.push("git push origin feature/*".into());
    let e = PolicyEngine::new(&cfg, &f.root).unwrap();
    let v = decide(&e, &bash("git push origin feature/x"), Mode::Ask);
    assert_eq!(
        (v.decision, v.rule.origin.as_str(), v.outward),
        (Decision::Allow, "harness.ini [policy] bash.allow", true)
    );
    let v = decide(&e, &bash("git -C sub push origin feature/x"), Mode::Ask);
    assert_eq!(v.decision, Decision::Allow, "canonical forms count");
    assert_eq!(
        d(&e, &bash("git push origin feature/$B"), Mode::Auto),
        Decision::Ask
    );
    assert_eq!(
        d(&e, &bash("git push origin main"), Mode::Auto),
        Decision::Ask
    );
    // Deny still wins over an explicit allow.
    assert_eq!(
        d(&e, &bash("git push --force origin feature/x"), Mode::Auto),
        Decision::Deny
    );
    // A session grant (explicit user approval) allows it.
    e.grants().allow_command("git push origin main*");
    assert_eq!(
        d(&e, &bash("git push origin main"), Mode::Auto),
        Decision::Allow
    );
    // YAML can add outward rules.
    let o = PolicyOverlay {
        outward_ask: vec!["make release*".into()],
        ..PolicyOverlay::default()
    };
    let e = e.with_overlay("workflow release", &o).unwrap();
    let v = decide(&e, &bash("make release"), Mode::Auto);
    assert_eq!(
        (v.decision, v.outward, v.rule.layer),
        (Decision::Ask, true, Layer::Yaml)
    );
}

#[test]
fn from_tool_call_mapping() {
    assert_eq!(
        Action::from_tool_call("bash", &json!({"command": "ls"})),
        Action::Bash {
            command: "ls".into()
        }
    );
    assert!(matches!(
        Action::from_tool_call("edit", &json!({"path": "a"})),
        Action::Write { .. }
    ));
    assert!(matches!(
        Action::from_tool_call("read", &json!({"path": "a"})),
        Action::Read { .. }
    ));
    assert_eq!(
        Action::from_tool_call("spawn_agent", &json!({})),
        Action::Tool {
            name: "spawn_agent".into()
        }
    );
}

#[test]
fn invalid_globs_are_reported() {
    let f = fx();
    let mut cfg = f.config();
    cfg.paths_protected.push("a/[".into());
    let err = PolicyEngine::new(&cfg, &f.root).unwrap_err();
    assert!(err.to_string().contains("a/["));
}

// ---- harness.ini integration ---------------------------------------------

#[test]
fn builds_from_harness_ini() {
    let f = fx();
    let ini = "[general]\nmode = ask\n\n[policy]\nbash.allow = make test*\nbash.deny = curl*\npaths.protected = ~/.ssh/**\nouterward = x\n";
    assert!(
        harness_config::ini::load_str(ini, None).is_err(),
        "config rejects unknown keys"
    );
    let ini = ini.replace("outerward = x\n", "outward.ask = ship*\n");
    let cfg = harness_config::ini::load_str(&ini, None).unwrap();
    let pc = PolicyConfig::from_ini(
        &cfg,
        vec![f.ini.clone(), f.data.clone()],
        Some(f.home.clone()),
    );
    let e = PolicyEngine::new(&pc, &f.root).unwrap();
    let mode = Mode::from(cfg.general.mode);
    assert_eq!(mode, Mode::Ask);
    assert_eq!(d(&e, &bash("make test"), mode), Decision::Allow);
    assert_eq!(d(&e, &bash("curl x"), Mode::Auto), Decision::Deny);
    assert_eq!(d(&e, &bash("ship it"), Mode::Auto), Decision::Ask);
    assert_eq!(d(&e, &read("~/.ssh/id_rsa"), Mode::Auto), Decision::Deny);
    assert_eq!(
        d(&e, &read(f.data.join("trust/t")), Mode::Auto),
        Decision::Deny
    );
    let full = PolicyConfig::from_config(&cfg);
    assert!(!full.protected_roots.is_empty());
    assert!(
        full.protected_roots
            .iter()
            .any(|p| p.ends_with("harness.ini")),
        "{:?}",
        full.protected_roots
    );
}

#[test]
fn agent_policy_fragment_becomes_overlay() {
    let f = fx();
    let frag = harness_config::agents::PolicyFragment {
        bash_deny: vec!["git push*".into()],
        bash_ask: vec![],
        paths_protected: vec!["secrets/**".into()],
        outward_ask: vec![],
    };
    let e = f
        .engine()
        .with_overlay("agent a", &PolicyOverlay::from(&frag))
        .unwrap();
    assert_eq!(d(&e, &bash("git push"), Mode::Auto), Decision::Deny);
    assert_eq!(d(&e, &write("secrets/x"), Mode::Auto), Decision::Deny);
}

// ---- TASK-146.2: MCP tools ---------------------------------------------

#[test]
fn mcp_tools_follow_ini_rules_and_are_outward_by_default() {
    use crate::mcp::{McpRules, ReadOnlyTools, action_name};
    use harness_config::mcp_tools::{McpPolicy, McpToolRules};
    let f = fx();
    let mut p = McpPolicy::new();
    p.insert(
        "github".into(),
        McpToolRules {
            allow: vec!["get_*".into()],
            ask: vec!["get_secret*".into()],
            deny: vec!["delete_*".into()],
            trust_annotations: false,
        },
    );
    p.insert(
        "docs".into(),
        McpToolRules {
            trust_annotations: true,
            ..McpToolRules::default()
        },
    );
    let ro = Arc::new(ReadOnlyTools::default());
    ro.set("docs", ["search".to_owned()]);
    ro.set("github", ["list_repos".to_owned()]);
    let e = f.engine().with_mcp(McpRules::new(p, ro));
    let mcp = |s: &str, t: &str| Action::Tool {
        name: action_name(s, t),
    };
    for mode in Mode::ALL {
        let v = decide(&e, &mcp("github", "delete_repo"), mode);
        assert_eq!(v.decision, Decision::Deny, "{mode}");
        assert_eq!(v.rule.origin, "harness.ini [policy] mcp.github.deny");
        assert_eq!(v.rule.rule, "delete_*");
        let v = decide(&e, &mcp("github", "get_issue"), mode);
        assert_eq!((v.decision, v.rule.layer), (Decision::Allow, Layer::Ini));
        assert_eq!(
            d(&e, &mcp("github", "get_secret_value"), mode),
            Decision::Ask
        );
        // No rule: an outward action that asks even in auto mode.
        let v = decide(&e, &mcp("github", "create_issue"), mode);
        assert_eq!((v.decision, v.outward), (Decision::Ask, true), "{mode}");
        assert!(
            v.rule.reason.contains("mcp.github.allow"),
            "{}",
            v.rule.reason
        );
        // Annotations count only where the user trusts them.
        assert_eq!(d(&e, &mcp("docs", "search"), mode), Decision::Allow);
        assert_eq!(d(&e, &mcp("docs", "publish"), mode), Decision::Ask);
        assert_eq!(d(&e, &mcp("github", "list_repos"), mode), Decision::Ask);
        // Unconfigured servers ask too.
        assert_eq!(d(&e, &mcp("other", "x"), mode), Decision::Ask);
    }
    // Session grants turn asks into allows, never denies.
    e.grants().allow_tool("mcp:github/*");
    assert_eq!(
        d(&e, &mcp("github", "create_issue"), Mode::Ask),
        Decision::Allow
    );
    assert_eq!(
        d(&e, &mcp("github", "get_secret_value"), Mode::Ask),
        Decision::Allow
    );
    assert_eq!(
        d(&e, &mcp("github", "delete_repo"), Mode::Auto),
        Decision::Deny
    );
    // Without MCP rules every MCP tool asks; other tools stay mode-decided.
    let plain = f.engine();
    assert_eq!(
        d(&plain, &mcp("github", "get_issue"), Mode::Auto),
        Decision::Ask
    );
    assert_eq!(
        d(
            &plain,
            &Action::Tool {
                name: "todo".into()
            },
            Mode::Ask
        ),
        Decision::Allow
    );
}
