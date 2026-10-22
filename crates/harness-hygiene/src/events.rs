//! Where hygiene components send their run events.
//!
//! The ledger, verify runner and bug-fix proof all describe what they do as
//! [`harness_events::schema::EventKind`]s. They emit them through the small
//! [`EventSink`] trait so they can be used with a real
//! [`harness_events::store::RunRecorder`] or, in tests, with
//! [`RecordingSink`].

use std::sync::Mutex;

use harness_events::schema::{EventKind, NodeId};
use harness_events::store::{EventDraft, RunRecorder};

/// Receives events produced by hygiene components.
pub trait EventSink: Send + Sync {
    /// Record one event.
    fn emit(&self, draft: EventDraft);
}

/// Discards events.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoEvents;

impl EventSink for NoEvents {
    fn emit(&self, _: EventDraft) {}
}

impl EventSink for RunRecorder {
    fn emit(&self, draft: EventDraft) {
        let ty = draft.kind.type_name();
        if let Err(e) = RunRecorder::emit(self, draft) {
            tracing::warn!(target: "harness::hygiene", "failed to record {ty} event: {e}");
        }
    }
}

/// Keeps events in memory (tests and previews).
#[derive(Debug, Default)]
pub struct RecordingSink(Mutex<Vec<EventDraft>>);

impl RecordingSink {
    /// Everything emitted so far.
    pub fn drafts(&self) -> Vec<EventDraft> {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// The event kinds emitted so far, in order.
    pub fn kinds(&self) -> Vec<EventKind> {
        self.drafts().into_iter().map(|d| d.kind).collect()
    }
}

impl EventSink for RecordingSink {
    fn emit(&self, draft: EventDraft) {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).push(draft);
    }
}

/// A draft for `node` (or a run-level draft when `node` is `None`).
pub(crate) fn draft(node: Option<&NodeId>, kind: EventKind) -> EventDraft {
    match node {
        Some(n) => EventDraft::node(n.clone(), kind),
        None => EventDraft::run(kind),
    }
}
