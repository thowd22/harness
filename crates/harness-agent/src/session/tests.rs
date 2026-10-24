use super::*;
use serde_json::json;
use time::{OffsetDateTime, UtcOffset};

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
    let line = list[1].listing_line(UtcOffset::UTC, OffsetDateTime::now_utc());
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

fn conv(turns: &[(&str, &[(&str, &str)])]) -> Conversation {
    let mut c = Conversation::new(None);
    for (i, (prompt, tools)) in turns.iter().enumerate() {
        c.push_user(prompt.to_string());
        let mut content = vec![ContentBlock::text("on it")];
        for (j, (tool, path)) in tools.iter().enumerate() {
            content.push(ContentBlock::tool_use(
                format!("{i}-{j}"),
                *tool,
                json!({ "path": path }),
            ));
        }
        let has_tools = !tools.is_empty();
        c.messages.push(Message {
            role: Role::Assistant,
            content,
        });
        if has_tools {
            c.messages.push(Message {
                role: Role::User,
                content: tools
                    .iter()
                    .enumerate()
                    .map(|(j, _)| ContentBlock::tool_result(format!("{i}-{j}"), "ok", false))
                    .collect(),
            });
            c.messages.push(Message {
                role: Role::Assistant,
                content: vec![ContentBlock::text("done")],
            });
        }
    }
    c
}

#[test]
fn derived_session_names_are_readable() {
    let t0 = time::macros::datetime!(2026-10-23 14:32 UTC);
    for (prompt, want) in [
        ("hi there", "Hi there"),
        (
            "Fix flaky login test in the auth module when CI is slow",
            "Fix flaky login test",
        ),
    ] {
        let mut meta = SessionMeta::new("/proj", "coder", "fake/m", "");
        meta.refresh_from(&conv(&[(prompt, &[])]), t0);
        assert_eq!(meta.name, want, "{prompt:?}");
        assert_eq!(meta.display_name(), want, "{prompt:?}");
    }
}

#[test]
fn names_and_descriptions_are_derived_from_the_conversation() {
    // The TUI starts a session before the first prompt is known.
    let mut meta = SessionMeta::new("/proj", "coder", "fake/m", "");
    assert_eq!(meta.title, UNTITLED);
    assert_eq!(meta.display_name(), names::UNNAMED_SESSION);
    assert_eq!(meta.display_description(), "");

    let t0 = time::macros::datetime!(2026-10-23 14:32 UTC);
    let c = conv(&[(
        "Please fix the header parser so it keeps colons",
        &[("read", "src/parse.rs"), ("edit", "src/parse.rs")],
    )]);
    meta.refresh_from(&c, t0);
    assert_eq!(
        meta.title,
        "Please fix the header parser so it keeps colons"
    );
    assert_eq!(meta.name, "Fix header parser");
    assert_eq!(
        meta.description,
        "Please fix the header parser so it keeps colons · edited src/parse.rs"
    );
    // Prompt, reply, reply after the tool results; tool results are not
    // messages.
    assert_eq!(meta.messages, 3);
    assert_eq!(meta.last_message_at, Some(t0));

    // A later turn: the name stays, the description follows the latest
    // request and every edited file.
    let t1 = t0 + time::Duration::minutes(5);
    let c = conv(&[
        (
            "Please fix the header parser so it keeps colons",
            &[("edit", "src/parse.rs")],
        ),
        (
            "now add tests for quoted values",
            &[
                ("write", "tests/parse.rs"),
                ("edit", "/proj/src/lib.rs"),
                ("write", "docs/parse.md"),
                ("write", "CHANGELOG.md"),
            ],
        ),
    ]);
    meta.refresh_from(&c, t1);
    assert_eq!(meta.name, "Fix header parser");
    assert_eq!(
        meta.description,
        "Please fix the header parser so it keeps colons → now add tests… · edited src/parse.rs, tests/parse.rs, src/lib.rs +2",
        "the requests are cut first so the edited files stay visible"
    );
    assert!(meta.description.chars().count() <= DESCRIPTION_MAX_CHARS);
    assert_eq!(meta.messages, 6);
    assert_eq!(meta.last_message_at, Some(t1));

    // Nothing new: the last-message time does not move.
    meta.refresh_from(&c, t1 + time::Duration::hours(1));
    assert_eq!(meta.last_message_at, Some(t1));
}

#[test]
fn descriptions_are_one_line_and_cut() {
    let long = "x".repeat(40);
    let prompt = format!("{long} {long} {long} {long}\nsecond line");
    let c = conv(&[(prompt.as_str(), &[])]);
    let d = describe(&c, Path::new("/p"), UNTITLED);
    assert!(
        !d.contains('\n') && d.chars().count() <= DESCRIPTION_MAX_CHARS,
        "{d}"
    );
}

#[test]
fn renamed_sessions_keep_their_name() {
    let dir = tempfile::tempdir().unwrap();
    let store = SessionStore::new(dir.path());
    let mut h =
        SessionHandle::start(store.clone(), SessionMeta::new("/p", "coder", "fake/m", "")).unwrap();
    assert!(matches!(h.rename("   "), Err(SessionError::EmptyName)));
    h.rename("  Parser   work ").unwrap();
    assert_eq!(h.meta().name, "Parser work");
    assert!(h.meta().renamed);
    // Turns and summaries never overwrite a chosen name.
    let mut meta = h.meta().clone();
    meta.refresh_from(&conv(&[("fix the bug", &[])]), OffsetDateTime::now_utc());
    assert_eq!(meta.name, "Parser work");
    h.apply_summary(&SummaryUpdate {
        name: Some("Model name".into()),
        description: Some("Fixes the bug".into()),
        tokens: 30,
        cost_usd: Some(0.001),
        cost_unknown: false,
    })
    .unwrap();
    assert_eq!(h.meta().name, "Parser work");
    assert_eq!(h.meta().description, "Fixes the bug");
    let saved = store.load(&h.meta().id).unwrap();
    assert_eq!(saved.name, "Parser work");
    // The summary call's spend counts towards the session.
    assert_eq!((saved.tokens, saved.cost_usd), (30, 0.001));
    // A reply without a description keeps the current one.
    let saved = store
        .update(&saved.id, |m| m.apply_summary(&SummaryUpdate::default()))
        .unwrap();
    assert_eq!(saved.description, "Fixes the bug");
    // The store renames by any query.
    let r = store.rename("parser work", "Header parser").unwrap();
    assert_eq!(r.name, "Header parser");
    assert_eq!(store.find("header parser").unwrap().id, r.id);
    assert_eq!(r.display_name(), "Header parser");
}

fn saved(store: &SessionStore, name: &str, at: OffsetDateTime) -> SessionMeta {
    let mut m = SessionMeta::new("/p", "coder", "fake/m", name);
    m.last_message_at = Some(at);
    store.save(&m).unwrap();
    m
}

#[test]
fn sessions_are_ordered_by_last_message() {
    let dir = tempfile::tempdir().unwrap();
    let store = SessionStore::new(dir.path());
    let t = time::macros::datetime!(2026-10-23 12:00 UTC);
    let old = saved(&store, "old work", t - time::Duration::days(3));
    let new = saved(&store, "new work", t);
    let mid = saved(&store, "mid work", t - time::Duration::hours(2));
    // Resuming touches `updated_at` without a message: order is unchanged.
    let mut touched = old.clone();
    touched.updated_at = t + time::Duration::hours(1);
    store.save(&touched).unwrap();
    // A legacy session without `last_message_at` sorts by its last save.
    let mut legacy = SessionMeta::new("/p", "coder", "fake/m", "legacy work");
    legacy.updated_at = t - time::Duration::hours(1);
    store.save(&legacy).unwrap();
    let ids: Vec<String> = store.list().unwrap().into_iter().map(|m| m.id).collect();
    assert_eq!(ids, [new.id, legacy.id, mid.id, old.id]);
}

#[test]
fn find_resolves_last_ids_tags_and_names() {
    let dir = tempfile::tempdir().unwrap();
    let store = SessionStore::new(dir.path());
    let t = time::macros::datetime!(2026-10-23 12:00 UTC);
    let parser = saved(
        &store,
        "fix the header parser",
        t - time::Duration::hours(1),
    );
    let docs = saved(&store, "write the docs", t);
    let twin_a = saved(&store, "deploy", t - time::Duration::hours(3));
    let twin_b = saved(&store, "deploy", t - time::Duration::hours(4));

    assert_eq!(store.find("last").unwrap().id, docs.id);
    assert_eq!(store.find(&parser.id).unwrap().id, parser.id);
    assert_eq!(store.find("Fix header parser").unwrap().id, parser.id);
    assert_eq!(store.find("FIX HEADER PARSER").unwrap().id, parser.id);
    assert_eq!(
        store.find("write").unwrap().id,
        docs.id,
        "unique name prefix"
    );
    assert_eq!(store.find(&parser.tag()).unwrap().id, parser.id);
    assert_eq!(
        store.find(parser.tag().trim_start_matches('#')).unwrap().id,
        parser.id
    );
    assert!(matches!(
        store.find("deploy"),
        Err(SessionError::Ambiguous { count: 2, .. })
    ));
    assert_eq!(store.find(&twin_b.tag()).unwrap().id, twin_b.id);
    assert_eq!(
        store.find(&twin_a.id[..twin_a.id.len() - 2]).unwrap().id,
        twin_a.id
    );
    assert!(matches!(
        store.find("nothing like it"),
        Err(SessionError::NotFound(_))
    ));
    assert!(matches!(store.find(""), Err(SessionError::NotFound(_))));
}

#[test]
fn listing_lines_show_name_time_project_model_messages_cost_and_description() {
    let now = time::macros::datetime!(2026-10-23 18:00 UTC);
    let mut m = SessionMeta::new(
        "/home/me/proj",
        "coder",
        "anthropic/claude-x",
        "fix the parser bug",
    );
    m.last_message_at = Some(time::macros::datetime!(2026-10-23 14:32 UTC));
    m.messages = 12;
    m.cost_usd = 0.0412;
    let line = m.listing_line(UtcOffset::UTC, now);
    for part in [
        "Fix parser bug",
        "14:32 today",
        "/home/me/proj",
        "anthropic/claude-x",
        "12 msgs",
        "$0.0412",
        &m.tag(),
        "fix the parser bug",
    ] {
        assert!(line.contains(part), "{part:?} missing: {line}");
    }
    assert!(line.starts_with("Fix parser bug"), "{line}");
    let east = UtcOffset::from_hms(10, 0, 0).unwrap();
    assert!(m.listing_line(east, now).contains("00:32 today"));
    m.last_message_at = Some(time::macros::datetime!(2026-10-22 09:10 UTC));
    assert!(
        m.listing_line(UtcOffset::UTC, now)
            .contains("yesterday 09:10")
    );
    m.messages = 1;
    m.cost_unknown = true;
    m.cost_usd = 0.0;
    let line = m.listing_line(UtcOffset::UTC, now);
    assert!(
        line.contains("1 msg ") && line.contains("unknown"),
        "{line}"
    );
}

#[test]
fn sessions_saved_before_names_still_load() {
    let dir = tempfile::tempdir().unwrap();
    let store = SessionStore::new(dir.path());
    let id = "0199f1a2-3b4c-7d5e-8f60-123456abcdef";
    std::fs::create_dir_all(dir.path().join(id)).unwrap();
    std::fs::write(
        dir.path().join(id).join(SESSION_FILE),
        format!(
            r#"{{"id":"{id}","title":"fix the parser bug","project":"/p","created_at":"2026-10-20T10:00:00Z","updated_at":"2026-10-21T16:05:00Z","agent":"coder","model":"fake/m","cost_usd":0.0,"tokens":0,"runs":[]}}"#
        ),
    )
    .unwrap();
    let m = store.find("fix parser bug").unwrap();
    assert_eq!(m.display_name(), "Fix parser bug");
    assert_eq!(m.display_description(), "fix the parser bug");
    assert_eq!(
        m.last_activity(),
        time::macros::datetime!(2026-10-21 16:05 UTC)
    );
    assert_eq!(m.messages, 0);
}

#[test]
fn summary_replies_parse_and_requests_stay_small() {
    assert_eq!(
        parse_summary(
            "name: Header parser fix\ndescription: Fixed colon handling in the header parser."
        ),
        Some((
            Some("Header parser fix".into()),
            "Fixed colon handling in the header parser".into()
        ))
    );
    assert_eq!(
        parse_summary("**Description:** Adds tests\n"),
        Some((None, "Adds tests".into()))
    );
    assert_eq!(parse_summary("I cannot help with that"), None);
    let c = conv(&[("fix the bug", &[("edit", "src/a.rs")])]);
    let t = summary_transcript(&c, SUMMARY_TRANSCRIPT_CHARS);
    assert!(t.starts_with("user: fix the bug"), "{t}");
    assert!(t.contains("tool: edit src/a.rs"), "{t}");
    assert_eq!(summary_transcript(&c, 10).chars().count(), 10);
    let req = summary_request("cheap-model", &t);
    assert_eq!(req.model, "cheap-model");
    assert!(req.tools.is_empty());
    assert_eq!(req.messages.len(), 1);
    assert!(req.system.as_deref().unwrap().contains("description:"));
}

#[test]
fn sessions_without_a_prompt_are_skipped_by_last_and_names() {
    let dir = tempfile::tempdir().unwrap();
    let store = SessionStore::new(dir.path());
    let t = time::macros::datetime!(2026-10-23 12:00 UTC);
    let real = saved(&store, "fix the bug", t - time::Duration::hours(1));
    // The TUI saves a session at startup, before any prompt.
    let mut empty = SessionMeta::new("/p", "coder", "fake/m", "");
    empty.updated_at = t;
    store.save(&empty).unwrap();
    assert!(empty.is_empty() && !real.is_empty());
    assert_eq!(store.list().unwrap()[0].id, empty.id);
    assert_eq!(store.find("last").unwrap().id, real.id);
    assert!(matches!(
        store.find("new session"),
        Err(SessionError::NotFound(_))
    ));
    assert_eq!(
        store.find(&empty.tag()).unwrap().id,
        empty.id,
        "ids still work"
    );
}

#[test]
fn verifier_feedback_and_handoffs_are_not_requests() {
    use crate::compaction::Handoff;
    use harness_events::redact::Redactor;
    use harness_policy::provenance::{Labeler, ProjectTrust};

    let t0 = time::macros::datetime!(2026-10-23 05:32 UTC);
    let labeler = Labeler::new(
        std::sync::Arc::new(Redactor::new(Vec::<String>::new())),
        ProjectTrust::Trusted,
    );
    // A failed verification: the loop sends fix feedback as a user
    // message, wrapped in a provenance marker (and unwrapped without a
    // labeler).
    let mut c = conv(&[("fix the header parser", &[("write", "out.txt")])]);
    let feedback =
        format!("{VERIFY_FEEDBACK_PREFIX} 1 of 1): `exit 7` exited with code 7.\n\nFix it.");
    c.messages.push(Message::user(
        labeler.tool_result("verify", "verify-1", &feedback, true),
    ));
    c.messages.push(Message::assistant("fixed"));
    c.messages.push(Message::user(feedback.clone()));
    c.messages.push(Message::assistant("fixed again"));
    let mut meta = SessionMeta::new("/proj", "coder", "fake/m", "");
    meta.refresh_from(&c, t0);
    assert_eq!(user_prompts(&c), ["fix the header parser"]);
    assert_eq!(
        meta.description, "fix the header parser · edited out.txt",
        "no verifier markup as the latest request"
    );
    // Prompt, reply, reply after the tool result, two fix replies.
    assert_eq!(meta.messages, 5);
    assert!(!summary_transcript(&c, 10_000).contains("tool_result"));

    // A compacted conversation: the handoff opens the history, and the
    // next prompt is merged into the handoff message.
    let handoff = Handoff {
        goal: "fix the header parser".into(),
        ..Handoff::default()
    }
    .render();
    let mut first = Message::user(handoff.clone());
    first.content.push(ContentBlock::text("now add tests"));
    let mut c = Conversation::new(None);
    c.messages.push(first);
    c.messages.push(Message::assistant("added"));
    c.messages.push(Message::user(handoff));
    c.messages.push(Message::assistant("continuing"));
    assert_eq!(user_prompts(&c), ["now add tests"]);
    let mut meta = SessionMeta::new("/proj", "coder", "fake/m", "");
    meta.refresh_from(&c, t0);
    assert_eq!(meta.description, "now add tests");
    assert_eq!(meta.messages, 3, "the handoff-only message is not counted");

    // A user who types an angle bracket is still a user.
    assert!(!is_harness_text("<b> is bold, fix the renderer"));
    assert!(!is_harness_text("<tool_resultish> parsing"));
    assert!(is_harness_text("  <web_content url=\"x\">"));
}

#[test]
fn effort_override_is_saved_and_old_sessions_have_none() {
    use harness_config::agents::Effort;
    let dir = tempfile::tempdir().unwrap();
    let store = SessionStore::new(dir.path());
    let mut h =
        SessionHandle::start(store.clone(), SessionMeta::new("/p", "coder", "fake/m", "")).unwrap();
    let id = h.meta().id.clone();
    // No override: the key is not written at all.
    let raw = std::fs::read_to_string(dir.path().join(&id).join("session.json")).unwrap();
    assert!(!raw.contains("effort"), "{raw}");
    h.set_effort(Some(Effort::Low)).unwrap();
    assert_eq!(store.load(&id).unwrap().effort, Some(Effort::Low));
    let raw = std::fs::read_to_string(dir.path().join(&id).join("session.json")).unwrap();
    assert!(
        raw.contains("\"effort\": \"low\"") || raw.contains("\"effort\":\"low\""),
        "{raw}"
    );
    h.set_effort(None).unwrap();
    assert_eq!(store.load(&id).unwrap().effort, None);
}
