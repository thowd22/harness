//! Session lists for the TUI and the CLI, and the optional summary model.
//!
//! - [`picker_rows`] and [`palette_entries`] turn saved sessions into the
//!   `/resume` picker's rows and the palette's session entries: name, last
//!   activity ([`names::activity_label`]), project, model, message count,
//!   cost and description.
//! - [`local_offset`] is the local UTC offset, read once at startup
//!   ([`init_local_offset`]) while the process is still single-threaded,
//!   so relative times (`14:32 today`) use the user's day.
//! - [`summary_model`] and [`summarize`] run the optional cheap-model
//!   summary (`[general] session_summary_model`), recorded as a
//!   [`SUMMARY_NODE`] node with its usage and cost. The core runs it in
//!   the background after a turn. By default names and descriptions are
//!   derived locally and no model is called.

use std::path::Path;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use std::time::Instant;

use harness_agent::agent_loop::{Conversation, CostFn};
use harness_agent::session::{self, SessionMeta, SummaryUpdate};
use harness_config::ini::Config;
use harness_events::names;
use harness_events::redact::Redactor;
use harness_events::schema::{
    EventKind, NodeId, NodeKind, NodeStatus, Payload, Usage as EventUsage,
};
use harness_events::store::{EventDraft, RunRecorder};
use harness_providers::provider::{Provider, ProviderError};
use harness_tui::palette::{EntryKind, PaletteEntry, ids::SESSION_PREFIX};
use harness_tui::session_picker::SessionRow;
use serde_json::json;
use time::{OffsetDateTime, UtcOffset};
use tokio_util::sync::CancellationToken;

/// Saved sessions offered in the palette.
pub const PALETTE_SESSIONS: usize = 10;

/// Longest wait for the optional summary model.
pub const SUMMARY_TIMEOUT: Duration = Duration::from_secs(30);

static OFFSET: OnceLock<UtcOffset> = OnceLock::new();

/// Reads the local UTC offset. Call it first thing in `main`: the OS
/// lookup is refused once other threads exist.
pub fn init_local_offset() {
    let _ = OFFSET.get_or_init(|| UtcOffset::current_local_offset().unwrap_or(UtcOffset::UTC));
}

/// The local UTC offset ([`init_local_offset`]); UTC when it is unknown.
pub fn local_offset() -> UtcOffset {
    *OFFSET.get_or_init(|| UtcOffset::current_local_offset().unwrap_or(UtcOffset::UTC))
}

/// Longest project label in characters; longer paths keep their tail.
const PROJECT_LABEL_MAX: usize = 32;

/// A project path for lists: the home directory as `~`, and a long path
/// shortened to its last components (`…/work/proj`), which identify it.
pub fn project_label(project: &Path) -> String {
    let home = std::env::var_os("HOME").map(std::path::PathBuf::from);
    shorten_project(project, home.as_deref())
}

fn shorten_project(project: &Path, home: Option<&Path>) -> String {
    let full = match home.and_then(|h| project.strip_prefix(h).ok()) {
        Some(rest) if rest.as_os_str().is_empty() => return "~".into(),
        Some(rest) => format!("~/{}", rest.display()),
        None => project.display().to_string(),
    };
    if full.chars().count() <= PROJECT_LABEL_MAX {
        return full;
    }
    // Keep as many trailing components as fit after "…/"; at least one.
    let parts: Vec<&str> = full.split('/').filter(|p| !p.is_empty()).collect();
    let mut tail: Vec<&str> = Vec::new();
    for part in parts.iter().rev() {
        let len =
            tail.iter().map(|p| p.chars().count() + 1).sum::<usize>() + part.chars().count() + 2;
        if !tail.is_empty() && len > PROJECT_LABEL_MAX {
            break;
        }
        tail.insert(0, part);
    }
    let short = format!("…/{}", tail.join("/"));
    if short.chars().count() > PROJECT_LABEL_MAX {
        // A single very long last component: keep its end.
        let keep: String = short
            .chars()
            .rev()
            .take(PROJECT_LABEL_MAX - 1)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        return format!("…{keep}");
    }
    short
}

/// The `/resume` picker's row for `meta`.
pub fn picker_row(
    meta: &SessionMeta,
    current: Option<&str>,
    offset: UtcOffset,
    now: OffsetDateTime,
) -> SessionRow {
    SessionRow {
        id: meta.id.clone(),
        name: meta.display_name(),
        description: meta.display_description(),
        when: names::activity_label(meta.last_activity(), offset, now),
        project: project_label(&meta.project),
        model: meta.model.clone(),
        messages: meta.messages_label(),
        cost: meta.cost_label(),
        tag: meta.tag(),
        current: current == Some(meta.id.as_str()),
    }
}

/// Picker rows for `list` (already newest activity first), without
/// sessions that never got a prompt.
pub fn picker_rows(
    list: &[SessionMeta],
    current: Option<&str>,
    offset: UtcOffset,
    now: OffsetDateTime,
) -> Vec<SessionRow> {
    list.iter()
        .filter(|m| !m.is_empty())
        .map(|m| picker_row(m, current, offset, now))
        .collect()
}

/// Palette entries for the [`PALETTE_SESSIONS`] most recently active
/// sessions other than `current` (and ones without a prompt): the name as the title (the last
/// activity kept visible when it is cut), the details and description
/// below. Choosing one resumes it.
pub fn palette_entries(
    list: &[SessionMeta],
    current: Option<&str>,
    offset: UtcOffset,
    now: OffsetDateTime,
) -> Vec<PaletteEntry> {
    list.iter()
        .filter(|m| current != Some(m.id.as_str()) && !m.is_empty())
        .take(PALETTE_SESSIONS)
        .map(|m| {
            let row = picker_row(m, current, offset, now);
            let tail = format!(" · {}", row.when);
            let title = format!("Resume {}{tail}", row.name);
            let details = [
                row.project.as_str(),
                row.model.as_str(),
                row.messages.as_str(),
                row.cost.as_str(),
            ]
            .join(" · ");
            let description = if row.description.is_empty() {
                details
            } else {
                format!("{} · {details}", row.description)
            };
            PaletteEntry::new(
                format!("{SESSION_PREFIX}{}", m.id),
                title,
                description,
                EntryKind::Session,
            )
            .with_keep_tail(tail.chars().count())
        })
        .collect()
}

/// The optional summary model (`[general] session_summary_model`).
#[derive(Clone)]
pub struct SummaryModel {
    /// Provider serving the model.
    pub provider: Arc<dyn Provider>,
    /// Model id sent in the request.
    pub model: String,
    /// Prices a call's usage; `None` when the model has no price.
    pub cost: Option<CostFn>,
}

impl std::fmt::Debug for SummaryModel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SummaryModel")
            .field("model", &self.model)
            .finish_non_exhaustive()
    }
}

/// Name (and agent) of the child node recorded for each summary call.
pub const SUMMARY_NODE: &str = "session-summary";

/// The summary model of `[general] session_summary_model`, or `None` when
/// it is off (the default) or cannot be used. Scripted sessions use the
/// fake provider.
pub fn summary_model(cfg: &Config, fake: Option<Arc<dyn Provider>>) -> Option<SummaryModel> {
    let model = cfg.general.session_summary_model.clone()?;
    if let Some(f) = fake {
        return Some(SummaryModel {
            provider: f,
            model,
            cost: None,
        });
    }
    let registry = crate::wiring::registry(cfg).ok()?;
    let resolved = registry
        .resolve(&model)
        .map_err(
            |e| tracing::warn!(target: "harness::session", error = %e, "session summaries off"),
        )
        .ok()?;
    let provider = crate::wiring::provider_sync(cfg, &registry, &resolved)
        .map_err(
            |e| tracing::warn!(target: "harness::session", error = %e, "session summaries off"),
        )
        .ok()?;
    Some(SummaryModel {
        provider,
        model: resolved.reference.model.clone(),
        cost: Some(crate::wiring::cost_fn(&resolved)),
    })
}

fn emit(recorder: &RunRecorder, node: &NodeId, parent: &NodeId, kind: EventKind) {
    let mut draft = EventDraft::node(node.clone(), kind);
    if matches!(draft.kind, EventKind::NodeStarted { .. }) {
        draft = draft.with_parent(parent.clone());
    }
    if let Err(e) = recorder.emit(draft) {
        tracing::warn!(target: "harness::session", error = %e, "could not record summary event");
    }
}

/// Asks the summary model for a name and description of `conversation`
/// (redacted first), recorded as a [`SUMMARY_NODE`] child node of
/// `parent` with its model call, usage and cost, like any other call.
///
/// Returns `None` when nothing was sent (empty conversation, or `cancel`
/// already fired). Otherwise the update carries the call's tokens and
/// cost, and a name and description when the reply had them; errors,
/// cancellation and [`SUMMARY_TIMEOUT`] leave the description unset so
/// the locally derived one stays.
pub async fn summarize(
    summary: &SummaryModel,
    conversation: &Conversation,
    redactor: &Redactor,
    recorder: &RunRecorder,
    parent: &NodeId,
    cancel: &CancellationToken,
) -> Option<SummaryUpdate> {
    let transcript = session::summary_transcript(conversation, session::SUMMARY_TRANSCRIPT_CHARS);
    if transcript.is_empty() || cancel.is_cancelled() {
        return None;
    }
    let transcript = redactor.redact(&transcript).into_owned();
    let req = session::summary_request(summary.model.clone(), &transcript);
    let node = NodeId::random();
    emit(
        recorder,
        &node,
        parent,
        EventKind::NodeStarted {
            kind: NodeKind::Agent,
            name: SUMMARY_NODE.into(),
            agent: Some(SUMMARY_NODE.into()),
            model: Some(summary.model.clone()),
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
            provider: summary.provider.name().to_owned(),
            model: summary.model.clone(),
            request: Payload::Json(json!({
                "purpose": SUMMARY_NODE,
                "system": req.system,
                "messages": req.messages,
            })),
        },
    );
    let start = Instant::now();
    let result = tokio::select! {
        r = tokio::time::timeout(SUMMARY_TIMEOUT, summary.provider.complete(req, cancel.child_token())) => {
            r.unwrap_or(Err(ProviderError::Timeout(SUMMARY_TIMEOUT)))
        }
        _ = cancel.cancelled() => Err(ProviderError::Cancelled),
    };
    let millis = u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX);
    let mut update = SummaryUpdate::default();
    let response = match result {
        Ok(r) => r,
        Err(e) => {
            emit(
                recorder,
                &node,
                parent,
                EventKind::ModelCallFinished {
                    call_id,
                    usage: EventUsage::default(),
                    stop_reason: Some("error".into()),
                    response: None,
                    duration_ms: millis,
                },
            );
            let status = if matches!(e.root(), ProviderError::Cancelled) {
                NodeStatus::Cancelled
            } else {
                tracing::warn!(target: "harness::session", error = %e, "session summary failed");
                NodeStatus::Failed
            };
            emit(
                recorder,
                &node,
                parent,
                EventKind::NodeFinished {
                    status,
                    error: Some(format!("error: session summary failed: {e}")),
                },
            );
            return Some(update);
        }
    };
    let usage = response.usage;
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
            },
            stop_reason: response.stop.raw.clone(),
            response: Some(Payload::Json(
                serde_json::to_value(&response.message).unwrap_or(serde_json::Value::Null),
            )),
            duration_ms: millis,
        },
    );
    update.tokens = usage.total();
    update.cost_usd = harness_providers::openai::notice::reported_cost(&response)
        .or_else(|| summary.cost.as_ref().and_then(|f| f(&usage)));
    match update.cost_usd {
        Some(usd) => emit(recorder, &node, parent, EventKind::Cost { usd }),
        None => update.cost_unknown = true,
    }
    let parsed = session::parse_summary(&response.message.text());
    emit(
        recorder,
        &node,
        parent,
        EventKind::NodeFinished {
            status: if parsed.is_some() {
                NodeStatus::Succeeded
            } else {
                NodeStatus::Failed
            },
            error: parsed
                .is_none()
                .then(|| "error: the summary reply had no description".to_owned()),
        },
    );
    if let Some((name, description)) = parsed {
        update.name = name.map(|n| redactor.redact(&n).into_owned());
        update.description = Some(redactor.redact(&description).into_owned());
    }
    Some(update)
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::datetime;

    #[test]
    fn project_labels_use_tilde_and_keep_the_tail_of_long_paths() {
        let home = Path::new("/home/me");
        let label = |p: &str| shorten_project(Path::new(p), Some(home));
        assert_eq!(label("/home/me"), "~");
        assert_eq!(label("/home/me/code/proj"), "~/code/proj");
        assert_eq!(label("/srv/proj"), "/srv/proj");
        // macOS temp dirs: the project's own directories survive.
        let mac = label("/private/var/folders/n6/hn37q6pj7sj_9dmqqc2113800000gn/T/.tmpvU3SXL/proj");
        assert_eq!(mac, "…/T/.tmpvU3SXL/proj");
        assert!(mac.chars().count() <= PROJECT_LABEL_MAX);
        let deep = label("/home/me/work/clients/acme/services/billing-api");
        assert_eq!(deep, "…/acme/services/billing-api");
        // One huge component keeps its end.
        let huge = label(&format!("/x/{}", "a".repeat(80)));
        assert!(
            huge.starts_with('…') && huge.chars().count() == PROJECT_LABEL_MAX,
            "{huge}"
        );
    }

    fn meta(prompt: &str, at: OffsetDateTime) -> SessionMeta {
        let mut m = SessionMeta::new("/srv/proj", "coder", "anthropic/claude-x", prompt);
        m.last_message_at = Some(at);
        m.messages = 4;
        m.cost_usd = 0.5;
        m
    }

    #[test]
    fn rows_and_palette_entries_carry_every_detail() {
        let now = datetime!(2026-10-23 18:00 UTC);
        let a = meta("fix the header parser", datetime!(2026-10-23 14:32 UTC));
        let b = meta("write the docs", datetime!(2026-10-22 09:10 UTC));
        let list = vec![a.clone(), b.clone()];
        let rows = picker_rows(&list, Some(&a.id), UtcOffset::UTC, now);
        assert_eq!(rows[0].name, "Fix header parser");
        assert_eq!(rows[0].when, "14:32 today");
        assert!(rows[0].current && !rows[1].current);
        assert_eq!(rows[1].when, "yesterday 09:10");
        assert_eq!(
            rows[1].details(),
            "yesterday 09:10 · 4 msgs · $0.5000 · anthropic/claude-x · /srv/proj"
        );
        let entries = palette_entries(&list, Some(&a.id), UtcOffset::UTC, now);
        assert_eq!(entries.len(), 1, "the open session is left out");
        let e = &entries[0];
        assert_eq!(e.id, format!("{SESSION_PREFIX}{}", b.id));
        assert_eq!(e.title, "Resume Write docs · yesterday 09:10");
        assert_eq!(e.keep_tail, " · yesterday 09:10".chars().count());
        assert_eq!(
            e.description,
            "write the docs · /srv/proj · anthropic/claude-x · 4 msgs · $0.5000"
        );
        assert_eq!(e.kind, EntryKind::Session);
        assert!(names::find_raw_id(&e.title).is_none());
    }

    #[test]
    fn projects_under_home_are_shortened() {
        let Some(home) = std::env::var_os("HOME").map(std::path::PathBuf::from) else {
            return;
        };
        assert_eq!(project_label(&home.join("code/proj")), "~/code/proj");
        assert_eq!(project_label(&home), "~");
        assert_eq!(project_label(Path::new("/elsewhere/p")), "/elsewhere/p");
    }

    #[test]
    fn summaries_are_off_by_default() {
        let cfg = Config::defaults();
        let fake: Arc<dyn Provider> =
            Arc::new(harness_providers::fake::FakeProvider::from_yaml("responses: []\n").unwrap());
        assert!(summary_model(&cfg, Some(fake.clone())).is_none());
        let mut cfg = Config::defaults();
        cfg.general.session_summary_model = Some("haiku".into());
        let summary = summary_model(&cfg, Some(fake)).unwrap();
        assert_eq!(summary.model, "haiku");
    }

    async fn recorder(dir: &Path) -> RunRecorder {
        RunRecorder::create(
            dir,
            harness_events::schema::RunId::new(),
            Arc::new(Redactor::default()),
            harness_events::store::RecorderOptions::default(),
        )
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn summaries_come_from_the_model_and_are_redacted() {
        let secret = "sk-summary-secret-123456";
        let provider = Arc::new(
            harness_providers::fake::FakeProvider::from_yaml(&format!(
                "responses:\n  - content:\n      - text: \"name: Parser fix\\ndescription: Fixed the parser, key {secret}\"\n    usage: {{ input_tokens: 30, output_tokens: 6 }}\n"
            ))
            .unwrap(),
        );
        let summary = SummaryModel {
            provider: provider.clone(),
            model: "cheap".into(),
            cost: Some(Arc::new(|_| Some(0.0025))),
        };
        let tmp = tempfile::tempdir().unwrap();
        let rec = recorder(tmp.path()).await;
        let mut events = rec.subscribe();
        let parent = NodeId::random();
        let redactor = Redactor::new([secret.to_owned()]);
        let mut conv = Conversation::new(None);
        conv.push_user(format!("fix the parser, key {secret}"));
        let cancel = CancellationToken::new();
        let update = summarize(&summary, &conv, &redactor, &rec, &parent, &cancel)
            .await
            .unwrap();
        assert_eq!(update.name.as_deref(), Some("Parser fix"));
        let description = update.description.clone().unwrap();
        assert!(!description.contains(secret), "{description}");
        assert!(description.starts_with("Fixed the parser"), "{description}");
        assert_eq!((update.tokens, update.cost_usd), (36, Some(0.0025)));
        let sent = provider.requests();
        assert_eq!(sent.len(), 1);
        assert!(
            !serde_json::to_string(&sent[0]).unwrap().contains(secret),
            "the transcript is redacted before it is sent"
        );
        // Recorded like any call: a child node with the call and its cost.
        let mut kinds = Vec::new();
        while let Ok(e) = events.try_recv() {
            assert!(e.parent_node_id.as_ref() == Some(&parent), "{e:?}");
            kinds.push(e.kind);
        }
        assert!(matches!(&kinds[0], EventKind::NodeStarted { name, .. } if name == SUMMARY_NODE));
        assert!(
            kinds
                .iter()
                .any(|k| matches!(k, EventKind::ModelCallStarted { .. }))
        );
        assert!(
            kinds
                .iter()
                .any(|k| matches!(k, EventKind::Cost { usd } if *usd == 0.0025))
        );
        assert!(matches!(
            kinds.last(),
            Some(EventKind::NodeFinished {
                status: NodeStatus::Succeeded,
                ..
            })
        ));

        // An empty conversation, or a cancelled one, makes no call.
        let empty = Conversation::new(None);
        assert!(
            summarize(&summary, &empty, &redactor, &rec, &parent, &cancel)
                .await
                .is_none()
        );
        cancel.cancel();
        assert!(
            summarize(&summary, &conv, &redactor, &rec, &parent, &cancel)
                .await
                .is_none()
        );
        assert_eq!(provider.requests().len(), 1);
    }

    #[tokio::test]
    async fn a_cancel_stops_a_summary_in_flight() {
        let provider = Arc::new(
            harness_providers::fake::FakeProvider::from_yaml(
                "responses:\n  - delay_ms: 60000\n    content: [ { text: \"name: Late\\ndescription: late\" } ]\n",
            )
            .unwrap(),
        );
        let summary = SummaryModel {
            provider,
            model: "cheap".into(),
            cost: None,
        };
        let tmp = tempfile::tempdir().unwrap();
        let rec = recorder(tmp.path()).await;
        let mut events = rec.subscribe();
        let mut conv = Conversation::new(None);
        conv.push_user("fix the parser".to_owned());
        let cancel = CancellationToken::new();
        let stop = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            stop.cancel();
        });
        let update = tokio::time::timeout(
            Duration::from_secs(20),
            summarize(
                &summary,
                &conv,
                &Redactor::default(),
                &rec,
                &NodeId::random(),
                &cancel,
            ),
        )
        .await
        .expect("the cancel ends the call")
        .unwrap();
        assert_eq!(update.description, None, "the local description stays");
        let mut last = None;
        while let Ok(e) = events.try_recv() {
            last = Some(e.kind);
        }
        assert!(matches!(
            last,
            Some(EventKind::NodeFinished {
                status: NodeStatus::Cancelled,
                ..
            })
        ));
    }
}
