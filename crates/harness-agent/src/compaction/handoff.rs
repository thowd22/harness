//! Compaction of older history into a structured handoff.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Instant;

use harness_events::schema::{
    EventKind, NodeId, NodeKind, NodeStatus, Payload, Usage as EventUsage,
};
use harness_events::store::{EventDraft, RunRecorder};
use harness_providers::model::{ChatRequest, ContentBlock, Message, Role, ToolResultPart, Usage};
use harness_providers::provider::{Provider, ProviderError};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use crate::agent_loop::{Conversation, CostFn};
use crate::context::estimate_messages;
use crate::context::reasoning::is_user_prompt;

/// Name (and agent) of the child node recorded for each compaction.
pub const COMPACTION_NODE: &str = "compaction";
/// How a rendered handoff message starts ([`Handoff::render`]).
pub const HANDOFF_OPEN: &str = "<handoff>";

/// Handoff section headings, in order.
pub const HANDOFF_SECTIONS: [&str; 6] = [
    "Goal",
    "Decisions",
    "Current state",
    "Files touched",
    "Next steps",
    "Open questions",
];

/// Longest tool result quoted in the summarization transcript, in chars.
const RESULT_CHARS: usize = 1500;

/// System prompt of the summarization call.
pub const COMPACTION_SYSTEM: &str = "\
You write handoff notes for a coding agent that will continue a task. The \
notes replace the earlier part of the conversation, so they must hold \
everything needed to continue: the user's goal in their own words, the \
decisions made and why, the current state of the work (what is done, what \
is half done, what failed), the files touched, the concrete next steps and \
any open questions for the user. Be specific: name files, functions, \
commands and error messages. Answer with exactly these Markdown sections: \
## Goal, ## Decisions, ## Current state, ## Files touched, ## Next steps, \
## Open questions. Write \"none\" in a section that has nothing.";

/// A structured handoff.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Handoff {
    /// The task's goal.
    pub goal: String,
    /// Decisions taken and why.
    pub decisions: String,
    /// What is done, in progress or failing.
    pub current_state: String,
    /// Files read or changed (harness-recorded first, then the model's).
    pub files_touched: Vec<String>,
    /// Next steps.
    pub next_steps: String,
    /// Questions for the user.
    pub open_questions: String,
}

fn none_if_empty(s: &str) -> &str {
    if s.trim().is_empty() {
        "none"
    } else {
        s.trim()
    }
}

impl Handoff {
    /// Parses the model's Markdown answer. Missing sections stay empty;
    /// text before the first heading goes to the current state.
    pub fn parse(text: &str) -> Self {
        let mut handoff = Handoff::default();
        let mut current: Option<usize> = None;
        let mut bodies: [String; 6] = Default::default();
        let mut preamble = String::new();
        for line in text.lines() {
            let heading = line.trim_start_matches('#');
            if line.starts_with('#') {
                let title = heading.trim().trim_end_matches(':').to_lowercase();
                if let Some(i) = HANDOFF_SECTIONS
                    .iter()
                    .position(|s| s.to_lowercase() == title)
                {
                    current = Some(i);
                    continue;
                }
            }
            let target = match current {
                Some(i) => &mut bodies[i],
                None => &mut preamble,
            };
            target.push_str(line);
            target.push('\n');
        }
        let [goal, decisions, state, files, next, open] = bodies;
        handoff.goal = goal.trim().to_owned();
        handoff.decisions = decisions.trim().to_owned();
        handoff.current_state = match (preamble.trim(), state.trim()) {
            ("", s) => s.to_owned(),
            (p, "") => p.to_owned(),
            (p, s) => format!("{s}\n{p}"),
        };
        handoff.files_touched = files
            .lines()
            .map(|l| l.trim().trim_start_matches(['-', '*']).trim().to_owned())
            .filter(|l| !l.is_empty() && !l.eq_ignore_ascii_case("none"))
            .collect();
        handoff.next_steps = next.trim().to_owned();
        handoff.open_questions = open.trim().to_owned();
        handoff
    }

    /// Puts `recorded` (from tool calls) first and keeps the model's
    /// entries that do not mention one of them.
    pub fn merge_files(&mut self, recorded: &[String]) {
        let model = std::mem::take(&mut self.files_touched);
        let mut files: Vec<String> = recorded.to_vec();
        for entry in model {
            if !recorded.iter().any(|r| entry.contains(r.as_str())) {
                files.push(entry);
            }
        }
        self.files_touched = files;
    }

    /// The handoff message text sent to the model.
    pub fn render(&self) -> String {
        let files = if self.files_touched.is_empty() {
            "none".to_owned()
        } else {
            self.files_touched
                .iter()
                .map(|f| format!("- {f}"))
                .collect::<Vec<_>>()
                .join("\n")
        };
        format!(
            "{HANDOFF_OPEN}\nThe harness condensed the earlier part of this conversation into \
             this handoff. Continue the task from here.\n\n\
             ## Goal\n{}\n\n## Decisions\n{}\n\n## Current state\n{}\n\n\
             ## Files touched\n{files}\n\n## Next steps\n{}\n\n## Open questions\n{}\n</handoff>",
            none_if_empty(&self.goal),
            none_if_empty(&self.decisions),
            none_if_empty(&self.current_state),
            none_if_empty(&self.next_steps),
            none_if_empty(&self.open_questions),
        )
    }
}

/// What one compaction did.
#[derive(Debug, Clone, PartialEq)]
pub struct CompactionReport {
    /// The compaction child node.
    pub node: NodeId,
    /// Messages before.
    pub messages_before: usize,
    /// Messages after.
    pub messages_after: usize,
    /// Messages summarized.
    pub summarized: usize,
    /// Estimated message tokens before.
    pub tokens_before: u64,
    /// Estimated message tokens after.
    pub tokens_after: u64,
    /// The handoff.
    pub handoff: Handoff,
    /// Usage of the summarization call.
    pub usage: Usage,
    /// Its cost, when known.
    pub cost_usd: Option<f64>,
    /// The summary ran in the background (`[context] background_compaction`)
    /// and was swapped in later; the turn did not wait for the whole call.
    pub background: bool,
}

/// Why compaction did not happen.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum CompactionError {
    /// Everything is within the turns kept verbatim.
    #[error("nothing to compact: the history fits in the turns kept verbatim")]
    NothingToCompact,
    /// The summarization call failed.
    #[error("compaction call failed: {0}")]
    Provider(#[from] ProviderError),
    /// The model returned no text.
    #[error("compaction call returned no handoff text")]
    EmptyHandoff,
    /// The background summary task ended abnormally.
    #[error("background compaction stopped: {0}")]
    Background(String),
}

impl CompactionError {
    /// True when the summarization call was cancelled.
    pub fn is_cancelled(&self) -> bool {
        matches!(self, CompactionError::Provider(e) if matches!(e.root(), ProviderError::Cancelled))
    }
}

/// Summarizes older history into a [`Handoff`].
#[derive(Clone)]
pub struct Compactor {
    provider: Arc<dyn Provider>,
    model: String,
    keep_recent_turns: u32,
    cost: Option<CostFn>,
    max_transcript_chars: usize,
}

impl std::fmt::Debug for Compactor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Compactor")
            .field("provider", &self.provider.name())
            .field("model", &self.model)
            .field("keep_recent_turns", &self.keep_recent_turns)
            .finish_non_exhaustive()
    }
}

fn truncate_middle(text: &str, max: usize) -> String {
    let chars: Vec<char> = text.chars().collect();
    if chars.len() <= max {
        return text.to_owned();
    }
    let half = max / 2;
    let head: String = chars[..half].iter().collect();
    let tail: String = chars[chars.len() - half..].iter().collect();
    format!(
        "{head}\n[... {} characters omitted ...]\n{tail}",
        chars.len() - max
    )
}

/// Renders messages as a plain transcript for the summarization call.
/// Reasoning and opaque provider blocks are left out.
pub fn transcript(messages: &[Message], max_chars: usize) -> String {
    let mut out = String::new();
    for m in messages {
        for b in &m.content {
            let line = match (m.role, b) {
                (Role::User, ContentBlock::Text { text }) => format!("User: {text}"),
                (Role::Assistant, ContentBlock::Text { text }) if !text.is_empty() => {
                    format!("Assistant: {text}")
                }
                (_, ContentBlock::ToolUse { name, input, .. }) => {
                    format!("Assistant called `{name}` with {input}")
                }
                (
                    _,
                    ContentBlock::ToolResult {
                        content, is_error, ..
                    },
                ) => {
                    let text: Vec<&str> = content
                        .iter()
                        .map(|p| match p {
                            ToolResultPart::Text { text } => text.as_str(),
                            ToolResultPart::Image { .. } => "[image]",
                        })
                        .collect();
                    format!(
                        "Tool result{}: {}",
                        if *is_error { " (error)" } else { "" },
                        truncate_middle(&text.join("\n"), RESULT_CHARS)
                    )
                }
                (_, ContentBlock::Image { .. }) => "[image]".to_owned(),
                _ => continue,
            };
            out.push_str(&line);
            out.push_str("\n\n");
        }
    }
    truncate_middle(out.trim_end(), max_chars)
}

/// Paths of `write`/`edit` (changed) and `read` calls, in first-use order.
pub fn files_touched(messages: &[Message]) -> Vec<String> {
    let mut seen = BTreeSet::new();
    let mut changed = Vec::new();
    let mut read = Vec::new();
    for b in messages.iter().flat_map(|m| &m.content) {
        let ContentBlock::ToolUse { name, input, .. } = b else {
            continue;
        };
        let Some(path) = input.get("path").and_then(Value::as_str) else {
            continue;
        };
        match name.as_str() {
            "write" | "edit" if seen.insert(("w", path.to_owned())) => {
                changed.push(format!("{path} (changed)"));
            }
            "read" if seen.insert(("r", path.to_owned())) => read.push(path.to_owned()),
            _ => {}
        }
    }
    let changed_paths: Vec<&str> = changed
        .iter()
        .map(|c| c.trim_end_matches(" (changed)"))
        .collect();
    let read: Vec<String> = read
        .into_iter()
        .filter(|r| !changed_paths.contains(&r.as_str()))
        .map(|r| format!("{r} (read)"))
        .collect();
    changed.into_iter().chain(read).collect()
}

/// Index where the verbatim tail starts: the `keep`-th assistant message
/// from the end, or the user prompt right before it.
pub fn cut_index(messages: &[Message], keep: u32) -> Option<usize> {
    let assistants: Vec<usize> = messages
        .iter()
        .enumerate()
        .filter(|(_, m)| m.role == Role::Assistant)
        .map(|(i, _)| i)
        .collect();
    let keep = usize::try_from(keep).unwrap_or(usize::MAX).max(1);
    if assistants.len() <= keep {
        // Everything but the latest prompt would be summarized only when
        // there is no assistant turn at all to keep.
        return None;
    }
    let mut cut = assistants[assistants.len() - keep];
    if cut > 0 && is_user_prompt(&messages[cut - 1]) {
        cut -= 1;
    }
    (cut > 0).then_some(cut)
}

impl Compactor {
    /// Summarizes with `model` on `provider`, keeping the latest
    /// `keep_recent_turns` assistant turns verbatim.
    pub fn new(
        provider: Arc<dyn Provider>,
        model: impl Into<String>,
        keep_recent_turns: u32,
    ) -> Self {
        Self {
            provider,
            model: model.into(),
            keep_recent_turns,
            cost: None,
            max_transcript_chars: 60_000,
        }
    }

    /// Prices the summarization call.
    pub fn with_cost(mut self, cost: CostFn) -> Self {
        self.cost = Some(cost);
        self
    }

    /// Caps the transcript sent for summarization (default 60 000 chars).
    pub fn with_max_transcript_chars(mut self, max: usize) -> Self {
        self.max_transcript_chars = max;
        self
    }

    /// The summarization request for `older` messages.
    pub fn request(&self, older: &[Message], instructions: Option<&str>) -> ChatRequest {
        let mut prompt = format!(
            "Conversation so far:\n\n{}\n\nWrite the handoff now.",
            transcript(older, self.max_transcript_chars)
        );
        if let Some(extra) = instructions.filter(|s| !s.trim().is_empty()) {
            prompt.push_str("\n\nAlso follow these instructions from the user: ");
            prompt.push_str(extra.trim());
        }
        ChatRequest {
            system: Some(COMPACTION_SYSTEM.to_owned()),
            messages: vec![Message::user(prompt)],
            max_tokens: Some(4096),
            ..ChatRequest::new(self.model.clone())
        }
    }

    fn emit(recorder: &RunRecorder, node: &NodeId, parent: &NodeId, kind: EventKind) {
        let mut draft = EventDraft::node(node.clone(), kind);
        if matches!(draft.kind, EventKind::NodeStarted { .. }) {
            draft = draft.with_parent(parent.clone());
        }
        if let Err(e) = recorder.emit(draft) {
            tracing::warn!(target: "harness::agent", error = %e, "could not record compaction event");
        }
    }

    /// Compacts `conv` in place. Records a `compaction` child node of
    /// `parent` with the summarization call, then replaces the older
    /// history with one handoff message and marks the conversation for
    /// re-logging.
    pub async fn compact(
        &self,
        conv: &mut Conversation,
        recorder: &RunRecorder,
        parent: &NodeId,
        instructions: Option<&str>,
        cancel: &CancellationToken,
    ) -> Result<CompactionReport, CompactionError> {
        let call = self.start(&conv.messages, recorder, parent, instructions)?;
        let cut = call.cut;
        let summary = call.run(cancel.clone()).await?;
        Ok(self.apply(conv, cut, summary))
    }

    /// Starts a summarization of `messages[..cut]`, `cut` from
    /// `keep_recent_turns`: records the `compaction` node and its
    /// `model_call_started` now and returns the call to run. The returned
    /// call owns everything it needs, so it can run in the background.
    pub(crate) fn start(
        &self,
        messages: &[Message],
        recorder: &RunRecorder,
        parent: &NodeId,
        instructions: Option<&str>,
    ) -> Result<SummaryCall, CompactionError> {
        let cut =
            cut_index(messages, self.keep_recent_turns).ok_or(CompactionError::NothingToCompact)?;
        let older = &messages[..cut];
        let request = self.request(older, instructions);
        let node = NodeId::random();
        Self::emit(
            recorder,
            &node,
            parent,
            EventKind::NodeStarted {
                kind: NodeKind::Agent,
                name: COMPACTION_NODE.into(),
                agent: Some(COMPACTION_NODE.into()),
                model: Some(self.model.clone()),
                host: None,
                needs: Vec::new(),
                queued: false,
            },
        );
        let call_id = format!("call_{}", uuid::Uuid::now_v7().simple());
        Self::emit(
            recorder,
            &node,
            parent,
            EventKind::ModelCallStarted {
                call_id: call_id.clone(),
                provider: self.provider.name().to_owned(),
                model: self.model.clone(),
                request: Payload::Json(json!({
                    "purpose": COMPACTION_NODE,
                    "messages_summarized": cut,
                    "messages_kept": messages.len() - cut,
                    "system": request.system,
                    "messages": request.messages,
                })),
            },
        );
        Ok(SummaryCall {
            compactor: self.clone(),
            cut,
            files: files_touched(older),
            request,
            node,
            parent: parent.clone(),
            call_id,
            recorder: recorder.clone(),
        })
    }

    /// Replaces `conv.messages[..cut]` with the handoff of `summary`; the
    /// messages from `cut` on stay verbatim (a leading user prompt is
    /// merged into the handoff message). Marks the conversation for
    /// re-logging.
    pub(crate) fn apply(
        &self,
        conv: &mut Conversation,
        cut: usize,
        summary: Summary,
    ) -> CompactionReport {
        let Summary {
            node,
            handoff,
            usage,
            cost_usd,
        } = summary;
        let messages_before = conv.messages.len();
        let tokens_before = estimate_messages(&conv.messages);
        let kept = conv.messages.split_off(cut);
        let mut first = Message::user(handoff.render());
        let mut rest = kept.into_iter().peekable();
        if rest.peek().is_some_and(|m| m.role == Role::User)
            && let Some(prompt) = rest.next()
        {
            first.content.extend(prompt.content);
        }
        conv.messages = std::iter::once(first).chain(rest).collect();
        conv.invalidate_log_from(0);
        let report = CompactionReport {
            node,
            messages_before,
            messages_after: conv.messages.len(),
            summarized: cut,
            tokens_before,
            tokens_after: estimate_messages(&conv.messages),
            handoff,
            usage,
            cost_usd,
            background: false,
        };
        tracing::info!(
            target: "harness::agent",
            summarized = report.summarized,
            tokens_before = report.tokens_before,
            tokens_after = report.tokens_after,
            "context compacted"
        );
        report
    }
}

/// A finished summarization call, ready to be swapped in with
/// [`Compactor::apply`].
#[derive(Debug, Clone)]
pub(crate) struct Summary {
    node: NodeId,
    handoff: Handoff,
    usage: Usage,
    cost_usd: Option<f64>,
}

impl Summary {
    /// Usage of the summarization call.
    pub(crate) fn usage(&self) -> Usage {
        self.usage
    }

    /// Its cost, when known.
    pub(crate) fn cost_usd(&self) -> Option<f64> {
        self.cost_usd
    }
}

/// A started summarization call (see [`Compactor::start`]).
pub(crate) struct SummaryCall {
    compactor: Compactor,
    /// Index where the verbatim tail starts.
    pub(crate) cut: usize,
    files: Vec<String>,
    request: ChatRequest,
    node: NodeId,
    parent: NodeId,
    call_id: String,
    recorder: RunRecorder,
}

impl SummaryCall {
    /// Makes the call and records its result on the compaction node
    /// (`model_call_finished`, cost, `node_finished`).
    pub(crate) async fn run(self, cancel: CancellationToken) -> Result<Summary, CompactionError> {
        let SummaryCall {
            compactor,
            files,
            request,
            node,
            parent,
            call_id,
            recorder,
            ..
        } = self;
        let emit = |kind| Compactor::emit(&recorder, &node, &parent, kind);
        let start = Instant::now();
        let result = tokio::select! {
            r = compactor.provider.complete(request, cancel.child_token()) => r,
            _ = cancel.cancelled() => Err(ProviderError::Cancelled),
        };
        let millis = u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX);
        let response = match result {
            Ok(r) => r,
            Err(e) => {
                emit(EventKind::ModelCallFinished {
                    call_id,
                    usage: EventUsage::default(),
                    stop_reason: Some("error".into()),
                    response: None,
                    duration_ms: millis,
                });
                let status = if matches!(e.root(), ProviderError::Cancelled) {
                    NodeStatus::Cancelled
                } else {
                    NodeStatus::Failed
                };
                emit(EventKind::NodeFinished {
                    status,
                    error: Some(format!("error: compaction call failed: {e}")),
                });
                return Err(e.into());
            }
        };
        let usage = response.usage;
        emit(EventKind::ModelCallFinished {
            call_id,
            usage: EventUsage {
                input: usage.input_tokens + usage.cache_write_tokens,
                cached_input: usage.cache_read_tokens,
                output: usage.output_tokens,
                reasoning: usage.reasoning_tokens,
                cache_write: usage.cache_write_tokens,
            },
            stop_reason: response.stop.raw.clone(),
            response: Some(Payload::Json(
                serde_json::to_value(&response.message).unwrap_or(Value::Null),
            )),
            duration_ms: millis,
        });
        let cost = harness_providers::openai::notice::reported_cost(&response)
            .or_else(|| compactor.cost.as_ref().and_then(|f| f(&usage)));
        if let Some(usd) = cost {
            emit(EventKind::Cost { usd });
        }
        let text = response.message.text();
        if text.trim().is_empty() {
            emit(EventKind::NodeFinished {
                status: NodeStatus::Failed,
                error: Some("error: compaction call returned no handoff text".into()),
            });
            return Err(CompactionError::EmptyHandoff);
        }
        let mut handoff = Handoff::parse(&text);
        handoff.merge_files(&files);
        emit(EventKind::NodeFinished {
            status: NodeStatus::Succeeded,
            error: None,
        });
        Ok(Summary {
            node,
            handoff,
            usage,
            cost_usd: cost,
        })
    }
}
