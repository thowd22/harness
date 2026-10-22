//! DAG executor integration tests: the fake provider for agents, real
//! local processes for shell / verify steps, and a real run store.

mod common;

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use common::*;
use harness_agent::scheduler::{Scheduler, SchedulerConfig};
use harness_agent::todo::TaskStatus;
use harness_events::reducer::RunState;
use harness_events::schema::{EventKind, NodeId, NodeKind, NodeStatus, RunEvent};
use harness_providers::fake::{FakeBlock, FakeProvider, FakeTurn};
use harness_workflow::executor::{Runners, StepFailure, StepOutput};
use harness_workflow::gates::{ChannelGates, GateDecision, HeadlessGates, waiting_gates};
use harness_workflow::placement::{HostInfo, HostPool};
use harness_workflow::rerun::{
    OriginKind, StepStatus, WorkflowState, WorkflowStatus, lineage, plan_rerun, plan_resume,
};
use harness_workflow::schema::parse;
use serde_json::{Map, Value, json};
use tokio_util::sync::CancellationToken;

fn inputs(v: Value) -> Map<String, Value> {
    v.as_object().cloned().unwrap_or_default()
}

fn status(state: &WorkflowState, id: &str) -> StepStatus {
    state.steps[id].status
}

fn node<'a>(s: &'a RunState, id: &str) -> &'a harness_events::reducer::NodeState {
    s.node(&NodeId::from(id))
        .unwrap_or_else(|| panic!("no node {id}"))
}

const CHILD: &str = "name: child\ninputs:\n  word: { type: string, required: true }\nsteps:\n  - id: echo\n    run: \"echo nested-{{ inputs.word }}\"\n";

#[tokio::test]
async fn every_step_type_runs_with_the_fake_provider() {
    let env = Env::new().await;
    std::fs::write(env.work.join("flag"), "broken").unwrap();
    let implementer = FakeProvider::with_turns([
        FakeTurn::text("Wrote the test.\n```json\n{\"test_name\": \"flag_is_fixed\"}\n```"),
        FakeTurn::text("Fixed it."),
    ]);
    let agents = Agents::default().with(
        "name: implementer\nprompt_text: You implement.\n",
        implementer.clone(),
    );
    let wf = wf(r#"name: all-types
inputs:
  issue: { type: string, required: true }
steps:
  - id: repro
    agent: implementer
    prompt: "Reproduce {{ inputs.issue }}"
    outputs: [test_name]
  - id: prove-fail
    needs: [repro]
    verify: { command: "test \"$(cat flag)\" = fixed # {{ steps.repro.outputs.test_name }}", expect: fail }
  - id: fix
    needs: [prove-fail]
    agent: implementer
    prompt: "Make {{ steps.repro.outputs.test_name }} pass"
  - id: apply
    needs: [fix]
    run: "echo fixed > flag && echo done=yes"
    outputs: [done]
  - id: proof
    needs: [apply]
    type: bugfix-proof
    test: "test \"$(cat flag)\" = fixed"
  - id: confirm
    needs: [proof]
    type: approval
    message: "Ship {{ inputs.issue }}?"
  - id: nested
    needs: [confirm]
    uses: child
    with: { word: "{{ steps.apply.outputs.done }}" }
"#);
    let lib: std::collections::BTreeMap<String, _> =
        [("child".to_owned(), common::wf(CHILD))].into();
    let exec = env
        .executor(env.runners(agents, headless_approve_all()))
        .with_library(Arc::new(lib));
    let out = exec
        .run(
            &wf,
            &inputs(json!({"issue": "#142"})),
            None,
            &CancellationToken::new(),
        )
        .await;
    assert_eq!(out.status, WorkflowStatus::Succeeded, "{:?}", out.error);
    for id in [
        "repro",
        "prove-fail",
        "fix",
        "apply",
        "proof",
        "confirm",
        "nested",
    ] {
        assert_eq!(
            status(&out.state, id),
            StepStatus::Succeeded,
            "{id}: {:?}",
            out.state.steps[id].error
        );
    }
    assert_eq!(
        out.state.steps["repro"].outputs["test_name"],
        "flag_is_fixed"
    );
    assert_eq!(out.state.steps["apply"].outputs["done"], "yes");
    assert_eq!(out.state.steps["proof"].outputs["proven"], true);
    assert_eq!(out.state.steps["confirm"].outputs["approved"], true);
    assert_eq!(
        out.state.steps["nested"].outputs["echo"]["stdout"],
        "nested-yes"
    );
    assert!(out.state.steps["fix"].checkpoint.is_some());
    // The second agent got the first one's output in its brief.
    let reqs = implementer.requests();
    assert!(reqs[0].messages[0].text().starts_with("Reproduce #142"));
    assert!(
        reqs[0].messages[0].text().contains("\"test_name\""),
        "outputs are requested"
    );
    assert!(
        reqs[1].messages[0]
            .text()
            .starts_with("Make flag_is_fixed pass")
    );

    // Every step is a node in the event stream.
    let evs = events(&env.recorder).await;
    let s = RunState::from_events(&evs);
    let wf_node = node(&s, "all-types");
    assert_eq!(wf_node.kind, Some(NodeKind::Workflow));
    assert_eq!(wf_node.status, NodeStatus::Succeeded);
    let kinds = [
        ("all-types/repro", NodeKind::Agent),
        ("all-types/prove-fail", NodeKind::Verify),
        ("all-types/apply", NodeKind::Shell),
        ("all-types/proof", NodeKind::Verify),
        ("all-types/confirm", NodeKind::Approval),
        ("all-types/nested", NodeKind::Workflow),
    ];
    for (id, k) in kinds {
        let n = node(&s, id);
        assert_eq!((n.kind, n.status), (Some(k), NodeStatus::Succeeded), "{id}");
        assert_eq!(n.parent, Some(NodeId::from("all-types")));
    }
    // The agent loop is a child of its step; needs edges exist.
    assert_eq!(
        node(&s, "all-types/repro.agent").parent,
        Some(NodeId::from("all-types/repro"))
    );
    assert!(
        s.edges
            .iter()
            .any(|e| e.from == NodeId::from("all-types/repro")
                && e.to == NodeId::from("all-types/prove-fail"))
    );
    assert_eq!(
        node(&s, "all-types/nested/child/echo").status,
        NodeStatus::Succeeded
    );
    assert!(matches!(
        evs.last().unwrap().kind,
        EventKind::RunFinished { .. }
    ));
}

#[tokio::test]
async fn ready_steps_run_in_parallel_within_scheduler_limits() {
    for (limit, expect) in [(2, 2), (1, 1)] {
        let mut env = Env::new().await;
        env.scheduler = Scheduler::new(SchedulerConfig {
            max_concurrent_agents: limit,
            ..SchedulerConfig::default()
        });
        let r = Scripted::new(80, |_, _| Ok(StepOutput::default()));
        let runners = Runners::new(headless_approve_all()).with_shell(r.clone());
        let w = common::wf(
            "name: par\nsteps:\n  - id: a\n    run: x\n  - id: b\n    run: x\n  - id: c\n    run: x\n  - id: d\n    needs: [a, b, c]\n    run: x\n",
        );
        let out = env
            .executor(runners)
            .run(&w, &Map::new(), None, &CancellationToken::new())
            .await;
        assert_eq!(out.status, WorkflowStatus::Succeeded);
        assert_eq!(
            r.max_running.load(Ordering::SeqCst),
            expect,
            "limit {limit}"
        );
        assert_eq!(
            r.steps().last().map(String::as_str),
            Some("d"),
            "d waits for its needs"
        );
        // Steps are declared queued before they run.
        let evs = events(&env.recorder).await;
        let first_queued = evs
            .iter()
            .position(|e| matches!(e.kind, EventKind::NodeStarted { queued: true, .. }))
            .unwrap();
        let early = RunState::from_events(&evs[..=first_queued + 3]);
        assert!(
            early
                .nodes
                .iter()
                .filter(|n| n.status == NodeStatus::Queued)
                .count()
                >= 3
        );
    }
}

#[tokio::test]
async fn if_retries_timeout_and_on_failure() {
    let env = Env::new().await;
    let flaky = Scripted::new(0, |req, _| {
        if req.step == "flaky" && req.attempt == 1 {
            Err(StepFailure::new("transient"))
        } else if req.step == "bad" || req.step == "soft" {
            Err(StepFailure::new("broken"))
        } else {
            Ok(StepOutput::default())
        }
    });
    let mut runners = env.runners(Agents::default(), headless_approve_all());
    runners = runners.with_verify(flaky.clone());
    let w = common::wf(
        r#"name: flow
steps:
  - id: skipped
    if: "{{ false }}"
    verify: x
  - id: after-skip
    needs: [skipped]
    verify: x
  - id: guarded
    needs: [skipped]
    if: "{{ steps.skipped.status == 'skipped' }}"
    verify: x
  - id: flaky
    verify: x
    retries: 2
  - id: slow
    run: "sleep 5"
    timeout: 1
    on_failure: continue
  - id: after-slow
    needs: [slow]
    if: "{{ steps.slow.status == 'failed' }}"
    verify: x
  - id: soft
    verify: x
    on_failure: continue
  - id: after-soft
    needs: [soft]
    verify: x
"#,
    );
    let out = env
        .executor(runners)
        .run(&w, &Map::new(), None, &CancellationToken::new())
        .await;
    assert_eq!(out.status, WorkflowStatus::Succeeded, "{:?}", out.error);
    let st = &out.state;
    assert_eq!(status(st, "skipped"), StepStatus::Skipped);
    assert_eq!(status(st, "after-skip"), StepStatus::Skipped);
    assert_eq!(status(st, "guarded"), StepStatus::Succeeded);
    assert_eq!(status(st, "flaky"), StepStatus::Succeeded);
    assert_eq!(st.steps["flaky"].attempts, 2);
    assert_eq!(status(st, "slow"), StepStatus::Failed);
    assert!(
        st.steps["slow"]
            .error
            .as_deref()
            .unwrap()
            .contains("timed out after 1s")
    );
    assert_eq!(status(st, "after-slow"), StepStatus::Succeeded);
    assert_eq!(status(st, "soft"), StepStatus::Failed);
    assert_eq!(
        status(st, "after-soft"),
        StepStatus::Succeeded,
        "continue lets dependents run"
    );
    let evs = events(&env.recorder).await;
    let s = RunState::from_events(&evs);
    assert_eq!(node(&s, "flow/flaky").retries, 1);
    assert_eq!(node(&s, "flow/skipped").status, NodeStatus::Skipped);

    // on_failure: abort (the default) fails the workflow and skips the rest.
    let env = Env::new().await;
    let r = Scripted::new(0, |req, _| {
        if req.step == "bad" {
            Err(StepFailure::new("broken"))
        } else {
            Ok(StepOutput::default())
        }
    });
    let runners = Runners::new(headless_approve_all()).with_shell(r.clone());
    let w = common::wf(
        "name: ab\nsteps:\n  - id: bad\n    run: x\n  - id: next\n    needs: [bad]\n    run: x\n",
    );
    let out = env
        .executor(runners)
        .run(&w, &Map::new(), None, &CancellationToken::new())
        .await;
    assert_eq!(out.status, WorkflowStatus::Failed);
    assert!(out.error.unwrap().contains("step `bad` failed: broken"));
    assert_eq!(status(&out.state, "next"), StepStatus::Skipped);
    assert_eq!(r.steps(), ["bad"]);
}

#[tokio::test]
async fn matrix_for_each_and_loops() {
    let env = Env::new().await;
    let r = Scripted::new(0, |req, n| {
        let mut o = StepOutput::default();
        if let harness_workflow::executor::StepAction::Shell { command } = &req.action {
            o.outputs.insert("cmd".into(), json!(command));
        }
        o.outputs.insert("n".into(), json!(n));
        Ok(o)
    });
    let runners = Runners::new(headless_approve_all()).with_shell(r.clone());
    let w = common::wf(
        r#"name: expand
inputs:
  files: { type: list, default: [a.rs, b.rs] }
steps:
  - id: grid
    matrix: { os: [linux, mac], opt: [o1, o2] }
    run: "build {{ matrix.os }} {{ matrix.opt }}"
  - id: each
    for_each: "{{ inputs.files }}"
    run: "lint {{ item }}"
    outputs: [cmd]
  - id: fix
    needs: [grid, each]
    run: "fix"
  - id: review
    needs: [fix]
    run: "review"
    outputs: [n]
    loop: { until: "{{ steps.review.outputs.n >= 9 }}", max_iterations: 5, retry_step: fix }
"#,
    );
    let out = env
        .executor(runners)
        .run(&w, &Map::new(), None, &CancellationToken::new())
        .await;
    assert_eq!(out.status, WorkflowStatus::Succeeded, "{:?}", out.error);
    let st = &out.state;
    let grid_cmds = st.steps["grid"].outputs["cmd"].as_array().unwrap().clone();
    assert_eq!(grid_cmds.len(), 4);
    assert!(grid_cmds.contains(&json!("build mac o2")));
    assert_eq!(
        st.steps["each"].outputs["cmd"],
        json!(["lint a.rs", "lint b.rs"])
    );
    // 6 expansion calls (n = 0..5), then fix/review pairs until n >= 9.
    assert_eq!(st.steps["review"].iterations, 2);
    let tail: Vec<String> = r.steps().into_iter().skip(6).collect();
    assert_eq!(tail, ["fix", "review", "fix", "review"]);
    let evs = events(&env.recorder).await;
    let s = RunState::from_events(&evs);
    assert_eq!(node(&s, "expand/grid").children.len(), 4);
    assert!(node(&s, "expand/each[1]").name.contains("b.rs"));
    assert_eq!(node(&s, "expand/review").retries, 1);
    assert_eq!(node(&s, "expand/fix").retries, 1);
    assert_eq!(node(&s, "expand/review").status, NodeStatus::Succeeded);

    // A loop that never converges fails after max_iterations.
    let env = Env::new().await;
    let r = Scripted::new(0, |_, _| Ok(StepOutput::default()));
    let runners = Runners::new(headless_approve_all()).with_shell(r.clone());
    let w = common::wf(
        "name: l\nsteps:\n  - id: s\n    run: x\n    loop: { until: \"{{ false }}\", max_iterations: 2 }\n",
    );
    let out = env
        .executor(runners)
        .run(&w, &Map::new(), None, &CancellationToken::new())
        .await;
    assert_eq!(status(&out.state, "s"), StepStatus::Failed);
    assert!(
        out.state.steps["s"]
            .error
            .as_deref()
            .unwrap()
            .contains("not met after 2")
    );
    assert_eq!(r.steps().len(), 2);
}

#[tokio::test]
async fn cancellation_stops_running_steps_and_cancels_the_rest() {
    let env = Env::new().await;
    let runners = env.runners(Agents::default(), headless_approve_all());
    let w = common::wf(
        "name: c\nsteps:\n  - id: long\n    run: \"sleep 30\"\n  - id: later\n    needs: [long]\n    run: \"true\"\n",
    );
    let cancel = CancellationToken::new();
    let c2 = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(200)).await;
        c2.cancel();
    });
    let start = std::time::Instant::now();
    let out = env
        .executor(runners)
        .run(&w, &Map::new(), None, &cancel)
        .await;
    assert!(start.elapsed() < Duration::from_secs(10));
    assert_eq!(out.status, WorkflowStatus::Cancelled);
    assert_eq!(status(&out.state, "long"), StepStatus::Cancelled);
    assert_eq!(status(&out.state, "later"), StepStatus::Cancelled);
    let s = RunState::from_events(&events(&env.recorder).await);
    assert_eq!(node(&s, "c/long").status, NodeStatus::Cancelled);
    assert_eq!(node(&s, "c/later").status, NodeStatus::Cancelled);
    assert_eq!(node(&s, "c").status, NodeStatus::Cancelled);
}

#[tokio::test]
async fn state_is_persisted_and_a_crashed_workflow_resumes() {
    let env = Env::new().await;
    let r = Scripted::new(0, |req, _| {
        let mut o = StepOutput::default();
        o.outputs
            .insert("v".into(), json!(format!("{}-out", req.step)));
        o.artifacts
            .insert("note".into(), format!("note of {}", req.step).into_bytes());
        Ok(o)
    });
    let slow = Scripted::new(30_000, |_, _| Ok(StepOutput::default()));
    let runners = Runners::new(headless_approve_all())
        .with_shell(r.clone())
        .with_verify(slow.clone());
    let w = common::wf(
        "name: res\nsteps:\n  - id: a\n    run: x\n    outputs: [v]\n    artifacts: [note]\n  - id: b\n    needs: [a]\n    verify: \"check {{ steps.a.outputs.v }}\"\n",
    );
    let cancel = CancellationToken::new();
    let exec = env.executor(runners);
    let run1 = env.recorder.dir().to_path_buf();
    let handle = {
        let w = w.clone();
        let cancel = cancel.clone();
        tokio::spawn(async move { exec.run(&w, &Map::new(), None, &cancel).await })
    };
    // While b runs, the state on disk already has a finished.
    let mut seen = None;
    for _ in 0..100 {
        if let Ok(s) = WorkflowState::load(&run1)
            && s.steps["b"].status == StepStatus::Running
        {
            seen = Some(s);
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let mid = seen.expect("state written while running");
    assert_eq!(mid.steps["a"].status, StepStatus::Succeeded);
    assert_eq!(mid.status, WorkflowStatus::Running);
    cancel.cancel(); // simulate the crash
    handle.await.unwrap();

    // Resume in a new run: a is reused (not run again), b runs.
    let env2 = Env::new().await;
    let r2 = Scripted::new(0, |_, _| Ok(StepOutput::default()));
    let runners = Runners::new(headless_approve_all())
        .with_shell(r2.clone())
        .with_verify(r2.clone());
    let plan = plan_resume(&run1, &w).unwrap();
    let out = env2
        .executor(runners)
        .run(&w, &Map::new(), Some(plan), &CancellationToken::new())
        .await;
    assert_eq!(out.status, WorkflowStatus::Succeeded, "{:?}", out.error);
    assert_eq!(r2.steps(), ["b"]);
    assert_eq!(
        r2.calls()[0].action,
        harness_workflow::executor::StepAction::Verify {
            command: "check a-out".into(),
            expect: harness_workflow::schema::Expect::Pass
        }
    );
    let a = &out.state.steps["a"];
    assert_eq!(a.reused_from, mid.run_id);
    assert!(
        a.artifacts["note"].starts_with(env2.recorder.dir()),
        "artifacts copied into the new run"
    );
    assert_eq!(
        std::fs::read_to_string(&a.artifacts["note"]).unwrap(),
        "note of a"
    );
    assert_eq!(out.state.origin.as_ref().unwrap().kind, OriginKind::Resume);
}

#[tokio::test]
async fn rerun_from_a_selected_step_reuses_upstream_and_links_runs() {
    let shared_data = tempfile::tempdir().unwrap();
    let r = Scripted::new(0, |req, _| {
        let mut o = StepOutput::default();
        o.outputs
            .insert("v".into(), json!(format!("{}#{}", req.step, req.attempt)));
        if req.step == "plan" {
            o.artifacts.insert("plan".into(), b"the plan".to_vec());
        }
        Ok(o)
    });
    let w = common::wf(
        "name: rr\nsteps:\n  - id: plan\n    run: x\n    outputs: [v]\n    artifacts: [plan]\n  - id: build\n    needs: [plan]\n    run: \"build {{ steps.plan.outputs.v }} {{ steps.plan.artifacts.plan }}\"\n  - id: test\n    needs: [build]\n    run: t\n",
    );
    let rec1 = recorder(shared_data.path()).await;
    let runners = Runners::new(headless_approve_all()).with_shell(r.clone());
    let exec = harness_workflow::executor::Executor::new(
        rec1.clone(),
        runners.clone(),
        Scheduler::new(SchedulerConfig::default()),
    );
    let first = exec
        .run(&w, &Map::new(), None, &CancellationToken::new())
        .await;
    assert_eq!(first.status, WorkflowStatus::Succeeded);
    assert_eq!(r.steps(), ["plan", "build", "test"]);

    let rec2 = recorder(shared_data.path()).await;
    let plan = plan_rerun(rec1.dir(), &w, &["build".into()]).unwrap();
    let exec = harness_workflow::executor::Executor::new(
        rec2.clone(),
        runners,
        Scheduler::new(SchedulerConfig::default()),
    );
    let second = exec
        .run(&w, &Map::new(), Some(plan), &CancellationToken::new())
        .await;
    assert_eq!(second.status, WorkflowStatus::Succeeded);
    assert_eq!(
        r.steps(),
        ["plan", "build", "test", "build", "test"],
        "plan is not paid for twice"
    );
    let build_cmd = format!("{:?}", r.calls()[3].action);
    assert!(build_cmd.contains("build plan#1"), "{build_cmd}");
    assert!(
        build_cmd.contains(&rec2.dir().display().to_string()),
        "artifact path points into the new run: {build_cmd}"
    );
    // History links the runs.
    let l = lineage(shared_data.path(), rec2.run_id());
    assert_eq!(l.len(), 2);
    assert_eq!(l[1].0, rec1.run_id());
    let origin = l[0].1.clone().unwrap();
    assert_eq!(
        (origin.kind, origin.from_steps),
        (OriginKind::Rerun, vec!["build".to_owned()])
    );
    let evs = events(&rec2).await;
    match &evs[0].kind {
        EventKind::RunStarted { name, .. } => assert!(
            name.contains(&format!("re-run of {}", rec1.run_id().0)),
            "{name}"
        ),
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn artifacts_are_written_to_the_run_dir_and_referenced_by_name() {
    let env = Env::new().await;
    let writer = FakeProvider::with_turns([FakeTurn::text(
        "```json\n{\"plan\": \"1. parse\\n2. test\"}\n```",
    )]);
    let reader = FakeProvider::with_turns([FakeTurn::text("ok")]);
    let agents = Agents::default()
        .with("name: planner\nprompt_text: p\n", writer)
        .with("name: doer\nprompt_text: d\n", reader.clone());
    let runners = env.runners(agents, headless_approve_all());
    let w = common::wf(
        r#"name: art
steps:
  - id: plan
    agent: planner
    prompt: "Plan it"
    artifacts: [plan]
  - id: report
    run: "printf 'all good' > \"$ARTIFACTS_DIR/report\""
    artifacts: [report]
  - id: use
    needs: [plan, report]
    run: "cat {{ steps.report.artifacts.report }}"
  - id: do
    needs: [plan]
    agent: doer
    prompt: "Follow {{ steps.plan.artifacts.plan }}"
"#,
    );
    let out = env
        .executor(runners)
        .run(&w, &Map::new(), None, &CancellationToken::new())
        .await;
    assert_eq!(out.status, WorkflowStatus::Succeeded, "{:?}", out.error);
    let plan = &out.state.steps["plan"].artifacts["plan"];
    assert!(plan.starts_with(env.recorder.dir().join("artifacts")));
    assert_eq!(std::fs::read_to_string(plan).unwrap(), "1. parse\n2. test");
    assert_eq!(out.state.steps["use"].outputs["stdout"], "all good");
    // The agent cannot read the run dir, so the content is inlined.
    let brief = reader.requests()[0].messages[0].text();
    assert!(brief.contains("--- artifact `plan`"), "{brief}");
    assert!(brief.contains("1. parse"));
    let all = env
        .executor(Runners::new(headless_approve_all()))
        .artifacts()
        .list_all()
        .unwrap();
    assert_eq!(all.keys().collect::<Vec<_>>(), ["plan", "report"]);

    // A declared artifact that is not produced fails the step.
    let env = Env::new().await;
    let runners = env.runners(Agents::default(), headless_approve_all());
    let w = common::wf("name: miss\nsteps:\n  - id: s\n    run: \"true\"\n    artifacts: [out]\n");
    let out = env
        .executor(runners)
        .run(&w, &Map::new(), None, &CancellationToken::new())
        .await;
    assert!(
        out.state.steps["s"]
            .error
            .as_deref()
            .unwrap()
            .contains("declared artifact `out`")
    );
}

#[tokio::test]
async fn approval_gates_pause_with_message_and_artifacts() {
    let env = Env::new().await;
    let (gates, mut prompts) = ChannelGates::new(4);
    let runners = env.runners(Agents::default(), Arc::new(gates));
    let w = common::wf(
        r#"name: ship
steps:
  - id: plan
    run: "echo '1. ship' > \"$ARTIFACTS_DIR/plan\""
    artifacts: [plan]
  - id: confirm
    needs: [plan]
    type: approval
    message: "Ship the plan?"
    artifacts: [plan]
  - id: deploy
    needs: [confirm]
    run: "true"
"#,
    );
    let exec = env.executor(runners);
    let task = tokio::spawn({
        let w = w.clone();
        async move {
            exec.run(&w, &Map::new(), None, &CancellationToken::new())
                .await
        }
    });
    let p = prompts.recv().await.unwrap();
    assert_eq!(p.request.message, "Ship the plan?");
    assert_eq!(p.request.node, NodeId::from("ship/confirm"));
    assert_eq!(p.request.artifacts.len(), 1);
    assert_eq!(p.request.artifacts[0].name, "plan");
    // While waiting, the gate node is highlighted as waiting and reachable.
    let s = RunState::from_events(&events(&env.recorder).await);
    let waiting = waiting_gates(&s);
    assert_eq!(waiting.len(), 1);
    assert_eq!(waiting[0].id, NodeId::from("ship/confirm"));
    assert_eq!(
        node(&s, "ship/deploy").status,
        NodeStatus::Queued,
        "the workflow is paused"
    );
    p.reply.send(GateDecision::approve("user")).unwrap();
    let out = task.await.unwrap();
    assert_eq!(out.status, WorkflowStatus::Succeeded);
    assert_eq!(out.state.steps["confirm"].outputs["by"], "user");
    let evs = events(&env.recorder).await;
    assert!(evs.iter().any(|e| matches!(&e.kind, EventKind::ApprovalRequested { action, .. } if action == "Ship the plan?")));
    assert!(waiting_gates(&RunState::from_events(&evs)).is_empty());

    // Headless: resolved by flag; without one the gate is denied.
    for (gates, ok) in [
        (
            HeadlessGates::from_flags(false, ["confirm".to_owned()], []),
            true,
        ),
        (HeadlessGates::from_flags(false, [], []), false),
    ] {
        let env = Env::new().await;
        let runners = env.runners(Agents::default(), Arc::new(gates));
        let out = env
            .executor(runners)
            .run(&w, &Map::new(), None, &CancellationToken::new())
            .await;
        assert_eq!(out.status == WorkflowStatus::Succeeded, ok);
        if !ok {
            assert!(
                out.state.steps["confirm"]
                    .error
                    .as_deref()
                    .unwrap()
                    .contains("--approve confirm")
            );
            assert_eq!(status(&out.state, "deploy"), StepStatus::Skipped);
        }
    }
}

fn costly(usd: f64) -> Arc<Scripted> {
    Scripted::new(0, move |_, _| {
        Ok(StepOutput {
            cost_usd: usd,
            tokens: 100,
            ..StepOutput::default()
        })
    })
}

fn warnings(evs: &[RunEvent]) -> Vec<String> {
    evs.iter()
        .filter_map(|e| match &e.kind {
            EventKind::Error {
                message,
                recoverable: true,
                ..
            } => Some(message.clone()),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn workflow_and_step_budgets_warn_then_stop() {
    // Hard stop at the workflow budget.
    let env = Env::new().await;
    let r = costly(0.6);
    let runners = Runners::new(headless_approve_all()).with_shell(r.clone());
    let w = common::wf(
        "name: b\nbudget: { usd: 1, warn_at: 0.5 }\nsteps:\n  - id: a\n    run: x\n  - id: b\n    needs: [a]\n    run: x\n  - id: c\n    needs: [b]\n    run: x\n",
    );
    let out = env
        .executor(runners)
        .run(&w, &Map::new(), None, &CancellationToken::new())
        .await;
    assert_eq!(out.status, WorkflowStatus::Failed);
    assert!(
        out.error
            .as_deref()
            .unwrap()
            .contains("budget: workflow `b` exhausted"),
        "{:?}",
        out.error
    );
    assert_eq!(status(&out.state, "c"), StepStatus::Skipped);
    assert!(out.state.budget.warned && out.state.budget.exceeded);
    assert!((out.state.budget.spent_usd - 1.2).abs() < 1e-9);
    let w_msgs = warnings(&events(&env.recorder).await);
    assert!(
        w_msgs[0].starts_with("budget warning: workflow `b` has used $0.60 of $1.00 (60%)"),
        "{w_msgs:?}"
    );
    // The remaining budget is passed down as a hard limit.
    let limits: Vec<Option<f64>> = r.calls().iter().map(|c| c.limits.usd).collect();
    assert_eq!(limits[0], Some(1.0));
    assert!((limits[1].unwrap() - 0.4).abs() < 1e-9);

    // on_exceed: warn keeps going.
    let env = Env::new().await;
    let runners = Runners::new(headless_approve_all()).with_shell(costly(0.6));
    let w = common::wf(
        "name: b\nbudget: { usd: 1, on_exceed: warn }\nsteps:\n  - id: a\n    run: x\n  - id: b\n    needs: [a]\n    run: x\n",
    );
    let out = env
        .executor(runners)
        .run(&w, &Map::new(), None, &CancellationToken::new())
        .await;
    assert_eq!(out.status, WorkflowStatus::Succeeded);
    let msgs = warnings(&events(&env.recorder).await);
    assert!(
        msgs.iter()
            .any(|m| m.starts_with("budget: workflow `b` exhausted")),
        "{msgs:?}"
    );

    // Step budgets.
    let env = Env::new().await;
    let r = costly(2.0);
    let runners = Runners::new(headless_approve_all()).with_shell(r.clone());
    let w = common::wf(
        "name: s\nsteps:\n  - id: big\n    run: x\n    budget: { usd: 1 }\n    on_failure: continue\n  - id: soft\n    run: x\n    budget: { usd: 1, on_exceed: warn }\n",
    );
    let out = env
        .executor(runners)
        .run(&w, &Map::new(), None, &CancellationToken::new())
        .await;
    assert_eq!(status(&out.state, "big"), StepStatus::Failed);
    assert!(
        out.state.steps["big"]
            .error
            .as_deref()
            .unwrap()
            .contains("step `big` exhausted its budget")
    );
    assert_eq!(status(&out.state, "soft"), StepStatus::Succeeded);
    let big = r.calls().into_iter().find(|c| c.step == "big").unwrap();
    assert_eq!(big.limits.usd, Some(1.0));
    let soft = r.calls().into_iter().find(|c| c.step == "soft").unwrap();
    assert_eq!(
        soft.limits.usd, None,
        "warn-only budgets are not hard limits"
    );
}

#[tokio::test]
async fn goal_and_tasks_seed_the_board_and_follow_steps() {
    let env = Env::new().await;
    let worker = FakeProvider::with_turns([
        FakeTurn {
            content: vec![FakeBlock::ToolUse {
                id: Some("t".into()),
                name: "todo".into(),
                input: json!({"action": "add", "title": "Survey other callers"}),
            }],
            ..FakeTurn::default()
        },
        FakeTurn::text("done"),
    ]);
    let agents =
        Agents::default().with("name: implementer\nprompt_text: i\ntools: [todo]\n", worker);
    let runners = env.runners(agents, headless_approve_all());
    let w = common::wf(
        r#"name: goal
inputs:
  issue: { type: string, required: true }
goal: "Fix: {{ inputs.issue }}"
tasks:
  - { id: repro, title: "Reproduce", step: repro }
  - { id: ship, title: "Ship it" }
  - { id: gate, title: "Approve", step: confirm }
steps:
  - id: repro
    agent: implementer
    prompt: "go"
  - id: confirm
    needs: [repro]
    type: approval
    message: ok?
"#,
    );
    let mut rx = env.board.subscribe();
    let out = env
        .executor(runners)
        .run(
            &w,
            &inputs(json!({"issue": "#142"})),
            None,
            &CancellationToken::new(),
        )
        .await;
    assert_eq!(out.status, WorkflowStatus::Succeeded, "{:?}", out.error);
    let snap = env.board.snapshot();
    assert_eq!(snap.goal.as_deref(), Some("Fix: #142"));
    let repro = snap.tasks.iter().find(|t| t.id == "repro").unwrap();
    assert_eq!(repro.status, TaskStatus::Done);
    assert_eq!(repro.agent.as_deref(), Some("implementer"));
    assert_eq!(repro.owner, Some(NodeId::from("goal/repro")));
    assert_eq!(
        snap.tasks.iter().find(|t| t.id == "ship").unwrap().status,
        TaskStatus::Pending
    );
    assert_eq!(
        snap.tasks.iter().find(|t| t.id == "gate").unwrap().status,
        TaskStatus::Done
    );
    // The agent added its own task through the todo tool.
    let extra = snap
        .tasks
        .iter()
        .find(|t| t.title == "Survey other callers")
        .unwrap();
    assert_eq!(extra.owner, Some(NodeId::from("goal/repro.agent")));
    // Changes were streamed live, including in-progress states.
    let mut saw_in_progress = false;
    while let Ok(ev) = rx.try_recv() {
        if ev
            .task
            .as_ref()
            .is_some_and(|t| t.id == "repro" && t.status == TaskStatus::InProgress)
        {
            saw_in_progress = true;
        }
    }
    assert!(saw_in_progress);
}

#[tokio::test]
async fn runs_on_steps_are_placed_on_matching_hosts() {
    let env = Env::new().await;
    let r = Scripted::new(50, |_, _| Ok(StepOutput::default()));
    let runners = Runners::new(headless_approve_all()).with_shell(r.clone());
    let pool = Arc::new(HostPool::new(vec![
        HostInfo::new("gpu-box", vec!["gpu".into()], 1),
        HostInfo::new("cpu-box", vec![], 2),
    ]));
    let w = parse(
        "name: place\nsteps:\n  - id: train\n    run: x\n    runs_on: { labels: [gpu] }\n  - id: train2\n    run: x\n    runs_on: { labels: [gpu], fallback: cpu-box }\n  - id: build\n    run: x\n    runs_on: cpu-box\n  - id: here\n    run: x\n",
        None,
    )
    .unwrap();
    let out = env
        .executor(runners)
        .with_hosts(pool.clone())
        .run(&w, &Map::new(), None, &CancellationToken::new())
        .await;
    assert_eq!(out.status, WorkflowStatus::Succeeded);
    let hosts: std::collections::BTreeMap<String, Option<String>> =
        r.calls().into_iter().map(|c| (c.step, c.host)).collect();
    assert_eq!(hosts["train"].as_deref(), Some("gpu-box"));
    // gpu-box has max_jobs 1, so train2 waited and then ran there (the
    // fallback is only for when no host matches).
    assert_eq!(hosts["train2"].as_deref(), Some("gpu-box"));
    assert_eq!(hosts["build"].as_deref(), Some("cpu-box"));
    assert_eq!(hosts["here"], None);
    assert_eq!(pool.running("gpu-box"), 0, "leases are released");
    let s = RunState::from_events(&events(&env.recorder).await);
    assert_eq!(node(&s, "place/train").host.as_deref(), Some("gpu-box"));
    assert_eq!(node(&s, "place/here").host, None);
}

#[tokio::test]
async fn dream_steps_run_on_the_dream_runner() {
    let w = common::wf(
        "name: dream\nsteps:\n  - id: consolidate\n    type: dream\n    with: { grace_days: 7 }\n  - id: after\n    needs: [consolidate]\n    run: \"echo ${{ steps.consolidate.outputs.summary }}\"\n",
    );
    let env = Env::new().await;
    let dream = Scripted::new(0, |req, _| {
        let harness_workflow::executor::StepAction::Dream { settings } = &req.action else {
            panic!("not a dream action: {:?}", req.action)
        };
        assert_eq!(settings["grace_days"], 7);
        let mut outputs = Map::new();
        outputs.insert("summary".into(), json!("merged 2"));
        Ok(StepOutput {
            outputs,
            ..StepOutput::default()
        })
    });
    let shell = Scripted::ok();
    let runners = Runners::new(headless_approve_all())
        .with_dream(dream.clone())
        .with_shell(shell.clone());
    let out = env
        .executor(runners)
        .run(&w, &Map::new(), None, &CancellationToken::new())
        .await;
    assert_eq!(out.status, WorkflowStatus::Succeeded, "{:?}", out.error);
    assert_eq!(dream.steps(), ["consolidate"]);
    // Without a dream runner the step fails with a clear message.
    let env = Env::new().await;
    let out = env
        .executor(Runners::new(headless_approve_all()).with_shell(Scripted::ok()))
        .run(&w, &Map::new(), None, &CancellationToken::new())
        .await;
    assert_eq!(out.status, WorkflowStatus::Failed);
    assert!(
        out.state.steps["consolidate"]
            .error
            .as_deref()
            .unwrap_or("")
            .contains("no runner is configured for `dream` steps")
    );
}

#[test]
fn workflows_can_cap_memory() {
    use harness_config::agents::MemoryAccess;
    let w = common::wf("name: quiet\nmemory: off\nsteps:\n  - id: a\n    run: x\n");
    assert_eq!(w.memory, Some(MemoryAccess::Off));
    let w = common::wf("name: q\nmemory: read\nsteps:\n  - id: a\n    run: x\n");
    assert_eq!(w.memory, Some(MemoryAccess::Read));
    assert_eq!(
        common::wf("name: q\nsteps:\n  - id: a\n    run: x\n").memory,
        None
    );
    let e = parse(
        "name: q\nmemory: lots\nsteps:\n  - id: a\n    run: x\n",
        None,
    )
    .unwrap_err();
    assert!(e[0].to_string().contains("memory"), "{e:?}");
}
