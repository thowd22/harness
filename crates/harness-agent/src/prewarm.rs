//! Prompt-cache pre-warm calls (`[experimental] cache_prewarm`).
//!
//! A pre-warm writes the prompt cache for the prefix of the next request
//! (tools, system prompt, and the conversation so far) with a call that
//! generates nothing ([`Provider::prewarm_cache`]; Anthropic sends
//! `max_tokens: 0`), so the next real request reads the prefix instead of
//! writing it. It is worth it when there is idle time before that request:
//! at session start and resume (while the user types), after `/compact`,
//! and before a subagent fan-out ([`crate::stagger`]).
//!
//! [`prewarm`] records the call as a [`PREWARM_NODE`] child node with its
//! model call, usage and cost, like the session summary, so budgets and
//! cost views count it. It never touches the conversation: answers are the
//! same with or without it.

use std::time::{Duration, Instant};

use harness_events::schema::{
    EventKind, NodeId, NodeKind, NodeStatus, Payload, Usage as EventUsage,
};
use harness_events::store::{EventDraft, RunRecorder};
use harness_providers::model::{ChatRequest, ToolSpec, Usage};
use harness_providers::provider::{Provider, ProviderError};
use serde_json::json;
use tokio_util::sync::CancellationToken;

use crate::agent_loop::{Conversation, CostFn, LoopConfig};

/// Node name (and agent name) of a pre-warm call.
pub const PREWARM_NODE: &str = "cache-prewarm";

/// What a pre-warm cost.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PrewarmReport {
    /// The call's usage (normally all cache writes or reads, no output).
    pub usage: Usage,
    /// Known cost in USD.
    pub cost_usd: Option<f64>,
    /// Wall time of the call.
    pub duration: Duration,
}

/// The request whose prefix a pre-warm writes: what the agent loop would
/// send next for `conversation` with `tools`, minus anything new.
pub fn prewarm_request(
    config: &LoopConfig,
    conversation: &Conversation,
    tools: Vec<ToolSpec>,
) -> ChatRequest {
    let mut r = ChatRequest::new(config.model.clone());
    r.system.clone_from(&conversation.system);
    r.system_layers.clone_from(&conversation.system_layers);
    r.messages.clone_from(&conversation.messages);
    r.tools = tools;
    r.tool_choice = config.tool_choice.clone();
    r.max_tokens = config.max_tokens;
    r.thinking = config.thinking;
    r
}

fn emit(recorder: &RunRecorder, node: &NodeId, parent: &NodeId, kind: EventKind) {
    let mut draft = EventDraft::node(node.clone(), kind);
    if matches!(draft.kind, EventKind::NodeStarted { .. }) {
        draft = draft.with_parent(parent.clone());
    }
    if let Err(e) = recorder.emit(draft) {
        tracing::warn!(target: "harness::prewarm", error = %e, "could not record pre-warm event");
    }
}

/// Pre-warms the cache for `request` through `provider`, recorded as a
/// [`PREWARM_NODE`] child of `parent`; `reason` (`session_start`,
/// `compaction`, `fan_out`) goes into the call's request payload.
///
/// `None` when the provider cannot pre-warm this request (nothing is sent
/// or recorded). A failed pre-warm is recorded and returned; callers carry
/// on, since the real request simply writes the cache itself.
pub async fn prewarm(
    provider: &dyn Provider,
    request: ChatRequest,
    recorder: &RunRecorder,
    parent: &NodeId,
    reason: &str,
    cost: Option<&CostFn>,
    cancel: &CancellationToken,
) -> Option<Result<PrewarmReport, ProviderError>> {
    if cancel.is_cancelled() || !provider.can_prewarm_cache(&request) {
        return None;
    }
    let node = NodeId::random();
    emit(
        recorder,
        &node,
        parent,
        EventKind::NodeStarted {
            kind: NodeKind::Agent,
            name: PREWARM_NODE.into(),
            agent: Some(PREWARM_NODE.into()),
            model: Some(request.model.clone()),
            host: None,
            needs: Vec::new(),
            queued: false,
        },
    );
    let call_id = format!("call_{node}");
    emit(
        recorder,
        &node,
        parent,
        EventKind::ModelCallStarted {
            call_id: call_id.clone(),
            provider: provider.name().to_owned(),
            model: request.model.clone(),
            request: Payload::Json(json!({
                "purpose": PREWARM_NODE,
                "reason": reason,
                "shared_messages": request.messages.len(),
                "tools": request.tools.len(),
            })),
        },
    );
    let start = Instant::now();
    let result = tokio::select! {
        r = provider.prewarm_cache(request, cancel.child_token()) => r,
        _ = cancel.cancelled() => Err(ProviderError::Cancelled),
    };
    let duration = start.elapsed();
    let duration_ms = u64::try_from(duration.as_millis()).unwrap_or(u64::MAX);
    let usage = result.as_ref().copied().unwrap_or_default();
    emit(
        recorder,
        &node,
        parent,
        EventKind::ModelCallFinished {
            call_id,
            usage: EventUsage {
                input: usage.input_tokens + usage.cache_write_tokens,
                cached_input: usage.cache_read_tokens,
                output: usage.output_tokens,
                reasoning: usage.reasoning_tokens,
                cache_write: usage.cache_write_tokens,
            },
            stop_reason: Some(match &result {
                Ok(_) => "max_tokens".into(),
                Err(e) if matches!(e.root(), ProviderError::Cancelled) => "cancelled".into(),
                Err(_) => "error".into(),
            }),
            response: None,
            duration_ms,
        },
    );
    let report = result.map(|usage| PrewarmReport {
        usage,
        cost_usd: cost.and_then(|f| f(&usage)),
        duration,
    });
    if let Ok(PrewarmReport {
        cost_usd: Some(usd),
        ..
    }) = &report
    {
        emit(recorder, &node, parent, EventKind::Cost { usd: *usd });
    }
    let (status, error) = match &report {
        Ok(_) => (NodeStatus::Succeeded, None),
        Err(e) if matches!(e.root(), ProviderError::Cancelled) => (
            NodeStatus::Cancelled,
            Some(format!("cancelled: cache pre-warm cancelled: {e}")),
        ),
        Err(e) => {
            tracing::debug!(target: "harness::prewarm", error = %e, "cache pre-warm failed");
            (
                NodeStatus::Failed,
                Some(format!("error: cache pre-warm failed: {e}")),
            )
        }
    };
    emit(
        recorder,
        &node,
        parent,
        EventKind::NodeFinished { status, error },
    );
    Some(report)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use harness_events::redact::Redactor;
    use harness_events::schema::RunId;
    use harness_events::store::{RecorderOptions, read_run};
    use harness_providers::fake::FakeProvider;
    use harness_providers::model::{Message, ThinkingConfig, ToolChoice};

    use super::*;

    #[tokio::test]
    async fn unsupported_providers_send_and_record_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let rec = RunRecorder::create(
            dir.path(),
            RunId::new(),
            Arc::new(Redactor::default()),
            RecorderOptions::default(),
        )
        .await
        .unwrap();
        let fake = FakeProvider::with_turns([]);
        let out = prewarm(
            &fake,
            ChatRequest::new("m"),
            &rec,
            &NodeId::from("p"),
            "session_start",
            None,
            &CancellationToken::new(),
        )
        .await;
        assert!(out.is_none());
        assert!(fake.requests().is_empty());
        rec.flush().await.unwrap();
        assert!(read_run(rec.dir()).unwrap().events.is_empty());
    }

    #[test]
    fn the_request_is_the_next_one_minus_anything_new() {
        let mut config = LoopConfig::new("coder", "claude-opus-5-5");
        config.max_tokens = Some(64_000);
        config.thinking = Some(ThinkingConfig {
            budget_tokens: None,
            effort: Some(harness_providers::model::ReasoningEffort::High),
        });
        let mut conv = Conversation::new(Some("sys".into()));
        conv.system_layers = vec![1];
        conv.messages.push(Message::user("summary"));
        let r = prewarm_request(&config, &conv, Vec::new());
        assert_eq!(r.model, "claude-opus-5-5");
        assert_eq!(r.system.as_deref(), Some("sys"));
        assert_eq!(r.system_layers, [1]);
        assert_eq!(r.messages, conv.messages);
        assert_eq!(r.max_tokens, Some(64_000));
        assert_eq!(r.thinking, config.thinking);
        assert_eq!(r.tool_choice, ToolChoice::Auto);
    }
}
