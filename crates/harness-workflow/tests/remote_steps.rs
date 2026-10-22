//! Steps placed with `runs_on` run their tools on the remote host, end to
//! end: a real helper (`harness_remote::helper::Helper`) served over a
//! local pipe stands in for `ssh host harness remote-agent --stdio`; its
//! "remote" workdir is a temp dir. The fake provider drives the agent, so
//! model calls stay local while read/write/bash go through the helper.

mod common;

use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};

use async_trait::async_trait;
use common::*;
use harness_events::reducer::RunState;
use harness_events::schema::{EventKind, NodeId, Payload};
use harness_providers::fake::{FakeBlock, FakeProvider, FakeTurn};
use harness_remote::audit::AuditSink;
use harness_remote::client::{
    ClientOptions, Connection, Connector, InProcessConnector, RemoteClient,
};
use harness_remote::helper::{Helper, HelperOptions};
use harness_remote::node::{HostConnect, HostSession};
use harness_remote::sync::{CleanupPolicy, Pusher};
use harness_remote::workspace::is_host_specific;
use harness_tools::bash::{EnvScrubber, NONINTERACTIVE_ENV};
use harness_tools::config::ToolsConfig;
use harness_tools::local::LocalExec;
use harness_tools::spill::MemoryBlobSink;
use harness_workflow::executor::{Runners, StepFailure, StepOutput, StepRequest, StepRunner};
use harness_workflow::placement::{HostInfo, HostPool};
use harness_workflow::remote::{PATCH_FILE, RemoteSteps};
use harness_workflow::rerun::{StepStatus, WorkflowStatus};
use harness_workflow::runners::{AgentStepRunner, ShellStepRunner, VerifyStepRunner};
use serde_json::{Map, json};
use tokio::io::AsyncWrite;
use tokio_util::sync::CancellationToken;

/// The helper over an in-memory pipe; the host's workdir is local.
struct PipeHost {
    workdir: PathBuf,
    connects: AtomicUsize,
    /// Every byte the client sent to the helper (helper protocol frames).
    sent: Arc<Mutex<Vec<u8>>>,
}

impl PipeHost {
    /// The params of every `exec.start` request sent to the host.
    async fn exec_starts(&self) -> Vec<serde_json::Value> {
        let sent = self.sent.lock().unwrap().clone();
        let mut reader = tokio::io::BufReader::new(&sent[..]);
        let mut starts = Vec::new();
        while let Some(frame) =
            harness_remote::wire::read_wire_frame(&mut reader, harness_remote::protocol::MAX_FRAME)
                .await
                .expect("valid frames")
        {
            let v: serde_json::Value = serde_json::from_str(&frame.json).expect("JSON message");
            if v["method"] == "exec.start" {
                starts.push(v["params"].clone());
            }
        }
        starts
    }
}

/// Records what the client writes, then passes it on.
struct Tee {
    inner: Box<dyn AsyncWrite + Send + Unpin>,
    log: Arc<Mutex<Vec<u8>>>,
}

impl AsyncWrite for Tee {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        let r = Pin::new(&mut self.inner).poll_write(cx, buf);
        if let Poll::Ready(Ok(n)) = &r {
            self.log.lock().unwrap().extend_from_slice(&buf[..*n]);
        }
        r
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

struct TeeConnector {
    inner: InProcessConnector,
    log: Arc<Mutex<Vec<u8>>>,
}

#[async_trait]
impl Connector for TeeConnector {
    async fn connect(&self, session: &str) -> std::io::Result<Connection> {
        let mut c = self.inner.connect(session).await?;
        c.writer = Box::new(Tee {
            inner: c.writer,
            log: self.log.clone(),
        });
        Ok(c)
    }

    fn host(&self) -> String {
        self.inner.host()
    }
}

#[async_trait]
impl HostConnect for PipeHost {
    async fn connect(&self, host: &str, audit: Arc<dyn AuditSink>) -> Result<HostSession, String> {
        self.connects.fetch_add(1, Ordering::SeqCst);
        let c = TeeConnector {
            inner: InProcessConnector::new(Helper::new(HelperOptions::default())),
            log: self.sent.clone(),
        };
        let client = RemoteClient::connect(Arc::new(c), ClientOptions::new(host).with_audit(audit))
            .await
            .map_err(|e| e.to_string())?;
        Ok(HostSession {
            client,
            workdir: Some(self.workdir.display().to_string()),
            pusher: Pusher::Rsync(None),
            cleanup: CleanupPolicy::OnSuccess,
        })
    }
}

fn has_rsync() -> bool {
    std::process::Command::new("rsync")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

struct Remote {
    env: Env,
    host: Arc<PipeHost>,
}

impl Remote {
    async fn new() -> Self {
        let env = Env::new().await;
        std::fs::create_dir_all(env.work.join("src")).unwrap();
        std::fs::write(env.work.join("src/lib.rs"), "pub fn a() {}\n").unwrap();
        let host = Arc::new(PipeHost {
            workdir: env.dir.path().join("gpu-box-work"),
            connects: AtomicUsize::new(0),
            sent: Arc::default(),
        });
        Self { env, host }
    }

    fn runners(&self, agents: Agents) -> Runners {
        let env = &self.env;
        let exec = Arc::new(LocalExec::new());
        let steps = RemoteSteps::new(self.host.clone(), env.recorder.clone(), &env.work);
        let local_shell = Arc::new(
            ShellStepRunner::new(exec.clone(), &env.work).with_recorder(env.recorder.clone()),
        );
        let local_verify = Arc::new(VerifyStepRunner::new(exec, &env.work, env.recorder.clone()));
        Runners::new(headless_approve_all())
            .with_agent(steps.agent(
                AgentStepRunner::new(env.runtime(agents)),
                ToolsConfig::default(),
                EnvScrubber::new(Vec::new(), Vec::new()),
                Arc::new(MemoryBlobSink::new()),
            ))
            .with_shell(steps.shell(local_shell, vec![("GREETING".into(), "hi".into())]))
            .with_verify(steps.verify(local_verify, Vec::new()))
            .with_proof(steps.local_only("bugfix-proof", Arc::new(NeverRuns)))
    }

    fn pool() -> Arc<HostPool> {
        Arc::new(HostPool::new(vec![HostInfo::new(
            "gpu-box",
            vec!["gpu".into()],
            2,
        )]))
    }

    fn remote_dirs(&self) -> Vec<PathBuf> {
        fn walk(d: &Path, out: &mut Vec<PathBuf>) {
            if let Ok(rd) = std::fs::read_dir(d) {
                for e in rd.flatten() {
                    if e.path().is_dir() {
                        out.push(e.path());
                        walk(&e.path(), out);
                    }
                }
            }
        }
        let mut out = Vec::new();
        walk(&self.host.workdir, &mut out);
        out
    }
}

struct NeverRuns;

#[async_trait]
impl StepRunner for NeverRuns {
    async fn run(&self, _req: StepRequest) -> Result<StepOutput, StepFailure> {
        Err(StepFailure::new("the local proof runner must not run"))
    }
}

fn tool(id: &str, name: &str, input: serde_json::Value) -> FakeTurn {
    FakeTurn {
        content: vec![FakeBlock::ToolUse {
            id: Some(id.into()),
            name: name.into(),
            input,
        }],
        ..FakeTurn::default()
    }
}

const WF: &str = r#"name: remote-e2e
steps:
  - id: build
    run: "echo built-on-host > built.txt && echo greeting=$GREETING && pwd > where.txt && echo report > $ARTIFACTS_DIR/report"
    runs_on: gpu-box
    outputs: [greeting]
    artifacts: [report]
  - id: fix
    needs: [build]
    agent: implementer
    prompt: "Fix it"
    runs_on: { labels: [gpu] }
  - id: check
    needs: [fix]
    verify: "grep -q 'b()' src/lib.rs && test -f built.txt"
    runs_on: gpu-box
  - id: here
    needs: [check]
    run: "cat built.txt src/lib.rs"
"#;

#[tokio::test(flavor = "multi_thread")]
async fn placed_steps_run_their_tools_on_the_host_and_events_carry_it() {
    if !has_rsync() {
        eprintln!("rsync not installed; skipping (push needs rsync)");
        return;
    }
    let r = Remote::new().await;
    let implementer = FakeProvider::with_turns([
        tool("r1", "read", json!({"path": "src/lib.rs"})),
        tool(
            "w1",
            "write",
            json!({"path": "src/lib.rs", "content": "pub fn a() { b() }\npub fn b() {}\n"}),
        ),
        tool("b1", "bash", json!({"command": "cat where.txt; pwd"})),
        FakeTurn::text("Fixed on the host."),
    ]);
    let agents = Agents::default().with(
        "name: implementer\nprompt_text: You implement.\ntools: [read, write, bash]\n",
        implementer.clone(),
    );
    let out = r
        .env
        .executor(r.runners(agents))
        .with_hosts(Remote::pool())
        .run(&wf(WF), &Map::new(), None, &CancellationToken::new())
        .await;
    assert_eq!(
        out.status,
        WorkflowStatus::Succeeded,
        "{:?}\n{:#?}",
        out.error,
        out.state.steps
    );
    assert_eq!(implementer.remaining(), 0, "the model ran locally");
    assert_eq!(
        r.host.connects.load(Ordering::SeqCst),
        3,
        "one session per placed step"
    );

    // Results came back and were applied locally; `here` ran locally on them.
    let work = &r.env.work;
    assert_eq!(
        std::fs::read_to_string(work.join("built.txt")).unwrap(),
        "built-on-host\n"
    );
    assert_eq!(
        std::fs::read_to_string(work.join("src/lib.rs")).unwrap(),
        "pub fn a() { b() }\npub fn b() {}\n"
    );
    let where_ = std::fs::read_to_string(work.join("where.txt")).unwrap();
    // `pwd` prints the resolved path; on macOS `$TMPDIR` is under the
    // `/var` -> `/private/var` symlink, so accept the workdir as given or
    // resolved (the run dir itself is gone by now).
    let ran_in = Path::new(where_.trim());
    let resolved = r.host.workdir.canonicalize().unwrap();
    assert!(
        ran_in.starts_with(&r.host.workdir) || ran_in.starts_with(&resolved),
        "the shell step ran in the host's run dir: {where_}"
    );
    let steps = &out.state.steps;
    assert_eq!(steps["build"].outputs["greeting"], "hi");
    assert!(
        steps["here"].outputs["stdout"]
            .as_str()
            .unwrap()
            .contains("built-on-host")
    );
    let report = steps["build"].artifacts["report"].clone();
    assert_eq!(std::fs::read_to_string(report).unwrap(), "report\n");
    assert!(
        r.remote_dirs().iter().all(|d| !d.join("src").exists()),
        "run dirs cleaned up"
    );

    // Host-tagged events.
    let evs = events(&r.env.recorder).await;
    let s = RunState::from_events(&evs);
    let host_of = |id: &str| {
        s.node(&NodeId::from(id))
            .unwrap_or_else(|| panic!("no node {id}"))
            .host
            .clone()
    };
    for id in [
        "remote-e2e/build",
        "remote-e2e/fix",
        "remote-e2e/fix.agent",
        "remote-e2e/check",
    ] {
        assert_eq!(host_of(id).as_deref(), Some("gpu-box"), "{id}");
    }
    assert_eq!(host_of("remote-e2e/here"), None);

    let mut remote_nodes = std::collections::BTreeSet::new();
    let mut ops = Vec::new();
    for e in &evs {
        match &e.kind {
            EventKind::RemoteOp { host, op, ok, .. } => {
                assert_eq!(host, "gpu-box");
                assert!(*ok, "{op} failed");
                remote_nodes.insert(e.node_id.clone().unwrap().as_str().to_owned());
                ops.push(op.clone());
            }
            EventKind::RemoteConnection { host, .. } => assert_eq!(host, "gpu-box"),
            _ => {}
        }
    }
    assert_eq!(
        remote_nodes.into_iter().collect::<Vec<_>>(),
        ["remote-e2e/build", "remote-e2e/check", "remote-e2e/fix"],
        "remote ops are recorded on the placed steps only"
    );
    for want in [
        "sync.push",
        "exec.start",
        "fs.write",
        "sync.changes",
        "sync.apply",
    ] {
        assert!(ops.iter().any(|o| o == want), "{want} missing: {ops:?}");
    }
    // The agent's tool calls are local events of its (host-tagged) node.
    let bash_out = evs
        .iter()
        .find_map(|e| match &e.kind {
            EventKind::ToolResult {
                call_id, output, ..
            } if call_id == "b1" => {
                assert_eq!(e.node_id, Some(NodeId::from("remote-e2e/fix.agent")));
                Some(format!("{output:?}"))
            }
            _ => None,
        })
        .unwrap();
    assert!(
        bash_out.contains(&r.host.workdir.display().to_string()),
        "bash ran in the host's run dir: {bash_out}"
    );
    let changed: Vec<&str> = evs
        .iter()
        .filter_map(|e| match &e.kind {
            EventKind::FileChanged { path, .. } => Some(path.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(
        changed,
        ["src/lib.rs"],
        "file changes are workspace-relative"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn changes_that_fail_the_sync_gates_are_not_applied() {
    if !has_rsync() {
        eprintln!("rsync not installed; skipping (push needs rsync)");
        return;
    }
    let r = Remote::new().await;
    let w = wf(r#"name: gated
steps:
  - id: edit
    run: "echo changed > src/lib.rs && echo x > src/lib.rs.orig"
    runs_on: gpu-box
"#);
    let out = r
        .env
        .executor(r.runners(Agents::default()))
        .with_hosts(Remote::pool())
        .run(&w, &Map::new(), None, &CancellationToken::new())
        .await;
    assert_eq!(out.status, WorkflowStatus::Failed);
    let step = &out.state.steps["edit"];
    assert_eq!(step.status, StepStatus::Failed);
    let err = step.error.clone().unwrap();
    assert!(err.contains("were not applied"), "{err}");
    assert!(err.contains("src/lib.rs.orig"), "{err}");
    assert_eq!(
        std::fs::read_to_string(r.env.work.join("src/lib.rs")).unwrap(),
        "pub fn a() {}\n",
        "nothing is applied"
    );
    assert!(!r.env.work.join("src/lib.rs.orig").exists());
    let patch = r.env.recorder.dir().join("artifacts/edit").join(PATCH_FILE);
    let text = std::fs::read_to_string(&patch).unwrap();
    assert!(text.contains("+changed"), "{text}");
    assert!(
        r.remote_dirs()
            .iter()
            .any(|d| d.join("src/lib.rs.orig").exists()),
        "the run dir is kept for inspection"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn proof_steps_on_a_host_fail_with_a_fix() {
    let r = Remote::new().await;
    let runner = RemoteSteps::new(r.host.clone(), r.env.recorder.clone(), &r.env.work)
        .local_only("bugfix-proof", Arc::new(NeverRuns));
    let req = StepRequest {
        workflow: "w".into(),
        step: "proof".into(),
        instance: "proof".into(),
        node: NodeId::from("w/proof"),
        action: harness_workflow::executor::StepAction::Proof {
            test: "true".into(),
            keep: Vec::new(),
        },
        outputs: Vec::new(),
        artifacts: Vec::new(),
        artifacts_dir: r.env.dir.path().join("a"),
        scope: Vec::new(),
        host: Some("gpu-box".into()),
        limits: Default::default(),
        checkpoint: None,
        slot: None,
        cancel: CancellationToken::new(),
        attempt: 1,
    };
    let err = runner.run(req).await.unwrap_err();
    assert!(err.message.contains("remove `runs_on`"), "{}", err.message);
    assert_eq!(r.host.connects.load(Ordering::SeqCst), 0);
}

/// Names of local variables that are neither credentials nor host-specific
/// (e.g. the `CARGO_*` variables cargo sets for test runs): before the fix
/// these went to the host. `SHLVL` and `_` are set by every shell.
fn local_only_names() -> Vec<String> {
    let scrubber = EnvScrubber::new(Vec::new(), Vec::new());
    std::env::vars()
        .filter(|(k, v)| !scrubber.is_secret(k, v) && !is_host_specific(k))
        .filter(|(k, _)| !NONINTERACTIVE_ENV.iter().any(|(n, _)| n == k))
        .filter(|(k, _)| k != "SHLVL" && k != "_")
        .map(|(k, _)| k)
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn the_local_environment_is_not_sent_to_the_host() {
    if !has_rsync() {
        eprintln!("rsync not installed; skipping (push needs rsync)");
        return;
    }
    let local = local_only_names();
    assert!(
        !local.is_empty(),
        "the test process needs a non-host, non-secret variable (cargo sets CARGO_*)"
    );
    let r = Remote::new().await;
    let implementer = FakeProvider::with_turns([
        tool("e1", "bash", json!({"command": "env"})),
        FakeTurn::text("Looked."),
    ]);
    let agents = Agents::default().with(
        "name: implementer\nprompt_text: You implement.\ntools: [bash]\n",
        implementer.clone(),
    );
    let w = wf(r#"name: noenv
steps:
  - id: sh
    run: "true"
    runs_on: gpu-box
  - id: fix
    needs: [sh]
    agent: implementer
    prompt: "Look"
    runs_on: gpu-box
  - id: check
    needs: [fix]
    verify: "true"
    runs_on: gpu-box
"#);
    let out = r
        .env
        .executor(r.runners(agents))
        .with_hosts(Remote::pool())
        .run(&w, &Map::new(), None, &CancellationToken::new())
        .await;
    assert_eq!(out.status, WorkflowStatus::Succeeded, "{:?}", out.error);

    // On the wire: shell, bash and verify each sent only declared and
    // harness-set variables, never the local environment.
    let starts = r.host.exec_starts().await;
    assert_eq!(starts.len(), 3, "{starts:#?}");
    let allowed: Vec<&str> = ["GREETING", "ARTIFACTS_DIR"]
        .into_iter()
        .chain(NONINTERACTIVE_ENV.iter().map(|(n, _)| *n))
        .collect();
    for p in &starts {
        for pair in p["env"].as_array().unwrap() {
            let name = pair[0].as_str().unwrap();
            assert!(
                allowed.contains(&name),
                "{name} was sent to the host by `{}`",
                p["program"]
            );
        }
    }

    // On the host: agent bash shows none of the local-only variables.
    let evs = events(&r.env.recorder).await;
    let shown = evs
        .iter()
        .find_map(|e| match &e.kind {
            EventKind::ToolResult {
                call_id, output, ..
            } if call_id == "e1" => match output {
                Payload::Text(t) => Some(t.clone()),
                other => panic!("unexpected payload {other:?}"),
            },
            _ => None,
        })
        .unwrap();
    let names: Vec<&str> = shown
        .lines()
        .filter_map(|l| l.split_once('=').map(|(k, _)| k))
        .collect();
    for want in ["TERM", "PATH", "HOME"] {
        assert!(names.contains(&want), "{want} missing on the host: {shown}");
    }
    for name in &local {
        assert!(
            !names.contains(&name.as_str()),
            "{name} reached the host: {shown}"
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_retried_step_gets_a_fresh_run_dir_and_no_stale_artifacts() {
    if !has_rsync() {
        eprintln!("rsync not installed; skipping (push needs rsync)");
        return;
    }
    let r = Remote::new().await;
    let mark = r.env.dir.path().join("attempted");
    let w = wf(&format!(
        r#"name: retry
steps:
  - id: flaky
    run: "if [ ! -f {m} ]; then touch {m}; echo stale > $ARTIFACTS_DIR/report; echo junk > junk.txt; exit 1; fi; test ! -e junk.txt && echo clean=yes"
    runs_on: gpu-box
    retries: 1
    outputs: [clean]
    artifacts: [report]
"#,
        m = mark.display()
    ));
    let out = r
        .env
        .executor(r.runners(Agents::default()))
        .with_hosts(Remote::pool())
        .run(&w, &Map::new(), None, &CancellationToken::new())
        .await;
    assert_eq!(r.host.connects.load(Ordering::SeqCst), 2, "two attempts");
    assert_eq!(out.status, WorkflowStatus::Failed);
    let err = out.state.steps["flaky"].error.clone().unwrap();
    // Attempt 2 started from a clean copy (no junk.txt) and did not write
    // the artifact; attempt 1's stale report is not returned for it.
    assert!(
        err.contains("did not produce its declared artifact `report`"),
        "{err}"
    );
    let dirs = r.remote_dirs();
    let first = dirs
        .iter()
        .find(|d| {
            d.file_name()
                .unwrap()
                .to_string_lossy()
                .ends_with("-flaky-a1")
        })
        .unwrap_or_else(|| panic!("attempt 1's run dir is kept: {dirs:?}"));
    assert_eq!(
        std::fs::read_to_string(first.join("junk.txt")).unwrap(),
        "junk\n",
        "attempt 1's run dir is kept as it was"
    );
    assert!(
        !dirs.iter().any(|d| d
            .file_name()
            .unwrap()
            .to_string_lossy()
            .ends_with("-flaky-a2")),
        "attempt 2 succeeded on the host, so its run dir was cleaned up: {dirs:?}"
    );
    assert!(!r.env.work.join("junk.txt").exists());
}
