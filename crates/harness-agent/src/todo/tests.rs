use harness_tools::tool::ToolContext;

use super::*;

fn ctx() -> ToolContext {
    ToolContext::new("c1")
}

fn actor(n: &str) -> Actor {
    Actor::new(NodeId::from(n), format!("agent-{n}"))
}

async fn call(tool: &TodoTool, args: Value) -> Result<String, ToolError> {
    tool.call(&ctx(), args).await.map(|o| o.text().to_owned())
}

#[tokio::test]
async fn add_update_list_with_all_fields() {
    let board = Arc::new(TaskBoard::new());
    let tool = TodoTool::new(board.clone(), actor("a"));
    let out = call(&tool, json!({"action": "add", "title": "Write parser"}))
        .await
        .unwrap();
    assert_eq!(out, "ok: t1 [pending] Write parser (agent-a)");
    call(
        &tool,
        json!({"action": "add", "id": "docs", "title": "Docs", "status": "blocked"}),
    )
    .await
    .unwrap();
    call(
        &tool,
        json!({"action": "update", "id": "t1", "status": "in_progress"}),
    )
    .await
    .unwrap();
    for s in TaskStatus::ALL {
        call(
            &tool,
            json!({"action": "update", "id": "docs", "status": s.as_str()}),
        )
        .await
        .unwrap();
        assert_eq!(board.get("docs").unwrap().status, s);
    }
    let list = call(&tool, json!({"action": "list"})).await.unwrap();
    assert!(list.starts_with("0/2 done"), "{list}");
    assert!(list.contains("t1 [in_progress] Write parser (agent-a)"));
    assert!(list.contains("docs [cancelled] Docs (agent-a)"));
    let t = board.get("t1").unwrap();
    assert_eq!(t.owner, Some(NodeId::from("a")));
    assert_eq!(t.agent.as_deref(), Some("agent-a"));
}

#[tokio::test]
async fn errors_are_actionable() {
    let tool = TodoTool::new(Arc::new(TaskBoard::new()), actor("a"));
    let e = call(
        &tool,
        json!({"action": "update", "id": "zz", "status": "done"}),
    )
    .await
    .unwrap_err();
    assert!(e.message.contains("no task `zz`"), "{}", e.message);
    let e = call(
        &tool,
        json!({"action": "add", "title": "x", "status": "nope"}),
    )
    .await
    .unwrap_err();
    assert!(e.message.contains("unknown status"));
    let e = call(&tool, json!({"action": "add", "title": "  "}))
        .await
        .unwrap_err();
    assert!(e.message.contains("title"));
    call(&tool, json!({"action": "add", "id": "a", "title": "x"}))
        .await
        .unwrap();
    let e = call(&tool, json!({"action": "add", "id": "a", "title": "y"}))
        .await
        .unwrap_err();
    assert!(e.message.contains("already exists"));
    let e = call(&tool, json!({"action": "delete"})).await.unwrap_err();
    assert!(e.message.contains("unknown action"));
}

#[tokio::test]
async fn owner_defaults_to_caller_and_subagents_claim_tasks() {
    let board = Arc::new(TaskBoard::new());
    let parent = TodoTool::new(board.clone(), actor("parent"));
    let child = TodoTool::new(board.clone(), actor("child"));
    call(&parent, json!({"action": "add", "title": "Survey callers"}))
        .await
        .unwrap();
    // Updating without claim keeps the owner.
    call(
        &child,
        json!({"action": "update", "id": "t1", "status": "in_progress"}),
    )
    .await
    .unwrap();
    assert_eq!(board.get("t1").unwrap().owner, Some(NodeId::from("parent")));
    call(
        &child,
        json!({"action": "update", "id": "t1", "claim": true}),
    )
    .await
    .unwrap();
    let t = board.get("t1").unwrap();
    assert_eq!(t.owner, Some(NodeId::from("child")));
    assert_eq!(t.agent.as_deref(), Some("agent-child"));
    assert_eq!(t.status, TaskStatus::InProgress);
}

#[test]
fn schema_is_compact() {
    let s = serde_json::to_string(&schema()).unwrap();
    assert!(s.len() < 400, "todo schema is {} bytes", s.len());
    assert!(DESCRIPTION.len() < 200);
}

#[tokio::test]
async fn every_change_is_broadcast_and_journaled_and_survives_resume() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("run1").join(TASKS_FILE);
    let board = Arc::new(TaskBoard::open(&path).unwrap());
    let mut rx = board.subscribe();
    let tool = TodoTool::new(board.clone(), actor("a"));
    board.set_goal(Some("Fix bug".into())).unwrap();
    call(&tool, json!({"action": "add", "title": "one"}))
        .await
        .unwrap();
    call(
        &tool,
        json!({"action": "update", "id": "t1", "status": "done"}),
    )
    .await
    .unwrap();
    let kinds: Vec<TaskChange> = (0..3).map(|_| rx.try_recv().unwrap().change).collect();
    assert_eq!(
        kinds,
        [TaskChange::Goal, TaskChange::Added, TaskChange::Updated]
    );
    let before = board.snapshot();
    drop(tool);
    drop(board);

    // Reopening the same journal restores the state.
    let reopened = TaskBoard::open(&path).unwrap();
    assert_eq!(reopened.snapshot(), before);
    // New ids continue after the old ones.
    let ev = reopened
        .apply(
            TaskOp::Add {
                id: None,
                title: "two".into(),
                status: None,
            },
            &actor("b"),
        )
        .unwrap();
    assert_eq!(ev.task.unwrap().id, "t2");

    // A resumed session starts a new run that carries the tasks over.
    let next = dir.path().join("run2").join(TASKS_FILE);
    let resumed = TaskBoard::continue_from(&path, &next).unwrap();
    let snap = resumed.snapshot();
    assert_eq!(snap.goal.as_deref(), Some("Fix bug"));
    assert_eq!(snap.tasks.len(), 2);
    assert_eq!(snap.progress(), (1, 2));
    drop(resumed);
    assert_eq!(TaskBoard::open(&next).unwrap().snapshot().tasks.len(), 2);
}

#[tokio::test]
async fn torn_journal_line_is_ignored() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(TASKS_FILE);
    {
        let b = TaskBoard::open(&path).unwrap();
        b.apply(
            TaskOp::Add {
                id: None,
                title: "x".into(),
                status: None,
            },
            &actor("a"),
        )
        .unwrap();
    }
    let mut f = OpenOptions::new().append(true).open(&path).unwrap();
    f.write_all(b"{\"version\":2,\"cha").unwrap();
    assert_eq!(TaskBoard::open(&path).unwrap().snapshot().tasks.len(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_updates_are_ordered_without_lost_writes() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(TASKS_FILE);
    let board = Arc::new(TaskBoard::open(&path).unwrap());
    let mut handles = Vec::new();
    for agent in 0..8 {
        let tool = TodoTool::new(board.clone(), actor(&format!("n{agent}")));
        handles.push(tokio::spawn(async move {
            for i in 0..25 {
                call(
                    &tool,
                    json!({"action": "add", "id": format!("a{agent}-{i}"), "title": format!("task {i}")}),
                )
                .await
                .unwrap();
            }
        }));
    }
    for h in handles {
        h.await.unwrap();
    }
    let snap = board.snapshot();
    assert_eq!(snap.tasks.len(), 200, "no add was lost");
    assert_eq!(snap.version, 200);
    // The journal has one line per change, in strictly increasing order.
    let text = std::fs::read_to_string(&path).unwrap();
    let versions: Vec<u64> = text
        .lines()
        .map(|l| serde_json::from_str::<TaskEvent>(l).unwrap().version)
        .collect();
    assert_eq!(versions, (1..=200).collect::<Vec<_>>());
    drop(board);
    assert_eq!(TaskBoard::open(&path).unwrap().snapshot(), snap);
}

#[test]
fn step_linked_tasks_follow_their_step() {
    let board = TaskBoard::new();
    board
        .seed([
            Task::new("repro", "Reproduce").with_step("repro"),
            Task::new("free", "Other"),
        ])
        .unwrap();
    // Seeding again keeps existing progress.
    board
        .sync_step(
            "repro",
            TaskStatus::InProgress,
            Some(&NodeId::from("wf/repro")),
            Some("implementer"),
        )
        .unwrap();
    assert!(
        board
            .seed([Task::new("repro", "Reproduce").with_step("repro")])
            .unwrap()
            .is_empty()
    );
    let t = board.get("repro").unwrap();
    assert_eq!(t.status, TaskStatus::InProgress);
    assert_eq!(t.agent.as_deref(), Some("implementer"));
    assert_eq!(t.owner, Some(NodeId::from("wf/repro")));
    // No change, no event.
    assert!(
        board
            .sync_step("repro", TaskStatus::InProgress, None, None)
            .unwrap()
            .is_empty()
    );
    assert_eq!(board.get("free").unwrap().status, TaskStatus::Pending);
}

#[tokio::test]
async fn board_changes_are_recorded_as_task_snapshots() {
    use harness_events::schema::EventKind;
    let data = tempfile::tempdir().unwrap();
    let rec = harness_events::store::RunRecorder::create(
        data.path(),
        harness_events::schema::RunId::new(),
        Arc::new(harness_events::redact::Redactor::default()),
        Default::default(),
    )
    .await
    .unwrap();
    let board = Arc::new(TaskBoard::new());
    board.set_goal(Some("Ship it".into())).unwrap();
    let task = record_tasks(&board, rec.clone());
    let actor = Actor::new(NodeId::from("main"), "coder");
    board
        .apply(
            TaskOp::Add {
                id: Some("t1".into()),
                title: "Write the test".into(),
                status: None,
            },
            &actor,
        )
        .unwrap();
    board
        .apply(
            TaskOp::Update {
                id: "t1".into(),
                title: None,
                status: Some(TaskStatus::InProgress),
                claim: true,
            },
            &actor,
        )
        .unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    rec.finish().await.unwrap();
    drop(board);
    let _ = task.await;
    let events = harness_events::store::read_run(rec.dir()).unwrap().events;
    type Snap = (Option<String>, Vec<(String, String)>, u64);
    let snaps: Vec<Snap> = events
        .iter()
        .filter_map(|e| match &e.kind {
            EventKind::Tasks {
                goal,
                tasks,
                version,
            } => Some((
                goal.clone(),
                tasks
                    .iter()
                    .map(|t| (t.id.clone(), t.status.clone()))
                    .collect(),
                *version,
            )),
            _ => None,
        })
        .collect();
    // The initial goal, then the changes (several may share a snapshot).
    assert!(snaps.len() >= 2, "{snaps:?}");
    assert_eq!(snaps[0].0.as_deref(), Some("Ship it"));
    assert!(snaps[0].1.is_empty());
    assert_eq!(
        snaps.last().unwrap().1,
        vec![("t1".to_owned(), "in_progress".to_owned())]
    );
    assert!(snaps.windows(2).all(|w| w[0].2 < w[1].2));
}
