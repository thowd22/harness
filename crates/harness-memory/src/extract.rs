//! Memory extraction: teaching agents when to write, and an optional
//! end-of-session pass that proposes lessons the agent missed.
//!
//! [`PROMPT_SECTION`] is the system-prompt guidance for memory-enabled
//! agents (the tool description carries the same rules).
//! [`Extractor`] sends the session transcript to a cheap model (by default
//! `[memory] dream_model`) and returns [`Proposal`]s. Every proposal passes
//! the same write guard as the memory tool ([`crate::tool::guard`]), so the
//! design doc's "not saved" rules hold for extracted lessons too.

use std::fmt::Write as _;
use std::sync::Arc;

use harness_events::redact::Redactor;
use harness_providers::model::{ChatRequest, ContentBlock, Message, Role, ToolResultPart};
use harness_providers::provider::{Provider, ProviderError};
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

use crate::tool::guard::{self, Rejection};
use crate::tool::{MemoryArgs, MemoryOutcome, MemoryTool};

/// System-prompt guidance for agents with memory. Bundled default prompts
/// include it for agents with `memory: read-write`.
pub const PROMPT_SECTION: &str = "## Memory\n\
You have a long-term memory tool. Search it when past preferences or gotchas may apply. \
Save a memory (one bullet, one lesson) when:\n\
- the user corrects you: save the rule to follow next time, not the mistake;\n\
- the user states a preference about how to work;\n\
- a decision is made: save it with its reason;\n\
- you discover a gotcha about this repo, the environment or a tool that cost time.\n\
Do not save session summaries, task state (PR or issue numbers, statuses, what is in progress), \
facts anyone can rediscover by reading the repo, or secrets. Put several lessons for one note in one write (`bullets`). \
If asked to index or learn the codebase into memory, save one compact project/overview note plus the non-obvious gotchas, not a file or module map, \
and tell the user the rest is re-read from the repo each session. New memories apply to future sessions.";

/// Default cap on proposals per pass.
pub const DEFAULT_MAX_PROPOSALS: usize = 5;
/// Default transcript budget in characters (the most recent part is kept).
pub const DEFAULT_MAX_TRANSCRIPT_CHARS: usize = 24_000;
/// Longest tool result kept per call in the transcript.
pub const TOOL_RESULT_CHARS: usize = 400;

const EXTRACT_SYSTEM: &str = "You review a finished coding-agent session and propose lessons worth remembering in future sessions. \
Propose only: corrections from the user (as the rule to follow), stated preferences, decisions with their reasons, and non-obvious gotchas about the repo, environment or tools. \
Never propose session summaries, task state (PR/issue numbers, statuses, work in progress), facts easy to rediscover from the repository, or secrets. \
Skip anything already in memory. Each lesson is one self-contained sentence. \
Paths: MEMORY.md for general preferences, project/<topic> for this project, tools/<tool> for tools. \
Reply with only a JSON array (possibly empty) of objects {\"path\": ..., \"text\": ..., \"reason\": ...}.";

/// Extraction settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtractConfig {
    /// Provider model id of the cheap model (resolve `[memory] dream_model`
    /// through the model registry first).
    pub model: String,
    /// Most proposals kept.
    pub max_proposals: usize,
    /// Transcript budget in characters.
    pub max_transcript_chars: usize,
}

impl ExtractConfig {
    /// Defaults for `model`.
    pub fn new(model: impl Into<String>) -> Self {
        Self {
            model: model.into(),
            max_proposals: DEFAULT_MAX_PROPOSALS,
            max_transcript_chars: DEFAULT_MAX_TRANSCRIPT_CHARS,
        }
    }
}

/// A lesson proposed by the model, before validation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Candidate {
    /// Note path (`MEMORY.md`, `project/<topic>`, `tools/<tool>`).
    pub path: String,
    /// The lesson.
    pub text: String,
    /// Why it is worth keeping.
    #[serde(default)]
    pub reason: String,
}

/// A validated proposal, ready to show the user or write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Proposal {
    /// Note path as proposed.
    pub path: String,
    /// Normalised bullet text.
    pub text: String,
    /// Why.
    pub reason: String,
}

/// The result of an extraction pass.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Extraction {
    /// Proposals that passed the write guard.
    pub proposals: Vec<Proposal>,
    /// Candidates the guard rejected, with the reason.
    pub rejected: Vec<(Candidate, Rejection)>,
}

/// Errors from an extraction pass.
#[derive(Debug, thiserror::Error)]
pub enum ExtractError {
    /// The model call failed.
    #[error("memory extraction model call failed: {0}")]
    Provider(#[from] ProviderError),
    /// The reply had no parseable JSON array.
    #[error("memory extraction reply was not a JSON array of lessons: {0}")]
    BadReply(String),
}

/// Renders a conversation as a plain transcript for extraction, keeping
/// the most recent `max_chars`. Tool results are shortened; reasoning and
/// opaque blocks are dropped.
pub fn transcript(messages: &[Message], max_chars: usize) -> String {
    let mut out = String::new();
    for m in messages {
        let who = match m.role {
            Role::User => "USER",
            Role::Assistant => "ASSISTANT",
        };
        for b in &m.content {
            match b {
                ContentBlock::Text { text } => {
                    let _ = writeln!(out, "{who}: {}", text.trim());
                }
                ContentBlock::ToolUse { name, input, .. } => {
                    let _ = writeln!(
                        out,
                        "{who} called {name}: {}",
                        clip(&input.to_string(), TOOL_RESULT_CHARS)
                    );
                }
                ContentBlock::ToolResult {
                    content, is_error, ..
                } => {
                    let text: String = content
                        .iter()
                        .filter_map(|p| match p {
                            ToolResultPart::Text { text } => Some(text.as_str()),
                            _ => None,
                        })
                        .collect::<Vec<_>>()
                        .join(" ");
                    let tag = if *is_error {
                        "TOOL ERROR"
                    } else {
                        "TOOL RESULT"
                    };
                    let _ = writeln!(out, "{tag}: {}", clip(text.trim(), TOOL_RESULT_CHARS));
                }
                _ => {}
            }
        }
    }
    if out.len() > max_chars {
        let mut start = out.len() - max_chars;
        while !out.is_char_boundary(start) {
            start += 1;
        }
        out = format!("[earlier transcript omitted]\n{}", &out[start..]);
    }
    out
}

fn clip(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_owned();
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &s[..end])
}

/// Parses the model's reply: the first JSON array in it (code fences and
/// surrounding prose are tolerated).
pub fn parse_reply(reply: &str) -> Result<Vec<Candidate>, ExtractError> {
    let start = reply.find('[');
    let end = reply.rfind(']');
    let (Some(s), Some(e)) = (start, end) else {
        return Err(ExtractError::BadReply(clip(reply, 200)));
    };
    if e < s {
        return Err(ExtractError::BadReply(clip(reply, 200)));
    }
    serde_json::from_str(&reply[s..=e]).map_err(|err| ExtractError::BadReply(err.to_string()))
}

/// Validates candidates with the write guard and caps them at `max`.
/// Duplicates (same path and text) are dropped.
pub fn screen(candidates: Vec<Candidate>, redactor: &Redactor, max: usize) -> Extraction {
    let mut out = Extraction::default();
    for c in candidates {
        match guard::validate(&c.text, redactor) {
            Ok(text) => {
                if out.proposals.len() >= max
                    || out
                        .proposals
                        .iter()
                        .any(|p| p.path == c.path && p.text == text)
                {
                    continue;
                }
                out.proposals.push(Proposal {
                    path: c.path.trim().to_owned(),
                    text,
                    reason: c.reason.trim().to_owned(),
                });
            }
            Err(r) => out.rejected.push((c, r)),
        }
    }
    out
}

/// The end-of-session extraction pass.
#[derive(Clone)]
pub struct Extractor {
    provider: Arc<dyn Provider>,
    config: ExtractConfig,
    redactor: Arc<Redactor>,
}

impl std::fmt::Debug for Extractor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Extractor")
            .field("provider", &self.provider.name())
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

impl Extractor {
    /// An extractor using `provider` (the cheap model's provider).
    pub fn new(
        provider: Arc<dyn Provider>,
        config: ExtractConfig,
        redactor: Arc<Redactor>,
    ) -> Self {
        Self {
            provider,
            config,
            redactor,
        }
    }

    /// The request sent for `messages`, given the memory already loaded
    /// (`existing`, e.g. the session index text).
    pub fn request(&self, messages: &[Message], existing: &str) -> ChatRequest {
        let transcript = transcript(messages, self.config.max_transcript_chars);
        let transcript = self.redactor.redact(&transcript).into_owned();
        let mut prompt = String::new();
        if !existing.trim().is_empty() {
            let _ = write!(prompt, "Already in memory:\n{}\n\n", existing.trim());
        }
        let _ = write!(
            prompt,
            "Session transcript:\n{transcript}\n\nPropose at most {} lessons as a JSON array.",
            self.config.max_proposals
        );
        let mut req = ChatRequest::new(self.config.model.clone());
        req.system = Some(EXTRACT_SYSTEM.to_owned());
        req.messages = vec![Message::user(prompt)];
        req.max_tokens = Some(1024);
        req
    }

    /// Runs the pass and returns screened proposals. Nothing is written.
    pub async fn extract(
        &self,
        messages: &[Message],
        existing: &str,
        cancel: CancellationToken,
    ) -> Result<Extraction, ExtractError> {
        if messages.is_empty() {
            return Ok(Extraction::default());
        }
        let resp = self
            .provider
            .complete(self.request(messages, existing), cancel)
            .await?;
        let reply = resp.message.text();
        let candidates = parse_reply(&reply)?;
        Ok(screen(
            candidates,
            &self.redactor,
            self.config.max_proposals,
        ))
    }
}

/// Writes accepted proposals through the memory tool (so they get the same
/// validation, commit and publish path). Returns one result per proposal.
pub fn write_proposals(
    tool: &MemoryTool,
    proposals: &[Proposal],
) -> Vec<Result<MemoryOutcome, harness_tools::tool::ToolError>> {
    proposals
        .iter()
        .map(|p| {
            tool.execute(MemoryArgs::Write {
                path: p.path.clone(),
                text: p.text.clone(),
                bullets: Vec::new(),
                title: None,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool::guard::RejectKind;

    #[test]
    fn prompt_section_teaches_the_four_triggers_and_exclusions() {
        let s = PROMPT_SECTION.to_lowercase();
        for needle in [
            "corrects you",
            "rule to follow",
            "preference",
            "decision",
            "reason",
            "gotcha",
            "session summaries",
            "task state",
            "rediscover",
            "secrets",
        ] {
            assert!(s.contains(needle), "missing {needle}");
            assert!(
                crate::tool::DESCRIPTION.to_lowercase().contains(needle)
                    || needle == "corrects you",
                "tool description missing {needle}"
            );
        }
    }

    #[test]
    fn parse_reply_tolerates_fences_and_prose() {
        let r = "Here you go:\n```json\n[{\"path\":\"tools/cargo\",\"text\":\"Use nextest\",\"reason\":\"faster\"}]\n```";
        let c = parse_reply(r).unwrap();
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].path, "tools/cargo");
        assert_eq!(parse_reply("[]").unwrap(), vec![]);
        assert!(parse_reply("nothing").is_err());
    }

    #[test]
    fn screen_enforces_not_saved_rules() {
        let cands = vec![
            Candidate {
                path: "tools/cargo".into(),
                text: "Use cargo nextest; plain cargo test times out in CI.".into(),
                reason: String::new(),
            },
            Candidate {
                path: "project/x".into(),
                text: "PR #12 is waiting on review".into(),
                reason: String::new(),
            },
            Candidate {
                path: "project/x".into(),
                text: "In this session we fixed the parser".into(),
                reason: String::new(),
            },
            Candidate {
                path: "project/x".into(),
                text: "The function main is defined in src/main.rs".into(),
                reason: String::new(),
            },
            Candidate {
                path: "MEMORY.md".into(),
                text: "Deploy token is ghp_abcdefghijklmnopqrstuvwxyz0123456789".into(),
                reason: String::new(),
            },
            Candidate {
                path: "tools/cargo".into(),
                text: "Use cargo nextest; plain cargo test times out in CI.".into(),
                reason: String::new(),
            },
        ];
        let e = screen(cands, &Redactor::default(), 5);
        assert_eq!(e.proposals.len(), 1);
        let kinds: Vec<_> = e.rejected.iter().map(|(_, r)| r.kind).collect();
        assert_eq!(
            kinds,
            vec![
                RejectKind::TaskState,
                RejectKind::SessionSummary,
                RejectKind::Rediscoverable,
                RejectKind::Secret
            ]
        );
    }

    #[test]
    fn transcript_keeps_tail_and_shortens_tool_results() {
        let msgs = vec![
            Message::user("first question"),
            Message {
                role: Role::User,
                content: vec![ContentBlock::tool_result("t1", "x".repeat(5000), false)],
            },
            Message::assistant("final answer"),
        ];
        let t = transcript(&msgs, 100_000);
        assert!(t.contains("USER: first question"));
        assert!(t.contains("ASSISTANT: final answer"));
        assert!(t.len() < 1000);
        let short = transcript(&msgs, 50);
        assert!(short.starts_with("[earlier transcript omitted]"));
        assert!(short.ends_with("ASSISTANT: final answer\n"));
    }
}
