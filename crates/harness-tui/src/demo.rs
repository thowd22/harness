//! UI gallery (`harness ui-demo`): scripted data for every component.
//!
//! [`events`] builds realistic event logs. [`scripted_run`] is the gallery's
//! run: a `fix-bug` workflow with four steps, two subagents, a remote host,
//! a retry, a failing verify, an approval and tool calls.

pub mod events;
pub mod gallery;

use harness_events::schema::{DeltaChannel, FileChangeKind, NodeStatus, RunEvent};
use serde_json::json;

use events::{EventBuilder, NodeSpec, usage};

/// The gallery's scripted run, stopped while `fix` waits for an approval.
pub fn scripted_run() -> Vec<RunEvent> {
    let mut b = EventBuilder::new();
    b.run_started("fix-bug", Some("fix-bug"));
    b.start(NodeSpec::step("repro").agent("implementer").model("sonnet"));
    b.start(
        NodeSpec::step("prove-fail")
            .kind(harness_events::schema::NodeKind::Verify)
            .needs(&["repro"])
            .queued(),
    );
    b.start(
        NodeSpec::step("fix")
            .agent("implementer")
            .model("sonnet")
            .host("gpu-box")
            .needs(&["prove-fail"])
            .queued(),
    );
    b.start(
        NodeSpec::step("proof")
            .agent("bugfix-proof")
            .needs(&["fix"])
            .queued(),
    );
    b.model_call("repro", "c1", "sonnet");
    b.delta(
        "repro",
        "c1",
        DeltaChannel::Reasoning,
        "The header parser panics on a missing colon.",
    );
    b.delta(
        "repro",
        "c1",
        DeltaChannel::Text,
        "Adding a failing test for headers without a colon.",
    );
    b.advance(4_200);
    b.finish_call("repro", "c1", usage(500, 11_900, 812, 120), 4_200);
    b.cost("repro", 0.021);
    b.tool_call(
        "repro",
        "t1",
        "write",
        json!({"path": "tests/header.rs", "content": "#[test]\nfn header_colon() {}\n"}),
    );
    b.advance(3);
    b.file_changed("repro", "tests/header.rs", FileChangeKind::Created, Some("--- /dev/null\n+++ b/tests/header.rs\n@@ -0,0 +1,2 @@\n+#[test]\n+fn header_colon() {}\n"));
    b.tool_result("repro", "t1", true, "wrote 2 lines", 3);
    b.advance(36_800);
    b.cost("repro", 0.059);
    b.finish("repro", NodeStatus::Succeeded, None);
    b.running("prove-fail");
    b.verify(
        "prove-fail",
        "cargo test header",
        false,
        "test header_colon ... FAILED\n1 failed",
        6_000,
    );
    b.finish("prove-fail", NodeStatus::Succeeded, None);
    b.running("fix");
    b.model_call("fix", "c2", "sonnet");
    b.delta(
        "fix",
        "c2",
        DeltaChannel::Text,
        "I'll update parse_header to return an error.",
    );
    b.advance(2_000);
    b.finish_call("fix", "c2", usage(400, 12_000, 600, 0), 2_000);
    b.cost("fix", 0.31);
    b.retry("fix", 1, "429 rate limited");
    b.start(
        NodeSpec::subagent("r1", "fix")
            .name("researcher")
            .agent("researcher")
            .model("haiku"),
    );
    b.start(
        NodeSpec::subagent("r2", "fix")
            .name("researcher")
            .agent("researcher")
            .model("haiku"),
    );
    b.model_call("r1", "c3", "haiku");
    b.tool_call("r1", "t2", "read", json!({"path": "src/net/mod.rs"}));
    b.advance(9_000);
    b.tool_result("r2", "t3", true, "ok", 4);
    b.cost("r2", 0.004);
    b.finish("r2", NodeStatus::Succeeded, None);
    b.advance(3_000);
    b.tool_call(
        "fix",
        "t4",
        "bash",
        json!({"command": "git push origin main"}),
    );
    b.approval("fix", "approval-1-1", "git push origin main");
    b.into_events()
}

/// Past runs for the gallery's history page and palette: agent runs
/// titled after their first prompt, a workflow titled after its inputs,
/// a failed and a running run, and two runs whose titles collide (so they
/// carry `#tags`). Ids are fixed so snapshots are stable.
pub fn history_runs() -> Vec<crate::history::RunSummary> {
    use harness_events::schema::{EventKind, RunId, RunStatus};
    let id = |n: u32| -> RunId {
        serde_json::from_value(json!(format!("0199f1a2-3b4c-7d5e-8f60-{n:012x}")))
            .expect("demo run id parses")
    };
    let started =
        |b: &mut EventBuilder, name: &str, workflow: Option<&str>, title: Option<&str>| {
            b.push(
                None,
                None,
                EventKind::RunStarted {
                    name: name.to_owned(),
                    workflow: workflow.map(str::to_owned),
                    host: None,
                    title: title.map(str::to_owned),
                    agent_names: Default::default(),
                },
            );
        };
    let agent_run = |n: u32, agent: &str, prompt: &str, at_min: i64, status: Option<RunStatus>| {
        let mut b = EventBuilder::with_run(id(n));
        b.advance(at_min * 60_000);
        started(&mut b, agent, None, None);
        b.start(NodeSpec::step("main").agent(agent).model("sonnet"));
        b.user_message("main", prompt);
        b.advance(134_000);
        b.cost("main", 0.12);
        if let Some(s) = status {
            b.file_changed("main", "src/parse.rs", FileChangeKind::Modified, None);
            b.finish("main", NodeStatus::Succeeded, None);
            b.run_finished(s);
        }
        b.into_events()
    };
    let mut wf = EventBuilder::with_run(id(0xabc001));
    wf.advance(-90 * 60_000);
    started(
        &mut wf,
        "fix-bug",
        Some("fix-bug"),
        Some("Header parser drops values after a colon"),
    );
    wf.start(NodeSpec::step("repro").agent("implementer").model("sonnet"));
    wf.advance(41_000);
    wf.cost("repro", 0.08);
    wf.finish("repro", NodeStatus::Failed, Some("verify failed: 2 tests"));
    wf.push(
        None,
        None,
        EventKind::RunFinished {
            status: RunStatus::Failed,
            error: Some("step `repro` failed: verify failed: 2 tests".into()),
        },
    );
    let logs = [
        agent_run(
            0xfed001,
            "researcher",
            "Survey how the net module retries requests",
            0,
            None,
        ),
        agent_run(
            0x7a2b01,
            "coder",
            "Fix the header parser.\nIt drops values after a colon.",
            -30,
            Some(RunStatus::Succeeded),
        ),
        agent_run(
            0x7a2b02,
            "coder",
            "Update the changelog",
            -60,
            Some(RunStatus::Succeeded),
        ),
        agent_run(
            0x7a2b03,
            "coder",
            "Update the changelog",
            -60,
            Some(RunStatus::Succeeded),
        ),
        wf.into_events(),
    ];
    let now = events::T0 + time::Duration::minutes(5);
    logs.iter()
        .filter_map(|events| {
            let state = harness_events::reducer::RunState::from_events(events);
            crate::history::RunSummary::from_state(&state, now)
        })
        .collect()
}
