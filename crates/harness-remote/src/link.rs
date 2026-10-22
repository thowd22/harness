//! Link speed and compression: per-host SSH channel compression and the
//! rsync compression algorithm.
//!
//! - [`measure_link`] times a burst of incompressible bytes through a
//!   [`RemoteShell`] (the remote probe's throughput measurement).
//! - [`LinkStore`] keeps the last measurement per host, optionally in a
//!   JSON file under the data directory so later runs can use it.
//! - [`decide`] turns `[remote.<name>] compression` (`auto`, `yes`, `no`)
//!   and the measurement into a [`CompressionDecision`]; the SSH target
//!   then adds `Compression=yes` or `Compression=no` and uses a separate
//!   ControlMaster socket for compressed connections (see
//!   [`crate::ssh::SshTarget::with_compression`]).
//! - [`RsyncCompression`] picks `--compress-choice=zstd` when both rsyncs
//!   list zstd in their `--version` output, and rsync's default otherwise.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tokio::time::Instant;

use harness_config::ini::Remote;
use harness_config::ini::config::SshCompression;
use serde::{Deserialize, Serialize};

use crate::shell::RemoteShell;

/// Bytes sent by one throughput measurement.
pub const DEFAULT_SAMPLE_BYTES: usize = 256 * 1024;
/// How long a throughput measurement may take; a link that cannot move
/// [`DEFAULT_SAMPLE_BYTES`] in this time is recorded at the upper bound
/// (sample size / timeout), which is far below any sensible threshold.
pub const MEASURE_TIMEOUT: Duration = Duration::from_secs(10);
/// How long a measurement is trusted by `compression = auto` before a new
/// connection measures again.
pub const LINK_MAX_AGE: Duration = Duration::from_secs(24 * 3600);
/// File name of the persisted measurements under the data directory.
pub const LINK_FILE: &str = "remote-links.json";

/// One link throughput measurement.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct LinkSample {
    /// Measured throughput in bytes per second.
    pub bytes_per_sec: f64,
    /// When it was measured (seconds since the Unix epoch).
    pub measured_at: u64,
    /// True when the measurement timed out and `bytes_per_sec` is an
    /// upper bound.
    #[serde(default)]
    pub timed_out: bool,
}

impl LinkSample {
    /// A sample measured now.
    pub fn now(bytes_per_sec: f64) -> Self {
        Self {
            bytes_per_sec,
            measured_at: unix_now(),
            timed_out: false,
        }
    }

    /// Throughput in Mbit/s.
    pub fn mbps(&self) -> f64 {
        self.bytes_per_sec * 8.0 / 1_000_000.0
    }

    /// True when measured within `max_age` of `now` (Unix seconds).
    pub fn is_fresh(&self, max_age: Duration, now: u64) -> bool {
        now.saturating_sub(self.measured_at) <= max_age.as_secs()
    }

    /// `12.3 Mbit/s` (`< 0.2 Mbit/s` for a timed-out measurement).
    pub fn describe(&self) -> String {
        if self.timed_out {
            format!("< {}", format_mbps(self.mbps()))
        } else {
            format_mbps(self.mbps())
        }
    }
}

fn format_mbps(v: f64) -> String {
    if v >= 100.0 {
        format!("{v:.0} Mbit/s")
    } else {
        format!("{v:.1} Mbit/s")
    }
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// The script that writes `bytes` incompressible bytes to stdout.
pub fn sample_script(bytes: usize) -> String {
    format!("head -c {bytes} /dev/urandom")
}

/// Throughput from a timed transfer: `bytes` took `transfer`, of which
/// `overhead` (an empty round trip) is subtracted. Never divides by less
/// than a millisecond.
pub fn throughput(bytes: usize, transfer: Duration, overhead: Duration) -> f64 {
    let secs = transfer
        .saturating_sub(overhead)
        .max(Duration::from_millis(1))
        .as_secs_f64();
    bytes as f64 / secs
}

/// Measure the link to `shell`'s host. Three steps:
///
/// 1. An untimed empty command brings the connection up. Over SSH the
///    first command of a cold connection becomes the ControlMaster and pays
///    the TCP connect, key exchange and authentication; later commands reuse
///    the mux. Timing this one as overhead would subtract the handshake from
///    the transfer and inflate the throughput.
/// 2. A second empty command on the warm connection is timed as the
///    per-command overhead (channel setup and round trip).
/// 3. [`sample_script`] with `bytes` random bytes is timed, and the
///    difference is reported as throughput. Random bytes do not compress,
///    so the result is the raw link speed whether or not the channel is
///    compressed.
///
/// Each step may take up to `timeout`. A failure or timeout in steps 1 and
/// 2 is an error (nothing was measured: the host is unreachable or slow to
/// authenticate). Only a transfer that does not finish within `timeout`
/// returns the upper bound `bytes / timeout` marked
/// [`LinkSample::timed_out`].
pub async fn measure_link(
    shell: &dyn RemoteShell,
    bytes: usize,
    timeout: Duration,
) -> Result<LinkSample, String> {
    let empty = async |what: &str| -> Result<(), String> {
        let out = tokio::time::timeout(timeout, shell.run("true", b""))
            .await
            .map_err(|_| format!("link test: {what} timed out after {timeout:?}"))?
            .map_err(|e| format!("link test: {what} failed: {e}"))?;
        if !out.success() {
            return Err(format!("link test: {what} failed: {}", out.stderr.trim()));
        }
        Ok(())
    };
    empty("connection setup").await?;
    let t0 = Instant::now();
    empty("round trip").await?;
    let overhead = t0.elapsed();
    let t1 = Instant::now();
    let out = match tokio::time::timeout(timeout, shell.run(&sample_script(bytes), b"")).await {
        Ok(r) => r.map_err(|e| e.to_string())?,
        Err(_) => {
            return Ok(LinkSample {
                bytes_per_sec: bytes as f64 / timeout.as_secs_f64().max(0.001),
                measured_at: unix_now(),
                timed_out: true,
            });
        }
    };
    let transfer = t1.elapsed();
    if !out.success() || out.stdout.len() != bytes {
        return Err(format!(
            "link test got {} of {bytes} bytes: {}",
            out.stdout.len(),
            out.stderr.trim()
        ));
    }
    Ok(LinkSample::now(throughput(bytes, transfer, overhead)))
}

/// The last link measurement per host, in memory and optionally persisted
/// as JSON (`<data_dir>/remote-links.json`).
#[derive(Debug, Default)]
pub struct LinkStore {
    path: Option<PathBuf>,
    samples: Mutex<BTreeMap<String, LinkSample>>,
}

impl LinkStore {
    /// A store that lives only in memory.
    pub fn in_memory() -> Self {
        Self::default()
    }

    /// A store backed by `path`, loading what is there (a missing or
    /// unreadable file starts empty).
    pub fn open(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        let samples = std::fs::read(&path)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        Self {
            path: Some(path),
            samples: Mutex::new(samples),
        }
    }

    /// The store at [`LINK_FILE`] under `data_root`.
    pub fn in_data_dir(data_root: &Path) -> Self {
        Self::open(data_root.join(LINK_FILE))
    }

    /// The backing file, if any.
    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// The last measurement for `host`.
    pub fn get(&self, host: &str) -> Option<LinkSample> {
        self.lock().get(host).copied()
    }

    /// The last measurement for `host` if it is younger than `max_age`.
    pub fn fresh(&self, host: &str, max_age: Duration) -> Option<LinkSample> {
        self.get(host).filter(|s| s.is_fresh(max_age, unix_now()))
    }

    /// Record a measurement for `host` (and write the file, best effort).
    pub fn put(&self, host: &str, sample: LinkSample) {
        let snapshot = {
            let mut map = self.lock();
            map.insert(host.to_owned(), sample);
            map.clone()
        };
        if let Some(path) = &self.path
            && let Err(e) = write_json(path, &snapshot)
        {
            tracing::debug!(target: "harness::remote::link", error = %e, "could not save link measurements");
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, BTreeMap<String, LinkSample>> {
        self.samples.lock().unwrap_or_else(|e| e.into_inner())
    }
}

fn write_json(path: &Path, map: &BTreeMap<String, LinkSample>) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(map)?)?;
    std::fs::rename(tmp, path)
}

/// Why compression is on or off for a host.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum CompressionReason {
    /// `compression = yes`.
    Forced,
    /// `compression = no`.
    Disabled,
    /// `compression = auto` and the link is slower than the threshold.
    AutoSlow {
        /// The measurement.
        link: LinkSample,
        /// `compression_below_mbps`.
        below_mbps: f64,
    },
    /// `compression = auto` and the link is at least as fast as the
    /// threshold.
    AutoFast {
        /// The measurement.
        link: LinkSample,
        /// `compression_below_mbps`.
        below_mbps: f64,
    },
    /// `compression = auto` with no measurement yet.
    AutoUnmeasured {
        /// `compression_below_mbps`.
        below_mbps: f64,
    },
}

/// Whether a host's SSH connection is compressed, and why.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CompressionDecision {
    /// True: `Compression=yes`.
    pub active: bool,
    /// Why.
    pub reason: CompressionReason,
}

impl CompressionDecision {
    /// `on (auto: link 3.2 Mbit/s < 20 Mbit/s)`, `off (no)`, ...
    pub fn describe(&self) -> String {
        let state = if self.active { "on" } else { "off" };
        let why = match self.reason {
            CompressionReason::Forced => "compression = yes".to_owned(),
            CompressionReason::Disabled => "compression = no".to_owned(),
            CompressionReason::AutoSlow { link, below_mbps } => format!(
                "auto: link {} < {}",
                link.describe(),
                format_mbps(below_mbps)
            ),
            CompressionReason::AutoFast { link, below_mbps } => format!(
                "auto: link {} >= {}",
                link.describe(),
                format_mbps(below_mbps)
            ),
            CompressionReason::AutoUnmeasured { .. } => "auto: link not measured yet".to_owned(),
        };
        format!("{state} ({why})")
    }

    /// `on` / `off`.
    pub fn badge(&self) -> &'static str {
        if self.active { "on" } else { "off" }
    }
}

/// The compression decision for `remote` given its last link measurement.
pub fn decide(remote: &Remote, link: Option<LinkSample>) -> CompressionDecision {
    let below_mbps = remote.compression_below_mbps;
    let (active, reason) = match remote.compression {
        SshCompression::Yes => (true, CompressionReason::Forced),
        SshCompression::No => (false, CompressionReason::Disabled),
        SshCompression::Auto => match link {
            None => (false, CompressionReason::AutoUnmeasured { below_mbps }),
            Some(link) if link.mbps() < below_mbps => {
                (true, CompressionReason::AutoSlow { link, below_mbps })
            }
            Some(link) => (false, CompressionReason::AutoFast { link, below_mbps }),
        },
    };
    CompressionDecision { active, reason }
}

/// The rsync compression algorithm for a push.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RsyncCompression {
    /// `--compress --compress-choice=zstd`: both rsyncs support zstd.
    Zstd,
    /// `--compress`: rsync's default (negotiated or zlib).
    Default,
}

impl RsyncCompression {
    /// Zstd when both `rsync --version` outputs list it, else the default.
    pub fn choose(local_version: &str, remote_version: &str) -> Self {
        if rsync_supports_zstd(local_version) && rsync_supports_zstd(remote_version) {
            Self::Zstd
        } else {
            Self::Default
        }
    }

    /// The rsync arguments.
    pub fn args(&self) -> Vec<String> {
        match self {
            Self::Zstd => vec!["--compress".into(), "--compress-choice=zstd".into()],
            Self::Default => vec!["--compress".into()],
        }
    }

    /// `zstd` / `default`.
    pub fn name(&self) -> &'static str {
        match self {
            Self::Zstd => "zstd",
            Self::Default => "default",
        }
    }
}

/// True when `rsync --version` output lists `zstd` under `Compress list:`
/// (rsync 3.2 and later built with zstd).
pub fn rsync_supports_zstd(version_output: &str) -> bool {
    let mut lines = version_output.lines();
    while let Some(line) = lines.next() {
        let t = line.trim();
        if let Some(rest) = t.strip_prefix("Compress list:") {
            // The list follows on the same line or the next one.
            let list = if rest.trim().is_empty() {
                lines.next().unwrap_or("")
            } else {
                rest
            };
            return list.split_whitespace().any(|w| w == "zstd");
        }
    }
    false
}

/// `rsync --version` on this machine (empty when rsync cannot run).
pub async fn local_rsync_version() -> String {
    tokio::process::Command::new("rsync")
        .arg("--version")
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .await
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default()
}

/// `rsync --version` on `shell`'s host (empty when it cannot run).
pub async fn remote_rsync_version(shell: &dyn RemoteShell) -> String {
    shell
        .run("rsync --version", b"")
        .await
        .map(|o| o.text())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::LocalShell;

    fn remote(compression: SshCompression) -> Remote {
        harness_config::ini::load_str(
            &format!("[remote.x]\nhost = h\ncompression = {compression}\n"),
            None,
        )
        .unwrap()
        .remotes
        .remove("x")
        .unwrap()
    }

    fn sample(mbps: f64) -> LinkSample {
        LinkSample::now(mbps * 1_000_000.0 / 8.0)
    }

    #[test]
    fn yes_and_no_ignore_the_link() {
        let y = decide(&remote(SshCompression::Yes), Some(sample(1000.0)));
        assert!(y.active);
        assert_eq!(y.describe(), "on (compression = yes)");
        let n = decide(&remote(SshCompression::No), Some(sample(0.5)));
        assert!(!n.active);
        assert_eq!(n.describe(), "off (compression = no)");
    }

    #[test]
    fn auto_compresses_only_below_the_threshold() {
        let r = remote(SshCompression::Auto);
        assert_eq!(r.compression_below_mbps, 20.0);
        let slow = decide(&r, Some(sample(3.2)));
        assert!(slow.active);
        assert_eq!(slow.describe(), "on (auto: link 3.2 Mbit/s < 20.0 Mbit/s)");
        let fast = decide(&r, Some(sample(940.0)));
        assert!(!fast.active);
        assert_eq!(
            fast.describe(),
            "off (auto: link 940 Mbit/s >= 20.0 Mbit/s)"
        );
        let at = decide(&r, Some(sample(20.0)));
        assert!(!at.active, "the threshold itself is fast enough");
        let none = decide(&r, None);
        assert!(!none.active);
        assert_eq!(none.describe(), "off (auto: link not measured yet)");
    }

    #[test]
    fn auto_threshold_is_configurable() {
        let mut r = remote(SshCompression::Auto);
        r.compression_below_mbps = 1000.0;
        assert!(decide(&r, Some(sample(940.0))).active);
        r.compression_below_mbps = 1.0;
        assert!(!decide(&r, Some(sample(3.2))).active);
    }

    #[test]
    fn throughput_subtracts_overhead_and_never_divides_by_zero() {
        let t = throughput(
            1_000_000,
            Duration::from_millis(1100),
            Duration::from_millis(100),
        );
        assert!((t - 1_000_000.0).abs() < 1.0, "{t}");
        let t = throughput(1000, Duration::from_millis(5), Duration::from_millis(10));
        assert_eq!(t, 1_000_000.0);
    }

    #[tokio::test]
    async fn measure_link_over_a_local_shell() {
        let s = measure_link(&LocalShell::default(), 64 * 1024, MEASURE_TIMEOUT)
            .await
            .unwrap();
        assert!(!s.timed_out);
        assert!(s.bytes_per_sec > 0.0);
        assert!(s.is_fresh(LINK_MAX_AGE, unix_now()));
    }

    /// A fake shell for timing tests: call `n` sleeps `delays[n]` (the last
    /// entry repeats), and `None` means the call hangs forever. `true`
    /// succeeds and the sample script returns the requested bytes.
    struct Scripted {
        delays: Vec<Option<Duration>>,
        calls: Mutex<Vec<String>>,
    }

    impl Scripted {
        fn new(delays: &[Option<u64>]) -> Self {
            Self {
                delays: delays
                    .iter()
                    .map(|d| d.map(Duration::from_millis))
                    .collect(),
                calls: Mutex::new(Vec::new()),
            }
        }

        fn calls(&self) -> Vec<String> {
            self.calls.lock().unwrap().clone()
        }
    }

    #[async_trait::async_trait]
    impl RemoteShell for Scripted {
        async fn run(&self, script: &str, _: &[u8]) -> std::io::Result<crate::shell::ShellOutput> {
            let n = {
                let mut calls = self.calls.lock().unwrap();
                calls.push(script.to_owned());
                calls.len() - 1
            };
            let delay = self.delays[n.min(self.delays.len() - 1)];
            match delay {
                Some(d) => tokio::time::sleep(d).await,
                None => std::future::pending::<()>().await,
            }
            let stdout = match script.strip_prefix("head -c ") {
                Some(rest) => {
                    let len: usize = rest.split_whitespace().next().unwrap().parse().unwrap();
                    vec![0u8; len]
                }
                None => Vec::new(),
            };
            Ok(crate::shell::ShellOutput {
                code: Some(0),
                stdout,
                stderr: String::new(),
            })
        }
        fn command_for(&self, _: &str) -> (String, Vec<String>) {
            unreachable!()
        }
        fn host(&self) -> String {
            "scripted".into()
        }
    }

    #[tokio::test(start_paused = true)]
    async fn the_ssh_handshake_does_not_inflate_the_throughput() {
        // The reviewer's example: a cold 10 Mbit/s link with a 50 ms RTT.
        // The first command pays a 300 ms handshake (it becomes the
        // ControlMaster); later commands pay 100 ms of mux setup and round
        // trips; 256 KiB at 10 Mbit/s adds about 210 ms.
        let bytes = DEFAULT_SAMPLE_BYTES;
        let shell = Scripted::new(&[Some(300), Some(100), Some(310)]);
        let s = measure_link(&shell, bytes, MEASURE_TIMEOUT).await.unwrap();
        assert_eq!(shell.calls(), vec!["true", "true", &sample_script(bytes)]);
        assert!(!s.timed_out);
        // 256 KiB / 210 ms = 9.99 Mbit/s; with the handshake as overhead it
        // would have been 256 KiB / 10 ms = 210 Mbit/s.
        assert!((s.mbps() - 9.986).abs() < 0.01, "{}", s.mbps());
        let r = remote(SshCompression::Auto);
        assert!(decide(&r, Some(s)).active, "a 10 Mbit/s link is compressed");
    }

    #[tokio::test(start_paused = true)]
    async fn a_measurement_that_times_out_records_an_upper_bound() {
        // The connection comes up and round-trips, but the transfer does
        // not finish within the timeout.
        let shell = Scripted::new(&[Some(5), Some(5), None]);
        let s = measure_link(&shell, 1000, Duration::from_millis(50))
            .await
            .unwrap();
        assert_eq!(shell.calls().len(), 3);
        assert!(s.timed_out);
        assert_eq!(s.bytes_per_sec, 20_000.0);
        assert!(s.describe().starts_with("< "));
        assert!(decide(&remote(SshCompression::Auto), Some(s)).active);
    }

    #[tokio::test(start_paused = true)]
    async fn a_connection_setup_that_hangs_is_an_error_and_stores_nothing() {
        // An unreachable or slow-to-authenticate host: the first command
        // never finishes. Nothing was measured, so nothing is recorded and
        // `auto` stays unmeasured instead of turning compression on.
        let host = crate::hosts::HostRegistry::from_config(
            &harness_config::ini::load_str("[remote.vpn]\nhost = h\n", None).unwrap(),
        );
        let host = host.get("vpn").unwrap();
        let links = LinkStore::in_memory();
        let shell = Scripted::new(&[None]);
        let err = crate::hosts::measure_host_link(host, &shell, &links)
            .await
            .unwrap_err();
        assert!(err.contains("connection setup timed out"), "{err}");
        assert_eq!(shell.calls(), vec!["true"]);
        assert!(links.get("vpn").is_none());
        assert!(!host.compression(links.get("vpn")).active);
        // Same when the round trip on the warm connection hangs.
        let shell = Scripted::new(&[Some(5), None]);
        let err = measure_link(&shell, 1000, Duration::from_millis(50))
            .await
            .unwrap_err();
        assert!(err.contains("round trip timed out"), "{err}");
    }

    #[tokio::test]
    async fn a_failing_connection_setup_is_an_error() {
        struct Refused;
        #[async_trait::async_trait]
        impl RemoteShell for Refused {
            async fn run(&self, _: &str, _: &[u8]) -> std::io::Result<crate::shell::ShellOutput> {
                Ok(crate::shell::ShellOutput {
                    code: Some(255),
                    stdout: Vec::new(),
                    stderr: "ssh: connect to host h port 22: Connection refused\n".into(),
                })
            }
            fn command_for(&self, _: &str) -> (String, Vec<String>) {
                unreachable!()
            }
            fn host(&self) -> String {
                "refused".into()
            }
        }
        let err = measure_link(&Refused, 1000, MEASURE_TIMEOUT)
            .await
            .unwrap_err();
        assert!(err.contains("connection setup failed"), "{err}");
        assert!(err.contains("Connection refused"), "{err}");
    }

    #[tokio::test]
    async fn a_failing_measurement_is_an_error() {
        let shell = LocalShell {
            env: vec![("PATH".into(), "/nonexistent".into())],
        };
        assert!(measure_link(&shell, 1000, MEASURE_TIMEOUT).await.is_err());
    }

    #[test]
    fn store_persists_and_ages_out() {
        let dir = tempfile::tempdir().unwrap();
        let store = LinkStore::in_data_dir(dir.path());
        assert!(store.get("gpu").is_none());
        store.put("gpu", sample(5.0));
        let again = LinkStore::in_data_dir(dir.path());
        assert_eq!(again.get("gpu"), store.get("gpu"));
        assert!(again.fresh("gpu", LINK_MAX_AGE).is_some());
        let old = LinkSample {
            measured_at: unix_now() - LINK_MAX_AGE.as_secs() - 10,
            ..sample(5.0)
        };
        again.put("gpu", old);
        assert!(again.fresh("gpu", LINK_MAX_AGE).is_none());
        assert!(again.get("gpu").is_some());
        assert!(LinkStore::in_memory().path().is_none());
    }

    const RSYNC_34: &str = "rsync  version 3.4.1  protocol version 32\nCapabilities:\n    64-bit files\nChecksum list:\n    xxh128 xxh3 xxh64 (xxhash) md5 md4 sha1 none\nCompress list:\n    zstd lz4 zlibx zlib none\nDaemon auth list:\n    sha512\n";
    const RSYNC_NO_ZSTD: &str =
        "rsync  version 3.2.3  protocol version 31\nCompress list:\n    lz4 zlibx zlib none\n";
    const RSYNC_31: &str = "rsync  version 3.1.3  protocol version 31\nCapabilities:\n    64-bit files, 64-bit inums\n";

    #[test]
    fn rsync_zstd_needs_both_ends() {
        assert!(rsync_supports_zstd(RSYNC_34));
        assert!(rsync_supports_zstd("Compress list: zstd zlib"));
        assert!(!rsync_supports_zstd(RSYNC_NO_ZSTD));
        assert!(!rsync_supports_zstd(RSYNC_31));
        assert!(!rsync_supports_zstd(""));
        assert_eq!(
            RsyncCompression::choose(RSYNC_34, RSYNC_34),
            RsyncCompression::Zstd
        );
        assert_eq!(
            RsyncCompression::choose(RSYNC_34, RSYNC_31),
            RsyncCompression::Default
        );
        assert_eq!(
            RsyncCompression::choose(RSYNC_NO_ZSTD, RSYNC_34),
            RsyncCompression::Default
        );
        assert_eq!(
            RsyncCompression::choose(RSYNC_34, ""),
            RsyncCompression::Default
        );
        assert_eq!(
            RsyncCompression::Zstd.args(),
            vec!["--compress", "--compress-choice=zstd"]
        );
        assert_eq!(RsyncCompression::Default.args(), vec!["--compress"]);
    }
}
