use super::*;
use crate::discovery::{BuiltinFile, Roots};
use std::path::Path;

const FULL: &str = r#"name: implementer
description: Writes code for a scoped task
model: sonnet
effort: medium
prompt: coder
tools: [read, write, edit, bash, todo]
subagents: [researcher]
memory: read-write
max_turns: 60
budget: { usd: 2.00, tokens: 2_000_000 }
scope: ["src/**", "tests/**"]
policy:
  bash.deny: ["git push*"]
  paths.protected: "secrets/**"
verify: cargo test -p core
runs_on: { labels: [gpu], min_mem_gb: 32 }
output_schema:
  type: object
  properties: { summary: { type: string }, files_changed: { type: array } }
"#;

fn load(text: &str) -> Result<Agent, YamlError> {
    Agent::load(&SourceFile::inline(
        Scope::Global,
        Kind::Agent,
        "agents/a.yaml",
        text,
    ))
}

fn err(text: &str) -> YamlError {
    load(text).unwrap_err()
}

#[test]
fn every_spec_field_is_supported() {
    let a = load(FULL).unwrap();
    assert_eq!(a.name, "implementer");
    assert_eq!(
        a.description.as_deref(),
        Some("Writes code for a scoped task")
    );
    assert_eq!(a.model.as_deref(), Some("sonnet"));
    assert_eq!(a.effort, Some(Effort::Medium));
    assert_eq!(a.prompt, PromptRef::Named("coder".into()));
    assert_eq!(a.tools, ["read", "write", "edit", "bash", "todo"]);
    assert_eq!(a.subagents, ["researcher"]);
    assert_eq!(a.memory, MemoryAccess::ReadWrite);
    assert_eq!(a.max_turns, Some(60));
    assert_eq!(
        a.budget,
        AgentBudget {
            usd: Some(2.0),
            tokens: Some(2_000_000)
        }
    );
    assert_eq!(a.scope, ["src/**", "tests/**"]);
    assert_eq!(a.policy.bash_deny, ["git push*"]);
    assert_eq!(a.policy.paths_protected, ["secrets/**"]);
    assert_eq!(a.verify.as_deref(), Some("cargo test -p core"));
    assert_eq!(
        a.runs_on,
        RunsOn::Labels {
            labels: vec!["gpu".into()],
            min_mem_gb: Some(32),
            min_cpus: None,
            min_gpus: None,
            fallback: None,
        }
    );
    assert_eq!(
        a.output_schema.as_ref().unwrap()["properties"]["summary"]["type"],
        "string"
    );

    let b = load("name: b\nprompt_text: hi\nruns_on: gpu-box\n").unwrap();
    assert_eq!(b.prompt, PromptRef::Inline("hi".into()));
    assert_eq!(b.runs_on, RunsOn::Remote("gpu-box".into()));
    assert_eq!(b.memory, MemoryAccess::Off);
    assert!(b.tools.is_empty());
    assert_eq!(
        load("name: c\nprompt_text: x\nruns_on: local\n")
            .unwrap()
            .runs_on,
        RunsOn::Local
    );
    for effort in ["low", "high"] {
        assert!(load(&format!("name: c\nprompt_text: x\neffort: {effort}\n")).is_ok());
    }
}

#[test]
fn file_local_errors_are_located() {
    let e = err("name: a\nprompt_text: x\ntools: [read, browse]\n");
    assert_eq!(
        (e.line, e.message.as_str()),
        (
            3,
            "unknown tool `browse`; known tools: read, write, edit, bash, grep, glob, todo, or mcp:<server>/<tool> for MCP tools"
        )
    );
    assert!(
        err("name: a\nprompt_text: x\ntools: [spawn_agent]\n")
            .message
            .contains("enabled by listing `subagents:`")
    );
    assert!(
        err("name: a\nprompt_text: x\ntools: [read, read]\n")
            .message
            .contains("listed twice")
    );
    assert!(err("name: a\n").message.contains("missing `prompt`"));
    assert!(
        err("name: a\nprompt: p\nprompt_text: x\n")
            .message
            .contains("not both")
    );
    let e = err("name: a\nprompt_text: x\nscope: [\"src/[\"]\n");
    assert_eq!(e.line, 3);
    assert!(e.message.contains("invalid scope glob"), "{e}");
    assert!(
        err("name: a\nprompt_text: x\nmax_turns: 0\n")
            .message
            .contains("at least 1")
    );
    assert!(
        err("name: a\nprompt_text: x\nmemory: write\n")
            .message
            .contains("unknown variant")
    );
    assert!(
        err("name: a\nprompt_text: x\nmodel: anthropic/\n")
            .message
            .contains("provider/model")
    );
    let e = err("name: a\nprompt_text: x\nbudget:\n  usd: -1\n");
    assert_eq!(e.line, 4);
    let e = err("name: a\nprompt_text: x\nbudget: { dollars: 1 }\n");
    assert!(e.message.contains("unknown field `dollars`"), "{e}");
    assert!(
        err("name: a\nprompt_text: x\noutput_schema: [1]\n")
            .message
            .contains("JSON Schema object")
    );
}

#[test]
fn policy_can_only_tighten() {
    let e = err("name: a\nprompt_text: x\npolicy:\n  bash.deny: [x]\n  bash.allow: [\"rm *\"]\n");
    assert_eq!(e.line, 5);
    assert!(
        e.message
            .contains("can only tighten; `bash.allow` would loosen it"),
        "{e}"
    );
    let e = err("name: a\nprompt_text: x\npolicy:\n  sandbox.enabled: [\"false\"]\n");
    assert_eq!(e.line, 4);
    assert!(
        e.message.contains("unknown policy key `sandbox.enabled`"),
        "{e}"
    );
    let a = load(
        "name: a\nprompt_text: x\npolicy:\n  bash.ask: \"npm *\"\n  outward.ask: [\"gh pr*\"]\n",
    )
    .unwrap();
    assert_eq!(a.policy.bash_ask, ["npm *"]);
    assert_eq!(a.policy.outward_ask, ["gh pr*"]);
}

#[test]
fn request_tools_are_exactly_the_configured_ones() {
    let a = load(FULL).unwrap();
    assert_eq!(
        a.request_tools(),
        [
            "read",
            "write",
            "edit",
            "bash",
            "todo",
            "spawn_agent",
            "memory"
        ]
    );
    let ro = load("name: r\nprompt_text: x\ntools: [read]\n").unwrap();
    assert_eq!(ro.request_tools(), ["read"]);
    let none = load("name: n\nprompt_text: x\n").unwrap();
    assert!(none.request_tools().is_empty());

    // select_tools filters an available set in the agent's order.
    let available = [
        "bash",
        "edit",
        "memory",
        "read",
        "spawn_agent",
        "todo",
        "write",
        "extra",
    ];
    let (picked, missing) = ro.select_tools(&available, |t| t);
    assert_eq!(picked, [&"read"]);
    assert!(missing.is_empty());
    let (picked, _) = a.select_tools(&available, |t| t);
    assert_eq!(
        picked.into_iter().copied().collect::<Vec<_>>(),
        a.request_tools()
    );
    let (_, missing) = a.select_tools(&["read"], |t| t);
    assert_eq!(
        missing,
        ["write", "edit", "bash", "todo", "spawn_agent", "memory"]
    );
}

static BUILTINS: &[BuiltinFile] = &[
    BuiltinFile {
        path: "prompts/coder.yaml",
        contents: "name: coder\nlayers: [{text: hi}]\n",
    },
    BuiltinFile {
        path: "agents/base.yaml",
        contents: "name: base\nprompt: coder\ntools: [read, edit]\nmemory: read\nmax_turns: 20\nbudget: { usd: 1, tokens: 1000 }\nscope: [\"src/**\"]\npolicy:\n  bash.deny: [\"git push*\"]\n",
    },
    BuiltinFile {
        path: "agents/researcher.yaml",
        contents: "name: researcher\nprompt: coder\ntools: [read]\n",
    },
];

fn ctx() -> ValidationContext {
    let mut cfg = Config::defaults();
    cfg.models
        .insert("sonnet".into(), "anthropic/claude-sonnet-5-5".into());
    cfg.budget.run_usd = Some(5.0);
    cfg.remotes.insert(
        "gpu-box".into(),
        crate::ini::Remote {
            host: "10.0.0.1".into(),
            user: None,
            port: 22,
            identity_file: None,
            labels: vec!["gpu".into(), "bigmem".into()],
            workdir: None,
            max_jobs: 1,
            sync: crate::ini::config::SyncMethod::Rsync,
            policy_bash_deny: vec![],
            policy_bash_ask: vec![],
            compression: crate::ini::config::SshCompression::Auto,
            compression_below_mbps: 20.0,
        },
    );
    ValidationContext::from_config(&cfg)
}

fn catalog_with(global: &[(&str, &str)], project: &[(&str, &str)]) -> (tempfile::TempDir, Catalog) {
    let tmp = tempfile::tempdir().unwrap();
    let put = |root: &Path, files: &[(&str, &str)]| {
        for (rel, text) in files {
            let p = root.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, text).unwrap();
        }
    };
    put(&tmp.path().join("g"), global);
    put(&tmp.path().join("p"), project);
    let roots = Roots::new(BUILTINS)
        .with_global(tmp.path().join("g"))
        .with_project(tmp.path().join("p"));
    let c = Catalog::load(&roots);
    assert!(c.errors().is_empty(), "{:?}", c.errors());
    (tmp, c)
}

fn messages(c: &Catalog) -> Vec<String> {
    c.validate_agents(&ctx())
        .iter()
        .map(ToString::to_string)
        .collect()
}

#[test]
fn references_are_validated_at_load() {
    let (_t, c) = catalog_with(
        &[(
            "agents/bad.yaml",
            "name: bad\nmodel: gpt9\nprompt: nope\nsubagents: [researcher, ghost]\nruns_on: tpu-box\nbudget: { usd: 9 }\n",
        )],
        &[],
    );
    let m = messages(&c);
    assert_eq!(m.len(), 5, "{m:?}");
    let find = |s: &str| {
        m.iter()
            .find(|x| x.contains(s))
            .unwrap_or_else(|| panic!("{s} not in {m:?}"))
            .clone()
    };
    assert!(find("unknown model alias `gpt9`").contains("bad.yaml:2:1: "));
    assert!(find("unknown prompt `nope`").contains("bad.yaml:3:1: "));
    assert!(find("unknown subagent `ghost`").contains("bad.yaml:4:1: "));
    assert!(find("unknown remote `tpu-box`").contains("bad.yaml:5:1: "));
    assert!(find("exceeds [budget] run_usd 5").contains("bad.yaml:6:1: "));

    let (_t, c) = catalog_with(
        &[
            (
                "agents/ok.yaml",
                "name: ok\nmodel: anthropic/claude-x\nprompt: coder\nsubagents: [researcher]\nruns_on: gpu-box\n",
            ),
            (
                "agents/lab.yaml",
                "name: lab\nmodel: sonnet\nprompt_text: x\nruns_on: { labels: [gpu, bigmem] }\n",
            ),
            (
                "agents/nolab.yaml",
                "name: nolab\nmodel: other/x\nprompt_text: x\nruns_on: { labels: [tpu] }\n",
            ),
        ],
        &[],
    );
    let m = messages(&c);
    assert_eq!(m.len(), 2, "{m:?}");
    assert!(m.iter().any(|x| x.contains("unknown provider `other`")));
    assert!(
        m.iter()
            .any(|x| x.contains("no [remote.<name>] in harness.ini has all labels [tpu]"))
    );
    // Built-in aliases from the model registry can be added.
    let refs = ModelRefs::default().with_aliases(["opus"]);
    assert!(refs.check("opus").is_ok());
}

#[test]
fn project_agent_may_not_loosen_the_agent_it_shadows() {
    let loose = "name: base\nprompt: coder\ntools: [read, edit, bash]\nmemory: read-write\nmax_turns: 50\n\
                 budget: { usd: 3 }\nscope: [\"**\"]\nsubagents: [researcher]\n";
    let (_t, c) = catalog_with(&[], &[("agents/base.yaml", loose)]);
    let m = messages(&c);
    for want in [
        "adds tools [\"bash\"]",
        "adds subagents",
        "raises memory access",
        "raises max_turns above 20",
        "raises budget.usd above 1",
        "raises budget.tokens above 1000",
        "widens scope with [\"**\"]",
        "drops policy rules [bash.deny: git push*]",
    ] {
        assert!(m.iter().any(|x| x.contains(want)), "missing {want}: {m:#?}");
    }
    assert!(
        m.iter()
            .all(|x| x.contains("project agents may only tighten `base`"))
    );

    let tight = "name: base\nprompt: coder\ntools: [read]\nmemory: off\nmax_turns: 10\n\
                 budget: { usd: 0.5, tokens: 500 }\nscope: [\"src/core/**\"]\n\
                 policy:\n  bash.deny: [\"git push*\", \"rm *\"]\n";
    let (_t, c) = catalog_with(&[], &[("agents/base.yaml", tight)]);
    assert!(messages(&c).is_empty(), "{:?}", messages(&c));
    // A global agent shadowing a built-in is user-owned and may differ.
    let (_t, c) = catalog_with(&[("agents/base.yaml", loose)], &[]);
    assert!(messages(&c).is_empty(), "{:?}", messages(&c));
}

#[test]
fn mode_parses_and_can_only_tighten_a_shadowed_agent() {
    use crate::ini::config::Mode;
    let a = load("name: a\nprompt_text: x\nmode: ask\n").unwrap();
    assert_eq!(a.mode, Some(Mode::Ask));
    assert_eq!(load("name: a\nprompt_text: x\n").unwrap().mode, None);
    let e = err("name: a\nprompt_text: x\nmode: yolo\n");
    assert!(e.message.contains("unknown mode `yolo`"), "{e}");
    assert_eq!(e.line, 3);

    let mut base = load("name: a\nprompt_text: x\nmode: auto-edit\n").unwrap();
    let looser = load("name: a\nprompt_text: x\nmode: auto\n").unwrap();
    let unset = load("name: a\nprompt_text: x\n").unwrap();
    let stricter = load("name: a\nprompt_text: x\nmode: ask\n").unwrap();
    let keys = |a: &Agent, b: &Agent| -> Vec<&'static str> {
        tighten_violations(a, b)
            .into_iter()
            .map(|(k, _)| k)
            .collect()
    };
    assert_eq!(keys(&looser, &base), ["mode"]);
    assert_eq!(
        keys(&unset, &base),
        ["mode"],
        "dropping the mode loosens it"
    );
    assert!(keys(&stricter, &base).is_empty());
    base.mode = None;
    assert!(keys(&looser, &base).is_empty(), "no base mode to loosen");
}

#[test]
fn runs_on_selectors_take_resource_minimums_and_a_fallback() {
    let a = load(
        "name: a\nprompt_text: x\nruns_on: { labels: [gpu], min_cpus: 8, min_gpus: 2, fallback: local }\n",
    )
    .unwrap();
    assert_eq!(
        a.runs_on,
        RunsOn::Labels {
            labels: vec!["gpu".into()],
            min_mem_gb: None,
            min_cpus: Some(8),
            min_gpus: Some(2),
            fallback: Some("local".into()),
        }
    );
    assert!(load("name: a\nprompt_text: x\nruns_on: { min_gpus: 1 }\n").is_ok());
    let e = err("name: a\nprompt_text: x\nruns_on: { fallback: local }\n");
    assert!(e.message.contains("needs `labels`"), "{e}");
    assert!(
        err("name: a\nprompt_text: x\nruns_on: { labels: [x], fallback: \"\" }\n")
            .message
            .contains("`fallback` must be")
    );
}

#[test]
fn runs_on_fallback_hosts_are_validated_and_allow_unmatched_labels() {
    let (_t, c) = catalog_with(
        &[
            (
                "agents/a.yaml",
                "name: a\nprompt_text: x\nruns_on: { labels: [tpu], fallback: local }\n",
            ),
            (
                "agents/b.yaml",
                "name: b\nprompt_text: x\nruns_on: { labels: [gpu], fallback: nowhere }\n",
            ),
        ],
        &[],
    );
    let m = messages(&c);
    assert!(
        !m.iter().any(|x| x.contains("agents/a.yaml")),
        "a fallback runs it anyway: {m:?}"
    );
    assert!(
        m.iter()
            .any(|x| x.contains("unknown fallback remote `nowhere`")),
        "{m:?}"
    );
}

#[test]
fn display_name_is_optional_and_defaults_from_the_name() {
    let a = load("name: bugfix-proof\nprompt_text: x\n").unwrap();
    assert_eq!(a.display_name, None);
    assert_eq!(a.display_name(), "Bugfix proof");
    assert_eq!(a.summary(), "Run the Bugfix proof agent");
    let b = load(
        "name: implementer\ndisplay_name: Code Smith\ndescription: |\n  Writes code\n  for a scoped task\nprompt_text: x\n",
    )
    .unwrap();
    assert_eq!(b.display_name(), "Code Smith");
    assert_eq!(b.summary(), "Writes code");
    let e = err("name: a\ndisplay_name: '  '\nprompt_text: x\n");
    assert!(e.to_string().contains("display_name"), "{e}");
}

#[test]
fn schema_lists_display_name() {
    let s = Agent::json_schema();
    assert_eq!(s["properties"]["display_name"]["type"], "string");
}

#[test]
fn builtin_agents_have_display_names_and_one_line_descriptions() {
    let c = crate::discovery::Catalog::load(&crate::builtins::roots());
    for (name, display) in [
        ("coder", "Coder"),
        ("researcher", "Researcher"),
        ("reviewer", "Reviewer"),
    ] {
        let a = c.agents.get(name).expect(name);
        assert_eq!(a.display_name.as_deref(), Some(display));
        let d = a.description.as_deref().expect(name);
        assert_eq!(d.lines().count(), 1, "{name}: {d}");
        assert_eq!(a.summary(), d);
    }
}

#[test]
fn mcp_tools_are_selected_explicitly_and_kept_apart() {
    let a =
        load("name: a\nprompt: coder\ntools: [read, mcp:github/create_issue, edit, mcp:db/*]\n")
            .unwrap();
    assert_eq!(a.tools, ["read", "edit"]);
    let sel: Vec<String> = a.mcp_tools.iter().map(ToString::to_string).collect();
    assert_eq!(sel, ["mcp:github/create_issue", "mcp:db/*"]);
    // Only built-in tools are request tools; MCP tools come from servers.
    assert_eq!(a.request_tools(), ["read", "edit"]);
    // Serialized like the YAML.
    let json = serde_json::to_value(&a).unwrap();
    assert_eq!(json["mcp_tools"][1], "mcp:db/*");
    // No MCP entry, no `mcp_tools` key (config hashes stay the same).
    let plain =
        serde_json::to_value(load("name: b\nprompt: coder\ntools: [read]\n").unwrap()).unwrap();
    assert!(plain.get("mcp_tools").is_none());

    for (yaml, want) in [
        ("tools: [mcp:github]", "mcp:github/*"),
        ("tools: [mcp:git hub/x]", "not a valid MCP server name"),
        ("tools: [mcp:db/*, mcp:db/*]", "listed twice"),
        ("tools: [github/create_issue]", "mcp:<server>/<tool>"),
    ] {
        let e = err(&format!("name: a\nprompt: coder\n{yaml}\n")).to_string();
        assert!(e.contains(want), "{yaml}: {e}");
        assert!(e.contains("a.yaml:3:"), "{yaml}: {e}");
    }
}

#[test]
fn mcp_tools_may_only_name_servers_from_harness_ini() {
    let (_t, c) = catalog_with(
        &[],
        &[(
            "agents/gh.yaml",
            "name: gh\nprompt: coder\ntools: [read, mcp:github/create_issue, mcp:ghost/*]\n",
        )],
    );
    let mut cx = ctx();
    cx.mcp_servers.insert("github".into());
    let m: Vec<String> = c
        .validate_agents(&cx)
        .iter()
        .map(ToString::to_string)
        .collect();
    assert_eq!(m.len(), 1, "{m:?}");
    assert!(m[0].contains("MCP server `ghost`"), "{m:?}");
    assert!(m[0].contains("[mcp.ghost]"), "{m:?}");
    assert!(m[0].contains("gh.yaml:3:1: "), "{m:?}");
    // From harness.ini.
    let cfg = crate::ini::load_str("[mcp.github]\ncommand = gh-mcp\n", None).unwrap();
    assert!(
        ValidationContext::from_config(&cfg)
            .mcp_servers
            .contains("github")
    );
}

#[test]
fn project_agent_may_not_add_mcp_tools_to_the_agent_it_shadows() {
    let (_t, c) = catalog_with(
        &[],
        &[(
            "agents/base.yaml",
            "name: base\nprompt: coder\ntools: [read, mcp:github/*]\nmemory: off\nmax_turns: 10\n\
             budget: { usd: 0.5, tokens: 500 }\nscope: [\"src/**\"]\npolicy:\n  bash.deny: [\"git push*\"]\n",
        )],
    );
    let mut cx = ctx();
    cx.mcp_servers.insert("github".into());
    let m: Vec<String> = c
        .validate_agents(&cx)
        .iter()
        .map(ToString::to_string)
        .collect();
    assert!(
        m.iter()
            .any(|x| x.contains("adds MCP tools [\"mcp:github/*\"]")),
        "{m:?}"
    );
}
