//! Audit trail and secret guard for remote operations.
//!
//! Every RPC the client sends produces one [`AuditEvent`] tagged with the
//! host, delivered to an [`AuditSink`]. [`TracingAudit`] writes them to the
//! log (target [`AUDIT_TARGET`]); the agent layer can implement the trait
//! over its run recorder.
//!
//! [`SecretGuard`] holds the resolved harness credentials and refuses any
//! outgoing frame or payload that contains one, so secrets never reach a
//! remote host.

use std::sync::Mutex;
use std::time::{Duration, SystemTime};

/// tracing target used by [`TracingAudit`].
pub const AUDIT_TARGET: &str = "harness::remote::audit";

/// Secrets shorter than this are not guarded (too many false positives).
pub const MIN_GUARDED_LEN: usize = harness_tools::bash::MIN_SECRET_VALUE_LEN;

/// One remote operation.
#[derive(Debug, Clone, PartialEq)]
pub struct AuditEvent {
    /// `[remote.<name>]` name.
    pub host: String,
    /// Protocol method (`fs.read`, `exec.start`, `connect`, ...).
    pub op: String,
    /// Path, program or other target, when there is one.
    pub target: Option<String>,
    /// Whether it succeeded.
    pub ok: bool,
    /// Error message on failure.
    pub error: Option<String>,
    /// How long it took.
    pub duration: Duration,
    /// When it started.
    pub at: SystemTime,
    /// Bytes not sent because the other side already held the content
    /// (content-hash cache, [`crate::cache`]).
    pub bytes_saved: u64,
}

/// Receives audit events.
pub trait AuditSink: Send + Sync {
    /// Record one event.
    fn record(&self, event: &AuditEvent);
}

/// Discards events.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoAudit;

impl AuditSink for NoAudit {
    fn record(&self, _event: &AuditEvent) {}
}

/// Writes events to the log.
#[derive(Debug, Clone, Copy, Default)]
pub struct TracingAudit;

impl AuditSink for TracingAudit {
    fn record(&self, e: &AuditEvent) {
        tracing::info!(
            target: AUDIT_TARGET,
            host = %e.host,
            op = %e.op,
            target_path = e.target.as_deref().unwrap_or(""),
            ok = e.ok,
            error = e.error.as_deref().unwrap_or(""),
            duration_ms = e.duration.as_millis() as u64,
            bytes_saved = e.bytes_saved,
            "remote op"
        );
    }
}

/// Keeps events in memory (tests, `/remote` detail view).
#[derive(Debug, Default)]
pub struct RecordingAudit {
    events: Mutex<Vec<AuditEvent>>,
}

impl RecordingAudit {
    /// Recorded events so far.
    pub fn events(&self) -> Vec<AuditEvent> {
        self.events
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
}

impl AuditSink for RecordingAudit {
    fn record(&self, event: &AuditEvent) {
        self.events
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(event.clone());
    }
}

/// Refuses outgoing data that contains a harness credential.
#[derive(Clone, Default)]
pub struct SecretGuard {
    values: Vec<String>,
}

impl std::fmt::Debug for SecretGuard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "SecretGuard([{} redacted])", self.values.len())
    }
}

impl SecretGuard {
    /// Guard against `values` (e.g. every resolved `api_key`); values
    /// shorter than [`MIN_GUARDED_LEN`] are ignored.
    pub fn new(values: impl IntoIterator<Item = String>) -> Self {
        Self {
            values: values
                .into_iter()
                .filter(|v| v.len() >= MIN_GUARDED_LEN)
                .collect(),
        }
    }

    /// True when no secret is guarded.
    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    /// True if `data` contains a guarded secret.
    pub fn contains_secret(&self, data: &[u8]) -> bool {
        self.values.iter().any(|s| {
            let s = s.as_bytes();
            data.windows(s.len()).any(|w| w == s)
        })
    }

    /// True if `text` contains a guarded secret.
    pub fn contains_secret_str(&self, text: &str) -> bool {
        self.values.iter().any(|s| text.contains(s.as_str()))
    }
}

/// Writes every remote operation to a run log as `remote_op` events,
/// tagged with the host (and the node that ran it, when known).
pub struct RecorderAudit {
    recorder: harness_events::store::RunRecorder,
    node: Option<harness_events::schema::NodeId>,
}

impl RecorderAudit {
    /// Records to `recorder`, on `node` when given.
    pub fn new(
        recorder: harness_events::store::RunRecorder,
        node: Option<harness_events::schema::NodeId>,
    ) -> Self {
        Self { recorder, node }
    }
}

impl AuditSink for RecorderAudit {
    fn record(&self, event: &AuditEvent) {
        let kind = harness_events::schema::EventKind::RemoteOp {
            host: event.host.clone(),
            op: event.op.clone(),
            target: event.target.clone().unwrap_or_default(),
            ok: event.ok,
            error: event.error.clone(),
            duration_ms: u64::try_from(event.duration.as_millis()).unwrap_or(u64::MAX),
            bytes_saved: event.bytes_saved,
        };
        let draft = match &self.node {
            Some(n) => harness_events::store::EventDraft::node(n.clone(), kind),
            None => harness_events::store::EventDraft::run(kind),
        };
        if let Err(e) = self.recorder.emit(draft) {
            tracing::debug!(target: "harness::remote::audit", error = %e, "could not record remote op");
        }
    }
}

/// The `remote_connection` event for a connection change.
pub fn connection_event(ev: &crate::client::ConnectionEvent) -> harness_events::schema::EventKind {
    use crate::client::ConnectionState as S;
    let (state, detail) = match &ev.state {
        S::Connecting => ("connecting", None),
        S::Connected {
            reconnected,
            reattached,
        } => (
            "connected",
            reconnected.then(|| format!("reconnected; {reattached} jobs reattached")),
        ),
        S::Disconnected { reason } => ("disconnected", Some(reason.clone())),
        S::Reconnecting { attempt, max } => {
            ("reconnecting", Some(format!("attempt {attempt}/{max}")))
        }
        S::Failed { reason } => ("failed", Some(reason.clone())),
        S::JobLost { job } => ("job_lost", Some(job.clone())),
    };
    harness_events::schema::EventKind::RemoteConnection {
        host: ev.host.clone(),
        state: state.to_owned(),
        detail,
    }
}

/// Records `client`'s connection changes to `recorder` until either closes.
pub fn record_connection(
    client: &crate::client::RemoteClient,
    recorder: harness_events::store::RunRecorder,
) -> tokio::task::JoinHandle<()> {
    record_connection_on(client, recorder, None)
}

/// [`record_connection`] on `node` (run-level when `None`).
pub fn record_connection_on(
    client: &crate::client::RemoteClient,
    recorder: harness_events::store::RunRecorder,
    node: Option<harness_events::schema::NodeId>,
) -> tokio::task::JoinHandle<()> {
    let mut rx = client.subscribe();
    tokio::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(ev) => {
                    let kind = connection_event(&ev);
                    let draft = match &node {
                        Some(n) => harness_events::store::EventDraft::node(n.clone(), kind),
                        None => harness_events::store::EventDraft::run(kind),
                    };
                    if recorder.emit(draft).is_err() {
                        return;
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guard_detects_secrets() {
        let g = SecretGuard::new(["sk-ant-1234567890".to_string(), "ab".to_string()]);
        assert!(g.contains_secret(b"key=sk-ant-1234567890;"));
        assert!(g.contains_secret_str("x sk-ant-1234567890"));
        assert!(!g.contains_secret(b"ab ab ab"), "short values ignored");
        assert!(!format!("{g:?}").contains("sk-ant"));
        assert!(SecretGuard::default().is_empty());
    }

    #[test]
    fn recording_sink() {
        let r = RecordingAudit::default();
        let e = AuditEvent {
            host: "h".into(),
            op: "fs.read".into(),
            target: Some("/x".into()),
            ok: true,
            error: None,
            duration: Duration::ZERO,
            at: SystemTime::now(),
            bytes_saved: 0,
        };
        r.record(&e);
        TracingAudit.record(&e);
        NoAudit.record(&e);
        assert_eq!(r.events(), vec![e]);
    }
}
