//! Helper protocol wire benchmark: bytes on the wire and latency of a
//! remote read, write and streaming exec over simulated 10, 100 and
//! 1000 Mbit/s links (1 ms one-way latency), for protocol 1 (base64),
//! protocol 2 without compression and protocol 2 with zstd.
//!
//! ```sh
//! cargo bench -p harness-remote --bench wire            # 1 MiB payloads
//! HARNESS_WIRE_BENCH_BYTES=4194304 cargo bench -p harness-remote --bench wire
//! ```

use harness_remote::simlink::{LinkSpec, WireMode, bench, format_table};

fn main() {
    // `cargo test --benches` runs this with `--bench` absent; keep it quick.
    let quick = !std::env::args().any(|a| a == "--bench");
    let payload: usize = std::env::var("HARNESS_WIRE_BENCH_BYTES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(if quick { 64 * 1024 } else { 1 << 20 });
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");
    let mut rows = Vec::new();
    for mbps in [10.0, 100.0, 1000.0] {
        for mode in WireMode::ALL {
            let dir = tempfile::tempdir().expect("temp dir");
            let r = rt
                .block_on(bench(LinkSpec::mbps(mbps), mode, payload, dir.path()))
                .unwrap_or_else(|e| panic!("{mbps} Mbit/s {}: {e}", mode.label()));
            rows.extend(r);
        }
    }
    rows.sort_by(|a, b| {
        a.mbps
            .total_cmp(&b.mbps)
            .then(a.op.cmp(b.op))
            .then((a.mode as u8).cmp(&(b.mode as u8)))
    });
    println!("payload: {payload} bytes of source-like text");
    print!("{}", format_table(&rows));
}
