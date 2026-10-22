use super::*;
use crate::checkpoints::CheckpointKind;
use crate::ledger::Ledger;
use crate::scope::ScopeSource;

fn write(root: &Path, rel: &str, text: &str) {
    let p = root.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, text).unwrap();
}

fn git(root: &Path, args: &[&str]) {
    let st = Command::new("git")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .args(["-c", "user.name=u", "-c", "user.email=u@x"])
        .args(args)
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        st.status.success(),
        "{}",
        String::from_utf8_lossy(&st.stderr)
    );
}

struct Fixture {
    work: tempfile::TempDir,
    _data: tempfile::TempDir,
    store: CheckpointStore,
    ledger: Ledger,
    node: NodeId,
    cp: u64,
}

/// A repo where a bash command makes one change of every kind.
fn fixture() -> Fixture {
    let work = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    let root = work.path();
    git(root, &["init", "-q"]);
    write(root, "src/lib.rs", "fn a() {}\n");
    write(root, "src/fmt.rs", "fn b() { 1 }\n");
    write(root, "build.rs", "fn main() {}\n");
    git(root, &["add", "."]);
    git(root, &["commit", "-q", "-m", "init"]);
    let store = CheckpointStore::open(data.path(), root).unwrap();
    let node = NodeId::from("fix");
    let cp = store
        .snapshot(CheckpointKind::Turn, Some(&node), Some(1), "t1")
        .unwrap()
        .id;
    let ledger = Ledger::new(root);
    let watch = ledger.begin_command().unwrap();
    write(root, "src/lib.rs", "fn a() { fixed() }\n"); // in scope
    write(root, "src/fmt.rs", "fn b() {\n    1\n}\n"); // whitespace-only
    write(root, "build.rs", "fn main() { println!() }\n"); // out of scope
    write(root, "src/new_test.rs", "#[test] fn t() {}\n"); // new untracked
    write(root, "debug_parse.py", "print(1)\n"); // temp + out of scope
    write(root, "src/out.log", "log\n"); // temp
    ledger
        .end_command(watch, Some(&node), "agent work")
        .unwrap();
    Fixture {
        work,
        _data: data,
        store,
        ledger,
        node,
        cp,
    }
}

fn build(f: &Fixture, use_before: bool) -> HygieneReport {
    let scope = Scope::new(ScopeSource::Agent("coder".into()), ["src/**"]).unwrap();
    let tracked = tracked_files(f.work.path()).unwrap();
    let changes = f.ledger.net_changes(Some(&f.node));
    let before = CheckpointBefore {
        store: &f.store,
        id: f.cp,
    };
    HygieneReport::build(ReportInput {
        root: f.work.path(),
        node_id: Some(&f.node),
        changes: &changes,
        scope: Some(&scope),
        tracked: Some(&tracked),
        before: use_before.then_some(&before as &dyn BeforeContent),
    })
}

fn cats(r: &HygieneReport, path: &str) -> Vec<Category> {
    r.files
        .iter()
        .find(|f| f.path == path)
        .unwrap_or_else(|| panic!("{path} missing"))
        .categories
        .iter()
        .copied()
        .collect()
}

#[test]
fn classifies_every_category() {
    let f = fixture();
    for use_before in [true, false] {
        let r = build(&f, use_before);
        use Category::*;
        assert_eq!(cats(&r, "src/lib.rs"), vec![InScope]);
        assert_eq!(cats(&r, "src/fmt.rs"), vec![InScope, WhitespaceOnly]);
        assert_eq!(cats(&r, "build.rs"), vec![OutOfScope]);
        assert_eq!(cats(&r, "src/new_test.rs"), vec![InScope, NewUntracked]);
        assert_eq!(
            cats(&r, "debug_parse.py"),
            vec![OutOfScope, NewUntracked, TempOrDebug]
        );
        assert_eq!(
            cats(&r, "src/out.log"),
            vec![InScope, NewUntracked, TempOrDebug]
        );
        // 6 files, 4 with problems (fmt, build, debug_parse, out.log).
        assert_eq!(r.score.total, 6);
        assert_eq!(r.score.score, 33);
        assert_eq!(r.score.out_of_scope, 2);
        assert_eq!(r.score.temp_or_debug, 2);
        assert_eq!(r.score.whitespace_only, 1);
        assert_eq!(r.score.new_untracked, 3);
    }
}

#[test]
fn report_lines_for_node_detail() {
    let f = fixture();
    let lines = build(&f, true).lines();
    assert!(
        lines[0].starts_with("Hygiene 33/100: 6 changed, 2 out of scope"),
        "{lines:?}"
    );
    assert_eq!(lines[1], "Scope: src/**");
    assert!(lines.contains(&"! + debug_parse.py  [out of scope, new untracked, temp/debug] (name starts with `debug`)".to_string()), "{lines:?}");
    assert!(
        lines.contains(&"  ~ src/lib.rs  [in scope]".to_string()),
        "{lines:?}"
    );
}

#[test]
fn revert_selected_and_remove_temp_files() {
    let f = fixture();
    let root = f.work.path();
    let r = build(&f, true);
    assert_eq!(r.temp_files(), vec!["debug_parse.py", "src/out.log"]);
    let removed = remove_temp_files(root, &r).unwrap();
    assert_eq!(removed, vec!["debug_parse.py", "src/out.log"]);
    assert!(!root.join("debug_parse.py").exists());
    revert_files(&f.store, f.cp, &["build.rs", "src/new_test.rs"]).unwrap();
    assert_eq!(
        std::fs::read_to_string(root.join("build.rs")).unwrap(),
        "fn main() {}\n"
    );
    assert!(!root.join("src/new_test.rs").exists());
    // Unselected files keep the agent's change.
    assert_eq!(
        std::fs::read_to_string(root.join("src/lib.rs")).unwrap(),
        "fn a() { fixed() }\n"
    );
}

#[test]
fn score_is_recorded_as_metric() {
    let f = fixture();
    let r = build(&f, true);
    let run_dir = tempfile::tempdir().unwrap();
    record_metric(run_dir.path(), &r).unwrap();
    record_metric(run_dir.path(), &r).unwrap();
    let text = std::fs::read_to_string(run_dir.path().join(METRICS_FILE)).unwrap();
    let lines: Vec<serde_json::Value> = text
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0]["name"], SCORE_METRIC);
    assert_eq!(lines[0]["node_id"], "fix");
    assert_eq!(lines[0]["value"], 33.0);
    assert_eq!(lines[0]["counts"]["out_of_scope"], 2);
}

#[test]
fn empty_report_scores_100_and_no_scope_means_in_scope() {
    let r = HygieneReport::build(ReportInput {
        root: Path::new("/nonexistent"),
        node_id: None,
        changes: &[],
        scope: None,
        tracked: None,
        before: None,
    });
    assert_eq!(r.score.score, 100);
    assert!(tracked_files(tempfile::tempdir().unwrap().path()).is_none());
}

#[test]
fn temp_heuristics() {
    for p in [
        "tmp/x.rs",
        "a/scratch/b",
        "x.orig",
        "notes.bak",
        "debug.py",
        "out.txt",
        "tmp_check.sh",
        "f~",
    ] {
        assert!(temp_or_debug(p).is_some(), "{p}");
    }
    for p in [
        "src/lib.rs",
        "tests/parse.rs",
        "README.md",
        "src/template.rs",
        "outline.md",
    ] {
        assert!(temp_or_debug(p).is_none(), "{p}");
    }
}
