//! Content-hash cache for remote file transfers (TASK-131): content the
//! other side already holds travels as its hash, every miss falls back to a
//! full transfer, tool results never change, and the bytes saved reach the
//! run log.

use std::path::Path;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use harness_events::redact::Redactor;
use harness_events::schema::{EventKind, RunId};
use harness_events::store::{RecorderOptions, RunRecorder, read_run};
use harness_remote::audit::{AuditEvent, RecorderAudit, RecordingAudit};
use harness_remote::cache::content_hash;
use harness_remote::client::{
    ClientOptions, Connection, Connector, InProcessConnector, RemoteClient, WireCompression,
};
use harness_remote::helper::{Helper, HelperOptions};
use harness_remote::protocol::{
    HELPER_VERSION, Message, WriteParams, b64, codes, methods, read_frame, write_frame,
};
use harness_remote::simlink::{LinkSpec, LinkStats, SimLinkConnector, sample_text};
use harness_remote::workspace::RemoteWorkspace;
use harness_tools::config::ToolsConfig;
use harness_tools::edit::EditTool;
use harness_tools::read::ReadTool;
use harness_tools::tool::{RecordingEvents, ToolContext, ToolEvent};
use harness_tools::tracker::ReadTracker;
use harness_tools::workspace::Workspace;
use harness_tools::write::WriteTool;
use serde_json::{Value, json};
use tokio::io::BufReader;

const FILE: usize = 100_000;

struct Rig {
    client: RemoteClient,
    helper: Helper,
    stats: Arc<LinkStats>,
    audit: Arc<RecordingAudit>,
}

fn helper_with(cache: usize, max_protocol: u32) -> Helper {
    Helper::new(HelperOptions {
        content_cache: cache,
        max_protocol,
        ..Default::default()
    })
}

/// A client over a byte-counting link. Wire compression is off so byte
/// counts reflect payload sizes.
async fn rig_with(helper: Helper, client_cache: usize) -> Rig {
    let conn = SimLinkConnector::new(helper.clone(), LinkSpec::mbps(10_000.0));
    let stats = conn.stats();
    let audit = Arc::new(RecordingAudit::default());
    let mut opts = ClientOptions::new("box")
        .with_compression(WireCompression::off())
        .with_content_cache(client_cache)
        .with_audit(audit.clone());
    opts.keepalive = None;
    let client = RemoteClient::connect(Arc::new(conn), opts).await.unwrap();
    Rig {
        client,
        helper,
        stats,
        audit,
    }
}

async fn rig() -> Rig {
    rig_with(Helper::new(HelperOptions::default()), 1 << 26).await
}

fn ws(r: &Rig, dir: &Path) -> RemoteWorkspace {
    RemoteWorkspace::new(r.client.clone(), dir.to_string_lossy())
}

fn last(audit: &RecordingAudit, op: &str) -> AuditEvent {
    audit
        .events()
        .into_iter()
        .rev()
        .find(|e| e.op == op)
        .unwrap_or_else(|| panic!("no {op} event"))
}

/// Bytes moved in each direction by `f`.
async fn measure<F: std::future::Future<Output = ()>>(stats: &LinkStats, f: F) -> (u64, u64) {
    let (u, d) = (stats.up(), stats.down());
    f.await;
    (stats.up() - u, stats.down() - d)
}

#[tokio::test(flavor = "multi_thread")]
async fn repeat_read_transfers_only_the_hash() {
    let r = rig().await;
    let dir = tempfile::tempdir().unwrap();
    let data = sample_text(FILE);
    std::fs::write(dir.path().join("a.txt"), &data).unwrap();
    let w = ws(&r, dir.path());

    let (_, down) = measure(&r.stats, async {
        assert_eq!(w.read(Path::new("a.txt")).await.unwrap(), data);
    })
    .await;
    assert!(down > FILE as u64, "first read is a full transfer: {down}");
    assert_eq!(last(&r.audit, "fs.read").bytes_saved, 0);

    let (up, down) = measure(&r.stats, async {
        assert_eq!(w.read(Path::new("a.txt")).await.unwrap(), data);
    })
    .await;
    assert!(down < 512 && up < 512, "hash only: {up} up, {down} down");
    let ev = last(&r.audit, "fs.read");
    assert!(ev.ok);
    assert_eq!(ev.bytes_saved, FILE as u64);
    assert_eq!(
        r.audit
            .events()
            .iter()
            .filter(|e| e.op == "fs.read")
            .count(),
        2,
        "one audit event per read"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn writes_of_content_the_helper_holds_transfer_only_the_hash() {
    let r = rig().await;
    let dir = tempfile::tempdir().unwrap();
    let data = sample_text(FILE);
    std::fs::write(dir.path().join("src.txt"), &data).unwrap();
    let w = ws(&r, dir.path());
    w.read(Path::new("src.txt")).await.unwrap();

    // The helper read this content, so a copy needs only its hash.
    let (up, _) = measure(&r.stats, async {
        w.write(Path::new("copy.txt"), &data).await.unwrap();
    })
    .await;
    assert!(up < 512, "write by hash: {up} bytes up");
    assert_eq!(std::fs::read(dir.path().join("copy.txt")).unwrap(), data);
    assert_eq!(last(&r.audit, "fs.write").bytes_saved, FILE as u64);

    // New content is sent in full; reading it back is then a hash.
    let mut changed = data.clone();
    changed.extend_from_slice(b"one more line\n");
    let (up, _) = measure(&r.stats, async {
        w.replace_atomic(Path::new("copy.txt"), &changed)
            .await
            .unwrap();
    })
    .await;
    assert!(up > FILE as u64, "{up}");
    assert_eq!(last(&r.audit, "fs.replace_atomic").bytes_saved, 0);
    let (_, down) = measure(&r.stats, async {
        assert_eq!(w.read(Path::new("copy.txt")).await.unwrap(), changed);
    })
    .await;
    assert!(down < 512, "read after write: {down}");
    // Reverting to content the helper holds is a hash again.
    let (up, _) = measure(&r.stats, async {
        w.write(Path::new("copy.txt"), &data).await.unwrap();
    })
    .await;
    assert!(up < 512, "{up}");
    assert_eq!(std::fs::read(dir.path().join("copy.txt")).unwrap(), data);
}

#[tokio::test(flavor = "multi_thread")]
async fn caches_are_bounded_per_side() {
    let cap = 64 * 1024;
    let r = rig_with(helper_with(cap, 2), cap).await;
    let dir = tempfile::tempdir().unwrap();
    let w = ws(&r, dir.path());
    for i in 0..40u8 {
        let body = vec![b'a' + (i % 26); 10_000 + usize::from(i)];
        w.write(Path::new(&format!("f{i}.txt")), &body)
            .await
            .unwrap();
        assert_eq!(w.read(Path::new(&format!("f{i}.txt"))).await.unwrap(), body);
    }
    for stats in [r.client.cache_stats(), r.helper.cache_stats()] {
        assert!(stats.bytes <= cap, "{stats:?}");
        assert!(stats.evictions > 0, "{stats:?}");
        assert!(stats.entries > 0, "{stats:?}");
    }
    // Files larger than a quarter of the cache are never cached.
    let big = sample_text(cap / 2);
    w.write(Path::new("big.txt"), &big).await.unwrap();
    let (_, down) = measure(&r.stats, async {
        assert_eq!(w.read(Path::new("big.txt")).await.unwrap(), big);
    })
    .await;
    assert!(down > (cap / 2) as u64);
}

#[tokio::test(flavor = "multi_thread")]
async fn misses_and_evictions_fall_back_to_full_transfers() {
    let r = rig().await;
    let dir = tempfile::tempdir().unwrap();
    let data = sample_text(FILE);
    std::fs::write(dir.path().join("a.txt"), &data).unwrap();
    let w = ws(&r, dir.path());
    w.read(Path::new("a.txt")).await.unwrap();

    // The helper lost its copy: the write by hash misses and is resent.
    r.helper.clear_cache();
    let (up, _) = measure(&r.stats, async {
        w.write(Path::new("b.txt"), &data).await.unwrap();
    })
    .await;
    assert!(up > FILE as u64, "resent in full: {up}");
    assert_eq!(std::fs::read(dir.path().join("b.txt")).unwrap(), data);
    let ev = last(&r.audit, "fs.write");
    assert!(ev.ok && ev.error.is_none(), "{ev:?}");
    assert_eq!(ev.bytes_saved, 0);

    // The client lost its copy: the read is a full transfer.
    r.client.clear_cache();
    let (_, down) = measure(&r.stats, async {
        assert_eq!(w.read(Path::new("a.txt")).await.unwrap(), data);
    })
    .await;
    assert!(down > FILE as u64, "{down}");

    // The file changed behind our back: the hash differs, the new bytes come.
    let edited = sample_text(FILE + 7);
    std::fs::write(dir.path().join("a.txt"), &edited).unwrap();
    assert_eq!(w.read(Path::new("a.txt")).await.unwrap(), edited);
    assert_eq!(last(&r.audit, "fs.read").bytes_saved, 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn helper_restart_on_reconnect_falls_back_without_errors() {
    let dir = tempfile::tempdir().unwrap();
    let data = sample_text(FILE);
    std::fs::write(dir.path().join("a.txt"), &data).unwrap();
    let conn = Arc::new(InProcessConnector::new(Helper::new(
        HelperOptions::default(),
    )));
    let audit = Arc::new(RecordingAudit::default());
    let client = RemoteClient::connect(
        conn.clone(),
        ClientOptions::new("box").with_audit(audit.clone()),
    )
    .await
    .unwrap();
    let w = RemoteWorkspace::new(client.clone(), dir.path().to_string_lossy());
    w.read(Path::new("a.txt")).await.unwrap();
    // The helper dies and a new one (empty cache) answers the reconnect.
    conn.set_helper(Helper::new(HelperOptions::default()));
    conn.disconnect_all();
    w.write(Path::new("b.txt"), &data).await.unwrap();
    assert_eq!(std::fs::read(dir.path().join("b.txt")).unwrap(), data);
    assert_eq!(w.read(Path::new("b.txt")).await.unwrap(), data);
    let writes: Vec<_> = audit
        .events()
        .into_iter()
        .filter(|e| e.op == "fs.write")
        .collect();
    assert_eq!(writes.len(), 1);
    assert!(writes[0].ok, "{:?}", writes[0]);
    assert_eq!(
        writes[0].bytes_saved, 0,
        "the hash missed and the bytes went"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn corrupted_client_copy_is_never_served() {
    let r = rig().await;
    let dir = tempfile::tempdir().unwrap();
    let data = sample_text(FILE);
    std::fs::write(dir.path().join("a.txt"), &data).unwrap();
    let w = ws(&r, dir.path());
    w.read(Path::new("a.txt")).await.unwrap();
    let h = content_hash(&data);
    r.client
        .with_cache_for_test(|c| c.corrupt_for_test(&h, b"garbage".to_vec()));
    // The helper says "cached", the client's copy fails verification, and
    // the bytes are fetched again.
    assert_eq!(w.read(Path::new("a.txt")).await.unwrap(), data);
    let ev = last(&r.audit, "fs.read");
    assert!(ev.ok);
    assert_eq!(ev.bytes_saved, 0);
    // The good copy is cached again.
    assert_eq!(w.read(Path::new("a.txt")).await.unwrap(), data);
    assert_eq!(last(&r.audit, "fs.read").bytes_saved, FILE as u64);
}

#[tokio::test(flavor = "multi_thread")]
async fn helper_refuses_unknown_and_mismatching_hashes() {
    let r = rig().await;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("x.txt").to_string_lossy().into_owned();
    let unknown = r
        .client
        .call::<_, Value>(
            methods::FS_WRITE,
            None,
            WriteParams {
                path: path.clone(),
                data: Vec::new(),
                hash: Some(content_hash(b"never sent")),
                cached: true,
            },
        )
        .await
        .unwrap_err();
    assert_eq!(unknown.rpc_code(), Some(codes::CACHE_MISS));
    let mismatch = r
        .client
        .call::<_, Value>(
            methods::FS_WRITE,
            None,
            WriteParams {
                path: path.clone(),
                data: b"actual".to_vec(),
                hash: Some(content_hash(b"claimed")),
                cached: false,
            },
        )
        .await
        .unwrap_err();
    assert_eq!(mismatch.rpc_code(), Some(codes::HASH_MISMATCH));
    assert!(!Path::new(&path).exists(), "nothing was written");
}

/// A protocol 1 helper that advertises a cache but misbehaves: it rejects
/// every write that carries a hash as mismatching, and answers every read
/// that offers a hash as `cached` under a different hash.
async fn liar(stream: tokio::io::DuplexStream, writes: Arc<Mutex<Vec<Value>>>) {
    let (r, mut w) = tokio::io::split(stream);
    let mut r = BufReader::new(r);
    while let Ok(Some(line)) = read_frame(&mut r).await {
        let Message::Request { id, method, params } = Message::parse(&line).unwrap() else {
            continue;
        };
        let result = match method.as_str() {
            "hello" => Ok(
                json!({"protocol": 1, "version": HELPER_VERSION, "arch": "x",
                "os": "linux", "session": "s", "pid": 7, "jobs": 0, "content_cache": 1_000_000}),
            ),
            "fs.read" => {
                let data = std::fs::read(params["path"].as_str().unwrap()).unwrap();
                if params.get("have").is_some() {
                    Ok(json!({"hash": content_hash(b"other"), "cached": true}))
                } else {
                    Ok(json!({"data": b64::encode(&data), "hash": content_hash(&data)}))
                }
            }
            "fs.write" => {
                writes.lock().unwrap().push(params.clone());
                if params.get("hash").is_some() {
                    Err(harness_remote::protocol::RpcError::new(
                        codes::HASH_MISMATCH,
                        "mismatch",
                    ))
                } else {
                    use base64::Engine;
                    let data = base64::engine::general_purpose::STANDARD
                        .decode(params["data"].as_str().unwrap())
                        .unwrap();
                    std::fs::write(params["path"].as_str().unwrap(), data).unwrap();
                    Ok(json!({}))
                }
            }
            _ => Ok(json!({})),
        };
        let resp = Message::Response { id, result };
        if write_frame(&mut w, &resp.to_line()).await.is_err() {
            break;
        }
    }
}

struct Liar {
    writes: Arc<Mutex<Vec<Value>>>,
}

#[async_trait]
impl Connector for Liar {
    async fn connect(&self, _session: &str) -> std::io::Result<Connection> {
        let (client_side, helper_side) = tokio::io::duplex(1 << 20);
        tokio::spawn(liar(helper_side, self.writes.clone()));
        let (cr, cw) = tokio::io::split(client_side);
        Ok(Connection {
            reader: Box::new(cr),
            writer: Box::new(cw),
            child: None,
            stderr: None,
        })
    }
    fn host(&self) -> String {
        "liar".into()
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn hash_mismatches_fall_back_to_full_transfers() {
    let dir = tempfile::tempdir().unwrap();
    let writes = Arc::new(Mutex::new(Vec::new()));
    let audit = Arc::new(RecordingAudit::default());
    let client = RemoteClient::connect(
        Arc::new(Liar {
            writes: writes.clone(),
        }),
        ClientOptions::new("liar").with_audit(audit.clone()),
    )
    .await
    .unwrap();
    let w = RemoteWorkspace::new(client, dir.path().to_string_lossy());
    let data = sample_text(5000);
    w.write(Path::new("a.txt"), &data).await.unwrap();
    assert_eq!(std::fs::read(dir.path().join("a.txt")).unwrap(), data);
    let sent = writes.lock().unwrap().clone();
    assert_eq!(sent.len(), 2, "hashed attempt, then plain bytes: {sent:?}");
    assert!(sent[0].get("hash").is_some() && sent[1].get("hash").is_none());
    let ev = last(&audit, "fs.write");
    assert!(ev.ok && ev.bytes_saved == 0, "{ev:?}");

    // A read answered `cached` under a hash we did not offer is refetched.
    std::fs::write(dir.path().join("b.txt"), &data).unwrap();
    assert_eq!(w.read(Path::new("b.txt")).await.unwrap(), data);
    assert_eq!(w.read(Path::new("b.txt")).await.unwrap(), data);
    let ev = last(&audit, "fs.read");
    assert!(ev.ok && ev.bytes_saved == 0, "{ev:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn cache_works_on_protocol_1() {
    let r = rig_with(helper_with(1 << 26, 1), 1 << 26).await;
    assert_eq!(r.client.hello().unwrap().protocol, 1);
    let dir = tempfile::tempdir().unwrap();
    let data = sample_text(FILE);
    std::fs::write(dir.path().join("a.txt"), &data).unwrap();
    let w = ws(&r, dir.path());
    assert_eq!(w.read(Path::new("a.txt")).await.unwrap(), data);
    let (up, down) = measure(&r.stats, async {
        assert_eq!(w.read(Path::new("a.txt")).await.unwrap(), data);
        w.write(Path::new("b.txt"), &data).await.unwrap();
    })
    .await;
    assert!(up < 1024 && down < 1024, "{up} up, {down} down");
    assert_eq!(std::fs::read(dir.path().join("b.txt")).unwrap(), data);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_helper_without_cache_gets_full_transfers() {
    let r = rig_with(helper_with(0, 2), 1 << 26).await;
    assert_eq!(r.client.hello().unwrap().content_cache, None);
    let dir = tempfile::tempdir().unwrap();
    let data = sample_text(FILE);
    let w = ws(&r, dir.path());
    w.write(Path::new("a.txt"), &data).await.unwrap();
    let (up, down) = measure(&r.stats, async {
        assert_eq!(w.read(Path::new("a.txt")).await.unwrap(), data);
        w.write(Path::new("b.txt"), &data).await.unwrap();
    })
    .await;
    assert!(
        up > FILE as u64 && down > FILE as u64,
        "{up} up, {down} down"
    );
    assert_eq!(std::fs::read(dir.path().join("b.txt")).unwrap(), data);
}

/// A tool session that reads, re-reads, edits, writes copies and hits a
/// stale read; returns every tool result, ledger event and final file.
async fn tool_session(ws: Arc<dyn Workspace>, root: &Path) -> Vec<String> {
    let tracker = Arc::new(ReadTracker::new());
    let events = Arc::new(RecordingEvents::new());
    let ctx = ToolContext::new("c").with_events(events.clone());
    let write = WriteTool::new(ws.clone(), tracker.clone());
    let read = ReadTool::new(ws.clone(), tracker.clone(), ToolsConfig::default());
    let edit = EditTool::new(ws.clone(), tracker.clone());
    let body: String = (0..400)
        .map(|i| format!("line {i}: let x{i} = {i};\n"))
        .collect();
    let mut out = Vec::new();
    let mut push = |r: Result<harness_tools::tool::ToolOutput, harness_tools::tool::ToolError>| {
        out.push(match r {
            Ok(o) => format!("ok {}", o.text()),
            Err(e) => format!("err {:?} {}", e.kind, e),
        })
    };
    push(
        write
            .call(&ctx, json!({"path": "src/lib.rs", "content": body}))
            .await,
    );
    push(read.call(&ctx, json!({"path": "src/lib.rs"})).await);
    push(read.call(&ctx, json!({"path": "src/lib.rs"})).await);
    push(
        read.call(
            &ctx,
            json!({"path": "src/lib.rs", "offset": 100, "limit": 5}),
        )
        .await,
    );
    push(
        edit.call(
            &ctx,
            json!({"path": "src/lib.rs", "old_string": "line 7: let x7 = 7;", "new_string": "line 7: let x7 = 70;"}),
        )
        .await,
    );
    push(read.call(&ctx, json!({"path": "src/lib.rs"})).await);
    push(
        write
            .call(&ctx, json!({"path": "src/copy.rs", "content": body}))
            .await,
    );
    push(read.call(&ctx, json!({"path": "src/copy.rs"})).await);
    // Out-of-band change on the host, then a stale edit and a fresh read.
    std::fs::write(root.join("src/lib.rs"), "changed elsewhere\n").unwrap();
    push(
        edit.call(
            &ctx,
            json!({"path": "src/lib.rs", "old_string": "changed", "new_string": "x"}),
        )
        .await,
    );
    push(read.call(&ctx, json!({"path": "src/lib.rs"})).await);
    push(
        edit.call(
            &ctx,
            json!({"path": "src/lib.rs", "old_string": "changed", "new_string": "x"}),
        )
        .await,
    );
    push(read.call(&ctx, json!({"path": "src/missing.rs"})).await);
    push(read.call(&ctx, json!({"path": "src/lib.rs"})).await);
    for (_, e) in events.events() {
        if let ToolEvent::FileChanged(fc) = e {
            out.push(format!(
                "{} {} {:?} {:?} {}\n{}",
                fc.tool,
                fc.path.strip_prefix(ws.root()).unwrap().display(),
                fc.kind,
                fc.before_hash,
                fc.after_hash,
                fc.diff
            ));
        }
    }
    for f in ["src/lib.rs", "src/copy.rs"] {
        out.push(String::from_utf8(std::fs::read(root.join(f)).unwrap()).unwrap());
    }
    let root_s = root.to_string_lossy().into_owned();
    out.into_iter()
        .map(|s| s.replace(&root_s, "<root>"))
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn tool_results_are_identical_with_and_without_the_cache() {
    let (cached_dir, plain_dir) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let cached = rig().await;
    let plain = rig_with(helper_with(0, 2), 0).await;
    let with = tool_session(Arc::new(ws(&cached, cached_dir.path())), cached_dir.path()).await;
    let without = tool_session(Arc::new(ws(&plain, plain_dir.path())), plain_dir.path()).await;
    assert_eq!(with, without);
    assert!(with.iter().any(|s| s.starts_with("err ")), "covers errors");
    let saved = |r: &Rig| r.audit.events().iter().map(|e| e.bytes_saved).sum::<u64>();
    assert!(saved(&cached) > 0, "the cache was used");
    assert_eq!(saved(&plain), 0);
    assert!(
        cached.stats.up() + cached.stats.down() < plain.stats.up() + plain.stats.down(),
        "fewer bytes moved with the cache"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn bytes_saved_reach_the_run_log() {
    let data_dir = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let body = sample_text(FILE);
    std::fs::write(work.path().join("a.txt"), &body).unwrap();
    let recorder = RunRecorder::create(
        data_dir.path(),
        RunId::new(),
        Arc::new(Redactor::default()),
        RecorderOptions::default(),
    )
    .await
    .unwrap();
    let opts = ClientOptions::new("gpu-box")
        .with_audit(Arc::new(RecorderAudit::new(recorder.clone(), None)));
    let client = RemoteClient::connect(
        Arc::new(InProcessConnector::new(Helper::new(
            HelperOptions::default(),
        ))),
        opts,
    )
    .await
    .unwrap();
    let w = RemoteWorkspace::new(client.clone(), work.path().to_string_lossy());
    w.read(Path::new("a.txt")).await.unwrap();
    w.read(Path::new("a.txt")).await.unwrap();
    w.write(Path::new("b.txt"), &body).await.unwrap();
    client.close();
    recorder.finish().await.unwrap();
    let ops: Vec<(String, u64)> = read_run(recorder.dir())
        .unwrap()
        .events
        .iter()
        .filter_map(|e| match &e.kind {
            EventKind::RemoteOp {
                op, bytes_saved, ..
            } if op.starts_with("fs.") => Some((op.clone(), *bytes_saved)),
            _ => None,
        })
        .collect();
    let n = FILE as u64;
    assert_eq!(
        ops,
        vec![
            ("fs.read".to_string(), 0),
            ("fs.read".to_string(), n),
            ("fs.write".to_string(), n)
        ]
    );
}
