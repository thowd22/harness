//! Independent review agent support.
//!
//! Agents are poor judges of their own work, so a separate reviewer
//! (ideally on a different model) checks it. The reviewer sees exactly a
//! [`ReviewInput`]: the task, the scope and the diff. The implementer's
//! transcript, reasoning and tool output are not part of the input type, so
//! they cannot leak into the review.
//!
//! The reviewer grades the diff against the [`RUBRIC`] (correctness,
//! unrequested changes, over-engineering, dead or compatibility code, test
//! adequacy) and answers with a JSON [`ReviewVerdict`]. Its
//! [`ReviewVerdict::outputs`] are the step outputs a workflow loop tests,
//! e.g. `until: "{{ steps.review.outputs.verdict == 'approve' }}"`.

use std::collections::BTreeMap;

use harness_providers::model::{ChatRequest, Message};
use harness_providers::provider::{Provider, ProviderError};
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

use crate::checkpoints::{CheckpointError, CheckpointStore};
use crate::ledger::NetChange;

/// Diff bytes sent to the reviewer; longer diffs are cut with a note.
pub const MAX_DIFF_BYTES: usize = 200_000;
/// Output token limit for the review.
pub const MAX_REVIEW_TOKENS: u32 = 4096;

/// One rubric criterion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Criterion {
    /// JSON key.
    pub id: &'static str,
    /// What the reviewer checks.
    pub question: &'static str,
}

/// The review rubric.
pub const RUBRIC: [Criterion; 5] = [
    Criterion {
        id: "correctness",
        question: "Does the change do what the task asks, including edge cases and error paths?",
    },
    Criterion {
        id: "unrequested_changes",
        question: "Is every change needed for the task? Flag unrelated edits, drive-by refactors, \
                   reformatting, stray debug or temp files.",
    },
    Criterion {
        id: "over_engineering",
        question: "Is the solution as simple as the task allows? Flag speculative abstractions, \
                   options or layers nobody asked for.",
    },
    Criterion {
        id: "dead_or_compat_code",
        question: "Is all new code used? Flag dead code, commented-out code, unused helpers and \
                   needless backwards-compatibility shims.",
    },
    Criterion {
        id: "test_adequacy",
        question: "Do tests cover the change and would they catch a regression? Flag weakened, \
                   skipped or deleted tests.",
    },
];

/// Everything the reviewer sees.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ReviewInput {
    /// The task as given to the implementer.
    pub task: String,
    /// The scope globs, if any.
    pub scope: Option<Vec<String>>,
    /// Unified diff of the change.
    pub diff: String,
}

impl ReviewInput {
    /// Build from explicit parts.
    pub fn new(
        task: impl Into<String>,
        scope: Option<Vec<String>>,
        diff: impl Into<String>,
    ) -> Self {
        Self {
            task: task.into(),
            scope,
            diff: diff.into(),
        }
    }

    /// Build from the ledger's net changes (their diffs, joined; deletions
    /// without a diff are listed by name).
    pub fn from_ledger(
        task: impl Into<String>,
        scope: Option<Vec<String>>,
        changes: &[NetChange],
    ) -> Self {
        let mut diff = String::new();
        for c in changes {
            if c.diffs.is_empty() {
                diff.push_str(&format!("# {:?} {} (no diff recorded)\n", c.kind, c.path));
            }
            for d in &c.diffs {
                diff.push_str(d);
                if !d.ends_with('\n') {
                    diff.push('\n');
                }
            }
        }
        Self::new(task, scope, diff)
    }

    /// Build from the diff between checkpoint `id` and the worktree.
    pub fn from_checkpoint(
        task: impl Into<String>,
        scope: Option<Vec<String>>,
        store: &CheckpointStore,
        id: u64,
    ) -> Result<Self, CheckpointError> {
        Ok(Self::new(task, scope, store.diff_since(id)?))
    }

    /// The user message sent to the reviewer.
    pub fn render(&self) -> String {
        let mut s = format!("# Task\n{}\n\n# Scope\n", self.task.trim());
        match &self.scope {
            Some(globs) if !globs.is_empty() => {
                for g in globs {
                    s.push_str(&format!("- `{g}`\n"));
                }
            }
            _ => s.push_str("(no scope declared)\n"),
        }
        let (diff, cut) = truncate(&self.diff, MAX_DIFF_BYTES);
        s.push_str("\n# Diff\n```diff\n");
        s.push_str(diff);
        if !diff.ends_with('\n') {
            s.push('\n');
        }
        s.push_str("```\n");
        if cut {
            s.push_str(&format!(
                "\n(The diff was cut at {MAX_DIFF_BYTES} bytes of {}.)\n",
                self.diff.len()
            ));
        }
        s
    }
}

fn truncate(s: &str, max: usize) -> (&str, bool) {
    if s.len() <= max {
        return (s, false);
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    (&s[..end], true)
}

/// The reviewer's system prompt (rubric and answer format included).
pub fn system_prompt() -> String {
    let mut s = String::from(
        "You are an independent code reviewer. You get a task, its scope and the diff that \
         claims to complete it. Judge the diff on its own merits, as a careful senior engineer \
         would before merging.\n\nGrade each criterion:\n",
    );
    for c in RUBRIC {
        s.push_str(&format!("- `{}`: {}\n", c.id, c.question));
    }
    s.push_str(
        "\nAnswer with one JSON object and nothing else:\n\
         {\n  \"verdict\": \"approve\" | \"request_changes\",\n  \"summary\": \"one or two sentences\",\n  \
         \"criteria\": { \"<criterion id>\": { \"ok\": true | false, \"notes\": \"...\" }, ... },\n  \
         \"findings\": [ { \"criterion\": \"<criterion id>\", \"severity\": \"blocker\" | \"major\" | \"minor\", \
         \"file\": \"path\" or null, \"line\": number or null, \"message\": \"what to change\" } ]\n}\n\
         Include every criterion. Approve when every criterion is ok and no finding is a blocker.\n",
    );
    s
}

/// The reviewer request for `input` on `model`.
pub fn build_request(model: &str, input: &ReviewInput) -> ChatRequest {
    ChatRequest {
        system: Some(system_prompt()),
        messages: vec![Message::user(input.render())],
        max_tokens: Some(MAX_REVIEW_TOKENS),
        ..ChatRequest::new(model)
    }
}

/// Approve or not.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Decision {
    /// Good to merge.
    Approve,
    /// Needs another iteration.
    RequestChanges,
}

impl Decision {
    /// `approve` or `request_changes`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Approve => "approve",
            Self::RequestChanges => "request_changes",
        }
    }
}

/// How serious a finding is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    /// Nice to fix.
    Minor,
    /// Should be fixed.
    Major,
    /// Must be fixed before approval.
    Blocker,
}

/// One criterion's grade.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CriterionResult {
    /// True when the diff meets the criterion.
    pub ok: bool,
    /// Explanation.
    #[serde(default)]
    pub notes: String,
}

/// One problem found.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Finding {
    /// Criterion id.
    pub criterion: String,
    /// Severity.
    pub severity: Severity,
    /// File, if specific.
    #[serde(default)]
    pub file: Option<String>,
    /// Line, if specific.
    #[serde(default)]
    pub line: Option<u32>,
    /// What to change.
    pub message: String,
}

/// The reviewer's structured answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewVerdict {
    /// The decision (see [`ReviewVerdict::parse`] for consistency rules).
    pub verdict: Decision,
    /// Short summary.
    #[serde(default)]
    pub summary: String,
    /// One grade per rubric criterion.
    pub criteria: BTreeMap<String, CriterionResult>,
    /// Problems found.
    #[serde(default)]
    pub findings: Vec<Finding>,
}

/// Review failures.
#[derive(Debug, thiserror::Error)]
pub enum ReviewError {
    /// The provider call failed.
    #[error("review request failed: {0}")]
    Provider(#[from] ProviderError),
    /// The answer was not a valid verdict.
    #[error("reviewer answer is not a valid verdict: {0}")]
    Invalid(String),
}

impl ReviewVerdict {
    /// Parse a reviewer answer: the first JSON object in `text` (code
    /// fences allowed). Every rubric criterion must be graded. An
    /// `approve` with a failed criterion or a blocker finding is turned
    /// into `request_changes`.
    pub fn parse(text: &str) -> Result<Self, ReviewError> {
        let start = text
            .find('{')
            .ok_or_else(|| ReviewError::Invalid("no JSON object found".into()))?;
        let end = text
            .rfind('}')
            .filter(|e| *e > start)
            .ok_or_else(|| ReviewError::Invalid("unterminated JSON object".into()))?;
        let mut v: ReviewVerdict = serde_json::from_str(&text[start..=end])
            .map_err(|e| ReviewError::Invalid(e.to_string()))?;
        let missing: Vec<&str> = RUBRIC
            .iter()
            .map(|c| c.id)
            .filter(|id| !v.criteria.contains_key(*id))
            .collect();
        if !missing.is_empty() {
            return Err(ReviewError::Invalid(format!(
                "missing criteria: {}",
                missing.join(", ")
            )));
        }
        let blocked = v.criteria.values().any(|c| !c.ok)
            || v.findings.iter().any(|f| f.severity == Severity::Blocker);
        if v.verdict == Decision::Approve && blocked {
            v.verdict = Decision::RequestChanges;
        }
        Ok(v)
    }

    /// Step outputs for workflow templating: `verdict`, `summary`,
    /// `criteria`, `findings`.
    pub fn outputs(&self) -> serde_json::Value {
        serde_json::json!({
            "verdict": self.verdict.as_str(),
            "summary": self.summary,
            "criteria": self.criteria,
            "findings": self.findings,
        })
    }
}

/// Ask `provider` to review `input` with `model`. If the first answer is
/// not a valid verdict, the reviewer is asked once more for just the JSON.
pub async fn review(
    provider: &dyn Provider,
    model: &str,
    input: &ReviewInput,
    cancel: &CancellationToken,
) -> Result<ReviewVerdict, ReviewError> {
    let mut req = build_request(model, input);
    let first = provider.complete(req.clone(), cancel.clone()).await?;
    let text = first.message.text();
    match ReviewVerdict::parse(&text) {
        Ok(v) => Ok(v),
        Err(e) => {
            req.messages.push(first.message);
            req.messages.push(Message::user(format!(
                "That answer could not be read ({e}). Reply with only the JSON object described \
                 in the instructions."
            )));
            let second = provider.complete(req, cancel.clone()).await?;
            ReviewVerdict::parse(&second.message.text())
        }
    }
}

#[cfg(test)]
mod tests;
