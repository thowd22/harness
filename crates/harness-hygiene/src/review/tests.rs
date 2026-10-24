use super::*;
use harness_providers::fake::{FakeProvider, FakeTurn};

fn verdict_json(verdict: &str, failing: &[&str], blocker: bool) -> String {
    let criteria: serde_json::Map<String, serde_json::Value> = RUBRIC
        .iter()
        .map(|c| {
            (
                c.id.to_string(),
                serde_json::json!({"ok": !failing.contains(&c.id), "notes": "n"}),
            )
        })
        .collect();
    let findings: Vec<serde_json::Value> = failing
        .iter()
        .map(|id| {
            serde_json::json!({"criterion": id, "severity": if blocker {"blocker"} else {"minor"},
                "file": "src/a.rs", "line": 3, "message": "remove it"})
        })
        .collect();
    serde_json::json!({"verdict": verdict, "summary": "s", "criteria": criteria, "findings": findings})
        .to_string()
}

fn input() -> ReviewInput {
    ReviewInput::new(
        "Fix the off-by-one in parse_header",
        Some(vec!["src/**".into()]),
        "--- a/src/a.rs\n+++ b/src/a.rs\n@@ -1 +1 @@\n-x\n+y\n",
    )
}

#[tokio::test]
async fn reviewer_sees_only_task_scope_and_diff() {
    let fake = FakeProvider::with_turns([FakeTurn::text(verdict_json("approve", &[], false))]);
    let v = review(&fake, "opus", &input(), &CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(v.verdict, Decision::Approve);
    let reqs = fake.requests();
    assert_eq!(reqs.len(), 1);
    let req = &reqs[0];
    assert_eq!(req.model, "opus");
    assert!(req.tools.is_empty(), "reviewer gets no tools");
    assert_eq!(
        req.messages.len(),
        1,
        "a single user message, no transcript"
    );
    let text = req.messages[0].text();
    assert!(text.contains("# Task\nFix the off-by-one in parse_header"));
    assert!(text.contains("# Scope\n- `src/**`"));
    assert!(text.contains("```diff\n--- a/src/a.rs"));
}

#[test]
fn rubric_is_in_the_system_prompt_and_lint_clean() {
    let sys = system_prompt();
    for id in [
        "correctness",
        "unrequested_changes",
        "over_engineering",
        "dead_or_compat_code",
        "test_adequacy",
    ] {
        assert!(sys.contains(&format!("`{id}`")), "{id}");
    }
    let src = harness_config::lint::LintSource {
        kind: harness_config::lint::SourceKind::AgentPrompt,
        label: "reviewer".into(),
        file: "reviewer".into(),
        text: sys,
        file_text: None,
    };
    assert!(harness_config::lint::blanket_prohibitions(&src).is_empty());
}

#[test]
fn verdict_parsing_and_outputs() {
    let text = format!(
        "Here you go:\n```json\n{}\n```",
        verdict_json("request_changes", &["over_engineering"], false)
    );
    let v = ReviewVerdict::parse(&text).unwrap();
    assert_eq!(v.verdict, Decision::RequestChanges);
    assert_eq!(v.findings[0].criterion, "over_engineering");
    assert_eq!(v.findings[0].line, Some(3));
    let out = v.outputs();
    assert_eq!(out["verdict"], "request_changes");
    assert_eq!(out["criteria"]["over_engineering"]["ok"], false);

    // An approval with a failed criterion is not an approval.
    let v = ReviewVerdict::parse(&verdict_json("approve", &["test_adequacy"], true)).unwrap();
    assert_eq!(v.verdict, Decision::RequestChanges);
    assert_eq!(v.outputs()["verdict"], "request_changes");

    // Every criterion must be graded.
    let partial = r#"{"verdict":"approve","criteria":{"correctness":{"ok":true}}}"#;
    assert!(
        matches!(ReviewVerdict::parse(partial), Err(ReviewError::Invalid(m)) if m.contains("unrequested_changes"))
    );
    assert!(ReviewVerdict::parse("looks good to me").is_err());
}

#[tokio::test]
async fn invalid_answer_gets_one_repair_attempt() {
    let fake = FakeProvider::with_turns([
        FakeTurn::text("LGTM!"),
        FakeTurn::text(verdict_json("approve", &[], false)),
    ]);
    let v = review(&fake, "m", &input(), &CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(v.verdict, Decision::Approve);
    let reqs = fake.requests();
    assert_eq!(reqs.len(), 2);
    // The repair turn only adds the reviewer's own answer and a format reminder.
    assert_eq!(reqs[1].messages.len(), 3);
    assert_eq!(reqs[1].messages[1].text(), "LGTM!");
}

#[test]
fn input_from_ledger_changes() {
    use harness_events::schema::FileChangeKind;
    let changes = vec![
        NetChange {
            path: "src/a.rs".into(),
            kind: FileChangeKind::Modified,
            sources: vec![],
            before_hash: None,
            after_hash: None,
            diffs: vec!["--- a/src/a.rs\n+++ b/src/a.rs\n".into()],
            generated: false,
        },
        NetChange {
            path: "old.rs".into(),
            kind: FileChangeKind::Deleted,
            sources: vec![],
            before_hash: None,
            after_hash: None,
            diffs: vec![],
            generated: false,
        },
    ];
    let i = ReviewInput::from_ledger("t", None, &changes);
    assert!(i.diff.contains("--- a/src/a.rs"));
    assert!(i.diff.contains("# Deleted old.rs (no diff recorded)"));
    assert!(i.render().contains("(no scope declared)"));
    let big = ReviewInput::new("t", None, "x".repeat(MAX_DIFF_BYTES + 10));
    assert!(big.render().contains("The diff was cut"));
}
