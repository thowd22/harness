//! The harness tools run unchanged on a remote host: change ledger, redaction
//! and policy behave as locally; secrets never cross the wire; every remote
//! operation is audited with its host.

use std::path::Path;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};

use async_trait::async_trait;
use harness_events::redact::Redactor;
use harness_policy::rules::{Action, Decision, Mode, PolicyConfig, PolicyEngine};
use harness_remote::audit::{RecordingAudit, SecretGuard};
use harness_remote::client::{
    ClientOptions, Connection, Connector, InProcessConnector, RemoteClient,
};
use harness_remote::helper::{Helper, HelperOptions};
use harness_remote::protocol::b64;
use harness_remote::workspace::{RemoteEnv, RemoteExec, RemoteWorkspace};
use harness_tools::bash::{BashTool, EnvScrubber};
use harness_tools::config::ToolsConfig;
use harness_tools::edit::EditTool;
use harness_tools::exec::{Exec, ExecRequest};
use harness_tools::local::{LocalExec, LocalWorkspace};
use harness_tools::read::ReadTool;
use harness_tools::spill::{MemoryBlobSink, Spiller};
use harness_tools::tool::{RecordingEvents, ToolContext, ToolEvent};
use harness_tools::tracker::ReadTracker;
use harness_tools::workspace::Workspace;
use harness_tools::write::WriteTool;
use serde_json::json;
use tokio::io::AsyncWrite;

const SECRET: &str = "sk-ant-api03-TOPSECRETVALUE0123456789";

/// Records every byte the client sends to the helper.
struct Tap<W> {
    inner: W,
    seen: Arc<Mutex<Vec<u8>>>,
}

impl<W: AsyncWrite + Unpin> AsyncWrite for Tap<W> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        let r = Pin::new(&mut self.inner).poll_write(cx, buf);
        if let Poll::Ready(Ok(n)) = &r {
            self.seen.lock().unwrap().extend_from_slice(&buf[..*n]);
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

struct Tapped {
    inner: InProcessConnector,
    seen: Arc<Mutex<Vec<u8>>>,
}

#[async_trait]
impl Connector for Tapped {
    async fn connect(&self, session: &str) -> std::io::Result<Connection> {
        let mut c = self.inner.connect(session).await?;
        c.writer = Box::new(Tap {
            inner: c.writer,
            seen: self.seen.clone(),
        });
        Ok(c)
    }
    fn host(&self) -> String {
        "tapped".into()
    }
}

struct Setup {
    client: RemoteClient,
    wire: Arc<Mutex<Vec<u8>>>,
    audit: Arc<RecordingAudit>,
}

async fn setup() -> Setup {
    let wire = Arc::new(Mutex::new(Vec::new()));
    let audit = Arc::new(RecordingAudit::default());
    let mut opts = ClientOptions::new("gpu-box");
    opts.guard = SecretGuard::new([SECRET.to_string()]);
    opts.audit = audit.clone();
    let conn = Tapped {
        inner: InProcessConnector::new(Helper::new(HelperOptions::default())),
        seen: wire.clone(),
    };
    let client = RemoteClient::connect(Arc::new(conn), opts).await.unwrap();
    Setup {
        client,
        wire,
        audit,
    }
}

fn scrubber() -> EnvScrubber {
    EnvScrubber::new(["MY_PROVIDER_KEY".to_string()], [SECRET.to_string()])
}

fn bash(exec: Arc<dyn Exec>, cwd: &Path) -> BashTool {
    let spiller = Spiller::new(Arc::new(MemoryBlobSink::new()), &ToolsConfig::default());
    BashTool::new(exec, cwd, ToolsConfig::default(), scrubber(), spiller).with_base_env(vec![
        ("FOO".into(), "visible".into()),
        ("ANTHROPIC_API_KEY".into(), SECRET.into()),
        ("MY_PROVIDER_KEY".into(), SECRET.into()),
        ("SNEAKY".into(), format!("Bearer {SECRET}")),
        ("HARNESS_INTERNAL".into(), "x".into()),
        ("SSH_AUTH_SOCK".into(), "/tmp/local-agent.sock".into()),
        ("HOME".into(), "/home/local-user".into()),
        ("PATH".into(), "/usr/local/bin:/usr/bin:/bin".into()),
    ])
}

fn wire_text(w: &Arc<Mutex<Vec<u8>>>) -> String {
    String::from_utf8_lossy(&w.lock().unwrap()).into_owned()
}

#[tokio::test(flavor = "multi_thread")]
async fn secrets_and_local_env_never_reach_the_remote_host() {
    let s = setup().await;
    let dir = tempfile::tempdir().unwrap();
    let exec: Arc<dyn Exec> = Arc::new(RemoteExec::new(
        s.client.clone(),
        RemoteEnv::new(scrubber()),
    ));
    let tool = bash(exec.clone(), dir.path());
    let out = tool
        .call(
            &ToolContext::new("c1"),
            json!({"command": "echo \"$FOO|${ANTHROPIC_API_KEY:-none}|${MY_PROVIDER_KEY:-none}|${SNEAKY:-none}|${SSH_AUTH_SOCK:-nosock}|$HOME\""}),
        )
        .await
        .unwrap();
    let text = out.text().to_owned();
    let remote_home = std::env::var("HOME").unwrap_or_default();
    assert!(
        text.contains(&format!("visible|none|none|none|nosock|{remote_home}")),
        "{text}"
    );

    // A file write carrying a credential is refused before it is encoded.
    let ws = Arc::new(RemoteWorkspace::new(
        s.client.clone(),
        dir.path().to_string_lossy(),
    ));
    let err = ws
        .write(Path::new("leak.txt"), format!("key={SECRET}\n").as_bytes())
        .await
        .unwrap_err();
    assert!(
        err.to_string().contains("contains a harness credential"),
        "{err}"
    );
    assert!(!dir.path().join("leak.txt").exists());

    // So is a command line carrying one.
    let req = ExecRequest::new("echo", vec![SECRET.into()], dir.path());
    assert!(exec.spawn(req).await.is_err());

    let wire = wire_text(&s.wire);
    assert!(wire.contains("visible"), "the tap sees traffic");
    assert!(!wire.contains(SECRET), "secret on the wire");
    assert!(
        !wire.contains(&b64::encode(SECRET.as_bytes())),
        "encoded secret on the wire"
    );
    assert!(!wire.contains("/home/local-user"), "local HOME forwarded");
    assert!(!wire.contains("local-agent.sock"), "agent socket forwarded");
    assert!(!wire.contains("HARNESS_INTERNAL"));
}

fn normalize(ev: Vec<(String, ToolEvent)>, root: &Path) -> Vec<String> {
    ev.into_iter()
        .filter_map(|(_, e)| match e {
            ToolEvent::FileChanged(fc) => Some(format!(
                "{} {} {:?} {:?} {}\n{}",
                fc.tool,
                fc.path.strip_prefix(root).unwrap().display(),
                fc.kind,
                fc.before_hash,
                fc.after_hash,
                fc.diff
            )),
            _ => None,
        })
        .collect()
}

async fn edit_session(ws: Arc<dyn Workspace>) -> (Vec<String>, String) {
    let tracker = Arc::new(ReadTracker::new());
    let events = Arc::new(RecordingEvents::new());
    let ctx = ToolContext::new("c").with_events(events.clone());
    let write = WriteTool::new(ws.clone(), tracker.clone());
    let read = ReadTool::new(ws.clone(), tracker.clone(), ToolsConfig::default());
    let edit = EditTool::new(ws.clone(), tracker.clone());
    write
        .call(
            &ctx,
            json!({"path": "src/lib.rs", "content": "fn a() {}\nfn b() {}\n"}),
        )
        .await
        .unwrap();
    let r = read
        .call(&ctx, json!({"path": "src/lib.rs"}))
        .await
        .unwrap();
    edit.call(
        &ctx,
        json!({"path": "src/lib.rs", "old_string": "fn b() {}", "new_string": "fn b() { a() }"}),
    )
    .await
    .unwrap();
    // A stale-read check works the same way: an out-of-band change is caught.
    ws.write(Path::new("src/lib.rs"), b"changed elsewhere\n")
        .await
        .unwrap();
    let stale = edit
        .call(
            &ctx,
            json!({"path": "src/lib.rs", "old_string": "changed", "new_string": "x"}),
        )
        .await
        .unwrap_err();
    (
        normalize(events.events(), ws.root()),
        format!("{}|{:?}", r.text(), stale.kind),
    )
}

#[tokio::test(flavor = "multi_thread")]
async fn change_ledger_and_stale_reads_match_local() {
    let local_dir = tempfile::tempdir().unwrap();
    let remote_dir = tempfile::tempdir().unwrap();
    let s = setup().await;
    let local = edit_session(Arc::new(LocalWorkspace::new(local_dir.path()))).await;
    let remote = edit_session(Arc::new(RemoteWorkspace::new(
        s.client.clone(),
        remote_dir.path().to_string_lossy(),
    )))
    .await;
    assert_eq!(local.0.len(), 2, "{:?}", local.0);
    assert_eq!(remote, local, "same FileChanged events, diffs and errors");
}

#[tokio::test(flavor = "multi_thread")]
async fn redaction_applies_to_remote_output_like_local() {
    let s = setup().await;
    let local_dir = tempfile::tempdir().unwrap();
    let remote_dir = tempfile::tempdir().unwrap();
    // A file that already holds the key on each host (e.g. a .env file).
    std::fs::write(local_dir.path().join(".env"), format!("KEY={SECRET}\n")).unwrap();
    std::fs::write(remote_dir.path().join(".env"), format!("KEY={SECRET}\n")).unwrap();
    let redactor = Redactor::new([SECRET]);
    let run = |exec: Arc<dyn Exec>, dir: &Path| {
        let tool = bash(exec, dir);
        async move {
            let out = tool
                .call(&ToolContext::new("c"), json!({"command": "cat .env"}))
                .await
                .unwrap();
            out.text().to_owned()
        }
    };
    let local = run(Arc::new(LocalExec::new()), local_dir.path()).await;
    let remote = run(
        Arc::new(RemoteExec::new(
            s.client.clone(),
            RemoteEnv::new(scrubber()),
        )),
        remote_dir.path(),
    )
    .await;
    let (l, r) = (redactor.redact(&local), redactor.redact(&remote));
    assert!(!r.contains(SECRET));
    assert!(r.contains("[REDACTED]"));
    assert_eq!(l, r);
}

#[tokio::test(flavor = "multi_thread")]
async fn policy_decisions_do_not_depend_on_where_tools_run() {
    let root = tempfile::tempdir().unwrap();
    let cfg = PolicyConfig {
        bash_deny: vec!["rm -rf *".into()],
        bash_allow: vec!["cargo test*".into()],
        home: None,
        ..PolicyConfig::default()
    };
    let engine = PolicyEngine::new(&cfg, root.path()).unwrap();
    // The agent decides on the tool call before dispatching it to a local
    // or remote Workspace/Exec, so the verdict is the same either way.
    for (tool, args, want) in [
        ("bash", json!({"command": "rm -rf /"}), Decision::Deny),
        ("bash", json!({"command": "cargo test -q"}), Decision::Allow),
        (
            "write",
            json!({"path": ".harness/x.yaml", "content": ""}),
            Decision::Deny,
        ),
        (
            "write",
            json!({"path": "src/a.rs", "content": ""}),
            Decision::Allow,
        ),
    ] {
        let action = Action::from_tool_call(tool, &args);
        assert_eq!(
            engine.decide(&action, Mode::AutoEdit, root.path()).decision,
            want,
            "{tool} {args}"
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn every_remote_operation_is_audited_with_its_host() {
    let s = setup().await;
    let dir = tempfile::tempdir().unwrap();
    let ws = RemoteWorkspace::new(s.client.clone(), dir.path().to_string_lossy());
    ws.write(Path::new("a.txt"), b"1").await.unwrap();
    ws.read(Path::new("a.txt")).await.unwrap();
    ws.stat(Path::new("a.txt")).await.unwrap();
    ws.list(Path::new(".")).await.unwrap();
    let _ = ws.read(Path::new("missing")).await;
    let ex = RemoteExec::new(s.client.clone(), RemoteEnv::default());
    ex.spawn(ExecRequest::new("true", vec![], dir.path()))
        .await
        .unwrap()
        .wait()
        .await
        .unwrap();
    let ev = s.audit.events();
    let ops: Vec<&str> = ev.iter().map(|e| e.op.as_str()).collect();
    assert_eq!(
        ops,
        vec![
            "fs.write",
            "fs.read",
            "fs.stat",
            "fs.list",
            "fs.read",
            "exec.start"
        ]
    );
    assert!(ev.iter().all(|e| e.host == "gpu-box"));
    assert!(
        !ev[4].ok && ev[4].error.is_some(),
        "failures are audited too"
    );
    assert_eq!(ev[5].target.as_deref(), Some("true"));
    assert!(ev[0].target.as_deref().unwrap().ends_with("a.txt"));
}

/// Liveness samples of a remote bash call come through the helper, so a busy
/// quiet command on the host shows advancing CPU like a local one.
#[tokio::test(flavor = "multi_thread")]
async fn remote_bash_reports_liveness_through_the_helper() {
    let helper = Helper::new(HelperOptions {
        liveness_interval: Some(std::time::Duration::from_millis(150)),
        ..Default::default()
    });
    let client = RemoteClient::connect(
        Arc::new(InProcessConnector::new(helper)),
        ClientOptions::new("gpu-box"),
    )
    .await
    .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let exec: Arc<dyn Exec> = Arc::new(RemoteExec::new(client, RemoteEnv::default()));
    let tool = bash(exec, dir.path());
    let events = Arc::new(RecordingEvents::new());
    let ctx = ToolContext::new("r1").with_events(events.clone());
    tool.call(
        &ctx,
        json!({"command": "end=$((SECONDS+2)); while [ $SECONDS -lt $end ]; do :; done"}),
    )
    .await
    .unwrap();
    let samples: Vec<_> = events
        .events()
        .into_iter()
        .filter_map(|(_, e)| match e {
            ToolEvent::Progress(p) => p.liveness,
            _ => None,
        })
        .collect();
    assert!(samples.len() >= 3, "{samples:?}");
    assert!(
        samples[samples.len() - 1].cpu > samples[0].cpu,
        "CPU advances on the host: {samples:?}"
    );
}

/// Runs the same grep and glob calls (and one bad regex) over `ws`.
async fn search_session(ws: Arc<dyn Workspace>) -> Vec<String> {
    use harness_tools::glob::GlobTool;
    use harness_tools::grep::GrepTool;
    let root = ws.root().to_path_buf();
    for (rel, text, age) in [
        (".gitignore", "build/\n", 500),
        ("src/lib.rs", "pub fn connect() {}\n// TODO\n", 300),
        ("src/net.rs", "fn connect_tcp() {}\n", 10),
        ("build/out.rs", "connect\n", 1),
    ] {
        ws.write(Path::new(rel), text.as_bytes()).await.unwrap();
        let t = std::time::SystemTime::now() - std::time::Duration::from_secs(age);
        std::fs::File::options()
            .write(true)
            .open(root.join(rel))
            .unwrap()
            .set_modified(t)
            .unwrap();
    }
    let spill = || Spiller::new(Arc::new(MemoryBlobSink::new()), &ToolsConfig::default());
    let grep = GrepTool::new(ws.clone(), spill());
    let glob = GlobTool::new(ws.clone(), spill());
    let ctx = ToolContext::new("c");
    let mut out = Vec::new();
    for args in [
        json!({"pattern": "connect"}),
        json!({"pattern": "connect|TODO", "output_mode": "content", "-C": 1}),
        json!({"pattern": "o", "output_mode": "count", "type": "rust"}),
        json!({"pattern": "a("}),
    ] {
        out.push(match grep.call(&ctx, args).await {
            Ok(o) => o.text().to_owned(),
            Err(e) => format!("error: {e}"),
        });
    }
    out.push(
        glob.call(&ctx, json!({"pattern": "**/*.rs"}))
            .await
            .unwrap()
            .text()
            .to_owned(),
    );
    out
}

#[tokio::test(flavor = "multi_thread")]
async fn grep_and_glob_run_through_the_helper_like_local() {
    let local_dir = tempfile::tempdir().unwrap();
    let remote_dir = tempfile::tempdir().unwrap();
    let s = setup().await;
    let local = search_session(Arc::new(LocalWorkspace::new(local_dir.path()))).await;
    let remote = search_session(Arc::new(RemoteWorkspace::new(
        s.client.clone(),
        remote_dir.path().to_string_lossy(),
    )))
    .await;
    assert_eq!(local[0], "src/lib.rs\nsrc/net.rs\n[2 files match.]");
    assert_eq!(local[4], "src/net.rs\nsrc/lib.rs");
    assert!(local[3].contains("invalid regex"), "{}", local[3]);
    assert_eq!(remote, local, "same results over the helper");
}

/// Polls `jobs` until a finished job is handed out.
async fn finished_job(
    jobs: &harness_tools::jobs::JobManager,
) -> (harness_tools::jobs::JobInfo, String) {
    for _ in 0..400 {
        if let Some(done) = jobs.take_finished().pop() {
            return done;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    panic!("no job finished: {:?}", jobs.list());
}

/// Background bash jobs run on a remote host through the helper like
/// locally: the call returns at once, output and the exit come back, and a
/// stop kills the job's whole process group on the host.
#[tokio::test(flavor = "multi_thread")]
async fn background_jobs_run_and_stop_through_the_helper() {
    let s = setup().await;
    let dir = tempfile::tempdir().unwrap();
    let exec: Arc<dyn Exec> = Arc::new(RemoteExec::new(
        s.client.clone(),
        RemoteEnv::new(scrubber()),
    ));
    let tool = bash(exec, dir.path());
    let ctx = ToolContext::new("r1");
    let out = tool
        .call(
            &ctx,
            json!({"command": "echo \"remote $FOO\"; exit 4", "run_in_background": true}),
        )
        .await
        .unwrap();
    assert!(
        out.text().contains("background job j1 started"),
        "{}",
        out.text()
    );
    let (info, tail) = finished_job(tool.jobs()).await;
    assert_eq!(info.status, harness_tools::jobs::JobStatus::Exited(4));
    assert_eq!(tail, "remote visible\n");

    tool.call(
        &ctx,
        json!({"command": "sleep 60 & echo $! > pid; wait", "run_in_background": true}),
    )
    .await
    .unwrap();
    let pid_file = dir.path().join("pid");
    let mut pid = String::new();
    for _ in 0..200 {
        if let Ok(p) = std::fs::read_to_string(&pid_file)
            && p.ends_with('\n')
        {
            pid = p.trim().to_owned();
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    let pid: u32 = pid.parse().expect("the job wrote its child's pid");
    let info = tool.jobs().stop("j2").await.unwrap();
    assert_eq!(info.status, harness_tools::jobs::JobStatus::Stopped);
    for _ in 0..100 {
        if harness_tools::conformance::local_process_gone(pid) {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    panic!("the job's child {pid} survived the stop");
}

/// Without the helper (the degraded shell fallback) background jobs still
/// work: harness supervises the shell session and reports the exit.
#[tokio::test(flavor = "multi_thread")]
async fn background_jobs_work_on_the_shell_fallback() {
    use harness_remote::shell::{LocalShell, ShellExec};
    let dir = tempfile::tempdir().unwrap();
    let exec: Arc<dyn Exec> = Arc::new(ShellExec::new(Arc::new(LocalShell::default())));
    let tool = bash(exec, dir.path());
    let out = tool
        .call(
            &ToolContext::new("s1"),
            json!({"command": "sleep 0.2; echo fallback", "run_in_background": true}),
        )
        .await
        .unwrap();
    assert!(
        out.text().contains("background job j1 started"),
        "{}",
        out.text()
    );
    let (info, tail) = finished_job(tool.jobs()).await;
    assert_eq!(info.status, harness_tools::jobs::JobStatus::Exited(0));
    assert!(tail.contains("fallback\n"), "{tail}");
}
