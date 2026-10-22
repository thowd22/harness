//! Helper protocol 2: negotiated zstd compression, binary payloads, frame
//! limits, and compatibility with protocol 1 helpers and clients.

use std::path::Path;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};

use async_trait::async_trait;
use harness_remote::client::{
    ClientOptions, CommandConnector, Connection, Connector, InProcessConnector, RemoteClient,
    SshConnector, WireCompression,
};
use harness_remote::helper::{Helper, HelperOptions};
use harness_remote::protocol::{
    HELPER_VERSION, MAX_FRAME, Message, PROTOCOL_VERSION, ZSTD, b64, read_frame, write_frame,
};
use harness_remote::simlink::{LinkSpec, SimLinkConnector, WireMode, bench, sample_text};
use harness_remote::ssh::{SshOptions, SshTarget};
use harness_remote::workspace::{RemoteEnv, RemoteExec, RemoteWorkspace};
use harness_tools::conformance::{exec_suite, workspace_suite};
use harness_tools::exec::{Exec, ExecRequest};
use harness_tools::workspace::Workspace;
use serde_json::{Value, json};
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt, BufReader, ReadBuf};

type Log = Arc<Mutex<Vec<u8>>>;

/// Records the bytes passing through a reader or writer.
struct Tap<T> {
    inner: T,
    log: Log,
}

impl<T: AsyncWrite + Unpin> AsyncWrite for Tap<T> {
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

impl<T: AsyncRead + Unpin> AsyncRead for Tap<T> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let before = buf.filled().len();
        let r = Pin::new(&mut self.inner).poll_read(cx, buf);
        if let Poll::Ready(Ok(())) = &r {
            let new = buf.filled()[before..].to_vec();
            self.log.lock().unwrap().extend_from_slice(&new);
        }
        r
    }
}

/// Taps both directions of an in-process helper connection.
struct Tapped {
    inner: InProcessConnector,
    up: Log,
    down: Log,
    compressed: bool,
}

impl Tapped {
    fn new(helper: Helper) -> Self {
        Self {
            inner: InProcessConnector::new(helper),
            up: Log::default(),
            down: Log::default(),
            compressed: false,
        }
    }
}

#[async_trait]
impl Connector for Tapped {
    async fn connect(&self, session: &str) -> std::io::Result<Connection> {
        let mut c = self.inner.connect(session).await?;
        c.writer = Box::new(Tap {
            inner: c.writer,
            log: self.up.clone(),
        });
        c.reader = Box::new(Tap {
            inner: c.reader,
            log: self.down.clone(),
        });
        Ok(c)
    }
    fn host(&self) -> String {
        "tapped".into()
    }
    fn transport_compressed(&self) -> bool {
        self.compressed
    }
}

fn old_helper() -> Helper {
    Helper::new(HelperOptions {
        max_protocol: 1,
        ..Default::default()
    })
}

fn new_helper() -> Helper {
    Helper::new(HelperOptions::default())
}

/// These tests measure the framing of full transfers, so the content-hash
/// cache (which would turn a read-after-write into a hash) is off; it has
/// its own tests in `tests/content_cache.rs`.
async fn connect(c: impl Connector + 'static, wire: WireCompression) -> RemoteClient {
    RemoteClient::connect(
        Arc::new(c),
        ClientOptions::new("box")
            .with_compression(wire)
            .with_content_cache(0),
    )
    .await
    .expect("connect")
}

/// Every byte is newline-delimited JSON (protocol 1).
fn assert_json_lines(bytes: &[u8], what: &str) {
    let text = std::str::from_utf8(bytes).unwrap_or_else(|_| panic!("{what}: not UTF-8"));
    for line in text.lines() {
        serde_json::from_str::<Value>(line)
            .unwrap_or_else(|e| panic!("{what}: not a JSON line ({e}): {line:.80}"));
    }
}

fn count(hay: &[u8], needle: &[u8]) -> usize {
    hay.windows(needle.len()).filter(|w| *w == needle).count()
}

#[tokio::test(flavor = "multi_thread")]
async fn handshake_negotiates_protocol_2_and_zstd() {
    let client = connect(Tapped::new(new_helper()), WireCompression::default()).await;
    let h = client.hello().unwrap();
    assert_eq!(h.protocol, PROTOCOL_VERSION);
    assert_eq!(PROTOCOL_VERSION, 2);
    assert_eq!(h.compression.as_deref(), Some(ZSTD));

    // A client that does not want compression gets binary frames only.
    let client = connect(Tapped::new(new_helper()), WireCompression::off()).await;
    let h = client.hello().unwrap();
    assert_eq!(h.protocol, 2);
    assert_eq!(h.compression, None);

    // A helper that refuses compression.
    let helper = Helper::new(HelperOptions {
        compression: false,
        ..Default::default()
    });
    let client = connect(Tapped::new(helper), WireCompression::default()).await;
    assert_eq!(client.hello().unwrap().compression, None);
}

#[tokio::test(flavor = "multi_thread")]
async fn old_helper_keeps_working_uncompressed() {
    let tapped = Tapped::new(old_helper());
    let (up, down) = (tapped.up.clone(), tapped.down.clone());
    let client = connect(tapped, WireCompression::default()).await;
    let h = client.hello().unwrap();
    assert_eq!(h.protocol, 1);
    assert_eq!(h.compression, None);

    let dir = tempfile::tempdir().unwrap();
    let ws = RemoteWorkspace::new(client.clone(), dir.path().to_string_lossy());
    let big = sample_text(64 * 1024);
    ws.write(Path::new("big.txt"), &big).await.unwrap();
    assert_eq!(ws.read(Path::new("big.txt")).await.unwrap(), big);
    let ex = RemoteExec::new(client.clone(), RemoteEnv::default());
    let req = ExecRequest::new("cat", vec!["big.txt".into()], dir.path());
    let mut handle = ex.spawn(req).await.unwrap();
    let mut out = Vec::new();
    while let Some(c) = handle.next_output().await {
        out.extend(c.data);
    }
    assert_eq!(out, big);

    // Both directions stayed protocol 1: JSON lines, payloads as base64.
    let (up, down) = (up.lock().unwrap().clone(), down.lock().unwrap().clone());
    assert_json_lines(&up, "client to helper");
    assert_json_lines(&down, "helper to client");
    let encoded = b64::encode(&big);
    assert_eq!(count(&up, encoded.as_bytes()), 1, "write sent as base64");
    assert_eq!(
        count(&down, encoded.as_bytes()),
        1,
        "read answered as base64"
    );
}

/// A helper exactly as protocol 1 shipped it: it insists on protocol 1,
/// knows nothing of the new hello fields, and speaks only JSON lines.
async fn strict_v1_helper(stream: tokio::io::DuplexStream, root: std::path::PathBuf) {
    let (r, mut w) = tokio::io::split(stream);
    let mut r = BufReader::new(r);
    while let Ok(Some(line)) = read_frame(&mut r).await {
        assert!(line.starts_with('{'), "v1 helper got a non-JSON frame");
        let Message::Request { id, method, params } = Message::parse(&line).unwrap() else {
            continue;
        };
        let result = match method.as_str() {
            "hello" => {
                #[derive(serde::Deserialize)]
                struct V1Hello {
                    protocol: u32,
                }
                let p: V1Hello = serde_json::from_value(params).unwrap();
                if p.protocol != 1 {
                    Err(harness_remote::protocol::RpcError::new(-32020, "mismatch"))
                } else {
                    Ok(
                        json!({"protocol": 1, "version": HELPER_VERSION, "arch": "x86_64",
                        "os": "linux", "session": "s", "pid": 1, "jobs": 0, "home": null}),
                    )
                }
            }
            "fs.read" => {
                let path = params["path"].as_str().unwrap().to_owned();
                assert!(path.starts_with(root.to_str().unwrap()));
                let data = std::fs::read(&path).unwrap();
                Ok(json!({ "data": b64::encode(&data) }))
            }
            "fs.write" => {
                use base64::Engine;
                let data = base64::engine::general_purpose::STANDARD
                    .decode(params["data"].as_str().expect("v1 write carries base64"))
                    .unwrap();
                std::fs::write(params["path"].as_str().unwrap(), data).unwrap();
                Ok(json!({}))
            }
            _ => Ok(json!({})),
        };
        let resp = Message::Response { id, result };
        if write_frame(&mut w, &resp.to_line()).await.is_err() {
            break;
        }
    }
}

struct StrictV1 {
    root: std::path::PathBuf,
}

#[async_trait]
impl Connector for StrictV1 {
    async fn connect(&self, _session: &str) -> std::io::Result<Connection> {
        let (client_side, helper_side) = tokio::io::duplex(1 << 20);
        tokio::spawn(strict_v1_helper(helper_side, self.root.clone()));
        let (cr, cw) = tokio::io::split(client_side);
        Ok(Connection {
            reader: Box::new(cr),
            writer: Box::new(cw),
            child: None,
            stderr: None,
        })
    }
    fn host(&self) -> String {
        "v1".into()
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn shipped_protocol_1_helper_accepts_the_new_handshake() {
    let dir = tempfile::tempdir().unwrap();
    let c = StrictV1 {
        root: dir.path().to_path_buf(),
    };
    let client = connect(c, WireCompression::default()).await;
    assert_eq!(client.hello().unwrap().protocol, 1);
    let ws = RemoteWorkspace::new(client, dir.path().to_string_lossy());
    let big = sample_text(20_000);
    ws.write(Path::new("f.txt"), &big).await.unwrap();
    assert_eq!(std::fs::read(dir.path().join("f.txt")).unwrap(), big);
    assert_eq!(ws.read(Path::new("f.txt")).await.unwrap(), big);
}

#[tokio::test(flavor = "multi_thread")]
async fn old_client_works_with_a_new_helper() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("f.txt");
    let big = sample_text(50_000);
    std::fs::write(&file, &big).unwrap();
    let (client_side, helper_side) = tokio::io::duplex(1 << 20);
    let (hr, hw) = tokio::io::split(helper_side);
    tokio::spawn(new_helper().serve(hr, hw));
    let (cr, mut cw) = tokio::io::split(client_side);
    let mut cr = BufReader::new(cr);
    // The protocol 1 client's hello and read, verbatim.
    let hello = json!({"jsonrpc": "2.0", "id": 1, "method": "hello",
        "params": {"protocol": 1, "version": HELPER_VERSION}});
    write_frame(&mut cw, &hello.to_string()).await.unwrap();
    let line = read_frame(&mut cr).await.unwrap().unwrap();
    let v: Value = serde_json::from_str(&line).unwrap();
    assert_eq!(v["result"]["protocol"], 1);
    assert!(v["result"].get("compression").is_none());
    let read = json!({"jsonrpc": "2.0", "id": 2, "method": "fs.read",
        "params": {"path": file.to_string_lossy()}});
    write_frame(&mut cw, &read.to_string()).await.unwrap();
    let line = read_frame(&mut cr).await.unwrap().unwrap();
    let v: Value = serde_json::from_str(&line).unwrap();
    assert_eq!(v["result"]["data"], b64::encode(&big));
    cw.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn payloads_travel_as_raw_bytes_and_large_frames_are_compressed() {
    let tapped = Tapped::new(new_helper());
    let (up, down) = (tapped.up.clone(), tapped.down.clone());
    let client = connect(tapped, WireCompression::off()).await;
    let dir = tempfile::tempdir().unwrap();
    let ws = RemoteWorkspace::new(client, dir.path().to_string_lossy());
    let big = sample_text(100_000);
    ws.write(Path::new("f.txt"), &big).await.unwrap();
    assert_eq!(ws.read(Path::new("f.txt")).await.unwrap(), big);
    let (up, down) = (up.lock().unwrap().clone(), down.lock().unwrap().clone());
    // Without compression the bytes appear verbatim, never as base64.
    assert_eq!(count(&up, &big), 1);
    assert_eq!(count(&down, &big), 1);
    let encoded = b64::encode(&big[..3000]);
    assert_eq!(count(&up, encoded.as_bytes()), 0);
    assert_eq!(count(&down, encoded.as_bytes()), 0);
    assert!(up.len() < big.len() + 1024, "{} bytes up", up.len());

    // With zstd the same transfer shrinks several times.
    let tapped = Tapped::new(new_helper());
    let (up, down) = (tapped.up.clone(), tapped.down.clone());
    let client = connect(tapped, WireCompression::default()).await;
    let ws = RemoteWorkspace::new(client, dir.path().to_string_lossy());
    ws.write(Path::new("g.txt"), &big).await.unwrap();
    assert_eq!(ws.read(Path::new("g.txt")).await.unwrap(), big);
    let (up, down) = (up.lock().unwrap().len(), down.lock().unwrap().len());
    assert!(up < big.len() / 3, "{up} bytes up for {} bytes", big.len());
    assert!(down < big.len() / 3, "{down} bytes down");
}

#[tokio::test(flavor = "multi_thread")]
async fn no_helper_compression_over_a_compressed_ssh_channel() {
    let mut tapped = Tapped::new(new_helper());
    tapped.compressed = true;
    let down = tapped.down.clone();
    let client = connect(tapped, WireCompression::default()).await;
    let h = client.hello().unwrap();
    assert_eq!(h.protocol, 2, "binary payloads still apply");
    assert_eq!(h.compression, None, "zstd not negotiated");
    let dir = tempfile::tempdir().unwrap();
    let big = sample_text(100_000);
    std::fs::write(dir.path().join("f.txt"), &big).unwrap();
    let ws = RemoteWorkspace::new(client, dir.path().to_string_lossy());
    assert_eq!(ws.read(Path::new("f.txt")).await.unwrap(), big);
    let down = down.lock().unwrap().clone();
    assert_eq!(count(&down, &big), 1, "sent uncompressed");
    assert_eq!(count(&down, b" zstd\n"), 0);

    // The ssh connector reports the target's compression.
    let target = |compression| SshTarget {
        name: "h".into(),
        host: "h".into(),
        user: None,
        port: 22,
        identity_file: None,
        options: SshOptions::new("/tmp/x"),
        compression,
    };
    assert!(SshConnector::new(target(true), "~/.harness/bin/harness").transport_compressed());
    assert!(!SshConnector::new(target(false), "~/.harness/bin/harness").transport_compressed());
}

#[tokio::test(flavor = "multi_thread")]
async fn helper_rejects_frames_that_decompress_past_the_limit() {
    let (client_side, helper_side) = tokio::io::duplex(1 << 16);
    let (hr, hw) = tokio::io::split(helper_side);
    let helper = new_helper();
    let served = tokio::spawn(helper.serve(hr, hw));
    let (cr, mut cw) = tokio::io::split(client_side);
    // Declares a decoded body one byte over the limit; the body itself is
    // a tiny zstd frame. The helper must refuse it without inflating.
    let bomb = zstd::bulk::compress(&vec![0u8; 1 << 20], 1).unwrap();
    let header = format!("~2 {} {} zstd\n", MAX_FRAME - 1, bomb.len());
    cw.write_all(header.as_bytes()).await.unwrap();
    cw.write_all(&bomb).await.unwrap();
    cw.flush().await.unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(5), served)
        .await
        .expect("helper closes the connection")
        .unwrap();
    drop(cr);
}

#[tokio::test(flavor = "multi_thread")]
async fn client_drops_a_connection_sending_a_decompression_bomb() {
    struct Bomber;
    #[async_trait]
    impl Connector for Bomber {
        async fn connect(&self, _s: &str) -> std::io::Result<Connection> {
            let (client_side, helper_side) = tokio::io::duplex(1 << 16);
            let (hr, mut hw) = tokio::io::split(helper_side);
            tokio::spawn(async move {
                let mut r = BufReader::new(hr);
                let line = read_frame(&mut r).await.unwrap().unwrap();
                let Message::Request { id, .. } = Message::parse(&line).unwrap() else {
                    panic!()
                };
                let hello = json!({"protocol": 2, "version": HELPER_VERSION, "arch": "x",
                    "os": "y", "session": "s", "pid": 1, "jobs": 0, "compression": "zstd"});
                let resp = Message::Response {
                    id,
                    result: Ok(hello),
                };
                write_frame(&mut hw, &resp.to_line()).await.unwrap();
                // Answer the next request with 96 MiB of zeros in a few KiB.
                let _ = read_frame(&mut r).await;
                let zeros = vec![0u8; 96 << 20];
                let z = zstd::bulk::compress(&zeros, 1).unwrap();
                let h = format!("~2 {} {} zstd\n", zeros.len() - 2, z.len());
                hw.write_all(h.as_bytes()).await.unwrap();
                hw.write_all(&z).await.unwrap();
                hw.flush().await.unwrap();
                while let Ok(Some(_)) = read_frame(&mut r).await {}
            });
            let (cr, cw) = tokio::io::split(client_side);
            Ok(Connection {
                reader: Box::new(cr),
                writer: Box::new(cw),
                child: None,
                stderr: None,
            })
        }
        fn host(&self) -> String {
            "bomber".into()
        }
    }
    let mut opts = ClientOptions::new("bomber");
    opts.reconnect_attempts = 1;
    opts.reconnect_delay = std::time::Duration::from_millis(1);
    opts.keepalive = None;
    let client = RemoteClient::connect(Arc::new(Bomber), opts).await.unwrap();
    let mut events = client.subscribe();
    let ws = RemoteWorkspace::new(client.clone(), "/tmp");
    let err = ws.read(Path::new("x")).await.unwrap_err();
    assert!(err.to_string().contains("remote host"), "{err}");
    let mut reasons = Vec::new();
    while let Ok(e) = events.try_recv() {
        if let harness_remote::client::ConnectionState::Disconnected { reason } = e.state {
            reasons.push(reason);
        }
    }
    assert!(
        reasons.iter().any(|r| r.contains("too large")),
        "{reasons:?}"
    );
}

/// Forces every frame through zstd (threshold 1 byte).
fn always_compress() -> WireCompression {
    WireCompression {
        min_size: 1,
        ..WireCompression::default()
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn conformance_with_compression_on_off_and_against_protocol_1() {
    let cases = [
        ("zstd on every frame", new_helper(), always_compress()),
        ("zstd default", new_helper(), WireCompression::default()),
        ("compression off", new_helper(), WireCompression::off()),
        (
            "protocol 1 helper",
            old_helper(),
            WireCompression::default(),
        ),
    ];
    for (name, helper, wire) in cases {
        let tapped = Tapped::new(helper);
        let down = tapped.down.clone();
        let client = connect(tapped, wire).await;
        let dir = tempfile::tempdir().unwrap();
        let ws = RemoteWorkspace::new(client.clone(), dir.path().to_string_lossy());
        workspace_suite(&ws).await;
        let dir = tempfile::tempdir().unwrap();
        let ex = RemoteExec::new(client.clone(), RemoteEnv::default());
        exec_suite(&ex, dir.path()).await;
        let zstd_frames = count(&down.lock().unwrap(), b" zstd\n");
        match name {
            "zstd on every frame" => assert!(zstd_frames > 10, "{name}: {zstd_frames}"),
            "compression off" | "protocol 1 helper" => assert_eq!(zstd_frames, 0, "{name}"),
            _ => {}
        }
    }
}

fn helper_bin() -> String {
    env!("CARGO_BIN_EXE_harness-remote-agent").to_owned()
}

async fn over_process(sock: &Path, extra: &[&str], wire: WireCompression) -> RemoteClient {
    let mut args: Vec<String> = vec![
        "remote-agent".into(),
        "--stdio".into(),
        "--grace".into(),
        "2".into(),
        "--socket-dir".into(),
        sock.display().to_string(),
    ];
    args.extend(extra.iter().map(|s| s.to_string()));
    connect(CommandConnector::new(helper_bin(), args), wire).await
}

#[tokio::test(flavor = "multi_thread")]
async fn helper_process_conformance_compressed_and_as_protocol_1() {
    for (extra, wire, protocol, zstd) in [
        (&[][..], always_compress(), 2, true),
        (
            &["--no-compression"][..],
            WireCompression::default(),
            2,
            false,
        ),
        (
            &["--max-protocol", "1"][..],
            WireCompression::default(),
            1,
            false,
        ),
    ] {
        let sock = tempfile::Builder::new()
            .prefix("hw")
            .tempdir_in("/tmp")
            .unwrap();
        let client = over_process(sock.path(), extra, wire).await;
        let h = client.hello().unwrap();
        assert_eq!(h.protocol, protocol, "{extra:?}");
        assert_eq!(h.compression.is_some(), zstd, "{extra:?}");
        let dir = tempfile::tempdir().unwrap();
        workspace_suite(&RemoteWorkspace::new(
            client.clone(),
            dir.path().to_string_lossy(),
        ))
        .await;
        let dir = tempfile::tempdir().unwrap();
        exec_suite(&RemoteExec::new(client, RemoteEnv::default()), dir.path()).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn benchmark_reports_bytes_and_latency_per_link() {
    let payload = 48 * 1024;
    let mut rows = Vec::new();
    for mbps in [10.0, 100.0, 1000.0] {
        for mode in WireMode::ALL {
            let dir = tempfile::tempdir().unwrap();
            rows.extend(
                bench(LinkSpec::mbps(mbps), mode, payload, dir.path())
                    .await
                    .unwrap(),
            );
        }
    }
    assert_eq!(rows.len(), 3 * 3 * 3);
    for op in ["read", "write", "exec"] {
        for mbps in [10.0, 100.0, 1000.0] {
            let bytes = |mode| {
                rows.iter()
                    .find(|r| r.op == op && r.mode == mode && r.mbps == mbps)
                    .unwrap()
                    .wire_bytes()
            };
            let (v1, raw, z) = (
                bytes(WireMode::Base64),
                bytes(WireMode::Binary),
                bytes(WireMode::Zstd),
            );
            assert!(
                v1 as f64 > payload as f64 * 1.3,
                "{op}: base64 inflates ({v1})"
            );
            assert!(
                raw < v1 && (raw as f64) < payload as f64 * 1.05,
                "{op}: {raw}"
            );
            assert!(z * 2 < raw, "{op} @ {mbps}: zstd {z} vs raw {raw}");
        }
    }
    // The slow link is visibly throttled: 48 KiB base64 at 10 Mbit/s > 50 ms.
    let slow = rows
        .iter()
        .find(|r| r.op == "read" && r.mode == WireMode::Base64 && r.mbps == 10.0)
        .unwrap();
    assert!(slow.latency.as_millis() >= 50, "{:?}", slow.latency);
    let table = harness_remote::simlink::format_table(&rows);
    assert!(
        table.contains("1000 Mb/s") && table.contains("v2 zstd"),
        "{table}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn simlink_streams_exec_output_over_zstd() {
    let conn = SimLinkConnector::new(new_helper(), LinkSpec::mbps(100.0));
    let stats = conn.stats();
    let client = connect(conn, WireCompression::default()).await;
    let dir = tempfile::tempdir().unwrap();
    let ex = RemoteExec::new(client, RemoteEnv::default());
    let req = ExecRequest::new(
        "sh",
        vec![
            "-c".into(),
            "for i in $(seq 1 3000); do echo line $i of output; done".into(),
        ],
        dir.path(),
    );
    let mut h = ex.spawn(req).await.unwrap();
    let mut out = Vec::new();
    while let Some(c) = h.next_output().await {
        out.extend(c.data);
    }
    assert!(h.wait().await.unwrap().success());
    let text = String::from_utf8(out).unwrap();
    assert_eq!(text.lines().count(), 3000);
    assert!(text.ends_with("line 3000 of output\n"));
    assert!(stats.down() > 0 && stats.up() > 0);
}
