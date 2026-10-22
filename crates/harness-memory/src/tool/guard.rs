//! Write guard: what may be saved to memory.
//!
//! Every write is checked, in order, for:
//! 1. format: one line, no metadata suffix, valid `[[links]]`, length cap;
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
use crate::drive::note::{MAX_BULLET_CHARS, links};

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

/// Checks bullet `text` (already trimmed) for format problems.
pub fn check_format(text: &str) -> Result<(), Rejection> {
    if text.is_empty() {
        return Err(reject(
            RejectKind::Format,
            "the memory text is empty. Write one self-contained lesson as a single sentence.",
        ));
    }
    if text.contains('\n') || text.contains('\r') {
        return Err(reject(
            RejectKind::Format,
            "memories are one-line bullets. Rewrite it as a single line (one lesson per write).",
        ));
    }
    if text.chars().count() > MAX_BULLET_CHARS {
        return Err(reject(
            RejectKind::Format,
            format!(
                "the memory is longer than {MAX_BULLET_CHARS} characters. Shorten it to the rule and its reason."
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

/// Runs every check on a candidate memory. Returns the normalised bullet
/// text (trimmed, leading `- ` removed).
pub fn validate(text: &str, redactor: &Redactor) -> Result<String, Rejection> {
    let t = text.trim();
    let t = t
        .strip_prefix("- ")
        .or_else(|| t.strip_prefix("* "))
        .unwrap_or(t)
        .trim();
    check_format(t)?;
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
