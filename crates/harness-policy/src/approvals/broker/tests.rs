use super::*;
use crate::rules::{Decision, Mode, PolicyConfig, PolicyEngine};
use harness_events::redact::Redactor;
use harness_events::reducer::RunState;
use harness_events::schema::{NodeKind, NodeStatus, RunId};
use harness_events::store::{RecorderOptions, read_run};
use std::path::Path;

type AnswerFn = Box<dyn Fn(&ApprovalRequest) -> Option<ApprovalAnswer> + Send + Sync>;

/// Answers every request with a fixed function and records prompts.
struct ScriptedUi {
    prompts: Mutex<Vec<ApprovalPrompt>>,
    answer: AnswerFn,
}

impl ScriptedUi {
    fn new(
        answer: impl Fn(&ApprovalRequest) -> Option<ApprovalAnswer> + Send + Sync + 'static,
    ) -> Arc<Self> {
        Arc::new(Self {
            prompts: Mutex::new(Vec::new()),
            answer: Box::new(answer),
        })
    }

    fn prompts(&self) -> Vec<ApprovalPrompt> {
        self.prompts.lock().unwrap().clone()
    }
}

#[async_trait]
impl ApprovalUi for ScriptedUi {
    async fn ask(&self, prompt: ApprovalPrompt) -> Vec<(String, ApprovalAnswer)> {
        let out = prompt
            .requests
            .iter()
            .filter_map(|r| (self.answer)(r).map(|a| (r.approval_id.clone(), a)))
            .collect();
        self.prompts.lock().unwrap().push(prompt);
        out
    }
}

fn engine(root: &Path) -> PolicyEngine {
    PolicyEngine::new(&PolicyConfig::default(), root).unwrap()
}

fn pending(e: &PolicyEngine, call: &str, cmd: &str) -> PendingApproval {
    let action = Action::Bash {
        command: cmd.into(),
    };
    let verdict = e.check(&action, Mode::AutoEdit, e.root());
    assert_eq!(verdict.decision, Decision::Ask, "{cmd}");
    PendingApproval {
        call_id: call.into(),
        node_id: Some(NodeId::from("agent-1")),
        action,
        verdict,
        preview: None,
    }
}

#[tokio::test]
async fn parallel_calls_are_batched_into_one_prompt() {
    let root = Path::new("/ws");
    let e = engine(root);
    let ui = ScriptedUi::new(|_| Some(ApprovalAnswer::Approve(ApprovalScope::Once)));
    let broker = ApprovalBroker::new(e.grants().clone(), ui.clone(), root);
    let res = broker
        .request(vec![
            pending(&e, "c1", "make a"),
            pending(&e, "c2", "make b"),
            pending(&e, "c3", "git push"),
        ])
        .await;
    assert_eq!(ui.prompts().len(), 1);
    assert_eq!(ui.prompts()[0].requests.len(), 3);
    assert_eq!(
        res.iter().map(|r| r.call_id.as_str()).collect::<Vec<_>>(),
        ["c1", "c2", "c3"]
    );
    assert!(res.iter().all(|r| r.approved));
    assert_eq!(broker.request(Vec::new()).await.len(), 0);
    assert_eq!(broker.metrics().prompts, 1);
}

#[tokio::test]
async fn scopes_once_session_always() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let e = engine(root);
    let ui = ScriptedUi::new(|r| {
        Some(match r.call_id.as_str() {
            "once" => ApprovalAnswer::Approve(ApprovalScope::Once),
            "session" => ApprovalAnswer::Approve(ApprovalScope::Session {
                pattern: r.suggested_pattern.clone().unwrap(),
            }),
            "always" => ApprovalAnswer::Approve(ApprovalScope::Always {
                pattern: "git push origin*".into(),
            }),
            _ => ApprovalAnswer::Deny {
                reason: Some("not now".into()),
            },
        })
    });
    let broker = ApprovalBroker::new(e.grants().clone(), ui.clone(), root);
    let res = broker
        .request(vec![
            pending(&e, "once", "make once"),
            pending(&e, "session", "cargo bench --all"),
            pending(&e, "always", "git push origin main"),
            pending(&e, "no", "make no"),
        ])
        .await;
    // Once: nothing remembered.
    assert!(res[0].approved && res[0].ini_rule.is_none());
    assert_eq!(
        e.check(
            &Action::Bash {
                command: "make once".into()
            },
            Mode::AutoEdit,
            root
        )
        .decision,
        Decision::Ask
    );
    // Session: the suggested pattern is remembered for the session.
    assert_eq!(
        ui.prompts()[0].requests[1].suggested_pattern.as_deref(),
        Some("cargo bench*")
    );
    let v = e.check(
        &Action::Bash {
            command: "cargo bench -p x".into(),
        },
        Mode::AutoEdit,
        root,
    );
    assert_eq!(v.decision, Decision::Allow);
    // Always: session-wide now, ini rule proposed but not written.
    let rule = res[2].ini_rule.clone().expect("proposal");
    assert!(rule.describe().contains("bash.allow"));
    assert_eq!(res[2].decision(), ApprovalDecision::ApprovedAlways);
    let ini = root.join("harness.ini");
    assert!(!ini.exists());
    rule.confirm(&ini).unwrap();
    assert_eq!(
        std::fs::read_to_string(&ini).unwrap(),
        "[policy]\nbash.allow = git push origin*\n"
    );
    // Deny carries the reason.
    assert!(!res[3].approved);
    assert_eq!(res[3].reason.as_deref(), Some("not now"));
    let m = broker.metrics();
    assert_eq!(
        m,
        ApprovalMetrics {
            prompts: 1,
            requests: 4,
            approved: 3,
            denied: 1,
            session_grants: 2,
            always_proposals: 1,
        }
    );
}

#[tokio::test]
async fn missing_answers_and_closed_ui_deny() {
    let root = Path::new("/ws");
    let e = engine(root);
    let ui = ScriptedUi::new(|_| None);
    let broker = ApprovalBroker::new(e.grants().clone(), ui, root);
    let res = broker.request(vec![pending(&e, "c", "make")]).await;
    assert!(!res[0].approved);
    assert_eq!(res[0].by, "harness");

    let (chan, rx) = ChannelUi::new(1);
    drop(rx);
    let broker = ApprovalBroker::new(e.grants().clone(), Arc::new(chan), root);
    assert!(!broker.request(vec![pending(&e, "c", "make")]).await[0].approved);
}

#[tokio::test]
async fn channel_ui_round_trip() {
    let root = Path::new("/ws");
    let e = engine(root);
    let (chan, mut rx) = ChannelUi::new(4);
    let front = tokio::spawn(async move {
        let env = rx.recv().await.unwrap();
        let answers = env
            .prompt
            .requests
            .iter()
            .map(|r| {
                (
                    r.approval_id.clone(),
                    ApprovalAnswer::Approve(ApprovalScope::Once),
                )
            })
            .collect();
        env.reply.send(answers).unwrap();
        env.prompt.requests.len()
    });
    let broker = ApprovalBroker::new(e.grants().clone(), Arc::new(chan), root);
    let res = broker
        .request(vec![pending(&e, "a", "make a"), pending(&e, "b", "make b")])
        .await;
    assert_eq!(front.await.unwrap(), 2);
    assert!(res.iter().all(|r| r.approved));
}

#[tokio::test]
async fn approvals_are_events_and_block_the_node_in_the_map() {
    let dir = tempfile::tempdir().unwrap();
    let rec = RunRecorder::create(
        dir.path(),
        RunId::new(),
        Arc::new(Redactor::default()),
        RecorderOptions::default(),
    )
    .await
    .unwrap();
    rec.emit(EventDraft::node(
        "agent-1",
        EventKind::NodeStarted {
            kind: NodeKind::Agent,
            name: "implementer".into(),
            agent: None,
            model: None,
            host: None,
            needs: vec![],
            queued: false,
        },
    ))
    .unwrap();
    let root = dir.path().to_path_buf();
    let e = engine(&root);
    // The UI checks the live map while the prompt is open.
    let mut live = rec.subscribe();
    let ui = ScriptedUi::new(|_| Some(ApprovalAnswer::Approve(ApprovalScope::Once)));
    let broker = ApprovalBroker::new(e.grants().clone(), ui, &root)
        .with_events(Arc::new(RecorderApprovalEvents(rec.clone())));
    broker
        .request(vec![
            pending(&e, "c1", "make a"),
            pending(&e, "c2", "make b"),
        ])
        .await;
    broker.request(vec![pending(&e, "c3", "make c")]).await;

    let mut state = RunState::new();
    let mut saw_waiting = false;
    while let Ok(ev) = live.try_recv() {
        state.apply(&ev);
        saw_waiting |= state.nodes[0].status == NodeStatus::WaitingApproval;
    }
    assert!(saw_waiting, "node was shown as waiting for approval");
    assert_eq!(state.nodes[0].status, NodeStatus::Running);
    assert_eq!(state.nodes[0].pending_approvals, 0);

    rec.finish().await.unwrap();
    let replay = read_run(rec.dir()).unwrap();
    let kinds: Vec<&str> = replay.events.iter().map(|e| e.kind.type_name()).collect();
    assert_eq!(
        kinds,
        [
            "node_started",
            "approval_requested",
            "approval_requested",
            "approval_resolved",
            "approval_resolved",
            "approval_requested",
            "approval_resolved",
        ]
    );
    assert!(
        replay.events[1..]
            .iter()
            .all(|e| e.node_id == Some(NodeId::from("agent-1")))
    );
    assert_eq!(count_prompts(&replay.events), 2);
    assert_eq!(broker.metrics().prompts, 2);
    match &replay.events[1].kind {
        EventKind::ApprovalRequested {
            action, preview, ..
        } => {
            assert_eq!(action, "bash: make a");
            assert_eq!(preview, &Some(Payload::text("make a")));
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn suggested_patterns() {
    assert_eq!(suggest_pattern("cargo test -p x"), "cargo test*");
    assert_eq!(suggest_pattern("ls -la"), "ls*");
    assert_eq!(suggest_pattern("make"), "make*");
    assert_eq!(suggest_pattern(""), "*");
}
