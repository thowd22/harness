use super::*;
use crate::events::RecordingSink;
use harness_events::schema::{RunId, SCHEMA_VERSION};

fn write(root: &Path, rel: &str, text: &str) {
    let p = root.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, text).unwrap();
}

fn tool_change(root: &Path, rel: &str, kind: ChangeKind) -> FileChange {
    FileChange {
        tool: "write",
        path: root.join(rel),
        kind,
        before_hash: (kind == ChangeKind::Modified).then(|| "b".to_string()),
        after_hash: "a".into(),
        diff: format!("--- a/{rel}\n+++ b/{rel}\n@@ -0,0 +1 @@\n+x\n"),
    }
}

#[test]
fn records_tool_changes_per_node_and_emits_events() {
    let dir = tempfile::tempdir().unwrap();
    let sink = Arc::new(RecordingSink::default());
    let ledger = Ledger::new(dir.path()).with_sink(sink.clone());
    let n1 = NodeId::from("n1");
    let n2 = NodeId::from("n2");
    ledger.record_tool_change(
        Some(&n1),
        &tool_change(dir.path(), "src/a.rs", ChangeKind::Created),
    );
    ledger.record_tool_change(
        Some(&n2),
        &tool_change(dir.path(), "src/b.rs", ChangeKind::Modified),
    );
    ledger.record(
        Some(&n2),
        Path::new("old.txt"),
        FileChangeKind::Deleted,
        ChangeSource::Tool {
            tool: "delete".into(),
        },
        Some("h".into()),
        None,
        None,
    );
    let n1_entries = ledger.for_node(&n1);
    assert_eq!(n1_entries.len(), 1);
    assert_eq!(n1_entries[0].path, "src/a.rs");
    assert_eq!(n1_entries[0].kind, FileChangeKind::Created);
    let n2_paths: Vec<_> = ledger
        .for_node(&n2)
        .into_iter()
        .map(|e| (e.path, e.kind))
        .collect();
    assert_eq!(
        n2_paths,
        vec![
            ("src/b.rs".to_string(), FileChangeKind::Modified),
            ("old.txt".to_string(), FileChangeKind::Deleted)
        ]
    );
    let drafts = sink.drafts();
    assert_eq!(drafts.len(), 3);
    assert_eq!(drafts[0].node_id, Some(n1.clone()));
    assert!(matches!(
        &drafts[0].kind,
        EventKind::FileChanged { path, change: FileChangeKind::Created, diff: Some(_), generated: false } if path == "src/a.rs"
    ));
}

#[test]
fn detects_bash_changes_including_deletes() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(root, "keep.txt", "same\n");
    write(root, "edit.txt", "before\n");
    write(root, "rm.txt", "bye\n");
    let sink = Arc::new(RecordingSink::default());
    let ledger = Ledger::new(root).with_sink(sink.clone());
    let node = NodeId::from("agent");

    let watch = ledger.begin_command().unwrap();
    // What a bash command would do.
    write(root, "edit.txt", "after\n");
    std::fs::remove_file(root.join("rm.txt")).unwrap();
    write(root, "tmp/debug.py", "print(1)\n");
    let entries = ledger
        .end_command(watch, Some(&node), "python gen.py")
        .unwrap();

    let got: Vec<_> = entries.iter().map(|e| (e.path.as_str(), e.kind)).collect();
    assert_eq!(
        got,
        vec![
            ("edit.txt", FileChangeKind::Modified),
            ("rm.txt", FileChangeKind::Deleted),
            ("tmp/debug.py", FileChangeKind::Created),
        ]
    );
    assert!(entries.iter().all(|e| e.source
        == ChangeSource::Bash {
            command: "python gen.py".into()
        }));
    assert!(entries[0].diff.as_deref().unwrap().contains("+after"));
    assert_eq!(sink.drafts().len(), 3);

    // A command that changes nothing records nothing.
    let watch = ledger.begin_command().unwrap();
    assert!(
        ledger
            .end_command(watch, Some(&node), "ls")
            .unwrap()
            .is_empty()
    );
}

#[test]
fn net_changes_fold_entries() {
    let dir = tempfile::tempdir().unwrap();
    let ledger = Ledger::new(dir.path());
    let n = NodeId::from("n");
    let rec = |p: &str, k: FileChangeKind, before: Option<&str>| {
        ledger.record(
            Some(&n),
            Path::new(p),
            k,
            ChangeSource::Tool {
                tool: "edit".into(),
            },
            before.map(str::to_string),
            None,
            None,
        );
    };
    rec("new.rs", FileChangeKind::Created, None);
    rec("new.rs", FileChangeKind::Modified, Some("1"));
    rec("scratch.txt", FileChangeKind::Created, None);
    rec("scratch.txt", FileChangeKind::Deleted, Some("2"));
    rec("old.rs", FileChangeKind::Modified, Some("0"));
    rec("old.rs", FileChangeKind::Deleted, Some("3"));
    rec("again.rs", FileChangeKind::Deleted, Some("4"));
    rec("again.rs", FileChangeKind::Created, None);
    let net: Vec<_> = ledger
        .net_changes(Some(&n))
        .into_iter()
        .map(|c| (c.path, c.kind))
        .collect();
    assert_eq!(
        net,
        vec![
            ("again.rs".to_string(), FileChangeKind::Modified),
            ("new.rs".to_string(), FileChangeKind::Created),
            ("old.rs".to_string(), FileChangeKind::Deleted),
        ]
    );
    let lines = summary_lines(&ledger.net_changes(None));
    assert_eq!(lines[1], "+ new.rs (edit)");
}

#[tokio::test]
async fn ledger_is_part_of_the_event_log() {
    let data = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let run_id = RunId::new();
    let recorder = harness_events::store::RunRecorder::create(
        data.path(),
        run_id,
        Arc::new(harness_events::redact::Redactor::default()),
        Default::default(),
    )
    .await
    .unwrap();
    let ledger = Ledger::new(work.path()).with_sink(Arc::new(recorder.clone()));
    let node = NodeId::from("fix");
    ledger.record_tool_change(
        Some(&node),
        &tool_change(work.path(), "src/lib.rs", ChangeKind::Modified),
    );
    let watch = ledger.begin_command().unwrap();
    write(work.path(), "out.log", "x\n");
    ledger.end_command(watch, Some(&node), "make").unwrap();
    recorder.finish().await.unwrap();

    let replay =
        harness_events::store::read_run(&harness_events::store::run_dir(data.path(), run_id))
            .unwrap();
    assert_eq!(replay.header.schema_version, SCHEMA_VERSION);
    let rebuilt = Ledger::from_events(work.path(), &replay.events);
    let got: Vec<_> = rebuilt
        .for_node(&node)
        .into_iter()
        .map(|e| (e.path, e.kind))
        .collect();
    assert_eq!(
        got,
        vec![
            ("src/lib.rs".to_string(), FileChangeKind::Modified),
            ("out.log".to_string(), FileChangeKind::Created),
        ]
    );
    // The reducer counts them on the node (what node detail shows).
    let state = harness_events::reducer::RunState::from_events(&replay.events);
    assert_eq!(state.node(&node).map(|n| n.files_changed), Some(2));
}

#[test]
fn builtin_artifacts_are_recorded_as_generated_without_a_gitignore() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(
        root,
        "calc/__init__.py",
        "def add(a, b):\n    return a + b\n",
    );
    let sink = Arc::new(RecordingSink::default());
    let ledger = Ledger::new(root).with_sink(sink.clone());
    let node = NodeId::from("agent");

    // A test run: only caches and bytecode.
    let watch = ledger.begin_command().unwrap();
    write(
        root,
        "calc/__pycache__/__init__.cpython-312.pyc",
        "\u{0}pyc",
    );
    write(root, ".pytest_cache/v/cache/nodeids", "[]");
    write(root, ".coverage", "x");
    let entries = ledger
        .end_command(watch, Some(&node), "./run_tests.sh")
        .unwrap();
    assert_eq!(entries.len(), 3);
    assert!(entries.iter().all(|e| e.generated), "{entries:?}");
    assert!(entries.iter().all(|e| e.diff.is_none()));
    // Still in the event log, labelled generated.
    assert_eq!(sink.drafts().len(), 3);
    assert!(sink.drafts().iter().all(|d| matches!(
        d.kind,
        EventKind::FileChanged {
            generated: true,
            ..
        }
    )));

    // A formatter rewriting a source file still counts.
    let watch = ledger.begin_command().unwrap();
    write(
        root,
        "calc/__init__.py",
        "def add(a, b):\n    return b + a\n",
    );
    write(
        root,
        "calc/__pycache__/__init__.cpython-312.pyc",
        "\u{0}pyc2",
    );
    let entries = ledger.end_command(watch, Some(&node), "black .").unwrap();
    let got: Vec<_> = entries
        .iter()
        .map(|e| (e.path.as_str(), e.generated))
        .collect();
    assert_eq!(
        got,
        vec![
            ("calc/__init__.py", false),
            ("calc/__pycache__/__init__.cpython-312.pyc", true),
        ]
    );
    assert!(
        entries[0]
            .diff
            .as_deref()
            .unwrap()
            .contains("+    return b + a")
    );

    // Net changes and summary lines keep the label; a rebuilt ledger too.
    let net = ledger.net_changes(Some(&node));
    let generated: Vec<_> = net
        .iter()
        .filter(|c| c.generated)
        .map(|c| c.path.as_str())
        .collect();
    assert_eq!(
        generated,
        vec![
            ".coverage",
            ".pytest_cache/v/cache/nodeids",
            "calc/__pycache__/__init__.cpython-312.pyc"
        ]
    );
    let lines = summary_lines(&net);
    assert!(
        lines.contains(&"+ .coverage (bash, generated)".to_string()),
        "{lines:?}"
    );
    assert!(lines.contains(&"~ calc/__init__.py (bash)".to_string()));
    let events: Vec<RunEvent> = sink
        .drafts()
        .into_iter()
        .enumerate()
        .map(|(i, d)| RunEvent {
            seq: i as u64,
            ts: time::OffsetDateTime::UNIX_EPOCH,
            run_id: RunId::new(),
            node_id: d.node_id,
            parent_node_id: None,
            kind: d.kind,
        })
        .collect();
    let rebuilt = Ledger::from_events(root, &events);
    assert_eq!(
        rebuilt
            .net_changes(None)
            .iter()
            .filter(|c| c.generated)
            .count(),
        3
    );
}

#[test]
fn gitignored_files_are_not_changes_and_tool_edits_are_never_generated() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(root, ".gitignore", "build/\n*.log\n");
    let ledger = Ledger::new(root);
    let node = NodeId::from("agent");
    let watch = ledger.begin_command().unwrap();
    write(root, "build/out.o", "x");
    write(root, "test.log", "x");
    assert!(
        ledger
            .end_command(watch, Some(&node), "make test")
            .unwrap()
            .is_empty()
    );
    // The model writing into an artifact directory did it on purpose.
    let e = ledger.record_tool_change(
        Some(&node),
        &tool_change(root, "node_modules/patch/index.js", ChangeKind::Created),
    );
    assert!(!e.generated);
}

fn git(root: &Path, args: &[&str]) {
    let st = std::process::Command::new("git")
        .args(["-c", "user.name=t", "-c", "user.email=t@t.invalid"])
        .args(args)
        .current_dir(root)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .stdout(std::process::Stdio::null())
        .status()
        .unwrap();
    assert!(st.success(), "git {args:?}");
}

#[test]
fn tracked_files_matching_the_artifact_list_still_count() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path();
    git(repo, &["init", "-q"]);
    write(repo, "app/infra/target/config.txt", "port = 1\n");
    write(repo, "app/src/target/mod.rs", "pub fn f() {}\n");
    write(repo, "app/coverage.xml", "<coverage/>\n");
    git(repo, &["add", "."]);
    git(repo, &["commit", "-q", "-m", "init"]);
    // The workspace is below the repository root.
    let ws = repo.join("app");
    let ledger = Ledger::new(&ws);
    let watch = ledger.begin_command().unwrap();
    write(&ws, "infra/target/config.txt", "port = 2\n");
    write(&ws, "src/target/mod.rs", "pub fn g() {}\n");
    std::fs::remove_file(ws.join("coverage.xml")).unwrap();
    // Untracked build output next to them is still generated.
    write(&ws, "target/debug/app", "bin");
    write(&ws, "src/target/__pycache__/m.pyc", "pyc");
    let entries = ledger
        .end_command(watch, None, "sed -i s/1/2/ infra/target/config.txt")
        .unwrap();
    let got: Vec<(&str, bool)> = entries
        .iter()
        .map(|e| (e.path.as_str(), e.generated))
        .collect();
    assert_eq!(
        got,
        vec![
            ("coverage.xml", false),
            ("infra/target/config.txt", false),
            ("src/target/__pycache__/m.pyc", true),
            ("src/target/mod.rs", false),
            ("target/debug/app", true),
        ]
    );
    let config = &entries[1];
    assert!(
        config
            .diff
            .as_deref()
            .is_some_and(|d| d.contains("+port = 2")),
        "{config:?}"
    );
}
