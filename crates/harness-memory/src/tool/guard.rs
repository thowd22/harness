//! Write guard: what may be saved to memory.
//!
//! Every write is checked, in order, for:
//! 1. format: one line, no metadata suffix, valid `[[links]]`, length cap
//!    ([`MAX_BULLET_CHARS`] per bullet, [`MAX_OVERVIEW_BULLET_CHARS`] in a
//!    project's overview note, see [`limit_for`]);
//! 2. secrets: anything the run's redactor would mask;
//! 3. the design doc's "not saved" list: task state (PR/issue numbers,
//!    statuses, work in progress), session summaries, and facts trivially
//!    rediscoverable from the repository.
//!
//! The content rules are heuristics tuned to reject the common shapes; they
//! err on the side of letting a genuine lesson through.

use std::sync::LazyLock;

use harness_events::redact::Redactor;
use regex::Regex;

use crate::drive::NotePath;
pub use crate::drive::note::MAX_BULLET_CHARS;
use crate::drive::note::links;

/// Topic of a project's overview note (`project/overview`, stored as
/// `projects/<key>/overview.md`). It answers "index this codebase into
/// memory" with a compact summary instead of a file-by-file map, so its
/// bullets may be longer ([`MAX_OVERVIEW_BULLET_CHARS`]) and it holds at most
/// [`MAX_OVERVIEW_BULLETS`] of them.
pub const OVERVIEW_TOPIC: &str = "overview";
/// Longest accepted bullet in a project's overview note, in characters.
pub const MAX_OVERVIEW_BULLET_CHARS: usize = 1000;
/// Most bullets a project's overview note may hold.
pub const MAX_OVERVIEW_BULLETS: usize = 10;

/// True when `path` is a project's overview note (`projects/<key>/overview.md`).
pub fn is_overview(path: &NotePath) -> bool {
    is_overview_path(path.as_str())
}

/// True when the path string `path` names a project's overview note, in
/// either the model-facing alias form (`project/overview`) or the stored
/// form (`projects/<key>/overview`), with or without `.md` or `[[ ]]`.
fn is_overview_path(path: &str) -> bool {
    let p = path
        .trim()
        .trim_start_matches("[[")
        .trim_end_matches("]]")
        .trim();
    let p = p.strip_suffix(".md").unwrap_or(p);
    let parts: Vec<&str> = p.split('/').collect();
    match parts.as_slice() {
        [alias, topic] => {
            format!("{alias}/") == crate::tool::PROJECT_ALIAS && *topic == OVERVIEW_TOPIC
        }
        ["projects", key, topic] => !key.is_empty() && *topic == OVERVIEW_TOPIC,
        _ => false,
    }
}

/// The per-bullet character limit for a note at `path`: the one place the
/// limits are defined. [`MAX_OVERVIEW_BULLET_CHARS`] for a project's
/// overview note (`project/overview` as the model and the extractor name
/// it, or `projects/<key>/overview.md` as stored), [`MAX_BULLET_CHARS`]
/// everywhere else. The memory tool, the end-of-session extractor and the
/// dream pass all read their limit from here.
pub fn limit_for_path(path: &str) -> usize {
    if is_overview_path(path) {
        MAX_OVERVIEW_BULLET_CHARS
    } else {
        MAX_BULLET_CHARS
    }
}

/// The per-bullet character limit for a note at `path` (see
/// [`limit_for_path`]).
pub fn limit_for(path: &NotePath) -> usize {
    limit_for_path(path.as_str())
}

/// Why a write was rejected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RejectKind {
    /// Not a single well-formed bullet.
    Format,
    /// Contains a secret or credential.
    Secret,
    /// Task state: PR/issue numbers, statuses, in-progress work.
    TaskState,
    /// A summary of the session rather than a reusable lesson.
    SessionSummary,
    /// A fact anyone can rediscover by looking at the repository.
    Rediscoverable,
}

/// A rejected write. `message` is written for the model and says how to fix
/// the write.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct Rejection {
    /// The rule that matched.
    pub kind: RejectKind,
    /// Explanation and recovery step.
    pub message: String,
}

fn reject(kind: RejectKind, message: impl Into<String>) -> Rejection {
    Rejection {
        kind,
        message: message.into(),
    }
}

fn rx(patterns: &[&str]) -> Vec<Regex> {
    patterns
        .iter()
        .map(|p| Regex::new(&format!("(?i){p}")).expect("valid guard regex"))
        .collect()
}

static TASK_STATE: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    rx(&[
        r"\b(pr|pull request|mr|merge request|issue|ticket|bug)\s*#?\d+",
        r"(^|\s)#\d{2,}\b",
        r"\b(is|are|was|were|got)\s+(now\s+|still\s+)?(in progress|blocked|merged|pending review|approved|closed|reopened|done|finished|failing|passing)\b",
        r"\b(todo|wip)\b",
        r"\bnext steps?\b",
        r"\b(currently|still)\s+(working|implementing|fixing|debugging|waiting|investigating)\b",
        r"\bcommit\s+[0-9a-f]{7,40}\b",
        r"\bstatus\s*:",
    ])
});

/// Upper-case prefixes that look like ticket keys but are standards.
const NOT_TICKETS: &[&str] = &[
    "UTF", "SHA", "ISO", "RFC", "TLS", "HTTP", "MD", "ES", "IPV", "PEP",
];

static TICKET_KEY: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\b([A-Z][A-Z0-9]{1,9})-\d+\b").expect("valid regex"));

/// True when `text` mentions a ticket key such as `PROJ-881`.
fn has_ticket_key(text: &str) -> bool {
    TICKET_KEY
        .captures_iter(text)
        .any(|c| !NOT_TICKETS.contains(&&c[1]))
}

static SESSION_SUMMARY: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    rx(&[
        r"^(in )?(this|today'?s) (session|conversation|chat)\b",
        r"\b(this|the current|our) (session|conversation|chat)\b",
        r"^(session )?summary\b",
        r"^(today|yesterday|earlier today|so far)\b",
        r"^(we|i) (just |have |had )?(implemented|fixed|added|changed|refactored|worked on|completed|finished|updated|wrote)\b",
        r"^(the user|user) (asked|wanted|requested) (me|us) to\b",
    ])
});

static REDISCOVERABLE: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    rx(&[
        r"^(the )?(file|function|fn|struct|enum|class|module|method|type|trait|crate|package|interface)\s+`?[\w:./-]+`?\s+(is|are|lives)\s+(defined|located|declared|implemented|found)?\s*in\b",
        r"^`?[\w./-]+\.(rs|py|ts|tsx|js|go|java|c|h|cpp|rb|toml|json|yaml|yml)`?\s+(contains|defines|has|exports|declares)\b",
        r"^(the )?(repo|repository|project|codebase|workspace)\s+(has|contains)\s+\d+\b",
        r"^(the )?(repo|repository|project|codebase)\s+is\s+(written|implemented)\s+in\s+\w+\.?$",
    ])
});

/// Checks bullet `text` (already trimmed) for format problems with the
/// default length limit ([`MAX_BULLET_CHARS`]).
pub fn check_format(text: &str) -> Result<(), Rejection> {
    check_format_with(text, MAX_BULLET_CHARS)
}

/// Checks bullet `text` (already trimmed) for format problems, allowing at
/// most `max_chars` characters.
pub fn check_format_with(text: &str, max_chars: usize) -> Result<(), Rejection> {
    if text.is_empty() {
        return Err(reject(
            RejectKind::Format,
            "the memory text is empty. Write one self-contained lesson as a single sentence.",
        ));
    }
    if text.contains('\n') || text.contains('\r') {
        return Err(reject(
            RejectKind::Format,
            "memories are one-line bullets. Put each lesson on its own line in `bullets: [...]` (one write), or rewrite it as a single line.",
        ));
    }
    let len = text.chars().count();
    if len > max_chars {
        return Err(reject(
            RejectKind::Format,
            format!(
                "the memory is {len} characters; the limit is {max_chars} per bullet. Split it into several self-contained bullets of at most {max_chars} characters and send them in one write as `bullets: [...]`, or shorten it to the rule and its reason."
            ),
        ));
    }
    if text.starts_with('#') {
        return Err(reject(
            RejectKind::Format,
            "the memory text must be a bullet, not a heading. Pass the heading as `title` instead.",
        ));
    }
    if text.contains("(src:") {
        return Err(reject(
            RejectKind::Format,
            "do not add a `(src: ...)` suffix; the harness adds the source run and date. Remove it and retry.",
        ));
    }
    for l in links(text) {
        if let Err(e) = NotePath::parse(&l) {
            return Err(reject(
                RejectKind::Format,
                format!("{e}. Fix the [[link]] (e.g. [[tools/cargo]]) and retry."),
            ));
        }
    }
    Ok(())
}

/// Checks `text` for secrets using `redactor` (configured credentials plus
/// built-in key patterns).
pub fn check_secrets(text: &str, redactor: &Redactor) -> Result<(), Rejection> {
    if redactor.redact(text) != text {
        return Err(reject(
            RejectKind::Secret,
            "the memory looks like it contains a secret or credential, which is never saved. Remove the value (describe where it comes from instead) and retry.",
        ));
    }
    Ok(())
}

/// Checks `text` against the "not saved" content rules.
pub fn check_content(text: &str) -> Result<(), Rejection> {
    if has_ticket_key(text) || TASK_STATE.iter().any(|r| r.is_match(text)) {
        return Err(reject(
            RejectKind::TaskState,
            "this looks like task state (PR or issue numbers, statuses, work in progress), which is not saved to memory. Save only a reusable rule, preference, decision or gotcha, or skip the write.",
        ));
    }
    if SESSION_SUMMARY.iter().any(|r| r.is_match(text)) {
        return Err(reject(
            RejectKind::SessionSummary,
            "this looks like a session summary, which is not saved to memory. Save only the reusable lesson (e.g. 'Use X because Y'), or skip the write.",
        ));
    }
    if REDISCOVERABLE.iter().any(|r| r.is_match(text)) {
        return Err(reject(
            RejectKind::Rediscoverable,
            "this fact can be rediscovered by reading the repository, so it is not saved. Save only non-obvious gotchas, preferences and decisions.",
        ));
    }
    Ok(())
}

/// Runs every check on a candidate memory with the default length limit.
/// Returns the normalised bullet text (trimmed, leading `- ` removed).
pub fn validate(text: &str, redactor: &Redactor) -> Result<String, Rejection> {
    validate_with(text, redactor, MAX_BULLET_CHARS)
}

/// Runs every check on a candidate memory for the note at `path` (its limit
/// comes from [`limit_for`]).
pub fn validate_for(path: &NotePath, text: &str, redactor: &Redactor) -> Result<String, Rejection> {
    validate_with(text, redactor, limit_for(path))
}

/// Runs every check on a candidate memory for the note named by the path
/// string `path` (alias or stored form; its limit comes from
/// [`limit_for_path`]). Used where the path is not yet resolved to a
/// [`NotePath`], such as extractor proposals.
pub fn validate_for_path(path: &str, text: &str, redactor: &Redactor) -> Result<String, Rejection> {
    validate_with(text, redactor, limit_for_path(path))
}

fn validate_with(text: &str, redactor: &Redactor, max_chars: usize) -> Result<String, Rejection> {
    let t = text.trim();
    let t = t
        .strip_prefix("- ")
        .or_else(|| t.strip_prefix("* "))
        .unwrap_or(t)
        .trim();
    check_format_with(t, max_chars)?;
    check_secrets(t, redactor)?;
    check_content(t)?;
    Ok(t.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(t: &str) -> Result<String, RejectKind> {
        validate(t, &Redactor::new(["hunter2-very-secret"])).map_err(|r| r.kind)
    }

    #[test]
    fn good_lessons_pass() {
        for t in [
            "Run `just test-int` for integration tests; plain `cargo test` skips them.",
            "- Prefer table-driven tests; user rejected per-case fns. See [[projects/my-app/style]]",
            "Use rustls, not openssl: the CI image has no libssl (decided 2026-10).",
            "docker compose needs --wait or healthchecks race the migrations.",
            "User prefers British spelling in docs.",
            "Config files are UTF-8 and hashes are SHA-256; never assume latin-1.",
            "When the user says 'ship it', run the full verify command first.",
        ] {
            assert!(v(t).is_ok(), "{t}: {:?}", v(t));
        }
        assert_eq!(v("- trimmed  ").unwrap(), "trimmed");
    }

    #[test]
    fn format_rules() {
        for t in [
            "",
            "two\nlines",
            "# heading",
            "x (src: harness://run/1 · 2026-01-01)",
            "see [[../etc/passwd]]",
        ] {
            assert_eq!(v(t), Err(RejectKind::Format), "{t:?}");
        }
        assert_eq!(
            v(&"a".repeat(MAX_BULLET_CHARS + 1)),
            Err(RejectKind::Format)
        );
    }

    #[test]
    fn overview_notes_get_the_higher_limit() {
        let overview = NotePath::parse("projects/app-1a2b3c4d/overview").unwrap();
        let other = NotePath::parse("projects/app-1a2b3c4d/testing").unwrap();
        let nested = NotePath::parse("projects/app-1a2b3c4d/sub/overview").unwrap();
        let tool = NotePath::parse("tools/overview").unwrap();
        assert!(is_overview(&overview));
        for p in [&other, &nested, &tool] {
            assert!(!is_overview(p), "{p}");
            assert_eq!(limit_for(p), MAX_BULLET_CHARS);
        }
        assert_eq!(limit_for(&overview), MAX_OVERVIEW_BULLET_CHARS);
        let r = Redactor::new(Vec::<String>::new());
        let long = "a".repeat(MAX_BULLET_CHARS + 1);
        assert!(validate_for(&overview, &long, &r).is_ok());
        let e = validate_for(&other, &long, &r).unwrap_err();
        assert_eq!(e.kind, RejectKind::Format);
        assert!(
            e.message
                .contains("401 characters; the limit is 400 per bullet")
        );
        assert!(e.message.contains("`bullets: [...]`"));
    }

    #[test]
    fn limit_for_path_covers_alias_and_stored_forms_at_and_over_the_limits() {
        for p in [
            "project/overview",
            "project/overview.md",
            " [[project/overview]] ",
            "projects/app-1a2b3c4d/overview",
            "projects/app-1a2b3c4d/overview.md",
        ] {
            assert_eq!(limit_for_path(p), MAX_OVERVIEW_BULLET_CHARS, "{p}");
        }
        for p in [
            "project/testing",
            "project/sub/overview",
            "projects/app-1a2b3c4d/sub/overview",
            "projects//overview",
            "tools/overview",
            "overview",
            "MEMORY.md",
        ] {
            assert_eq!(limit_for_path(p), MAX_BULLET_CHARS, "{p}");
        }
        let r = Redactor::new(Vec::<String>::new());
        let at = |n| "a".repeat(n);
        assert!(validate_for_path("project/overview", &at(MAX_OVERVIEW_BULLET_CHARS), &r).is_ok());
        let e = validate_for_path("project/overview", &at(MAX_OVERVIEW_BULLET_CHARS + 1), &r)
            .unwrap_err();
        assert!(e.message.contains("the limit is 1000 per bullet"), "{e}");
        assert!(validate_for_path("project/testing", &at(MAX_BULLET_CHARS), &r).is_ok());
        let e = validate_for_path("project/testing", &at(MAX_BULLET_CHARS + 1), &r).unwrap_err();
        assert!(e.message.contains("the limit is 400 per bullet"), "{e}");
    }

    #[test]
    fn secrets_are_rejected() {
        assert_eq!(
            v("The staging password is hunter2-very-secret"),
            Err(RejectKind::Secret)
        );
        assert_eq!(
            v("Use key sk-ant-api03-abcdefghijklmnopqrstuvwxyz0123456789 for tests"),
            Err(RejectKind::Secret)
        );
        assert_eq!(
            v("AWS key AKIAIOSFODNN7EXAMPLE works for the sandbox"),
            Err(RejectKind::Secret)
        );
    }

    #[test]
    fn task_state_is_rejected() {
        for t in [
            "PR #42 is waiting on review",
            "Fixing issue 1234 next",
            "See #512 for the flaky test",
            "PROJ-881 is blocked on infra",
            "The migration branch is still in progress",
            "TODO: finish the parser",
            "Next step is to wire the CLI",
            "Currently working on the retry layer",
            "Reverted commit 9fceb02 because it broke CI",
        ] {
            assert_eq!(v(t), Err(RejectKind::TaskState), "{t}");
        }
    }

    #[test]
    fn session_summaries_are_rejected() {
        for t in [
            "In this session we refactored the parser",
            "Today we added retries to the provider",
            "We implemented the memory tool and wrote tests",
            "Summary: fixed three bugs",
            "The user asked me to add a flag",
        ] {
            assert_eq!(v(t), Err(RejectKind::SessionSummary), "{t}");
        }
    }

    #[test]
    fn rediscoverable_facts_are_rejected() {
        for t in [
            "The function parse_args is defined in src/tool.rs",
            "struct Config lives in crates/harness-config",
            "main.rs contains the CLI entry point",
            "The repo has 13 crates",
            "The project is written in Rust.",
        ] {
            assert_eq!(v(t), Err(RejectKind::Rediscoverable), "{t}");
        }
    }
}
