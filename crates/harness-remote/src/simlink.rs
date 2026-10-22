//! A simulated network link to an in-process helper, for protocol tests and
//! the wire benchmark (`cargo bench -p harness-remote --bench wire`).
//!
//! [`SimLinkConnector`] serves a [`Helper`] through a pipe that delivers at
//! most [`LinkSpec::mbps`] and delays every byte by [`LinkSpec::latency`]
//! in each direction, and counts the bytes that cross it ([`LinkStats`]).
//! [`bench()`] measures bytes on the wire and latency of a remote read, write
//! and streaming exec over such a link.

use std::io;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use harness_tools::exec::{Exec, ExecRequest};
use harness_tools::workspace::Workspace;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::mpsc;

use crate::client::{
    ClientOptions, Connection, Connector, InProcessConnector, RemoteClient, WireCompression,
};
use crate::helper::{Helper, HelperOptions};
use crate::workspace::{RemoteEnv, RemoteExec, RemoteWorkspace};

/// Bandwidth and one-way latency of a simulated link.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LinkSpec {
    /// Throughput in Mbit/s, per direction.
    pub mbps: f64,
    /// Delay added to every byte, per direction.
    pub latency: Duration,
}

impl LinkSpec {
    /// A link of `mbps` with 1 ms one-way latency.
    pub fn mbps(mbps: f64) -> Self {
        Self {
            mbps,
            latency: Duration::from_millis(1),
        }
    }
}

/// Bytes that crossed a simulated link.
#[derive(Debug, Default)]
pub struct LinkStats {
    up: AtomicU64,
    down: AtomicU64,
}

impl LinkStats {
    /// Bytes from the client to the helper.
    pub fn up(&self) -> u64 {
        self.up.load(Ordering::SeqCst)
    }

    /// Bytes from the helper to the client.
    pub fn down(&self) -> u64 {
        self.down.load(Ordering::SeqCst)
    }
}

/// Serves a helper over a throttled, byte-counting in-memory link.
#[derive(Debug, Clone)]
pub struct SimLinkConnector {
    inner: InProcessConnector,
    spec: LinkSpec,
    stats: Arc<LinkStats>,
    transport_compressed: bool,
}

impl SimLinkConnector {
    /// `helper` behind a link of `spec`.
    pub fn new(helper: Helper, spec: LinkSpec) -> Self {
        Self {
            inner: InProcessConnector::new(helper),
            spec,
            stats: Arc::default(),
            transport_compressed: false,
        }
    }

    /// Report the transport as compressing (like SSH `Compression=yes`).
    pub fn with_transport_compressed(mut self, on: bool) -> Self {
        self.transport_compressed = on;
        self
    }

    /// The byte counters.
    pub fn stats(&self) -> Arc<LinkStats> {
        self.stats.clone()
    }
}

#[async_trait]
impl Connector for SimLinkConnector {
    async fn connect(&self, session: &str) -> io::Result<Connection> {
        let c = self.inner.connect(session).await?;
        let (client_w, pump_r) = tokio::io::duplex(1 << 16);
        let (pump_w, client_r) = tokio::io::duplex(1 << 16);
        let up = self.stats.clone();
        let down = self.stats.clone();
        tokio::spawn(pump(pump_r, c.writer, self.spec, move |n| {
            up.up.fetch_add(n, Ordering::SeqCst);
        }));
        tokio::spawn(pump(c.reader, pump_w, self.spec, move |n| {
            down.down.fetch_add(n, Ordering::SeqCst);
        }));
        Ok(Connection {
            reader: Box::new(client_r),
            writer: Box::new(client_w),
            child: None,
            stderr: None,
        })
    }

    fn host(&self) -> String {
        "simlink".into()
    }

    fn transport_compressed(&self) -> bool {
        self.transport_compressed
    }
}

/// Copy `from` to `to`, delivering at most `spec.mbps` and `spec.latency`
/// late. Sleeps shorter than a millisecond are skipped (timer granularity);
/// delivery times accumulate, so the average rate stays exact.
async fn pump<R, W>(mut from: R, mut to: W, spec: LinkSpec, count: impl Fn(u64) + Send + 'static)
where
    R: AsyncRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin + Send + 'static,
{
    let (tx, mut rx) = mpsc::unbounded_channel::<(Instant, Vec<u8>)>();
    let bytes_per_sec = spec.mbps * 1e6 / 8.0;
    let reader = tokio::spawn(async move {
        let mut buf = vec![0u8; 16 * 1024];
        let mut free = Instant::now();
        loop {
            let n = match from.read(&mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(n) => n,
            };
            count(n as u64);
            free = free.max(Instant::now()) + Duration::from_secs_f64(n as f64 / bytes_per_sec);
            if tx.send((free + spec.latency, buf[..n].to_vec())).is_err() {
                break;
            }
        }
    });
    while let Some((at, chunk)) = rx.recv().await {
        if at > Instant::now() + Duration::from_millis(1) {
            tokio::time::sleep_until(at.into()).await;
        }
        if to.write_all(&chunk).await.is_err() || to.flush().await.is_err() {
            break;
        }
    }
    reader.abort();
    let _ = to.shutdown().await;
}

/// Helper protocol variant measured by [`bench()`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WireMode {
    /// Protocol 1: JSON lines with base64 payloads (an old helper).
    Base64,
    /// Protocol 2 with binary payloads, compression off.
    Binary,
    /// Protocol 2 with binary payloads and zstd (the default).
    Zstd,
}

impl WireMode {
    /// All modes, oldest first.
    pub const ALL: [WireMode; 3] = [WireMode::Base64, WireMode::Binary, WireMode::Zstd];

    /// Short label.
    pub fn label(self) -> &'static str {
        match self {
            WireMode::Base64 => "v1 base64",
            WireMode::Binary => "v2 binary",
            WireMode::Zstd => "v2 zstd",
        }
    }
}

/// One measured operation.
#[derive(Debug, Clone, PartialEq)]
pub struct BenchRow {
    /// Link throughput.
    pub mbps: f64,
    /// Protocol variant.
    pub mode: WireMode,
    /// `read`, `write` or `exec`.
    pub op: &'static str,
    /// Payload size.
    pub payload: usize,
    /// Bytes sent by the client.
    pub bytes_up: u64,
    /// Bytes sent by the helper.
    pub bytes_down: u64,
    /// Wall time of the operation.
    pub latency: Duration,
}

impl BenchRow {
    /// Bytes in both directions.
    pub fn wire_bytes(&self) -> u64 {
        self.bytes_up + self.bytes_down
    }
}

/// Deterministic source-code-like text of `n` bytes (compresses like real
/// code, roughly 4 to 6 times with zstd level 1).
pub fn sample_text(n: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(n + 128);
    let mut x: u64 = 0x2545_F491_4F6C_DD1D;
    let words = [
        "value", "index", "buffer", "result", "config", "client", "frame", "length", "offset",
        "count", "handle", "state",
    ];
    let mut i = 0usize;
    while out.len() < n {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        let a = words[(x % 12) as usize];
        let b = words[((x >> 8) % 12) as usize];
        let line = match i % 4 {
            0 => format!("    let {a}_{} = {b}.get({})?;\n", x % 97, (x >> 16) % 1000),
            1 => format!(
                "    if {a}.len() > {} {{ return Err(Error::{b}); }}\n",
                x % 4096
            ),
            2 => format!(
                "    // update the {a} before the {b} is flushed ({})\n",
                x % 31
            ),
            _ => format!("fn {a}_{b}_{}(&mut self) -> Result<usize> {{\n", x % 53),
        };
        out.extend_from_slice(line.as_bytes());
        i += 1;
    }
    out.truncate(n);
    out
}

/// Connect over a simulated `spec` link in `mode` and measure a read and a
/// write of `payload` bytes and a streaming exec (`cat`) printing them.
/// `dir` is a scratch directory (the in-process helper works on the local
/// filesystem).
///
/// The content-hash cache is off, so every row is a full transfer (the
/// write sends the bytes the read just fetched).
pub async fn bench(
    spec: LinkSpec,
    mode: WireMode,
    payload: usize,
    dir: &Path,
) -> io::Result<Vec<BenchRow>> {
    let helper = Helper::new(HelperOptions {
        max_protocol: if mode == WireMode::Base64 { 1 } else { 2 },
        ..Default::default()
    });
    let conn = SimLinkConnector::new(helper, spec);
    let stats = conn.stats();
    let mut opts = ClientOptions::new("simlink");
    opts.keepalive = None;
    opts.content_cache = 0;
    if mode != WireMode::Zstd {
        opts.compression = WireCompression::off();
    }
    let client = RemoteClient::connect(Arc::new(conn), opts)
        .await
        .map_err(io::Error::other)?;
    let ws = RemoteWorkspace::new(client.clone(), dir.to_string_lossy());
    let ex = RemoteExec::new(client.clone(), RemoteEnv::default());
    let data = sample_text(payload);
    let source = dir.join("source.txt");
    std::fs::write(&source, &data)?;
    let mut rows = Vec::new();
    let mut measure = |op, t0: Instant, before: (u64, u64)| {
        rows.push(BenchRow {
            mbps: spec.mbps,
            mode,
            op,
            payload,
            bytes_up: stats.up() - before.0,
            bytes_down: stats.down() - before.1,
            latency: t0.elapsed(),
        });
    };

    let (before, t0) = ((stats.up(), stats.down()), Instant::now());
    let got = ws
        .read(Path::new("source.txt"))
        .await
        .map_err(io::Error::other)?;
    measure("read", t0, before);
    if got != data {
        return Err(io::Error::other("read returned different bytes"));
    }

    let (before, t0) = ((stats.up(), stats.down()), Instant::now());
    ws.write(Path::new("copy.txt"), &data)
        .await
        .map_err(io::Error::other)?;
    measure("write", t0, before);
    if std::fs::read(dir.join("copy.txt"))? != data {
        return Err(io::Error::other("write stored different bytes"));
    }

    let (before, t0) = ((stats.up(), stats.down()), Instant::now());
    let req = ExecRequest::new("cat", vec![source.to_string_lossy().into_owned()], dir);
    let mut h = ex.spawn(req).await.map_err(io::Error::other)?;
    let mut out = 0usize;
    while let Some(c) = h.next_output().await {
        out += c.data.len();
    }
    h.wait().await.map_err(io::Error::other)?;
    measure("exec", t0, before);
    if out != payload {
        return Err(io::Error::other(format!(
            "exec streamed {out} bytes, expected {payload}"
        )));
    }
    client.close();
    Ok(rows)
}

/// A plain-text table of `rows`.
pub fn format_table(rows: &[BenchRow]) -> String {
    let mut s = format!(
        "{:>9}  {:<5}  {:<9}  {:>12}  {:>11}  {:>10}\n",
        "link", "op", "protocol", "wire bytes", "per byte", "latency"
    );
    for r in rows {
        s.push_str(&format!(
            "{:>4} Mb/s  {:<5}  {:<9}  {:>12}  {:>11.3}  {:>7.1} ms\n",
            r.mbps,
            r.op,
            r.mode.label(),
            r.wire_bytes(),
            r.wire_bytes() as f64 / r.payload.max(1) as f64,
            r.latency.as_secs_f64() * 1e3,
        ));
    }
    s
}
