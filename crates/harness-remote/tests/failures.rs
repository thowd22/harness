//! Remote failure handling: disconnects mid-exec reattach when the helper is
//! alive, fail the step when it is not, and remote processes are cleaned up
//! on cancel and on failure.

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use harness_remote::client::{
    ClientOptions, Connection, ConnectionState, Connector, InProcessConnector, RemoteClient,
    spawn_connection,
};
use harness_remote::helper::{Helper, HelperOptions};
use harness_remote::workspace::{RemoteEnv, RemoteExec};
use harness_tools::exec::{Exec, ExecError, ExecHandle, ExecRequest};
use tokio::task::JoinHandle;

fn opts(host: &str) -> ClientOptions {
    let mut o = ClientOptions::new(host);
    o.reconnect_delay = Duration::from_millis(20);
    o.keepalive = None;
    o
}

fn sh(cwd: &Path, script: &str) -> ExecRequest {
    ExecRequest::new("sh", vec!["-c".into(), script.into()], cwd)
        .with_timeout(Duration::from_secs(30))
}

async fn read_until(h: &mut ExecHandle, buf: &mut String, needle: &str) {
    while !buf.contains(needle) {
        let c = tokio::time::timeout(Duration::from_secs(10), h.next_output())
            .await
            .expect("output in time")
            .expect("stream open");
        buf.push_str(&String::from_utf8_lossy(&c.data));
    }
}

/// `/proc` on Linux, `ps` elsewhere: never "dead" just because `/proc` is
/// missing (macOS).
fn pid_dead(pid: u32) -> bool {
    harness_tools::conformance::local_process_gone(pid)
}

async fn wait_dead(pid: u32) {
    for _ in 0..100 {
        if pid_dead(pid) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("process {pid} was not cleaned up");
}

async fn wait_for(mut f: impl FnMut() -> bool, what: &str) {
    for _ in 0..200 {
        if f() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("timed out waiting for {what}");
}

const COUNTER: &str = "for i in 1 2 3 4 5 6; do echo line$i; sleep 0.15; done";

#[tokio::test(flavor = "multi_thread")]
async fn disconnect_mid_exec_reattaches_without_gaps_or_duplicates() {
    let dir = tempfile::tempdir().unwrap();
    let conn = InProcessConnector::new(Helper::new(HelperOptions::default()));
    let client = RemoteClient::connect(Arc::new(conn.clone()), opts("box"))
        .await
        .unwrap();
    let mut events = client.subscribe();
    let ex = RemoteExec::new(client.clone(), RemoteEnv::default());
    let mut h = ex.spawn(sh(dir.path(), COUNTER)).await.unwrap();
    let mut out = String::new();
    read_until(&mut h, &mut out, "line2\n").await;

    conn.disconnect_all();

    let mut rest = Vec::new();
    let st = h.drive(|c| rest.extend_from_slice(&c.data)).await.unwrap();
    out.push_str(&String::from_utf8_lossy(&rest));
    assert!(st.success(), "{st:?}");
    let want: String = (1..=6).map(|i| format!("line{i}\n")).collect();
    assert_eq!(out, want, "every line exactly once, in order");

    let mut states = Vec::new();
    while let Ok(e) = events.try_recv() {
        states.push(e.state);
    }
    assert!(
        states
            .iter()
            .any(|s| matches!(s, ConnectionState::Disconnected { .. })),
        "{states:?}"
    );
    assert!(
        states.contains(&ConnectionState::Connected {
            reconnected: true,
            reattached: 1
        }),
        "{states:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn dead_helper_fails_the_step_and_its_processes_are_cleaned_up() {
    let dir = tempfile::tempdir().unwrap();
    let old = Helper::new(HelperOptions {
        grace: Duration::from_millis(300),
        ..Default::default()
    });
    let conn = InProcessConnector::new(old.clone());
    let client = RemoteClient::connect(Arc::new(conn.clone()), opts("box"))
        .await
        .unwrap();
    let mut events = client.subscribe();
    let ex = RemoteExec::new(client.clone(), RemoteEnv::default());
    let mut h = ex
        .spawn(sh(dir.path(), "sleep 30 & echo $!; wait"))
        .await
        .unwrap();
    let mut out = String::new();
    read_until(&mut h, &mut out, "\n").await;
    let pid: u32 = out.trim().parse().unwrap();

    // The helper "dies": the next connection reaches a fresh helper.
    conn.set_helper(Helper::new(HelperOptions::default()));
    conn.disconnect_all();

    let err = tokio::time::timeout(Duration::from_secs(10), h.drive(|_| {}))
        .await
        .expect("does not hang")
        .unwrap_err();
    assert!(
        matches!(err, ExecError::Lost),
        "step fails for retry: {err:?}"
    );
    let mut lost = false;
    while let Ok(e) = events.try_recv() {
        lost |= matches!(e.state, ConnectionState::JobLost { .. });
    }
    assert!(lost, "JobLost event emitted");

    // The orphaned helper kills its jobs once no client returns.
    tokio::time::timeout(Duration::from_secs(10), old.wait_idle())
        .await
        .unwrap();
    assert_eq!(old.running_jobs(), 0);
    wait_dead(pid).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn network_down_fails_jobs_and_helper_cleans_up() {
    let dir = tempfile::tempdir().unwrap();
    let helper = Helper::new(HelperOptions {
        grace: Duration::from_millis(300),
        ..Default::default()
    });
    let conn = InProcessConnector::new(helper.clone());
    let client = RemoteClient::connect(Arc::new(conn.clone()), opts("box"))
        .await
        .unwrap();
    let ex = RemoteExec::new(client.clone(), RemoteEnv::default());
    let mut h = ex
        .spawn(sh(dir.path(), "sleep 30 & echo $!; wait"))
        .await
        .unwrap();
    let mut out = String::new();
    read_until(&mut h, &mut out, "\n").await;
    let pid: u32 = out.trim().parse().unwrap();

    conn.set_refuse(true);
    conn.disconnect_all();
    let err = tokio::time::timeout(Duration::from_secs(10), h.drive(|_| {}))
        .await
        .expect("does not hang")
        .unwrap_err();
    assert!(matches!(err, ExecError::Lost));
    tokio::time::timeout(Duration::from_secs(10), helper.wait_idle())
        .await
        .unwrap();
    wait_dead(pid).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn cancel_kills_the_remote_process_group() {
    let dir = tempfile::tempdir().unwrap();
    let helper = Helper::new(HelperOptions::default());
    let conn = InProcessConnector::new(helper.clone());
    let client = RemoteClient::connect(Arc::new(conn), opts("box"))
        .await
        .unwrap();
    let ex = RemoteExec::new(client, RemoteEnv::default());
    let mut h = ex
        .spawn(sh(dir.path(), "sleep 30 & echo $!; wait"))
        .await
        .unwrap();
    let mut out = String::new();
    read_until(&mut h, &mut out, "\n").await;
    let pid: u32 = out.trim().parse().unwrap();
    drop(h); // the tool call's future was cancelled
    wait_dead(pid).await;
    wait_for(|| helper.running_jobs() == 0, "helper job to end").await;
}

/// Spawns the real helper binary and relays its stdio, so a test can cut
/// the "network" while the helper process lives on (as with a dropped ssh
/// connection).
#[derive(Clone)]
struct CuttablePipe {
    args: Vec<String>,
    children: Arc<Mutex<Vec<tokio::process::Child>>>,
    relays: Arc<Mutex<Vec<JoinHandle<()>>>>,
}

impl CuttablePipe {
    fn cut(&self) {
        for t in self.relays.lock().unwrap().drain(..) {
            t.abort();
        }
    }
}

#[async_trait]
impl Connector for CuttablePipe {
    async fn connect(&self, session: &str) -> std::io::Result<Connection> {
        let mut cmd = tokio::process::Command::new(env!("CARGO_BIN_EXE_harness-remote-agent"));
        cmd.args(&self.args).args(["--session", session]);
        let mut conn = spawn_connection(cmd)?;
        let child = conn.child.take().unwrap();
        self.children.lock().unwrap().push(child);
        let (client_side, relay_side) = tokio::io::duplex(1 << 20);
        let (mut rr, mut rw) = tokio::io::split(relay_side);
        let (mut hr, mut hw) = (conn.reader, conn.writer);
        let up = tokio::spawn(async move {
            let _ = tokio::io::copy(&mut rr, &mut hw).await;
        });
        let down = tokio::spawn(async move {
            let _ = tokio::io::copy(&mut hr, &mut rw).await;
        });
        self.relays.lock().unwrap().extend([up, down]);
        let (cr, cw) = tokio::io::split(client_side);
        Ok(Connection {
            reader: Box::new(cr),
            writer: Box::new(cw),
            child: None,
            stderr: None,
        })
    }

    fn host(&self) -> String {
        "pipe".into()
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn helper_process_survives_disconnect_and_reattaches_through_its_socket() {
    let dir = tempfile::tempdir().unwrap();
    let sock = socket_dir();
    let pipe = CuttablePipe {
        args: vec![
            "--stdio".into(),
            "--grace".into(),
            "5".into(),
            "--socket-dir".into(),
            sock.path().display().to_string(),
        ],
        children: Arc::default(),
        relays: Arc::default(),
    };
    let client = RemoteClient::connect(Arc::new(pipe.clone()), opts("pipe"))
        .await
        .unwrap();
    let first_pid = client.hello().unwrap().pid;
    let ex = RemoteExec::new(client.clone(), RemoteEnv::default());
    let mut h = ex.spawn(sh(dir.path(), COUNTER)).await.unwrap();
    let mut out = String::new();
    read_until(&mut h, &mut out, "line2\n").await;

    pipe.cut();

    let mut rest = Vec::new();
    let st = tokio::time::timeout(
        Duration::from_secs(20),
        h.drive(|c| rest.extend_from_slice(&c.data)),
    )
    .await
    .expect("finishes")
    .unwrap();
    out.push_str(&String::from_utf8_lossy(&rest));
    assert!(st.success(), "{st:?}");
    let want: String = (1..=6).map(|i| format!("line{i}\n")).collect();
    assert_eq!(out, want);
    assert_eq!(
        client.hello().unwrap().pid,
        first_pid,
        "the second connection proxied to the same helper process"
    );
    assert!(
        pipe.children.lock().unwrap().len() >= 2,
        "a new stdio process was started for the reconnect"
    );
    client.close();
}

/// A short directory for the helper's reattach socket. Unix socket paths are
/// capped (104 bytes on macOS, whose `$TMPDIR` alone is ~50), and
/// `helper-<32 hex>.sock` adds 44, so sockets live under `/tmp`, not `$TMPDIR`.
fn socket_dir() -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix("hs")
        .tempdir_in("/tmp")
        .expect("create a socket dir under /tmp")
}
