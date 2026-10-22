//! Tool-result elision, compaction and context-anxiety guards in the agent
//! loop (fake provider, real run store).

mod support;

use std::sync::Arc;

use harness_agent::agent_loop::{Conversation, StopReason};
use harness_agent::compaction::{
    COMPACTION_NODE, Compactor, ContextManager, ContextSettings, ELIDED_PREFIX, HANDOFF_SECTIONS,
};
use harness_agent::context::{CONTEXT_PROMPT_SECTION, mentions_remaining_context};
use harness_agent::session::rebuild_conversation;
use harness_events::schema::EventKind;
use harness_providers::fake::{FakeProvider, FakeTurn};
use harness_providers::model::{ChatRequest, ContentBlock, Message, Role, ToolResultPart};
use harness_tools::config::ToolsConfig;
use harness_tools::local::LocalWorkspace;
use harness_tools::read::{ReadArgs, ReadTool};
use harness_tools::spill::{BlobSink, MemoryBlobSink};
use harness_tools::tool::ToolContext;
use harness_tools::tracker::ReadTracker;
use serde_json::json;
use support::*;
use tokio_util::sync::CancellationToken;

fn big_echo(i: usize) -> FakeTurn {
    turn(vec![tool(
        "echo",
        json!({"text": format!("line {i} of a long build log\n"), "times": 400}),
    )])
}

fn result_texts(m: &Message) -> Vec<&str> {
    m.content
        .iter()
        .filter_map(|b| match b {
            ContentBlock::ToolResult { content, .. } => content.iter().find_map(|p| match p {
                ToolResultPart::Text { text } => Some(text.as_str()),
                _ => None,
            }),
            _ => None,
        })
        .collect()
}

fn is_prefix(a: &ChatRequest, b: &ChatRequest) -> bool {
    a.system == b.system
        && a.tools == b.tools
        && b.messages.len() >= a.messages.len()
        && b.messages[..a.messages.len()] == a.messages[..]
}

#[tokio::test]
async fn old_large_tool_results_are_elided_in_batches_and_can_be_reread() {
    let mut turns: Vec<FakeTurn> = (0..8).map(big_echo).collect();
    turns.push(FakeTurn::text("done"));
    let h = Harness::new(turns).await;
    let sink = Arc::new(MemoryBlobSink::default());
    let mut settings = ContextSettings::new(1_000_000);
    settings.elide_tool_results_over = 1000;
    settings.elide_after_turns = 2;
    let manager =
        Arc::new(ContextManager::new(settings).with_elision(sink.clone() as Arc<dyn BlobSink>));
    let mut conv = Conversation::new(Some("sys".into()));
    conv.push_user("summarize the logs");
    let out = h
        .agent(config())
        .with_hook(manager.clone())
        .run(&mut conv, &CancellationToken::new())
        .await;
    assert_eq!(out.stop, StopReason::EndTurn);

    // AC#1: results older than 2 turns and over 1000 tokens became stubs.
    let results: Vec<&str> = conv.messages.iter().flat_map(result_texts).collect();
    assert_eq!(results.len(), 8);
    let elided: Vec<&&str> = results
        .iter()
        .filter(|t| t.starts_with(ELIDED_PREFIX))
        .collect();
    assert!(elided.len() >= 4, "{} elided", elided.len());
    assert!(
        !results[7].starts_with(ELIDED_PREFIX),
        "the newest result stays"
    );
    assert!(elided[0].contains("`echo` result"));

    // AC#2: the history sent is append-only except at batch boundaries.
    let requests = h.provider.requests();
    let breaks = requests
        .windows(2)
        .filter(|w| !is_prefix(&w[0], &w[1]))
        .count();
    assert!(breaks >= 1, "elision happened");
    assert!(
        breaks <= requests.len() / 2,
        "prefix changed {breaks} times in {} requests",
        requests.len()
    );

    // AC#3: the model can re-read an elided result through `spill:<id>`.
    let id = elided[0]
        .split("spill:")
        .nth(1)
        .unwrap()
        .trim_end_matches(['"', '.']);
    let ws = Arc::new(LocalWorkspace::new(h.dir.path()));
    let read = ReadTool::new(ws, Arc::new(ReadTracker::default()), ToolsConfig::default())
        .with_spill(sink.clone() as Arc<dyn BlobSink>);
    let full = read
        .run(
            &ToolContext::new("c1"),
            ReadArgs {
                path: format!("spill:{id}"),
                offset: None,
                limit: None,
            },
        )
        .await
        .unwrap()
        .text()
        .to_owned();
    assert!(
        full.contains("line 0 of a long build log"),
        "{}",
        &full[..200.min(full.len())]
    );

    // The rewritten history is in the log: resume rebuilds it exactly.
    let events = h.events().await;
    let rebuilt = rebuild_conversation(&events, h.recorder.blobs()).unwrap();
    assert_eq!(rebuilt.messages, conv.messages);
    assert!(!manager.elisions().is_empty());
}

fn handoff_text() -> String {
    "## Goal\nAdd a --verbose flag to the CLI.\n\n## Decisions\nUse clap's ArgAction::Count.\n\n\
     ## Current state\nParser updated; tests not written yet.\n\n## Files touched\n- src/cli.rs\n\n\
     ## Next steps\nWrite tests/cli.rs and update the docs.\n\n## Open questions\nnone"
        .to_owned()
}

#[tokio::test]
async fn compaction_at_threshold_writes_a_handoff_keeps_recent_turns_and_the_task_completes() {
    // A long task: six tool turns of logs, then the actual change.
    let mut turns: Vec<FakeTurn> = (0..6)
        .map(|i| {
            turn(vec![tool(
                "echo",
                json!({"text": format!("step {i} output\n"), "times": 150}),
            )])
        })
        .collect();
    turns.push(turn(vec![tool(
        "write",
        json!({"path": "tests/cli.rs", "content": "#[test] fn verbose() {}"}),
    )]));
    turns.push(FakeTurn::text("Added the --verbose flag with tests."));
    let h = Harness::new(turns).await;
    let summarizer = FakeProvider::with_turns([FakeTurn::text(handoff_text())]);
    let mut settings = ContextSettings::new(4000);
    settings.compact_at = 0.5;
    settings.keep_recent_turns = 2;
    let manager = Arc::new(ContextManager::new(settings).with_compactor(Compactor::new(
        Arc::new(summarizer.clone()),
        "fake-small",
        2,
    )));
    let mut conv = Conversation::new(Some(format!(
        "You are a coder.\n\n{CONTEXT_PROMPT_SECTION}"
    )));
    conv.push_user("Add a --verbose flag to the CLI, with tests.");
    let mut agent = h.agent(config()).with_hook(manager.clone());
    let out = agent.run(&mut conv, &CancellationToken::new()).await;

    // Eval: the task completed after compaction.
    assert_eq!(out.stop, StopReason::EndTurn);
    assert_eq!(*h.write.written.lock().unwrap(), vec!["tests/cli.rs"]);
    assert_eq!(out.final_text, "Added the --verbose flag with tests.");

    // AC#1: a handoff with every section replaced the older history.
    let reports = manager.compactions();
    assert!(!reports.is_empty(), "compaction ran");
    let first = &conv.messages[0];
    assert_eq!(first.role, Role::User);
    let handoff = first.text();
    for section in HANDOFF_SECTIONS {
        assert!(
            handoff.contains(&format!("## {section}")),
            "missing {section}"
        );
    }
    assert!(handoff.contains("Use clap's ArgAction::Count"));
    // The summarizer saw the older turns, not the instructions to the agent.
    let sreq = &summarizer.requests()[0];
    assert!(sreq.messages[0].text().contains("step 0 output"));
    assert!(sreq.tools.is_empty());

    // AC#2: the latest turns are verbatim.
    let requests = h.provider.requests();
    let after = requests
        .iter()
        .position(|r| r.messages[0].text().contains("<handoff>"))
        .expect("a request after compaction");
    let before = &requests[after - 1];
    let kept = &requests[after].messages[1..];
    // Kept = the end of the previous request, plus that request's response
    // and tool results, byte for byte.
    let n = kept.len();
    assert!(n >= 3);
    assert_eq!(
        kept[..n - 2],
        before.messages[before.messages.len() - (n - 2)..]
    );
    let assistant_kept = kept.iter().filter(|m| m.role == Role::Assistant).count();
    assert_eq!(assistant_kept, 2, "keep_recent_turns = 2");

    // AC#3: compaction is a child node with its own model call and cost.
    let events = h.events().await;
    let comp = events
        .iter()
        .find(|e| matches!(&e.kind, EventKind::NodeStarted { name, .. } if name == COMPACTION_NODE))
        .expect("compaction node");
    assert_eq!(comp.parent_node_id.as_ref(), Some(&out.node));
    let comp_node = comp.node_id.clone();
    assert!(
        events.iter().any(
            |e| e.node_id == comp_node && matches!(e.kind, EventKind::ModelCallFinished { .. })
        )
    );
    assert!(
        events
            .iter()
            .any(|e| e.node_id == comp_node && matches!(e.kind, EventKind::NodeFinished { .. }))
    );

    // The map's state has the compaction node under the agent, with its cost.
    let state = harness_events::reducer::RunState::from_events(&events);
    let comp_state = state.node(comp_node.as_ref().unwrap()).unwrap();
    assert_eq!(comp_state.name, COMPACTION_NODE);
    assert_eq!(comp_state.parent.as_ref(), Some(&out.node));
    assert_eq!(comp_state.model_calls, 1);
    assert!(
        state
            .node(&out.node)
            .unwrap()
            .children
            .contains(comp_node.as_ref().unwrap())
    );

    // Resume rebuilds the compacted history.
    let rebuilt = rebuild_conversation(&events, h.recorder.blobs()).unwrap();
    assert_eq!(rebuilt.messages, conv.messages);

    // TASK-87 AC#1: no request tells the model how much context is left.
    for r in requests.iter().chain(summarizer.requests().iter()) {
        let all = format!(
            "{}\n{}",
            r.system.clone().unwrap_or_default(),
            serde_json::to_string(&r.messages).unwrap()
        );
        assert!(
            !mentions_remaining_context(&all),
            "context exposed in {all}"
        );
    }
}

#[tokio::test]
async fn slash_compact_compacts_below_the_threshold_with_instructions() {
    let turns = vec![
        big_echo(0),
        big_echo(1),
        big_echo(2),
        FakeTurn::text("first done"),
        FakeTurn::text("second done"),
    ];
    let h = Harness::new(turns).await;
    let summarizer = FakeProvider::with_turns([FakeTurn::text(handoff_text())]);
    let mut settings = ContextSettings::new(1_000_000);
    settings.keep_recent_turns = 1;
    let manager = Arc::new(ContextManager::new(settings).with_compactor(Compactor::new(
        Arc::new(summarizer.clone()),
        "fake-small",
        1,
    )));
    let mut conv = Conversation::new(Some("sys".into()));
    conv.push_user("read the logs");
    let mut agent = h.agent(config()).with_hook(manager.clone());
    agent.run(&mut conv, &CancellationToken::new()).await;
    assert!(manager.compactions().is_empty(), "under the threshold");

    manager.request_compaction(Some("focus on the failing step".into()));
    conv.push_user("now continue");
    agent.run(&mut conv, &CancellationToken::new()).await;
    assert_eq!(manager.compactions().len(), 1);
    assert!(!manager.compaction_requested());
    let sreq = &summarizer.requests()[0];
    assert!(
        sreq.messages[0]
            .text()
            .contains("focus on the failing step")
    );
    // The latest turn and the new prompt are kept verbatim after the handoff.
    let last = h.provider.requests().last().unwrap().clone();
    assert!(last.messages[0].text().contains("<handoff>"));
    assert_eq!(last.messages.len(), 3);
    assert_eq!(last.messages[1].text(), "first done");
    assert_eq!(last.messages[2].text(), "now continue");
}

#[tokio::test]
async fn compact_now_works_while_idle_and_reports_nothing_to_compact() {
    let h = Harness::new(vec![]).await;
    let summarizer = FakeProvider::with_turns([FakeTurn::text(handoff_text())]);
    let manager = ContextManager::new(ContextSettings::new(100_000))
        .with_compactor(Compactor::new(Arc::new(summarizer), "fake-small", 1));
    let node = harness_events::schema::NodeId::from("main");
    let mut short = Conversation::new(None);
    short.push_user("hi");
    short.messages.push(Message::assistant("hello"));
    let err = manager
        .compact_now(
            &mut short,
            &h.recorder,
            &node,
            None,
            &CancellationToken::new(),
        )
        .await
        .unwrap_err();
    assert_eq!(
        err,
        harness_agent::compaction::CompactionError::NothingToCompact
    );

    let mut conv = Conversation::new(None);
    for i in 0..3 {
        conv.push_user(format!("task {i}"));
        conv.messages
            .push(Message::assistant(format!("answer {i}")));
    }
    let report = manager
        .compact_now(
            &mut conv,
            &h.recorder,
            &node,
            None,
            &CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(report.messages_before, 6);
    assert_eq!(conv.messages.len(), 2);
    assert!(
        conv.messages[0].text().contains("task 2"),
        "prompt merged into the handoff"
    );
    assert_eq!(conv.messages[1].text(), "answer 2");
    assert_eq!(report.handoff.goal, "Add a --verbose flag to the CLI.");
}
