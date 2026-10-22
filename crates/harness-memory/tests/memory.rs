//! End-to-end tests for the memory drive, tool, retrieval, extraction and
//! concurrency. They use temp dirs and the local `git` binary only.

use std::path::Path;
use std::process::Command;
use std::sync::Arc;

use harness_config::agents::MemoryAccess;
use harness_events::redact::Redactor;
use harness_events::schema::MemoryAction;
use harness_memory::drive::{Bullet, Drive, Note, project_key};
use harness_memory::extract::{ExtractConfig, Extractor, write_proposals};
use harness_memory::index::{SessionIndex, UsageKind, UsageLog};
use harness_memory::sync::SessionCheckout;
use harness_memory::tool::{
    self, MemoryArgs, MemoryTool, MemoryToolConfig, effective_access, spec,
};
use harness_tools::tool::{ToolContext, ToolErrorKind};
use serde_json::json;
use time::macros::date;

fn drive() -> (tempfile::TempDir, Drive) {
    let tmp = tempfile::tempdir().unwrap();
    let d = Drive::open_or_init(tmp.path().join("memory")).unwrap();
    (tmp, d)
}

fn config(access: MemoryAccess, run: &str) -> MemoryToolConfig {
    MemoryToolConfig {
        access,
        run_id: run.to_owned(),
        project_key: Some("my-app-1234abcd".to_owned()),
        redactor: Arc::new(Redactor::new(["s3cr3t-value-xyz"])),
        date: Some(date!(2026 - 10 - 21)),
    }
}

fn tool_for(d: &Drive, session: &str, access: MemoryAccess) -> MemoryTool {
    let co = Arc::new(SessionCheckout::open(d, session).unwrap());
    MemoryTool::new(co, config(access, &format!("run-{session}")))
}

fn write(t: &MemoryTool, path: &str, text: &str) -> harness_memory::tool::MemoryOutcome {
    t.execute(MemoryArgs::Write {
        path: path.into(),
        text: text.into(),
        title: None,
    })
    .unwrap()
}

fn git(dir: &Path, args: &[&str]) -> String {
    let o = Command::new("git")
        .current_dir(dir)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args(args)
        .output()
        .unwrap();
    assert!(
        o.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&o.stderr)
    );
    String::from_utf8_lossy(&o.stdout).trim().to_owned()
}

// ---- TASK-90: drive ----

#[test]
fn project_key_uses_remote_then_canonical_path() {
    let tmp = tempfile::tempdir().unwrap();
    let a = tmp.path().join("clone-a");
    let b = tmp.path().join("clone-b");
    for (dir, url) in [
        (&a, "git@github.com:Me/My-App.git"),
        (&b, "https://github.com/me/my-app"),
    ] {
        std::fs::create_dir_all(dir).unwrap();
        git(dir, &["init", "-q"]);
        git(dir, &["remote", "add", "origin", url]);
    }
    let ka = project_key(&a);
    assert_eq!(ka, project_key(&b), "clones of one remote share a key");
    assert!(ka.key.starts_with("my-app-"));

    let plain = tmp.path().join("no-remote");
    std::fs::create_dir_all(&plain).unwrap();
    let kp = project_key(&plain);
    assert!(kp.key.starts_with("no-remote-"));
    assert_eq!(
        kp.source,
        harness_memory::drive::KeySource::Path(plain.canonicalize().unwrap().display().to_string())
    );
    // Same canonical path through a symlink gives the same key.
    let link = tmp.path().join("link");
    std::os::unix::fs::symlink(&plain, &link).unwrap();
    assert_eq!(project_key(&link), kp);
}

#[test]
fn internal_folder_is_protected_for_agent_tools() {
    use harness_policy::rules::{Action, Decision, Mode, PolicyConfig, PolicyEngine};
    let (tmp, d) = drive();
    let cfg = PolicyConfig {
        protected_roots: d.protected_paths(),
        ..Default::default()
    };
    let engine = PolicyEngine::new(&cfg, tmp.path()).unwrap();
    let internal = d.internal_dir();
    for (tool, args) in [
        (
            "write",
            json!({"path": internal.join("usage.jsonl"), "content": "x"}),
        ),
        ("edit", json!({"path": internal.join("search-index.json")})),
        (
            "bash",
            json!({"command": format!("rm -rf {}", internal.display())}),
        ),
        (
            "bash",
            json!({"command": format!("echo x > {}/usage.jsonl", internal.display())}),
        ),
    ] {
        let v = engine.decide(&Action::from_tool_call(tool, &args), Mode::Auto, tmp.path());
        assert_eq!(v.decision, Decision::Deny, "{tool} {args}: {v:?}");
    }
    // Notes themselves are not protected by this rule.
    let note = json!({"path": d.root().join("tools/cargo.md"), "content": "x"});
    let v = engine.decide(
        &Action::from_tool_call("write", &note),
        Mode::Auto,
        tmp.path(),
    );
    assert_ne!(v.decision, Decision::Deny);
}

// ---- TASK-91: tool ----

#[tokio::test]
async fn tool_search_read_write_forget() {
    let (_tmp, d) = drive();
    let t = tool_for(&d, "s1", MemoryAccess::ReadWrite);
    let ctx = ToolContext::new("call-1");

    let out = t
        .call(
            &ctx,
            json!({"action": "write", "path": "project/testing", "text": "Run `just test-int` for integration tests; plain `cargo test` skips them."}),
        )
        .await
        .unwrap();
    assert!(
        out.text()
            .starts_with("Saved to projects/my-app-1234abcd/testing.md"),
        "{}",
        out.text()
    );

    let o2 = write(
        &t,
        "project/testing",
        "Prefer table-driven tests; user rejected per-case fns. See [[tools/cargo]]",
    );
    let change = o2.change.unwrap();
    assert_eq!(change.action, MemoryAction::Updated);

    // Note format: title, one-line bullets with run link and date, [[links]].
    let text =
        std::fs::read_to_string(d.root().join("projects/my-app-1234abcd/testing.md")).unwrap();
    assert_eq!(
        text,
        "# Testing (my-app-1234abcd)\n\
         - Run `just test-int` for integration tests; plain `cargo test` skips them. (src: harness://run/run-s1 · 2026-10-21)\n\
         - Prefer table-driven tests; user rejected per-case fns. See [[tools/cargo]] (src: harness://run/run-s1 · 2026-10-21)\n"
    );
    let note = Note::parse(&text);
    assert_eq!(note.links(), vec!["tools/cargo"]);
    assert!(
        note.bullets()
            .all(|b: &Bullet| b.source.as_deref() == Some("harness://run/run-s1"))
    );

    let found = t
        .call(
            &ctx,
            json!({"action": "search", "query": "integration tests"}),
        )
        .await
        .unwrap();
    assert!(
        found
            .text()
            .contains("[[projects/my-app-1234abcd/testing]]"),
        "{}",
        found.text()
    );
    assert!(found.text().contains("just test-int"));

    let read = t
        .call(&ctx, json!({"action": "read", "path": "project/testing"}))
        .await
        .unwrap();
    assert_eq!(read.text(), text);

    let gone = t
        .execute(MemoryArgs::Forget {
            path: "project/testing".into(),
            text: "table-driven".into(),
        })
        .unwrap();
    let ch = gone.change.unwrap();
    assert_eq!(ch.action, MemoryAction::Deleted);
    assert_eq!(ch.bullets.len(), 1);
    let after =
        std::fs::read_to_string(d.root().join("projects/my-app-1234abcd/testing.md")).unwrap();
    assert!(!after.contains("table-driven"));

    let none = t
        .execute(MemoryArgs::Forget {
            path: "project/testing".into(),
            text: "nonexistent".into(),
        })
        .unwrap_err();
    assert_eq!(none.kind, ToolErrorKind::NoMatch);

    let missing = t
        .execute(MemoryArgs::Read {
            path: "tools/none".into(),
        })
        .unwrap_err();
    assert_eq!(missing.kind, ToolErrorKind::NotFound);

    // Forgetting the last bullet removes the note file.
    t.execute(MemoryArgs::Forget {
        path: "project/testing".into(),
        text: "just test-int".into(),
    })
    .unwrap();
    assert!(
        !d.root()
            .join("projects/my-app-1234abcd/testing.md")
            .exists()
    );
}

#[test]
fn writes_are_validated_scanned_and_screened() {
    let (_tmp, d) = drive();
    let t = tool_for(&d, "s1", MemoryAccess::ReadWrite);
    let head = d.head().unwrap();
    for (text, needle) in [
        ("two\nlines", "one-line"),
        ("The staging password is s3cr3t-value-xyz", "secret"),
        (
            "Token ghp_abcdefghijklmnopqrstuvwxyz0123456789 works",
            "secret",
        ),
        ("PR #42 is waiting on review", "task state"),
        (
            "In this session we refactored the parser",
            "session summary",
        ),
    ] {
        let e = t
            .execute(MemoryArgs::Write {
                path: "tools/git".into(),
                text: text.into(),
                title: None,
            })
            .unwrap_err();
        assert_eq!(e.kind, ToolErrorKind::InvalidArgs);
        assert!(e.message.contains(needle), "{text}: {}", e.message);
        assert!(
            !e.message.contains("s3cr3t-value-xyz"),
            "secret echoed back"
        );
    }
    let bad_path = t
        .execute(MemoryArgs::Write {
            path: ".harness/usage".into(),
            text: "x".into(),
            title: None,
        })
        .unwrap_err();
    assert_eq!(bad_path.kind, ToolErrorKind::InvalidArgs);
    assert_eq!(d.head().unwrap(), head, "rejected writes make no commit");
}

#[test]
fn each_write_is_a_commit_referencing_the_run() {
    let (_tmp, d) = drive();
    let t = tool_for(&d, "s1", MemoryAccess::ReadWrite);
    let before: usize = git(d.root(), &["rev-list", "--count", "main"])
        .parse()
        .unwrap();
    let o = write(
        &t,
        "tools/cargo",
        "Use cargo nextest; plain cargo test times out in CI.",
    );
    write(
        &t,
        "MEMORY.md",
        "User prefers terse answers without preamble.",
    );
    let after: usize = git(d.root(), &["rev-list", "--count", "main"])
        .parse()
        .unwrap();
    assert_eq!(after, before + 2);
    let msg = git(d.root(), &["log", "-1", "--format=%B", "main~1"]);
    assert!(msg.starts_with("memory: write tools/cargo.md"), "{msg}");
    assert!(msg.contains("Run: harness://run/run-s1"), "{msg}");
    assert_eq!(
        o.change.unwrap().commit.commit,
        git(d.root(), &["rev-parse", "main~1"])
    );
    // A duplicate write is a no-op without a commit.
    let dup = write(
        &t,
        "tools/cargo",
        "Use cargo nextest; plain cargo test times out in CI.",
    );
    assert!(dup.change.is_none());
    let count: usize = git(d.root(), &["rev-list", "--count", "main"])
        .parse()
        .unwrap();
    assert_eq!(count, after);
}

#[test]
fn tool_appears_only_for_agents_with_memory() {
    assert!(spec(MemoryAccess::Off).is_none());
    let ro = spec(MemoryAccess::Read).unwrap();
    assert_eq!(ro.name, "memory");
    assert_eq!(
        ro.input_schema["properties"]["action"]["enum"],
        json!(["search", "read"])
    );
    let rw = spec(MemoryAccess::ReadWrite).unwrap();
    assert_eq!(
        rw.input_schema["properties"]["action"]["enum"],
        json!(["search", "read", "write", "forget"])
    );

    let (_tmp, d) = drive();
    let co = Arc::new(SessionCheckout::open(&d, "s1").unwrap());
    assert!(MemoryTool::for_agent(co.clone(), config(MemoryAccess::Off, "r")).is_none());
    let ro_tool = MemoryTool::for_agent(co, config(MemoryAccess::Read, "r")).unwrap();
    let e = ro_tool
        .execute(MemoryArgs::Write {
            path: "tools/x".into(),
            text: "y".into(),
            title: None,
        })
        .unwrap_err();
    assert_eq!(e.kind, ToolErrorKind::Unsupported);
    assert!(
        ro_tool
            .execute(MemoryArgs::Search {
                query: "x".into(),
                limit: None
            })
            .is_ok()
    );

    // Agent YAML: request_tools lists memory only when memory != off.
    let mut cfg = harness_config::ini::Config::defaults().memory;
    assert_eq!(
        effective_access(MemoryAccess::ReadWrite, &cfg, false),
        MemoryAccess::ReadWrite
    );
    assert_eq!(
        effective_access(MemoryAccess::ReadWrite, &cfg, true),
        MemoryAccess::Off
    );
    cfg.headless_access = true;
    assert_eq!(
        effective_access(MemoryAccess::Read, &cfg, true),
        MemoryAccess::Read
    );
    cfg.enabled = false;
    assert_eq!(
        effective_access(MemoryAccess::ReadWrite, &cfg, false),
        MemoryAccess::Off
    );
}

// ---- TASK-92: retrieval ----

#[test]
fn session_index_is_capped_frozen_and_reads_are_recorded() {
    let (_tmp, d) = drive();
    let co = Arc::new(SessionCheckout::open(&d, "s1").unwrap());
    let t = MemoryTool::new(co.clone(), config(MemoryAccess::ReadWrite, "run-1"));
    write(
        &t,
        "MEMORY.md",
        "User prefers terse answers without preamble.",
    );
    write(
        &t,
        "project/testing",
        "Integration tests need docker running first.",
    );

    let idx = SessionIndex::load(co.path(), Some("my-app-1234abcd"), 1500).unwrap();
    assert!(idx.text().contains("User prefers terse answers"));
    assert!(idx.text().contains("[[projects/my-app-1234abcd/testing]]"));
    let frozen = idx.clone();

    // Writes during the session do not change the frozen index.
    write(&t, "MEMORY.md", "User wants British spelling in docs.");
    assert_eq!(idx, frozen);
    assert!(!idx.text().contains("British"));
    // A new session sees it.
    let next = SessionIndex::load(d.root(), Some("my-app-1234abcd"), 1500).unwrap();
    assert!(next.text().contains("British"));
    assert_ne!(next.sha256(), frozen.sha256());
    // Cap.
    let tiny = SessionIndex::load(d.root(), Some("my-app-1234abcd"), 30).unwrap();
    assert!(tiny.tokens() <= 30);

    // Reads are recorded per note and run.
    t.execute(MemoryArgs::Read {
        path: "project/testing".into(),
    })
    .unwrap();
    t.execute(MemoryArgs::Search {
        query: "docker".into(),
        limit: None,
    })
    .unwrap();
    let usage = UsageLog::new(&d.internal_dir());
    let recs = usage.records().unwrap();
    assert!(
        recs.iter()
            .any(|r| r.note == "projects/my-app-1234abcd/testing.md"
                && r.run == "run-1"
                && r.kind == UsageKind::Read)
    );
    assert!(recs.iter().any(|r| r.kind == UsageKind::SearchHit));
    assert_eq!(
        usage.summary().unwrap()["projects/my-app-1234abcd/testing.md"].reads,
        1
    );
}

#[test]
fn search_index_is_cached_and_incremental() {
    use harness_memory::index::{INDEX_FILE, SearchIndex};
    let (_tmp, d) = drive();
    let t = tool_for(&d, "s1", MemoryAccess::ReadWrite);
    write(
        &t,
        "tools/docker",
        "docker compose needs --wait or healthchecks race the migrations.",
    );
    t.execute(MemoryArgs::Search {
        query: "docker".into(),
        limit: None,
    })
    .unwrap();
    let cached = SearchIndex::load(&d.internal_dir().join(INDEX_FILE));
    assert!(cached.doc("tools/docker.md").is_some());
    let mut again = cached.clone();
    let stats = again.update(d.root()).unwrap();
    assert_eq!(stats.added + stats.updated + stats.removed, 0, "{stats:?}");
}

// ---- TASK-93: extraction ----

#[tokio::test]
async fn extraction_pass_proposes_screened_lessons_with_a_cheap_model() {
    use harness_providers::fake::{FakeProvider, FakeTurn};
    use harness_providers::model::Message;
    let reply = r#"[
      {"path": "tools/cargo", "text": "Use cargo nextest; plain cargo test times out in CI.", "reason": "user correction"},
      {"path": "project/status", "text": "PR #12 is waiting on review", "reason": "status"},
      {"path": "MEMORY.md", "text": "We implemented the parser today", "reason": "summary"}
    ]"#;
    let fake = FakeProvider::with_turns([FakeTurn::text(reply)]);
    let ex = Extractor::new(
        Arc::new(fake.clone()),
        ExtractConfig::new("claude-haiku-4-5"),
        Arc::new(Redactor::new(["s3cr3t-value-xyz"])),
    );
    let msgs = vec![
        Message::user("no, use nextest, cargo test times out in CI. token s3cr3t-value-xyz"),
        Message::assistant("Understood."),
    ];
    let out = ex
        .extract(
            &msgs,
            "# Memory\n",
            tokio_util::sync::CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(out.proposals.len(), 1);
    assert_eq!(out.proposals[0].path, "tools/cargo");
    assert_eq!(out.rejected.len(), 2);
    let req = fake.last_request().unwrap();
    assert_eq!(req.model, "claude-haiku-4-5");
    let sent = req.messages[0].text();
    assert!(!sent.contains("s3cr3t-value-xyz"), "transcript is redacted");
    assert!(sent.contains("Already in memory"));

    let (_tmp, d) = drive();
    let t = tool_for(&d, "s1", MemoryAccess::ReadWrite);
    let results = write_proposals(&t, &out.proposals);
    assert!(results[0].is_ok());
    assert!(
        std::fs::read_to_string(d.root().join("tools/cargo.md"))
            .unwrap()
            .contains("nextest")
    );
}

// ---- TASK-94: concurrency ----

#[test]
fn sessions_use_their_own_worktrees() {
    let (_tmp, d) = drive();
    let a = SessionCheckout::open(&d, "a").unwrap();
    let b = SessionCheckout::open(&d, "b").unwrap();
    assert_ne!(a.path(), b.path());
    assert!(a.path().starts_with(d.internal_dir()));
    assert_eq!(git(a.path(), &["branch", "--show-current"]), "session/a");
    assert_eq!(git(b.path(), &["branch", "--show-current"]), "session/b");
    let list = git(d.root(), &["worktree", "list"]);
    assert_eq!(list.lines().count(), 3, "{list}");
    // Drive root stays clean (worktrees are inside the ignored dir).
    assert_eq!(git(d.root(), &["status", "--porcelain"]), "");
    // Re-opening a session resumes its worktree.
    drop(a);
    let a2 = SessionCheckout::open(&d, "a").unwrap();
    assert_eq!(a2.branch(), "session/a");
    a2.close().unwrap();
    assert_eq!(git(d.root(), &["worktree", "list"]).lines().count(), 2);
    assert!(SessionCheckout::open(&d, "../evil").is_err());
}

#[test]
fn stale_write_is_rejected_then_rebased_and_retried() {
    let (_tmp, d) = drive();
    let a = SessionCheckout::open(&d, "a").unwrap();
    let b = SessionCheckout::open(&d, "b").unwrap();
    let ta = MemoryTool::new(Arc::new(b), config(MemoryAccess::ReadWrite, "run-b"));
    // While A edits its worktree, B publishes first, so A's commit is stale.
    let (_, published) = a
        .write("memory: write tools/cargo.md\n\nRun: harness://run/run-a", |dir| {
            write(&ta, "tools/cargo", "Use cargo nextest in CI.");
            let p = dir.join("tools/cargo.md");
            std::fs::write(&p, "# Cargo\n- Run clippy with -D warnings. (src: harness://run/run-a · 2026-10-21)\n").unwrap();
            Ok(())
        })
        .unwrap();
    let p = published.unwrap();
    assert_eq!(p.rebases, 1, "{p:?}");
    assert_eq!(p.attempts, 2);
    let text = std::fs::read_to_string(d.root().join("tools/cargo.md")).unwrap();
    assert!(text.contains("nextest"), "B's write kept:\n{text}");
    assert!(text.contains("clippy"), "A's write kept:\n{text}");
    // History is linear: A's commit sits on top of B's.
    assert_eq!(
        git(d.root(), &["rev-list", "--merges", "--count", "main"]),
        "0"
    );
    assert!(git(d.root(), &["log", "-1", "--format=%B", "main"]).contains("run-a"));
}

#[test]
fn concurrent_writers_merge_never_overwrite() {
    let (_tmp, d) = drive();
    let writers = 6;
    let per_writer = 4;
    let handles: Vec<_> = (0..writers)
        .map(|w| {
            let d = d.clone();
            std::thread::spawn(move || {
                let t = tool_for(&d, &format!("w{w}"), MemoryAccess::ReadWrite);
                for i in 0..per_writer {
                    // Everyone appends to the same note and MEMORY.md.
                    write(
                        &t,
                        "tools/shared",
                        &format!("Lesson {i} from writer {w} about builds."),
                    );
                    if i == 0 {
                        write(&t, "MEMORY.md", &format!("Preference from writer {w}."));
                    }
                }
            })
        })
        .collect();
    for h in handles {
        h.join().unwrap();
    }
    let shared = std::fs::read_to_string(d.root().join("tools/shared.md")).unwrap();
    let mem = std::fs::read_to_string(d.memory_file()).unwrap();
    for w in 0..writers {
        for i in 0..per_writer {
            let needle = format!("Lesson {i} from writer {w} about builds.");
            assert_eq!(
                shared.matches(&needle).count(),
                1,
                "missing or duplicated {needle}:\n{shared}"
            );
        }
        assert!(
            mem.contains(&format!("Preference from writer {w}.")),
            "{mem}"
        );
    }
    // Only one heading, no conflict markers, clean tree, all commits on main.
    assert_eq!(shared.matches("# Shared").count(), 1, "{shared}");
    assert!(!shared.contains("<<<<<<<") && !shared.contains(">>>>>>>"));
    assert_eq!(git(d.root(), &["status", "--porcelain"]), "");
    let commits: usize = git(d.root(), &["rev-list", "--count", "main"])
        .parse()
        .unwrap();
    assert_eq!(commits, 1 + writers * (per_writer + 1));
    let _ = tool::NAME;
}
