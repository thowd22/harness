use super::*;
use std::sync::Arc;

/// Run the user's own git (normal config resolution except global/system).
pub(crate) fn user_git(dir: &Path, args: &[&str]) -> String {
    let mut cmd = Command::new("git");
    for (k, _) in std::env::vars_os() {
        if k.to_string_lossy().starts_with("GIT_") {
            cmd.env_remove(&k);
        }
    }
    let out = cmd
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .args([
            "-c",
            "user.name=u",
            "-c",
            "user.email=u@x",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn write(root: &Path, rel: &str, text: &str) {
    let p = root.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, text).unwrap();
}

fn read(root: &Path, rel: &str) -> Option<String> {
    std::fs::read_to_string(root.join(rel)).ok()
}

/// Everything about the user's repository that must never change.
fn user_repo_state(root: &Path) -> (Vec<u8>, String, String, String, String) {
    (
        std::fs::read(root.join(".git/index")).unwrap(),
        user_git(root, &["branch", "-a", "-v"]),
        user_git(root, &["stash", "list"]),
        user_git(root, &["rev-parse", "HEAD"]),
        user_git(root, &["diff", "--cached", "--name-status"]),
    )
}

fn setup() -> (tempfile::TempDir, tempfile::TempDir, CheckpointStore) {
    let work = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    let root = work.path();
    user_git(root, &["init", "-q", "-b", "main"]);
    write(root, ".gitignore", "target/\n");
    write(root, "src/lib.rs", "fn a() {}\n");
    write(root, "README.md", "hi\n");
    user_git(root, &["add", "."]);
    user_git(root, &["commit", "-q", "-m", "init"]);
    user_git(root, &["branch", "feature"]);
    // A stash and a staged change the shadow repo must not touch.
    write(root, "README.md", "stashed\n");
    user_git(root, &["stash", "-q"]);
    write(root, "src/staged.rs", "staged\n");
    user_git(root, &["add", "src/staged.rs"]);
    let store = CheckpointStore::open(data.path(), root).unwrap();
    (work, data, store)
}

#[test]
fn shadow_repo_lives_outside_the_worktree() {
    let (work, data, store) = setup();
    assert!(store.git_dir().starts_with(data.path()));
    assert!(!store.git_dir().starts_with(work.path()));
    assert!(store.git_dir().join("HEAD").exists());
    // Reopening finds the same repository.
    let again = CheckpointStore::open(data.path(), work.path()).unwrap();
    assert_eq!(again.git_dir(), store.git_dir());
}

#[test]
fn undo_restores_previous_turn_without_touching_user_git() {
    let (work, _data, store) = setup();
    let root = work.path();
    let before = user_repo_state(root);
    let store = Arc::new(store);
    let turns = TurnCheckpointer::new(store.clone());
    let node = NodeId::from("coder");

    // Turn 1 edits lib.rs and adds a file; a second mutation in the same
    // turn does not take another checkpoint.
    let cp1 = turns.before_mutation(Some(&node), 1).unwrap().unwrap();
    assert!(turns.before_mutation(Some(&node), 1).unwrap().is_none());
    write(root, "src/lib.rs", "fn a() { turn1 }\n");
    write(root, "debug.py", "print('x')\n");
    write(root, "target/build.o", "ignored\n");

    // Turn 2 edits again and deletes README.
    let cp2 = turns.before_mutation(Some(&node), 2).unwrap().unwrap();
    write(root, "src/lib.rs", "fn a() { turn2 }\n");
    std::fs::remove_file(root.join("README.md")).unwrap();

    // /undo restores the state before turn 2.
    let undone = store.undo().unwrap();
    assert_eq!(undone.id, cp2.id);
    assert_eq!(read(root, "src/lib.rs").unwrap(), "fn a() { turn1 }\n");
    assert_eq!(read(root, "README.md").unwrap(), "hi\n");
    assert!(read(root, "debug.py").is_some());

    // A second /undo goes back before turn 1: the added file is removed,
    // ignored files are untouched.
    let undone = store.undo().unwrap();
    assert_eq!(undone.id, cp1.id);
    assert_eq!(read(root, "src/lib.rs").unwrap(), "fn a() {}\n");
    assert!(read(root, "debug.py").is_none());
    assert_eq!(read(root, "target/build.o").unwrap(), "ignored\n");
    assert_eq!(read(root, "src/staged.rs").unwrap(), "staged\n");
    assert!(matches!(store.undo(), Err(CheckpointError::NothingToUndo)));

    assert_eq!(user_repo_state(root), before, "user git state changed");
}

#[test]
fn restore_any_node_checkpoint_and_restore_is_undoable() {
    let (work, _data, store) = setup();
    let root = work.path();
    let before = user_repo_state(root);
    let a = NodeId::from("a");
    let b = NodeId::from("b");
    let cp_a = store
        .snapshot(CheckpointKind::Turn, Some(&a), Some(1), "a1")
        .unwrap();
    write(root, "src/lib.rs", "by a\n");
    let cp_b = store
        .snapshot(CheckpointKind::Turn, Some(&b), Some(1), "b1")
        .unwrap();
    write(root, "src/lib.rs", "by b\n");
    write(root, "new.rs", "new\n");

    assert_eq!(
        store
            .for_node(&b)
            .unwrap()
            .iter()
            .map(|c| c.id)
            .collect::<Vec<_>>(),
        vec![cp_b.id]
    );
    let safety = store.restore(cp_a.id).unwrap();
    assert_eq!(safety.kind, CheckpointKind::Restore);
    assert_eq!(read(root, "src/lib.rs").unwrap(), "fn a() {}\n");
    assert!(read(root, "new.rs").is_none());
    // Restoring the safety checkpoint brings the newer state back.
    store.restore(safety.id).unwrap();
    assert_eq!(read(root, "src/lib.rs").unwrap(), "by b\n");
    assert_eq!(read(root, "new.rs").unwrap(), "new\n");
    assert_eq!(user_repo_state(root), before);
}

#[test]
fn changed_paths_diff_and_partial_restore() {
    let (work, _data, store) = setup();
    let root = work.path();
    let cp = store
        .snapshot(CheckpointKind::Manual, None, None, "start")
        .unwrap();
    write(root, "src/lib.rs", "changed\n");
    write(root, "scratch.txt", "tmp\n");
    let mut changed = store.changed_since(cp.id).unwrap();
    changed.sort_by(|x, y| x.path.cmp(&y.path));
    assert_eq!(
        changed,
        vec![
            ChangedPath {
                status: 'A',
                path: "scratch.txt".into()
            },
            ChangedPath {
                status: 'M',
                path: "src/lib.rs".into()
            },
        ]
    );
    let diff = store.diff_since(cp.id).unwrap();
    assert!(diff.contains("+changed"), "{diff}");
    assert_eq!(
        store.file_text(cp.id, "src/lib.rs").unwrap().as_deref(),
        Some("fn a() {}\n")
    );
    assert_eq!(store.file_text(cp.id, "scratch.txt").unwrap(), None);
    store
        .restore_paths(cp.id, &["src/lib.rs", "scratch.txt"])
        .unwrap();
    assert_eq!(read(root, "src/lib.rs").unwrap(), "fn a() {}\n");
    assert!(read(root, "scratch.txt").is_none());
}

#[test]
fn works_outside_a_git_repository() {
    let work = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    write(work.path(), "a.txt", "1\n");
    let store = CheckpointStore::open(data.path(), work.path()).unwrap();
    let cp = store
        .snapshot(CheckpointKind::Turn, None, Some(1), "t1")
        .unwrap();
    write(work.path(), "a.txt", "2\n");
    store.restore(cp.id).unwrap();
    assert_eq!(read(work.path(), "a.txt").unwrap(), "1\n");
    assert!(!work.path().join(".git").exists());
}
