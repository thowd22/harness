//! Reminds an agent to plan with `todo` when it works without a task list.
//!
//! Models often skip the `todo` tool even when the prompt asks for it, which
//! leaves the goal bar empty. [`TodoNudge`] is a [`RequestHook`]: once an
//! agent that has the tool has made `after` model calls in one turn and the
//! run's board is still empty, it appends [`NUDGE_TEXT`] to the newest
//! tool-result message. That message has not been sent yet, so earlier
//! requests (and their prompt cache) are unchanged, and the reminder is
//! recorded with the request that carries it. It repeats at most every
//! `after` calls and stops once any task exists.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use harness_events::schema::NodeId;
use harness_providers::model::{ContentBlock, Role};

use super::{NAME, TaskBoard};
use crate::agent_loop::{HookDecision, RequestContext, RequestHook};

/// The reminder appended to the agent's next request.
pub const NUDGE_TEXT: &str = "Reminder from harness: this task has several steps and the \
todo list is empty. Add the steps with the `todo` tool now (action: add), then mark each \
in_progress and done as you go. The user follows progress in the goal bar. Skip this only \
if the task is a single step.";

/// Request hook that nudges the agent to use `todo` (see the module docs).
#[derive(Debug)]
pub struct TodoNudge {
    board: Arc<TaskBoard>,
    after: u32,
    /// Node of the current turn and the model-call count of its last nudge.
    last: Mutex<Option<(NodeId, u32)>>,
}

impl TodoNudge {
    /// Nudges after `after` model calls with an empty `board`; 0 never nudges
    /// (`[general] todo_nudge_after`).
    pub fn new(board: Arc<TaskBoard>, after: u32) -> Self {
        Self {
            board,
            after,
            last: Mutex::new(None),
        }
    }
}

#[async_trait]
impl RequestHook for TodoNudge {
    fn name(&self) -> &str {
        "todo-nudge"
    }

    async fn before_request(&self, ctx: &mut RequestContext<'_>) -> HookDecision {
        if self.after == 0
            || ctx.turn < self.after
            || !ctx.tools.iter().any(|t| t.name == NAME)
            || self.board.snapshot().has_tasks()
        {
            return HookDecision::Continue;
        }
        let mut last = self.last.lock().unwrap_or_else(|e| e.into_inner());
        let since = match &*last {
            Some((node, at)) if node == ctx.node => ctx.turn.saturating_sub(*at),
            _ => ctx.turn,
        };
        if since < self.after {
            return HookDecision::Continue;
        }
        // Only mid-work: the newest message carries tool results and has not
        // been sent yet.
        let conv = &mut *ctx.conversation;
        if conv.messages.len() <= conv.unlogged_from() {
            return HookDecision::Continue;
        }
        let Some(msg) = conv.messages.last_mut() else {
            return HookDecision::Continue;
        };
        let has_results = msg
            .content
            .iter()
            .any(|b| matches!(b, ContentBlock::ToolResult { .. }));
        if msg.role != Role::User || !has_results {
            return HookDecision::Continue;
        }
        msg.content.push(ContentBlock::text(NUDGE_TEXT));
        *last = Some((ctx.node.clone(), ctx.turn));
        HookDecision::Continue
    }
}
