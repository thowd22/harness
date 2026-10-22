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
