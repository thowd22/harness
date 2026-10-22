//! Recent run transcripts, the input for "add missed lessons".
//!
//! Transcripts are rebuilt from the run store: each `model_call_started`
//! carries the new (redacted) request messages and each
//! `model_call_finished` the assistant reply. Only main-agent and subagent
//! model calls are read; nothing else in the log is needed.

use std::path::Path;

use harness_events::blobs::BlobStore;
use harness_events::schema::{EventKind, Payload, RunId};
use harness_events::store::{list_runs, read_run, run_dir};
use harness_providers::model::Message;
use time::OffsetDateTime;

/// One run's conversation.
#[derive(Debug, Clone, PartialEq)]
pub struct RunTranscript {
    /// The run id (used for source links of lessons).
    pub run_id: String,
    /// When the run started.
    pub started_at: Option<OffsetDateTime>,
    /// Messages in order.
    pub messages: Vec<Message>,
}

fn payload_json(p: &Payload, blobs: &BlobStore) -> Option<serde_json::Value> {
    match blobs.resolve(p).ok()? {
        Payload::Json(v) => Some(v),
        Payload::Text(t) => serde_json::from_str(&t).ok(),
        Payload::Blob(_) => None,
    }
}

/// Rebuilds the transcript of the run in `dir`.
pub fn transcript(dir: &Path) -> Option<RunTranscript> {
    let replay = read_run(dir).ok()?;
    let blobs = BlobStore::new(dir.join(harness_events::blobs::BLOBS_DIR));
    let mut messages = Vec::new();
    let mut started_at = None;
    for e in &replay.events {
        match &e.kind {
            EventKind::RunStarted { .. } => started_at = Some(e.ts),
            EventKind::ModelCallStarted { request, .. } => {
                if let Some(v) = payload_json(request, &blobs)
                    && let Some(msgs) = v.get("messages")
                    && let Ok(msgs) = serde_json::from_value::<Vec<Message>>(msgs.clone())
                {
                    messages.extend(msgs);
                }
            }
            EventKind::ModelCallFinished {
                response: Some(resp),
                ..
            } => {
                if let Some(v) = payload_json(resp, &blobs)
                    && let Ok(m) = serde_json::from_value::<Message>(v)
                {
                    messages.push(m);
                }
            }
            _ => {}
        }
    }
    Some(RunTranscript {
        run_id: replay.header.run_id.to_string(),
        started_at: started_at.or(Some(replay.header.created_at)),
        messages,
    })
}

/// The newest `limit` runs under `data_dir` that started after `since`
/// (all runs when `None`), oldest first. Runs without messages are skipped.
pub fn recent(data_dir: &Path, since: Option<OffsetDateTime>, limit: usize) -> Vec<RunTranscript> {
    let mut ids: Vec<RunId> = list_runs(data_dir).unwrap_or_default();
    // Run ids are v7 UUIDs: lexical order is creation order.
    ids.sort_by_key(|r| r.to_string());
    let mut out: Vec<RunTranscript> = ids
        .into_iter()
        .rev()
        .filter_map(|id| transcript(&run_dir(data_dir, id)))
        .filter(|t| !t.messages.is_empty())
        .filter(|t| match (since, t.started_at) {
            (Some(s), Some(at)) => at > s,
            _ => true,
        })
        .take(limit)
        .collect();
    out.reverse();
    out
}

/// When the newest run under `data_dir` started (activity marker).
pub fn last_activity(data_dir: &Path) -> Option<OffsetDateTime> {
    let mut ids = list_runs(data_dir).unwrap_or_default();
    ids.sort_by_key(|r| r.to_string());
    ids.into_iter()
        .rev()
        .find_map(|id| read_run(&run_dir(data_dir, id)).ok())
        .map(|r| r.header.created_at)
}
