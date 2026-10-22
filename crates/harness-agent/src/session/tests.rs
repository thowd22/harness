use super::*;
use serde_json::json;

#[test]
fn titles_come_from_the_first_non_empty_line() {
    assert_eq!(
        title_from_prompt("\n  fix   the\tbug \nmore"),
        "fix the bug"
    );
    assert_eq!(title_from_prompt("   "), "(untitled)");
    assert_eq!(
        title_from_prompt(&"x".repeat(100)).chars().count(),
        TITLE_MAX_CHARS
    );
}

#[test]
fn store_saves_lists_and_finds_sessions() {
    let dir = tempfile::tempdir().unwrap();
    let store = SessionStore::new(dir.path());
    assert!(store.list().unwrap().is_empty());
    let mut a = SessionMeta::new("/p/a", "coder", "fake/m", "first task");
    a.cost_usd = 0.25;
    store.save(&a).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(5));
    let mut b = SessionMeta::new("/p/b", "coder", "fake/m", "second task");
    b.updated_at = a.updated_at + time::Duration::seconds(10);
    store.save(&b).unwrap();
    // Junk is skipped.
    std::fs::create_dir_all(dir.path().join("junk")).unwrap();
    std::fs::write(dir.path().join("junk").join(SESSION_FILE), "{not json").unwrap();

    let list = store.list().unwrap();
    assert_eq!(
        list.iter().map(|m| m.title.as_str()).collect::<Vec<_>>(),
        ["second task", "first task"]
    );
    assert_eq!(store.find("last").unwrap().id, b.id);
    assert_eq!(store.find(&a.id).unwrap(), a);
    assert!(matches!(store.find("zzz"), Err(SessionError::NotFound(_))));
    // Both v7 ids share their leading timestamp digits.
    assert!(matches!(
        store.find(&a.id[..4]),
        Err(SessionError::Ambiguous { count: 2, .. })
    ));
    let line = list[1].listing_line();
    assert!(
        line.contains("$0.2500") && line.contains("/p/a") && line.contains("first task"),
        "{line}"
    );
}

#[test]
fn drift_detects_modified_deleted_and_created_files() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.txt"), "a").unwrap();
    std::fs::write(dir.path().join("b.txt"), "b").unwrap();
    let saved = snapshot_files(dir.path(), ["a.txt", "b.txt", "c.txt", "same.txt"]);
    assert_eq!(saved["c.txt"], None);
    assert!(file_drift(dir.path(), &saved).is_empty());
    std::fs::write(dir.path().join("a.txt"), "changed").unwrap();
    std::fs::remove_file(dir.path().join("b.txt")).unwrap();
    std::fs::write(dir.path().join("c.txt"), "new").unwrap();
    let drift = file_drift(dir.path(), &saved);
    let kinds: Vec<(&str, DriftKind)> = drift.iter().map(|d| (d.path.as_str(), d.kind)).collect();
    assert_eq!(
        kinds,
        [
            ("a.txt", DriftKind::Modified),
            ("b.txt", DriftKind::Deleted),
            ("c.txt", DriftKind::Created)
        ]
    );
    assert!(drift[0].warning().contains("a.txt was modified on disk"));
}

#[test]
fn touched_paths_cover_file_tools_inside_the_project() {
    let mut conv = Conversation::new(None);
    conv.messages.push(Message {
        role: Role::Assistant,
        content: vec![
            ContentBlock::tool_use("1", "read", json!({"path": "src/./lib.rs"})),
            ContentBlock::tool_use("2", "edit", json!({"path": "/proj/README.md"})),
            ContentBlock::tool_use("3", "write", json!({"path": "../outside.txt"})),
            ContentBlock::tool_use("4", "bash", json!({"command": "ls"})),
            ContentBlock::tool_use("5", "read", json!({"path": "spill:abc"})),
        ],
    });
    let paths: Vec<String> = touched_paths(&conv, Path::new("/proj"))
        .into_iter()
        .collect();
    assert_eq!(paths, ["README.md", "src/lib.rs"]);
}
