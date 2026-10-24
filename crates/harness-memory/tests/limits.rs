//! TASK-155: the memory tool states its limits up front, accepts several
//! bullets per write, explains how to split an over-long memory, and answers
//! "index this codebase into memory" with a compact overview note instead of
//! a stream of rejected writes.

use std::path::Path;
use std::process::Command;
use std::sync::Arc;

use harness_config::agents::MemoryAccess;
use harness_events::redact::Redactor;
use harness_memory::drive::{Drive, Note, NotePath};
use harness_memory::extract::PROMPT_SECTION;
use harness_memory::sync::SessionCheckout;
use harness_memory::tool::guard::{
    MAX_BULLET_CHARS, MAX_OVERVIEW_BULLET_CHARS, MAX_OVERVIEW_BULLETS,
};
use harness_memory::tool::{
    DESCRIPTION, MAX_BULLETS_PER_WRITE, MemoryArgs, MemoryOutcome, MemoryTool, MemoryToolConfig,
    spec,
};
use harness_tools::tool::{ToolContext, ToolError, ToolErrorKind, ToolOutput};
use serde_json::json;
use time::macros::date;

const KEY: &str = "big-repo-0123abcd";

fn drive() -> (tempfile::TempDir, Drive) {
    let tmp = tempfile::tempdir().unwrap();
    let d = Drive::open_or_init(tmp.path().join("memory")).unwrap();
    (tmp, d)
}

fn tool(d: &Drive) -> MemoryTool {
    let co = Arc::new(SessionCheckout::open(d, "s1").unwrap());
    MemoryTool::new(
        co,
        MemoryToolConfig {
            access: MemoryAccess::ReadWrite,
            run_id: "run-s1".into(),
            project_key: Some(KEY.into()),
            redactor: Arc::new(Redactor::new(["s3cr3t-value-xyz"])),
            date: Some(date!(2026 - 10 - 23)),
        },
    )
}

fn write_many(t: &MemoryTool, path: &str, bullets: &[String]) -> Result<MemoryOutcome, ToolError> {
    t.execute(MemoryArgs::Write {
        path: path.into(),
        text: String::new(),
        bullets: bullets.to_vec(),
        title: None,
    })
}

fn write_one(t: &MemoryTool, path: &str, text: &str) -> Result<MemoryOutcome, ToolError> {
    t.execute(MemoryArgs::Write {
        path: path.into(),
        text: text.into(),
        bullets: Vec::new(),
        title: None,
    })
}

/// Number of commits on the drive's `main`.
fn commits(d: &Drive) -> usize {
    let mut cmd = Command::new("git");
    for (k, _) in std::env::vars_os() {
        if k.to_string_lossy().starts_with("GIT_") {
            cmd.env_remove(&k);
        }
    }
    let out = cmd
        .args(["rev-list", "--count", "main"])
        .current_dir(d.root())
        .output()
        .unwrap();
    assert!(out.status.success());
    String::from_utf8_lossy(&out.stdout).trim().parse().unwrap()
}

fn note(d: &Drive, path: &str) -> Note {
    let text = std::fs::read_to_string(d.root().join(NotePath::parse(path).unwrap().as_str()))
        .unwrap_or_default();
    Note::parse(&text)
}

/// A sentence of about `len` characters that passes the content rules.
fn lesson(seed: &str, len: usize) -> String {
    let mut s = format!("{seed}: ");
    while s.chars().count() < len {
        s.push_str("rebuild the fixtures before running the parser suite ");
    }
    // Exactly `len` characters, never ending in whitespace (which is trimmed).
    let mut s: String = s.chars().take(len - 1).collect();
    s.push('.');
    s
}

#[test]
fn description_and_schema_state_the_limits() {
    let spec = spec(MemoryAccess::ReadWrite).unwrap();
    let d = spec.description.to_lowercase();
    for needle in [
        "400 characters",
        "1000 in project/overview",
        "bullets",
        "up to 10",
    ] {
        assert!(d.contains(needle), "description lacks {needle:?}");
    }
    let props = &spec.input_schema["properties"];
    assert_eq!(props["text"]["maxLength"], json!(MAX_OVERVIEW_BULLET_CHARS));
    assert!(
        props["text"]["description"]
            .as_str()
            .unwrap()
            .contains("400")
    );
    assert_eq!(props["bullets"]["type"], json!("array"));
    assert_eq!(props["bullets"]["maxItems"], json!(MAX_BULLETS_PER_WRITE));
    assert_eq!(
        props["bullets"]["items"]["maxLength"],
        json!(MAX_OVERVIEW_BULLET_CHARS)
    );
    // Read-only agents see neither.
    let ro = spec_props(MemoryAccess::Read);
    assert!(ro.get("bullets").is_none() && ro.get("text").is_none());
    assert_eq!(MAX_BULLET_CHARS, 400);
}

fn spec_props(access: MemoryAccess) -> serde_json::Value {
    spec(access).unwrap().input_schema["properties"].clone()
}

#[test]
fn an_overlong_bullet_is_rejected_with_the_limit_and_how_to_split() {
    let (_tmp, d) = drive();
    let t = tool(&d);
    let before = commits(&d);
    let long = lesson("Gotcha", 612);
    let e = write_one(&t, "project/testing", &long).unwrap_err();
    assert_eq!(e.kind, ToolErrorKind::InvalidArgs);
    let m = &e.message;
    assert!(m.starts_with("memory not saved:"), "{m}");
    assert!(m.contains("612 characters"), "{m}");
    assert!(m.contains("limit is 400 per bullet"), "{m}");
    assert!(m.contains("Split it into several"), "{m}");
    assert!(m.contains("`bullets: [...]`"), "{m}");
    assert_eq!(commits(&d), before, "a rejected write makes no commit");

    // Exactly at the limit is fine.
    write_one(&t, "project/testing", &lesson("Gotcha", MAX_BULLET_CHARS)).unwrap();
}

#[test]
fn one_write_saves_several_bullets_in_one_commit_and_reports_rejects() {
    let (_tmp, d) = drive();
    let t = tool(&d);
    let before = commits(&d);
    let bullets = vec![
        "Run `just test-int` for integration tests; plain `cargo test` skips them.".to_owned(),
        "docker compose needs --wait or healthchecks race the migrations.".to_owned(),
        lesson("Too long", 450),
        "PR #42 is waiting on review".to_owned(),
        // A duplicate inside the batch is saved once.
        "docker compose needs --wait or healthchecks race the migrations.".to_owned(),
    ];
    let out = write_many(&t, "project/testing", &bullets).unwrap();
    assert_eq!(commits(&d), before + 1, "one commit for the whole write");
    let change = out.change.expect("a change");
    assert_eq!(change.bullets.len(), 2);
    assert!(out.output.contains("Saved 2 bullet(s)"), "{}", out.output);
    assert!(
        out.output
            .contains("bullet 3: the memory is 450 characters"),
        "{}",
        out.output
    );
    assert!(
        out.output.contains("bullet 4: this looks like task state"),
        "{}",
        out.output
    );
    assert!(out.output.contains("Resend only those"), "{}", out.output);
    assert_eq!(
        note(&d, &format!("projects/{KEY}/testing"))
            .bullets()
            .count(),
        2
    );

    // Re-sending the same bullets changes nothing.
    let again = write_many(&t, "project/testing", &bullets[..2]).unwrap();
    assert!(again.change.is_none());
    assert!(
        again.output.contains("Already in memory"),
        "{}",
        again.output
    );
    assert_eq!(commits(&d), before + 1);
}

#[test]
fn a_write_whose_bullets_all_fail_names_each_problem() {
    let (_tmp, d) = drive();
    let t = tool(&d);
    let before = commits(&d);
    let e = write_many(
        &t,
        "project/testing",
        &[lesson("A", 401), "TODO: finish the parser".to_owned()],
    )
    .unwrap_err();
    assert_eq!(e.kind, ToolErrorKind::InvalidArgs);
    assert!(
        e.message.contains("none of the 2 bullets passed"),
        "{}",
        e.message
    );
    assert!(
        e.message.contains("bullet 1: the memory is 401 characters"),
        "{}",
        e.message
    );
    assert!(
        e.message.contains("bullet 2: this looks like task state"),
        "{}",
        e.message
    );
    assert_eq!(commits(&d), before);

    let empty = write_many(&t, "project/testing", &[]).unwrap_err();
    assert!(empty.message.contains("`bullets`"), "{}", empty.message);

    let too_many: Vec<String> = (0..=MAX_BULLETS_PER_WRITE)
        .map(|i| format!("Lesson {i}: prefer the vendored protoc."))
        .collect();
    let e = write_many(&t, "project/testing", &too_many).unwrap_err();
    assert!(e.message.contains("at most 10 bullets"), "{}", e.message);
    assert_eq!(commits(&d), before);
}

#[tokio::test]
async fn the_model_can_send_bullets_as_json() {
    let (_tmp, d) = drive();
    let t = tool(&d);
    let ctx = ToolContext::new("call-1");
    let out = t
        .call(
            &ctx,
            json!({
                "action": "write",
                "path": "tools/cargo",
                "bullets": ["Use cargo nextest; plain cargo test times out in CI.", "Set CARGO_INCREMENTAL=0 in CI to keep caches small."],
            }),
        )
        .await
        .unwrap();
    let ToolOutput::Text(text) = out else {
        panic!("text output")
    };
    assert!(
        text.contains("Saved 2 bullet(s) to tools/cargo.md"),
        "{text}"
    );
    assert_eq!(note(&d, "tools/cargo").bullets().count(), 2);
}

/// The documented answer to "index this codebase and store it in memory":
/// one compact `project/overview` note (longer bullets, capped count) plus
/// non-obvious gotchas, sent as batched writes that do not bounce.
#[test]
fn indexing_a_codebase_fits_the_overview_note() {
    let (_tmp, d) = drive();
    let t = tool(&d);
    let before = commits(&d);

    // Guidance is in front of the model before it starts.
    for text in [DESCRIPTION, PROMPT_SECTION] {
        assert!(text.contains("project/overview"), "{text}");
        assert!(text.contains("index or learn the codebase"), "{text}");
        assert!(text.contains("not a file or module map"), "{text}");
        assert!(text.contains("tell the user"), "{text}");
    }

    // Overview bullets may be longer than ordinary ones.
    let overview: Vec<String> = (0..6)
        .map(|i| lesson(&format!("Overview part {i}"), 900))
        .collect();
    let out = write_many(&t, "project/overview", &overview).unwrap();
    assert!(out.output.contains("Saved 6 bullet(s)"), "{}", out.output);
    let gotchas = vec![
        "Snapshot tests need TZ=UTC or the dates shift.".to_owned(),
        "The xtask build must run before cargo test; it generates the proto stubs.".to_owned(),
    ];
    write_many(&t, "project/gotchas", &gotchas).unwrap();
    assert_eq!(commits(&d), before + 2, "two writes, no rejected retries");

    // Only the overview note gets the higher limit.
    let e = write_one(&t, "project/architecture", &overview[0]).unwrap_err();
    assert!(
        e.message.contains("limit is 400 per bullet"),
        "{}",
        e.message
    );
    let e = write_one(
        &t,
        "project/overview",
        &lesson("Huge", MAX_OVERVIEW_BULLET_CHARS + 1),
    )
    .unwrap_err();
    assert!(
        e.message.contains("limit is 1000 per bullet"),
        "{}",
        e.message
    );

    // The overview stays compact: at most MAX_OVERVIEW_BULLETS bullets.
    let more: Vec<String> = (0..5)
        .map(|i| format!("Overview extra {i}: the CLI and daemon share one config loader."))
        .collect();
    let e = write_many(&t, "project/overview", &more).unwrap_err();
    assert!(
        e.message.contains(&format!(
            "holds at most {MAX_OVERVIEW_BULLETS} bullets and already has 6"
        )),
        "{}",
        e.message
    );
    assert!(e.message.contains("forget or merge"), "{}", e.message);
    write_many(&t, "project/overview", &more[..4]).unwrap();
    // Re-sending existing bullets to a full overview is not refused.
    let again = write_many(&t, "project/overview", &overview[..2]).unwrap();
    assert!(again.change.is_none());
    let path = format!("projects/{KEY}/overview");
    assert_eq!(note(&d, &path).bullets().count(), MAX_OVERVIEW_BULLETS);
    assert!(Path::new(&d.root().join(format!("{path}.md"))).exists());
}

/// TASK-158: the end-of-session extractor screens proposals with the same
/// per-path limits as the tool, so an overview bullet it proposes survives
/// screening and is written; other paths keep the 400-character limit.
#[test]
fn extractor_overview_proposals_use_the_tool_limits() {
    use harness_memory::extract::{Candidate, screen, write_proposals};
    use harness_memory::tool::guard::limit_for_path;

    for p in ["project/overview", &format!("projects/{KEY}/overview.md")] {
        assert_eq!(limit_for_path(p), MAX_OVERVIEW_BULLET_CHARS, "{p}");
    }
    assert_eq!(limit_for_path("project/testing"), MAX_BULLET_CHARS);

    let cand = |path: &str, text: String| Candidate {
        path: path.into(),
        text,
        reason: "worth keeping".into(),
    };
    let r = Redactor::new(Vec::<String>::new());
    let ex = screen(
        vec![
            cand(
                "project/overview",
                lesson("Overview", MAX_OVERVIEW_BULLET_CHARS),
            ),
            cand(
                "project/overview",
                lesson("Too long", MAX_OVERVIEW_BULLET_CHARS + 1),
            ),
            cand("project/testing", lesson("Gotcha", MAX_BULLET_CHARS)),
            cand(
                "project/testing",
                lesson("Long gotcha", MAX_BULLET_CHARS + 1),
            ),
        ],
        &r,
        5,
    );
    assert_eq!(ex.proposals.len(), 2, "{:?}", ex.rejected);
    assert_eq!(ex.rejected.len(), 2);
    assert!(ex.rejected[0].1.message.contains("the limit is 1000"));
    assert!(ex.rejected[1].1.message.contains("the limit is 400"));

    let (_tmp, d) = drive();
    let t = tool(&d);
    for res in write_proposals(&t, &ex.proposals) {
        res.unwrap();
    }
    let overview = note(&d, &format!("projects/{KEY}/overview"));
    assert!(
        overview
            .bullets()
            .any(|b| b.text.chars().count() == MAX_OVERVIEW_BULLET_CHARS),
        "overview bullet missing"
    );
}
