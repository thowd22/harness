//! Reasoning retention across turns.
//!
//! By default every reasoning block stays in the history exactly as the
//! provider returned it: Anthropic needs signed thinking blocks of the
//! current tool loop sent back unchanged, OpenAI needs its encrypted
//! reasoning items (`Opaque{provider: "openai"}`) and OpenRouter its
//! `reasoning_details`. Dropping them silently degrades quality, so any
//! clearing is an explicit setting.

use harness_providers::model::{ContentBlock, Message, Role};

use crate::agent_loop::hooks::RequestContext;

/// How reasoning blocks are kept in the history.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ReasoningRetention {
    /// Keep every reasoning block (default).
    #[default]
    KeepAll,
    /// Drop reasoning blocks (and provider reasoning items) of assistant
    /// messages that come before the latest user prompt, i.e. of finished
    /// turns. The current tool loop is never touched. This changes the
    /// history once per user prompt, never inside a tool loop.
    DropCompletedTurns,
}

/// True for blocks that carry model reasoning.
pub fn is_reasoning_block(block: &ContentBlock) -> bool {
    match block {
        ContentBlock::Reasoning { .. } => true,
        ContentBlock::Opaque { provider, data } => {
            (provider == "openai" && data.get("type").and_then(|t| t.as_str()) == Some("reasoning"))
                || (provider == "openrouter" && data.get("reasoning_details").is_some())
        }
        _ => false,
    }
}

/// True when `message` is a user prompt (has text, not only tool results).
pub fn is_user_prompt(message: &Message) -> bool {
    message.role == Role::User
        && message
            .content
            .iter()
            .any(|b| matches!(b, ContentBlock::Text { .. }))
}

impl ReasoningRetention {
    /// Applies the policy; returns the index of the first changed message.
    pub fn apply(self, messages: &mut [Message]) -> Option<usize> {
        if self == Self::KeepAll {
            return None;
        }
        let last_prompt = messages.iter().rposition(is_user_prompt)?;
        let mut first = None;
        for (i, m) in messages[..last_prompt].iter_mut().enumerate() {
            if m.role != Role::Assistant {
                continue;
            }
            let before = m.content.len();
            m.content.retain(|b| !is_reasoning_block(b));
            if m.content.len() != before {
                first.get_or_insert(i);
            }
            if m.content.is_empty() {
                m.content.push(ContentBlock::text(""));
            }
        }
        first
    }

    /// Applies the policy to the conversation of a request hook and marks
    /// changed messages for re-logging.
    pub fn apply_to(self, ctx: &mut RequestContext<'_>) -> Option<usize> {
        let first = self.apply(&mut ctx.conversation.messages)?;
        ctx.conversation.invalidate_log_from(first);
        Some(first)
    }
}
