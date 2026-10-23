use super::*;

/// The doc-3 example, verbatim.
pub(crate) const FIX_BUG: &str = r#"name: fix-bug
description: Reproduce, fix and prove a bug fix
command: fix                  # slash command: /fix (defaults to name)
inputs:
  issue: { type: string, required: true, positional: 0 }
  scope: { type: list, default: ["src/**", "tests/**"] }
budget: { usd: 5 }
goal: "Fix: {{ inputs.issue }}"   # shown at the top of the goal bar
tasks:                        # optional; seeds the goal bar, agents may add more via the todo tool
  - { id: repro, title: "Reproduce with failing test", step: repro }
  - { id: fix,   title: "Fix the bug",                 step: fix }
  - { id: proof, title: "Bug-fix proof",               step: proof }
steps:
  - id: repro
    agent: implementer
    prompt: "Write a failing test that reproduces: {{ inputs.issue }}. Do not fix it yet."
    scope: ["tests/**"]
    outputs: [test_name]

  - id: prove-fail
    needs: [repro]
    verify: { command: "cargo test {{ steps.repro.outputs.test_name }}", expect: fail }

  - id: fix
    needs: [prove-fail]
    agent: implementer
    scope: "{{ inputs.scope }}"
    prompt: "Make {{ steps.repro.outputs.test_name }} pass with a minimal change."
    retries: 1

  - id: proof
    needs: [fix]
    type: bugfix-proof        # pass-after, revert fix → fail, restore
    test: "{{ steps.repro.outputs.test_name }}"

  - id: review
    needs: [proof]
    agent: reviewer
    model: opus               # independent model for checking
    loop: { until: "{{ steps.review.outputs.verdict == 'approve' }}", max_iterations: 3, retry_step: fix }

  - id: confirm
    needs: [review]
    type: approval
    message: "Ship this change?"
"#;

fn errs(text: &str) -> String {
    parse(text, Some(Path::new("wf.yaml")))
        .unwrap_err()
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n")
}

fn errs_with(text: &str, ctx: &CheckContext) -> String {
    parse_with(text, Some(Path::new("wf.yaml")), ctx)
        .unwrap_err()
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn parses_the_spec_example() {
    let wf = parse(FIX_BUG, None).unwrap();
    assert_eq!(wf.name, "fix-bug");
    assert_eq!(wf.command, "fix");
    assert_eq!(wf.inputs[0].name, "issue");
    assert!(wf.inputs[0].required);
    assert_eq!(wf.inputs[0].positional, Some(0));
    assert_eq!(wf.inputs[1].kind, InputType::List);
    assert!(!wf.inputs[1].required);
    assert_eq!(wf.budget.unwrap().usd, Some(5.0));
    assert_eq!(wf.tasks.len(), 3);
    assert_eq!(wf.tasks[1].step.as_deref(), Some("fix"));
    let kinds: Vec<&str> = wf.steps.iter().map(|s| s.kind.type_name()).collect();
    assert_eq!(
        kinds,
        [
            "agent",
            "verify",
            "agent",
            "bugfix-proof",
            "agent",
            "approval"
        ]
    );
    let fix = wf.step("fix").unwrap();
    assert_eq!(fix.retries, 1);
    assert_eq!(
        fix.scope,
        Some(ScopeSpec::Template("{{ inputs.scope }}".into()))
    );
    let review = wf.step("review").unwrap();
    let l = review.loop_spec.as_ref().unwrap();
    assert_eq!(
        (l.max_iterations, l.retry_step.as_deref()),
        (3, Some("fix"))
    );
    assert_eq!(
        wf.step("prove-fail").unwrap().kind,
        StepKind::Verify {
            command: "cargo test {{ steps.repro.outputs.test_name }}".into(),
            expect: Expect::Fail
        }
    );
    assert_eq!(
        wf.upstream("fix"),
        ["prove-fail", "repro"].map(String::from).into()
    );
    assert!(wf.downstream("fix").contains("confirm"));
    let order: Vec<&str> = wf.topo_order().map(|s| s.id.as_str()).collect();
    assert_eq!(
        order,
        ["repro", "prove-fail", "fix", "proof", "review", "confirm"]
    );
}

#[test]
fn supports_every_step_key() {
    let wf = parse(
        r#"name: all
inputs:
  files: { type: list, default: [a, b] }
steps:
  - id: build
    run: make
    outputs: [version]
    artifacts: [report]
    timeout: 10m
    on_failure: continue
    runs_on: local
    budget: { usd: 1, tokens: 2_000_000, on_exceed: warn, warn_at: 0.5 }
  - id: each
    needs: build
    if: "{{ steps.build.status == 'succeeded' }}"
    for_each: "{{ inputs.files }}"
    run: "lint {{ item }} {{ steps.build.outputs.version }}"
    retries: 2
    timeout: 30
  - id: grid
    needs: [build]
    matrix: { os: [linux, mac], opt: ["{{ 1 }}"] }
    run: "echo {{ matrix.os }}"
  - id: nested
    needs: [build]
    uses: other
    with: { path: "{{ steps.build.artifacts.report }}" }
  - id: gpu
    needs: [build]
    agent: trainer
    runs_on: { labels: [gpu], min_mem_gb: 32, min_cpus: 8, min_gpus: 1, fallback: local }
    scope: [src/**]
"#,
        None,
    );
    let wf = wf.unwrap_or_else(|e| panic!("{e:?}"));
    let build = wf.step("build").unwrap();
    assert_eq!(build.timeout, Some(Duration::from_secs(600)));
    assert_eq!(build.on_failure, OnFailure::Continue);
    assert_eq!(build.runs_on, Some(RunsOn::Local));
    let b = build.budget.unwrap();
    assert_eq!(
        (b.usd, b.tokens, b.on_exceed, b.warn_at),
        (Some(1.0), Some(2_000_000), OnExceed::Warn, 0.5)
    );
    let each = wf.step("each").unwrap();
    assert_eq!(each.needs, ["build"]);
    assert!(matches!(each.expand, Some(Expand::ForEach(_))));
    assert_eq!(each.timeout, Some(Duration::from_secs(30)));
    assert!(matches!(
        wf.step("grid").unwrap().expand,
        Some(Expand::Matrix(_))
    ));
    assert_eq!(wf.uses(), ["other"]);
    assert_eq!(
        wf.step("gpu").unwrap().runs_on,
        Some(RunsOn::Select(HostSelector {
            labels: vec!["gpu".into()],
            min_mem_gb: Some(32),
            min_cpus: Some(8),
            min_gpus: Some(1),
            fallback: Some(Fallback::Local),
        }))
    );
}

#[test]
fn cycles_are_load_errors_with_the_path() {
    let e = errs(
        "name: c\nsteps:\n  - id: a\n    run: x\n    needs: [c]\n  - id: b\n    run: x\n    needs: [a]\n  - id: c\n    run: x\n    needs: [b]\n",
    );
    assert!(e.contains("dependency cycle: a -> c -> b -> a"), "{e}");
    assert!(e.starts_with("wf.yaml:"), "{e}");
    let e = errs("name: c\nsteps:\n  - id: a\n    run: x\n    needs: [a]\n");
    assert!(e.contains("cannot need itself"), "{e}");
}

#[test]
fn unknown_references_are_load_errors() {
    let e = errs("name: c\nsteps:\n  - id: a\n    run: x\n    needs: [zz]\n");
    assert!(
        e.contains("wf.yaml:5:5: step `a`: unknown step `zz` in needs"),
        "{e}"
    );
    let e = errs(
        "name: c\nsteps:\n  - id: a\n    run: x\n  - id: b\n    run: \"{{ steps.a.outputs.nope }} {{ inputs.x }}\"\n",
    );
    assert!(e.contains("`a` is not upstream"), "{e}");
    assert!(e.contains("unknown input `x`"), "{e}");
    let e = errs(
        "name: c\nsteps:\n  - id: a\n    run: x\n  - id: b\n    needs: [a]\n    run: \"{{ steps.a.outputs.nope }}\"\n",
    );
    assert!(
        e.contains("has no output `nope` (outputs: exit_code, stdout)"),
        "{e}"
    );
    let e = errs(
        "name: c\nsteps:\n  - id: a\n    run: x\n    loop: { until: \"{{ steps.a.outputs.stdout == 'ok' }}\", retry_step: z }\n",
    );
    assert!(e.contains("retry_step `z`"), "{e}");
}

#[test]
fn invalid_step_types_and_misplaced_keys_are_errors() {
    let e = errs("name: c\nsteps:\n  - id: a\n    type: deploy\n    run: x\n");
    assert!(
        e.contains("wf.yaml:4:5: step `a`: invalid step type `deploy`"),
        "{e}"
    );
    let e = errs("name: c\nsteps:\n  - id: a\n    run: x\n    prompt: hi\n");
    assert!(e.contains("`prompt` is not valid in a `shell` step"), "{e}");
    let e = errs("name: c\nsteps:\n  - id: a\n    type: approval\n");
    assert!(e.contains("a `approval` step needs `message`"), "{e}");
    let e = errs("name: c\nsteps:\n  - id: a\n    needs: []\n");
    assert!(e.contains("cannot tell the step type"), "{e}");
    let e = errs("name: c\nsteps:\n  - id: a\n    run: x\n    colour: red\n");
    assert!(e.contains("unknown field `colour`"), "{e}");
    let e = errs("name: c\nsteps:\n  - id: a\n    run: x\n  - id: a\n    run: y\n");
    assert!(e.contains("duplicate step id"), "{e}");
}

#[test]
fn tasks_must_point_at_known_steps() {
    let e = errs(
        "name: c\ngoal: \"{{ inputs.nope }}\"\ntasks:\n  - { id: t, title: T, step: missing }\nsteps:\n  - id: a\n    run: x\n",
    );
    assert!(
        e.contains("task `t` refers to unknown step `missing`"),
        "{e}"
    );
    assert!(e.contains("goal: `inputs.nope`"), "{e}");
}

#[test]
fn context_checks_agents_hosts_and_nested_workflows() {
    let ctx = CheckContext {
        agents: Some(["implementer".to_owned()].into()),
        remotes: Some([("gpu-box".to_owned(), vec!["gpu".to_owned()])].into()),
        workflows: Some(["fix-bug".to_owned()].into()),
        agent_runs_on: Default::default(),
        mcp_servers: None,
    };
    let ok = "name: c\nsteps:\n  - id: a\n    agent: implementer\n    runs_on: gpu-box\n  - id: b\n    run: x\n    runs_on: { labels: [gpu] }\n  - id: n\n    uses: fix-bug\n";
    parse_with(ok, None, &ctx).unwrap();
    let e = errs_with(
        "name: c\nsteps:\n  - id: a\n    agent: ghost\n    runs_on: nowhere\n  - id: b\n    run: x\n    runs_on: { labels: [tpu] }\n  - id: c\n    run: x\n    runs_on: { labels: [tpu], fallback: lost }\n  - id: n\n    uses: other\n",
        &ctx,
    );
    assert!(e.contains("unknown agent `ghost`"), "{e}");
    assert!(
        e.contains("wf.yaml:5:5: step `a`: unknown host `nowhere`"),
        "{e}"
    );
    assert!(e.contains("no [remote.*] host has labels [tpu]"), "{e}");
    assert!(e.contains("unknown host `lost`"), "{e}");
    assert!(e.contains("unknown workflow `other`"), "{e}");
}

#[test]
fn uses_cycles_across_workflows_are_found() {
    let a = parse("name: a\nsteps:\n  - id: x\n    uses: b\n", None).unwrap();
    let b = parse("name: b\nsteps:\n  - id: y\n    uses: a\n", None).unwrap();
    let all: BTreeMap<String, Workflow> = [("a".into(), a), ("b".into(), b)].into();
    let c = check_uses_cycles(&all);
    assert!(c.iter().any(|m| m.contains("a -> b -> a")), "{c:?}");
}

#[test]
fn input_validation() {
    let e = errs(
        "name: c\ninputs:\n  n: { type: number, default: x }\n  a: { positional: 0 }\n  b: { positional: 0 }\nsteps:\n  - id: a\n    run: x\n",
    );
    assert!(e.contains("default does not match type `number`"), "{e}");
    assert!(e.contains("both use positional 0"), "{e}");
}

#[test]
fn agent_steps_without_runs_on_are_placed_like_their_agent() {
    let ctx = CheckContext {
        agents: Some(["trainer".to_owned(), "writer".to_owned()].into()),
        remotes: Some([("gpu-box".to_owned(), vec!["gpu".to_owned()])].into()),
        workflows: None,
        agent_runs_on: [("trainer".to_owned(), RunsOn::Host("gpu-box".into()))].into(),
        mcp_servers: None,
    };
    let wf = parse_with(
        "name: c\nsteps:\n  - id: a\n    agent: trainer\n  - id: b\n    agent: trainer\n    runs_on: local\n  - id: c\n    agent: writer\n  - id: d\n    run: x\n",
        None,
        &ctx,
    )
    .unwrap();
    assert_eq!(
        wf.step("a").unwrap().runs_on,
        Some(RunsOn::Host("gpu-box".into()))
    );
    assert_eq!(
        wf.step("b").unwrap().runs_on,
        Some(RunsOn::Local),
        "the step's own runs_on wins"
    );
    assert_eq!(wf.step("c").unwrap().runs_on, None);
    assert_eq!(wf.step("d").unwrap().runs_on, None);
}

#[test]
fn agent_steps_may_add_mcp_tools_from_servers_in_harness_ini() {
    let ctx = CheckContext {
        agents: Some(["implementer".to_owned()].into()),
        mcp_servers: Some(["github".to_owned()].into()),
        ..CheckContext::default()
    };
    let wf = parse_with(
        "name: c\nsteps:\n  - id: a\n    agent: implementer\n    tools: [mcp:github/create_issue, mcp:github/list_*]\n",
        None,
        &ctx,
    )
    .unwrap();
    let StepKind::Agent { tools, .. } = &wf.steps[0].kind else {
        panic!("agent step")
    };
    let tools: Vec<String> = tools.iter().map(ToString::to_string).collect();
    assert_eq!(tools, ["mcp:github/create_issue", "mcp:github/list_*"]);
    let e = errs_with(
        "name: c\nsteps:\n  - id: a\n    agent: implementer\n    tools: [read, mcp:ghost/*, mcp:github]\n  - id: b\n    run: x\n    tools: [mcp:github/x]\n",
        &ctx,
    );
    assert!(e.contains("can only add MCP tools"), "{e}");
    assert!(e.contains("[mcp.ghost]"), "{e}");
    assert!(e.contains("mcp:github/*"), "{e}");
    assert!(e.contains("`tools` is not valid in a `shell` step"), "{e}");
}
