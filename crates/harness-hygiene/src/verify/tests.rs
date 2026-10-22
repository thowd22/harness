use super::*;
use crate::events::RecordingSink;
use harness_tools::local::LocalExec;

fn ini(command: Option<&str>) -> harness_config::ini::config::Verify {
    harness_config::ini::config::Verify {
        command: command.map(str::to_string),
        baseline: true,
        max_fix_attempts: 3,
    }
}

fn set(v: &[&str]) -> BTreeSet<String> {
    v.iter().map(|s| s.to_string()).collect()
}

#[test]
fn command_resolution_order() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("Cargo.toml"), "").unwrap();
    let r = |step, agent, ini_cmd| VerifySpec::resolve(step, agent, &ini(ini_cmd), dir.path());
    let s = r(
        Some(("prove", "cargo test x")),
        Some(("coder", "cargo test -p a")),
        Some("make"),
    )
    .unwrap();
    assert_eq!(
        (s.command.as_str(), &s.source),
        ("cargo test x", &VerifySource::WorkflowStep("prove".into()))
    );
    let s = r(
        Some(("prove", " ")),
        Some(("coder", "cargo test -p a")),
        Some("make"),
    )
    .unwrap();
    assert_eq!(s.source, VerifySource::Agent("coder".into()));
    let s = r(None, None, Some("make check")).unwrap();
    assert_eq!(
        (s.command.as_str(), &s.source),
        ("make check", &VerifySource::Ini)
    );
    let s = r(None, None, None).unwrap();
    assert_eq!(s.command, "cargo test --workspace");
    assert_eq!(s.source, VerifySource::Detected("Cargo.toml".into()));
    let empty = tempfile::tempdir().unwrap();
    assert!(VerifySpec::resolve(None, None, &ini(None), empty.path()).is_none());
}

#[test]
fn parses_libtest_text() {
    let out = "running 3 tests\ntest a::one ... ok\ntest a::two ... FAILED\ntest b::three ... FAILED\n\nfailures:\n\ntest result: FAILED. 1 passed; 2 failed\n";
    let r = parse_test_output(out).unwrap();
    assert_eq!(r.failing, set(&["a::two", "b::three"]));
    assert_eq!(r.passed, 1);
    assert_eq!(r.formats, vec![TestFormat::Libtest]);
}

#[test]
fn parses_structured_json_in_preference() {
    let out = r#"{ "type": "suite", "event": "started", "test_count": 2 }
{ "type": "test", "event": "ok", "name": "a::one" }
{ "type": "test", "event": "failed", "name": "a::two", "stdout": "test a::fake ... FAILED" }
{"Action":"fail","Package":"example.com/p","Test":"TestX"}
{"Action":"pass","Package":"example.com/p","Test":"TestY"}
test a::fake ... FAILED
"#;
    let r = parse_test_output(out).unwrap();
    assert_eq!(r.failing, set(&["a::two", "example.com/p/TestX"]));
    assert_eq!(r.passed, 2);
    assert!(r.formats.iter().all(|f| f.is_structured()));
}

#[test]
fn parses_pytest_summary() {
    let out = "tests/test_a.py .F\n=== short test summary info ===\nFAILED tests/test_a.py::test_two - AssertionError: x\nERROR tests/test_b.py::test_c - fixture\n===== 1 failed, 4 passed in 0.12s =====\n";
    let r = parse_test_output(out).unwrap();
    assert_eq!(
        r.failing,
        set(&["tests/test_a.py::test_two", "tests/test_b.py::test_c"])
    );
    assert_eq!(r.passed, 4);
    assert!(parse_test_output("error: could not compile\n").is_none());
}

fn run(passed: bool, failing: Option<&[&str]>) -> VerifyRun {
    VerifyRun {
        command: "t".into(),
        exit_code: Some(if passed { 0 } else { 1 }),
        passed,
        timed_out: false,
        duration: Duration::ZERO,
        output: String::new(),
        tests: failing.map(|f| TestReport {
            failing: set(f),
            passed: 0,
            formats: vec![TestFormat::Libtest],
        }),
    }
}

#[test]
fn separates_new_and_pre_existing_failures() {
    let base = run(false, Some(&["old1", "old2"]));
    let after = run(false, Some(&["old1", "new1"]));
    let c = compare(Some(&base), &after);
    assert_eq!(c.status, VerifyStatus::NewFailures);
    assert_eq!(c.newly_failing, set(&["new1"]));
    assert_eq!(c.pre_existing, set(&["old1"]));
    assert_eq!(c.fixed, set(&["old2"]));
    let lines = c.lines();
    assert_eq!(
        lines[0],
        "verify failed: 1 newly failing test(s), 1 pre-existing"
    );
    assert_eq!(lines[1], "  newly failing: new1");

    let c = compare(Some(&base), &run(false, Some(&["old2"])));
    assert_eq!(c.status, VerifyStatus::PreExistingOnly);
    assert!(c.newly_failing.is_empty());

    let c = compare(Some(&base), &run(true, Some(&[])));
    assert_eq!(c.status, VerifyStatus::Passed);
    assert_eq!(c.fixed, set(&["old1", "old2"]));

    // Baseline passed without test output: everything failing now is new.
    let c = compare(Some(&run(true, None)), &after);
    assert_eq!(c.newly_failing, set(&["old1", "new1"]));

    // Nothing to compare against.
    let c = compare(None, &after);
    assert_eq!(c.status, VerifyStatus::Failed);
    assert!(c.newly_failing.is_empty());
    assert_eq!(c.failing, set(&["old1", "new1"]));
    assert_eq!(
        c.lines()[0],
        "verify failed (no baseline to compare against)"
    );
    // Exit status only.
    let c = compare(Some(&run(false, None)), &run(false, None));
    assert_eq!(c.status, VerifyStatus::Failed);
    assert!(c.baseline_failed);
}

/// A fake test suite: a script that fails the tests listed in `failing`.
fn suite(dir: &Path, failing: &[&str]) {
    let mut s = String::from("echo 'running'\n");
    for t in ["t::a", "t::b", "t::c"] {
        if failing.contains(&t) {
            s.push_str(&format!("echo 'test {t} ... FAILED'\n"));
        } else {
            s.push_str(&format!("echo 'test {t} ... ok'\n"));
        }
    }
    s.push_str(if failing.is_empty() {
        "exit 0\n"
    } else {
        "echo 'test result: FAILED' >&2\nexit 101\n"
    });
    std::fs::write(dir.join("suite.sh"), s).unwrap();
}

#[tokio::test]
async fn gate_runs_baseline_before_first_edit_and_reports_new_failures() {
    let dir = tempfile::tempdir().unwrap();
    let sink = Arc::new(RecordingSink::default());
    let runner = VerifyRunner::new(Arc::new(LocalExec::new()), dir.path())
        .with_timeout(Duration::from_secs(30))
        .with_sink(sink.clone());
    let spec = VerifySpec {
        command: "sh suite.sh".into(),
        source: VerifySource::Ini,
    };
    let node = NodeId::from("coder");
    let mut gate = VerifyGate::new(runner, Some(spec), true);

    suite(dir.path(), &["t::a"]);
    let base = gate.before_first_edit(Some(&node)).await.unwrap().unwrap();
    assert!(!base.passed);
    assert_eq!(base.exit_code, Some(101));
    assert_eq!(base.failing(), Some(&set(&["t::a"])));
    // Only once.
    suite(dir.path(), &[]);
    let again = gate.before_first_edit(Some(&node)).await.unwrap().unwrap();
    assert_eq!(again.failing(), Some(&set(&["t::a"])));

    // The agent "edits": t::b now fails too.
    suite(dir.path(), &["t::a", "t::b"]);
    let out = gate.verify(Some(&node)).await.unwrap().unwrap();
    assert_eq!(out.comparison.status, VerifyStatus::NewFailures);
    assert_eq!(out.comparison.newly_failing, set(&["t::b"]));
    assert_eq!(out.comparison.pre_existing, set(&["t::a"]));

    let kinds = sink.kinds();
    assert_eq!(kinds.len(), 4, "baseline + verify, start + finish each");
    assert!(matches!(&kinds[0], EventKind::VerifyStarted { command } if command == "sh suite.sh"));
    assert!(matches!(
        &kinds[3],
        EventKind::VerifyFinished { passed: false, exit_code: Some(101), output: Payload::Text(t), .. } if t.contains("t::b ... FAILED")
    ));
    assert!(
        sink.drafts()
            .iter()
            .all(|d| d.node_id.as_ref() == Some(&node))
    );
}

#[tokio::test]
async fn baseline_can_be_disabled() {
    let dir = tempfile::tempdir().unwrap();
    suite(dir.path(), &[]);
    let runner = VerifyRunner::new(Arc::new(LocalExec::new()), dir.path());
    let spec = VerifySpec {
        command: "sh suite.sh".into(),
        source: VerifySource::Ini,
    };
    let mut gate = VerifyGate::new(runner, Some(spec), false);
    assert!(gate.before_first_edit(None).await.unwrap().is_none());
    let out = gate.verify(None).await.unwrap().unwrap();
    assert_eq!(out.comparison.status, VerifyStatus::Passed);
    assert!(!out.comparison.had_baseline);
}
