use std::sync::atomic::{AtomicBool, Ordering};

use harness_events::redact::Redactor;
use harness_events::schema::{EventKind, RunEvent, RunId};
use harness_events::store::{RecorderOptions, read_run};
use harness_providers::model::{Message, StopKind, StopReason as ModelStop, ToolSpec};
use harness_providers::provider::collect;
use serde_json::json;

use super::*;

/// Records calls; every stream waits for `go` before its first event, and
/// the first stream fails before any output when `fail_first` is set.
struct Held {
    log: Arc<Mutex<Vec<String>>>,
    go: watch::Receiver<bool>,
    fail_first: AtomicBool,
    prewarm: bool,
}

fn label(r: &ChatRequest) -> String {
    r.messages.first().map(Message::text).unwrap_or_default()
}

#[async_trait]
impl Provider for Held {
    fn name(&self) -> &str {
        "held"
    }
    fn capabilities(&self, _model: &str) -> Capabilities {
        Capabilities::default()
    }
    fn can_prewarm_cache(&self, _request: &ChatRequest) -> bool {
        self.prewarm
    }
    async fn prewarm_cache(
        &self,
        request: ChatRequest,
        _cancel: CancellationToken,
    ) -> Result<Usage, ProviderError> {
        assert!(request.messages.is_empty(), "only the shared prefix");
        self.log.lock().unwrap().push("prewarm".into());
        Ok(Usage {
            cache_write_tokens: 4000,
            ..Usage::default()
        })
    }
    async fn stream(
        &self,
        request: ChatRequest,
        _cancel: CancellationToken,
    ) -> Result<EventStream, ProviderError> {
        let name = label(&request);
        self.log.lock().unwrap().push(format!("stream {name}"));
        let fail = self.fail_first.swap(false, Ordering::SeqCst);
        let mut go = self.go.clone();
        Ok(Box::pin(
            futures::stream::once(async move {
                let _ = go.wait_for(|g| *g).await;
                if fail {
                    return vec![Err(ProviderError::Connection("reset".into()))];
                }
                vec![
                    Ok(StreamEvent::MessageStart {
                        id: None,
                        model: None,
                    }),
                    Ok(StreamEvent::TextDelta {
                        index: 0,
                        text: format!("answer to {name}"),
                    }),
                    Ok(StreamEvent::MessageStop {
                        stop: ModelStop::new(StopKind::EndTurn),
                        usage: Usage::default(),
                    }),
                ]
            })
            .flat_map(futures::stream::iter),
        ))
    }
}

fn held(fail_first: bool, prewarm: bool) -> (Arc<Held>, watch::Sender<bool>) {
    let (tx, go) = watch::channel(false);
    (
        Arc::new(Held {
            log: Arc::default(),
            go,
            fail_first: AtomicBool::new(fail_first),
            prewarm,
        }),
        tx,
    )
}

fn request(system: &str, brief: &str) -> ChatRequest {
    let mut r = ChatRequest::new("m");
    r.system = Some(system.into());
    r.tools = vec![ToolSpec {
        name: "read".into(),
        description: "read".into(),
        input_schema: json!({"type": "object"}),
    }];
    r.messages.push(Message::user(brief));
    r
}

async fn settle() {
    for _ in 0..100 {
        tokio::task::yield_now().await;
    }
}

fn spawn(
    p: &Arc<StaggeredProvider>,
    req: ChatRequest,
) -> tokio::task::JoinHandle<Result<String, ProviderError>> {
    let p = p.clone();
    tokio::spawn(async move {
        let r = collect(p.stream(req, CancellationToken::new()).await?).await?;
        Ok(r.message.text())
    })
}

#[tokio::test]
async fn siblings_wait_for_the_first_response_to_begin() {
    let (inner, go) = held(false, false);
    let p = Arc::new(StaggeredProvider::new(
        inner.clone(),
        FanOut::new(false),
        None,
    ));
    let tasks: Vec<_> = ["a", "b", "c"]
        .into_iter()
        .map(|b| spawn(&p, request("sys", b)))
        .collect();
    settle().await;
    assert_eq!(
        inner.log.lock().unwrap().len(),
        1,
        "only the leader is sent while its response has not begun"
    );
    // Another prefix is not held back.
    let other = spawn(&p, request("other sys", "d"));
    settle().await;
    assert_eq!(inner.log.lock().unwrap().len(), 2);
    go.send(true).unwrap();
    let mut answers = Vec::new();
    for t in tasks {
        answers.push(t.await.unwrap().unwrap());
    }
    assert_eq!(answers, ["answer to a", "answer to b", "answer to c"]);
    assert_eq!(other.await.unwrap().unwrap(), "answer to d");
    let log = inner.log.lock().unwrap().clone();
    assert_eq!(log.len(), 4);
    assert_eq!(log[0], "stream a", "the first request leads");
    // Once warm, a later request passes at once.
    let late = spawn(&p, request("sys", "e"));
    assert_eq!(late.await.unwrap().unwrap(), "answer to e");
}

#[tokio::test]
async fn a_failed_leader_hands_the_lead_to_a_waiter() {
    let (inner, go) = held(true, false);
    let p = Arc::new(StaggeredProvider::new(
        inner.clone(),
        FanOut::new(false),
        None,
    ));
    let first = spawn(&p, request("sys", "a"));
    settle().await;
    let rest: Vec<_> = ["b", "c"]
        .into_iter()
        .map(|b| spawn(&p, request("sys", b)))
        .collect();
    settle().await;
    assert_eq!(inner.log.lock().unwrap().len(), 1);
    go.send(true).unwrap();
    assert!(
        first.await.unwrap().is_err(),
        "the leader's error is its own"
    );
    for t in rest {
        assert!(t.await.unwrap().unwrap().starts_with("answer to"));
    }
    assert_eq!(inner.log.lock().unwrap().len(), 3);
}

async fn events(rec: &RunRecorder) -> Vec<RunEvent> {
    rec.flush().await.unwrap();
    read_run(rec.dir()).unwrap().events
}

#[tokio::test]
async fn with_prewarm_the_leader_warms_then_everyone_goes() {
    let dir = tempfile::tempdir().unwrap();
    let rec = RunRecorder::create(
        dir.path(),
        RunId::new(),
        Arc::new(Redactor::default()),
        RecorderOptions::default(),
    )
    .await
    .unwrap();
    let (inner, go) = held(false, true);
    let sink = PrewarmSink {
        recorder: rec.clone(),
        parent: NodeId::from("child-a"),
        cost: Some(Arc::new(|u: &Usage| {
            Some(u.cache_write_tokens as f64 * 1e-6)
        })),
    };
    let gate = FanOut::new(true);
    assert!(gate.prewarms());
    let p = Arc::new(StaggeredProvider::new(inner.clone(), gate, Some(sink)));
    let tasks: Vec<_> = ["a", "b", "c"]
        .into_iter()
        .map(|b| spawn(&p, request("sys", b)))
        .collect();
    settle().await;
    // The pre-warm opened the gate: every request is sent although no
    // response has begun yet.
    let log = inner.log.lock().unwrap().clone();
    assert_eq!(log.len(), 4, "{log:?}");
    assert_eq!(log[0], "prewarm");
    go.send(true).unwrap();
    for (t, b) in tasks.into_iter().zip(["a", "b", "c"]) {
        assert_eq!(t.await.unwrap().unwrap(), format!("answer to {b}"));
    }
    let evs = events(&rec).await;
    let kinds: Vec<&str> = evs
        .iter()
        .map(|e| match &e.kind {
            EventKind::NodeStarted { name, .. } => {
                assert_eq!(name, crate::prewarm::PREWARM_NODE);
                "node_started"
            }
            EventKind::ModelCallStarted { .. } => "model_call_started",
            EventKind::ModelCallFinished { usage, .. } => {
                assert_eq!(usage.cache_write, 4000);
                "model_call_finished"
            }
            EventKind::Cost { .. } => "cost",
            EventKind::NodeFinished { .. } => "node_finished",
            _ => "other",
        })
        .collect();
    assert_eq!(
        kinds,
        [
            "node_started",
            "model_call_started",
            "model_call_finished",
            "cost",
            "node_finished"
        ]
    );
    assert_eq!(
        evs[0].parent_node_id.as_ref().map(|n| n.as_str()),
        Some("child-a")
    );
}

#[test]
fn the_key_covers_the_cached_prefix_only() {
    let a = FanOut::key("p", &request("sys", "one"));
    assert_eq!(
        a,
        FanOut::key("p", &request("sys", "two")),
        "messages differ"
    );
    assert_ne!(a, FanOut::key("q", &request("sys", "one")));
    assert_ne!(a, FanOut::key("p", &request("other", "one")));
    let mut r = request("sys", "one");
    r.tools.clear();
    assert_ne!(a, FanOut::key("p", &r));
    let mut r = request("sys", "one");
    r.model = "n".into();
    assert_ne!(a, FanOut::key("p", &r));
}

async fn recorder(dir: &std::path::Path) -> RunRecorder {
    RunRecorder::create(
        dir,
        RunId::new(),
        Arc::new(Redactor::default()),
        RecorderOptions::default(),
    )
    .await
    .unwrap()
}

fn priced_sink(rec: &RunRecorder) -> PrewarmSink {
    PrewarmSink {
        recorder: rec.clone(),
        parent: NodeId::from("child-a"),
        // 4000 cache-write tokens at $1 per million: $0.004.
        cost: Some(Arc::new(|u: &Usage| {
            Some(u.cache_write_tokens as f64 * 1e-6)
        })),
    }
}

#[tokio::test]
async fn the_leaders_prewarm_is_charged_to_the_budget() {
    use crate::budget::{BudgetMeter, BudgetScope, Budgets};
    let dir = tempfile::tempdir().unwrap();
    let rec = recorder(dir.path()).await;
    let (inner, go) = held(false, true);
    let meter = BudgetMeter::new(Budgets::new().with_limit(BudgetScope::Session, Some(1.0)));
    let gate = FanOut::new(true);
    gate.charge_to(meter.clone());
    let p = Arc::new(StaggeredProvider::new(
        inner.clone(),
        gate,
        Some(priced_sink(&rec)),
    ));
    let tasks: Vec<_> = ["a", "b"]
        .into_iter()
        .map(|b| spawn(&p, request("sys", b)))
        .collect();
    settle().await;
    go.send(true).unwrap();
    for t in tasks {
        t.await.unwrap().unwrap();
    }
    assert_eq!(inner.log.lock().unwrap()[0], "prewarm");
    for scope in [BudgetScope::Session, BudgetScope::Run, BudgetScope::Day] {
        assert!(
            (meter.spent(scope) - 0.004).abs() < 1e-12,
            "{scope:?}: {}",
            meter.spent(scope)
        );
    }
}

#[tokio::test]
async fn no_prewarm_while_a_budget_is_exhausted() {
    use crate::budget::{BudgetMeter, BudgetScope, Budgets};
    let dir = tempfile::tempdir().unwrap();
    let rec = recorder(dir.path()).await;
    let (inner, go) = held(false, true);
    let meter = BudgetMeter::new(Budgets::new().with_limit(BudgetScope::Session, Some(0.01)));
    meter.charge(0.02);
    let gate = FanOut::new(true);
    gate.charge_to(meter.clone());
    let p = Arc::new(StaggeredProvider::new(
        inner.clone(),
        gate,
        Some(priced_sink(&rec)),
    ));
    go.send(true).unwrap();
    assert_eq!(
        spawn(&p, request("sys", "a")).await.unwrap().unwrap(),
        "answer to a"
    );
    assert_eq!(*inner.log.lock().unwrap(), ["stream a"], "no pre-warm sent");
    assert!((meter.spent(BudgetScope::Session) - 0.02).abs() < 1e-12);
    assert!(events(&rec).await.is_empty(), "nothing recorded");
}
