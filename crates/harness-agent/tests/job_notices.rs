//! Background bash jobs: the exit of a job reaches the agent's next request
//! as a notice on the unsent tail, once, and is recorded with that request.

use std::sync::Arc;
use std::time::Duration;

use harness_agent::agent_loop::{AgentLoop, Conversation, LoopConfig};
use harness_agent::dispatch::{AllowAll, Dispatcher, RecorderToolEvents, StandardTools, ToolSet};
use harness_agent::job_notices::JobNotices;
use harness_events::redact::Redactor;
use harness_events::schema::{EventKind, NodeId, Payload, RunId};
use harness_events::store::{RecorderOptions, RunRecorder, read_run};
use harness_providers::fake::{FakeProvider, FakeTurn};
use harness_providers::model::{ChatRequest, ContentBlock};
use harness_tools::bash::EnvScrubber;
use harness_tools::config::ToolsConfig;
use harness_tools::local::{LocalExec, LocalWorkspace};
use harness_tools::spill::MemoryBlobSink;
use serde_json::json;
use tokio_util::sync::CancellationToken;

const SECRET: &str = "sk-test-SECRETVALUE1234567890";

struct Fx {
    _dir: tempfile::TempDir,
    recorder: RunRecorder,
    tools: ToolSet,
    provider: FakeProvider,
}

async fn fx(turns: Vec<FakeTurn>) -> Fx {
    let dir = tempfile::tempdir().unwrap();
    let recorder = RunRecorder::create(
        &dir.path().join("run"),
        RunId::new(),
        Arc::new(Redactor::new([SECRET])),
        RecorderOptions::default(),
    )
    .await
    .unwrap();
    let tools = ToolSet::standard(StandardTools {
        workspace: Arc::new(LocalWorkspace::new(dir.path())),
        exec: Arc::new(LocalExec::new()),
        cwd: dir.path().to_path_buf(),
        config: ToolsConfig::default(),
        scrubber: EnvScrubber::new(Vec::<String>::new(), Vec::<String>::new()),
        spill: Arc::new(MemoryBlobSink::default()),
    })
    .select(&["bash"])
    .0;
    Fx {
        _dir: dir,
        recorder,
        tools,
        provider: FakeProvider::with_turns(turns),
    }
}

impl Fx {
    async fn run(&self, conv: &mut Conversation, prompt: &str) {
        let node = NodeId::from("n1");
        let jobs = self.tools.jobs().expect("bash has jobs");
        let mut agent = AgentLoop::new(
            Arc::new(self.provider.clone()),
            Dispatcher::new(self.tools.clone(), Arc::new(AllowAll)).with_events(Arc::new(
                RecorderToolEvents::new(self.recorder.clone(), node.clone(), "/"),
            )),
            self.recorder.clone(),
            LoopConfig::new("coder", "fake-model"),
        )
        .with_node(node)
        .with_hook(Arc::new(JobNotices::new(jobs)));
        conv.push_user(prompt);
        agent.run(conv, &CancellationToken::new()).await;
    }
}

fn notices(r: &ChatRequest) -> Vec<String> {
    let mut out = Vec::new();
    for m in &r.messages {
        for b in &m.content {
            if let ContentBlock::Text { text } = b
                && text.starts_with("Notice from harness: background job")
            {
                out.push(text.clone());
            }
        }
    }
    out
}

fn last_has_notice(r: &ChatRequest) -> bool {
    r.messages.last().is_some_and(|m| {
        m.content.iter().any(
            |b| matches!(b, ContentBlock::Text { text } if text.starts_with("Notice from harness")),
        )
    })
}

#[tokio::test]
async fn an_ended_job_is_reported_once_on_the_next_request() {
    let f = fx(vec![
        FakeTurn::tool_use(
            "bash",
            json!({"command": format!("sleep 0.3; echo built {SECRET}; exit 2"), "run_in_background": true}),
        ),
        // A foreground call that outlives the job.
        FakeTurn::tool_use("bash", json!({"command": "sleep 2"})),
        FakeTurn::tool_use("bash", json!({"command": "true"})),
        FakeTurn::text("done"),
    ])
    .await;
    // bash_jobs comes with bash.
    assert_eq!(f.tools.names(), ["bash", "bash_jobs"]);
    let mut conv = Conversation::new(Some("system".into()));
    f.run(&mut conv, "build it").await;
    f.recorder.finish().await.unwrap();

    let requests = f.provider.requests();
    assert_eq!(requests.len(), 4);
    let started = requests[1].messages.last().unwrap();
    let result = format!("{:?}", started.content);
    assert!(result.contains("background job j1 started"), "{result}");
    assert!(!last_has_notice(&requests[1]));
    assert!(
        last_has_notice(&requests[2]),
        "{:?}",
        requests[2].messages.last()
    );
    let n = notices(&requests[2]);
    assert_eq!(n.len(), 1, "{n:?}");
    assert!(n[0].contains("job j1 ended (exit code 2)"), "{}", n[0]);
    assert!(n[0].contains("Command: sleep 0.3; echo built"), "{}", n[0]);
    assert!(n[0].contains("Last output:\nbuilt "), "{}", n[0]);
    assert!(n[0].contains("Full output ("), "{}", n[0]);
    assert!(!n[0].contains(SECRET), "output is redacted: {}", n[0]);
    // Reported once; later requests only repeat it as history.
    assert!(!last_has_notice(&requests[3]));
    assert_eq!(notices(&requests[3]).len(), 1);
    // History is never rewritten, so the cache prefix holds.
    for pair in requests.windows(2) {
        let (a, b) = (&pair[0].messages, &pair[1].messages);
        assert_eq!(a[..], b[..a.len()], "a later request rewrote history");
    }

    // The log records the notice with its request, and the job's start
    // and end as bash_job events of the starting call.
    let events = read_run(f.recorder.dir()).unwrap().events;
    let logged: Vec<String> = events
        .iter()
        .filter_map(|e| match &e.kind {
            EventKind::ModelCallStarted {
                request: Payload::Json(v),
                ..
            } => Some(v["messages"].to_string()),
            _ => None,
        })
        .collect();
    assert!(logged[2].contains("Notice from harness"), "{}", logged[2]);
    let jobs: Vec<(String, String, Option<i32>)> = events
        .iter()
        .filter_map(|e| match &e.kind {
            EventKind::BashJob {
                job_id,
                state,
                exit_code,
                command,
                ..
            } => {
                assert!(!command.contains(SECRET), "{command}");
                Some((job_id.clone(), state.clone(), *exit_code))
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        jobs,
        [
            ("j1".to_owned(), "running".to_owned(), None),
            ("j1".to_owned(), "failed".to_owned(), Some(2)),
        ]
    );
}

#[tokio::test]
async fn a_job_that_ends_between_turns_is_reported_with_the_next_prompt() {
    let f = fx(vec![
        FakeTurn::tool_use(
            "bash",
            json!({"command": "echo served", "run_in_background": true}),
        ),
        FakeTurn::text("started the server"),
        FakeTurn::text("ok"),
    ])
    .await;
    let mut conv = Conversation::new(Some("system".into()));
    f.run(&mut conv, "start it").await;
    let jobs = f.tools.jobs().unwrap();
    for _ in 0..200 {
        if jobs.running() == 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert_eq!(jobs.running(), 0);
    f.run(&mut conv, "is it up?").await;
    let requests = f.provider.requests();
    let last = requests.last().unwrap().messages.last().unwrap();
    assert_eq!(last.content.len(), 2, "{last:?}");
    assert!(matches!(&last.content[0], ContentBlock::Text { text } if text == "is it up?"));
    assert!(
        matches!(&last.content[1], ContentBlock::Text { text } if text.contains("job j1 ended (exit code 0)") && text.contains("served")),
        "{last:?}"
    );
    f.recorder.finish().await.unwrap();
}
