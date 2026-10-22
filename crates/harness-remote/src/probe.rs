//! Remote host capability probing and health.
//!
//! The helper answers the `probe` RPC with a [`ProbeReport`] built by
//! [`collect`]: CPU count and load, memory from `/proc/meminfo`, GPUs from
//! `nvidia-smi`, disk space from `df`, and the number of jobs it is
//! running. The client keeps reports in a [`ProbeCache`] with a TTL so the
//! scheduler and the `/remote` view do not probe on every frame, and
//! [`Health::assess`] turns a report (or a failure) into a badge.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

/// Default probe cache TTL.
pub const DEFAULT_PROBE_TTL: Duration = Duration::from_secs(30);
/// Free disk below this is reported as degraded.
pub const LOW_DISK_BYTES: u64 = 1024 * 1024 * 1024;
/// Available memory below this fraction of total is degraded.
pub const LOW_MEM_FRACTION: f64 = 0.05;

/// One GPU as reported by `nvidia-smi`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Gpu {
    /// Model name.
    pub name: String,
    /// Total memory in MiB.
    pub memory_total_mib: u64,
    /// Used memory in MiB.
    pub memory_used_mib: u64,
    /// Utilisation percent.
    pub utilization_pct: Option<u32>,
}

/// Disk usage of one filesystem.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Disk {
    /// Path that was measured.
    pub path: String,
    /// Total bytes.
    pub total_bytes: u64,
    /// Available bytes.
    pub available_bytes: u64,
}

/// What a host offers right now.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProbeReport {
    /// Helper version.
    pub helper_version: String,
    /// Architecture.
    pub arch: String,
    /// Operating system.
    pub os: String,
    /// Logical CPUs.
    pub cpus: u32,
    /// 1/5/15 minute load averages.
    pub load: Option<[f64; 3]>,
    /// Total memory in bytes.
    pub mem_total_bytes: Option<u64>,
    /// Available memory in bytes.
    pub mem_available_bytes: Option<u64>,
    /// GPUs (empty when `nvidia-smi` is missing or fails).
    pub gpus: Vec<Gpu>,
    /// Disk of the probed path.
    pub disk: Option<Disk>,
    /// Jobs currently running under this helper.
    pub jobs: usize,
}

impl ProbeReport {
    /// Total memory in GiB, rounded down.
    pub fn mem_gb(&self) -> Option<u64> {
        self.mem_total_bytes.map(|b| b / (1024 * 1024 * 1024))
    }

    /// One-line summary for `/remote`, e.g. `16 cpu, 62/64 GiB free, 1 gpu, 120 GiB disk, 0 jobs`.
    pub fn summary(&self) -> String {
        const GIB: u64 = 1024 * 1024 * 1024;
        let mut parts = vec![format!("{} cpu", self.cpus)];
        if let (Some(t), Some(a)) = (self.mem_total_bytes, self.mem_available_bytes) {
            parts.push(format!("{}/{} GiB mem free", a / GIB, t / GIB));
        }
        if !self.gpus.is_empty() {
            parts.push(format!(
                "{} gpu ({})",
                self.gpus.len(),
                self.gpus
                    .iter()
                    .map(|g| g.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        if let Some(d) = &self.disk {
            parts.push(format!("{} GiB disk free", d.available_bytes / GIB));
        }
        parts.push(format!(
            "{} job{}",
            self.jobs,
            if self.jobs == 1 { "" } else { "s" }
        ));
        parts.join(", ")
    }
}

/// Parse `/proc/meminfo` into (total, available) bytes.
pub fn parse_meminfo(text: &str) -> (Option<u64>, Option<u64>) {
    let field = |name: &str| {
        text.lines().find_map(|l| {
            let rest = l.strip_prefix(name)?.strip_prefix(':')?;
            let kb: u64 = rest.split_whitespace().next()?.parse().ok()?;
            Some(kb * 1024)
        })
    };
    (field("MemTotal"), field("MemAvailable"))
}

/// Parse `/proc/loadavg`.
pub fn parse_loadavg(text: &str) -> Option<[f64; 3]> {
    let mut it = text.split_whitespace().map(|s| s.parse::<f64>().ok());
    Some([it.next()??, it.next()??, it.next()??])
}

/// Parse `nvidia-smi --query-gpu=name,memory.total,memory.used,utilization.gpu
/// --format=csv,noheader,nounits`.
pub fn parse_nvidia_smi(text: &str) -> Vec<Gpu> {
    text.lines()
        .filter_map(|l| {
            let cols: Vec<&str> = l.split(',').map(str::trim).collect();
            if cols.len() < 3 || cols[0].is_empty() {
                return None;
            }
            Some(Gpu {
                name: cols[0].to_owned(),
                memory_total_mib: cols[1].parse().ok()?,
                memory_used_mib: cols[2].parse().ok()?,
                utilization_pct: cols.get(3).and_then(|c| c.parse().ok()),
            })
        })
        .collect()
}

/// Parse `df -Pk <path>` output.
pub fn parse_df(path: &str, text: &str) -> Option<Disk> {
    let line = text.lines().nth(1)?;
    let cols: Vec<&str> = line.split_whitespace().collect();
    if cols.len() < 4 {
        return None;
    }
    Some(Disk {
        path: path.to_owned(),
        total_bytes: cols[1].parse::<u64>().ok()? * 1024,
        available_bytes: cols[3].parse::<u64>().ok()? * 1024,
    })
}

/// The `nvidia-smi` arguments used by [`collect`].
pub const NVIDIA_SMI_ARGS: [&str; 2] = [
    "--query-gpu=name,memory.total,memory.used,utilization.gpu",
    "--format=csv,noheader,nounits",
];

async fn command_stdout(program: &str, args: &[&str]) -> Option<String> {
    let fut = tokio::process::Command::new(program)
        .args(args)
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .output();
    let out = tokio::time::timeout(Duration::from_secs(10), fut)
        .await
        .ok()?
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Probe this machine (runs inside the helper). `disk_path` defaults to
/// the home directory.
pub async fn collect(disk_path: Option<&Path>, jobs: usize) -> ProbeReport {
    let cpus = std::thread::available_parallelism()
        .map(|n| n.get() as u32)
        .unwrap_or(1);
    let (mem_total_bytes, mem_available_bytes) = tokio::fs::read_to_string("/proc/meminfo")
        .await
        .map(|t| parse_meminfo(&t))
        .unwrap_or((None, None));
    let load = tokio::fs::read_to_string("/proc/loadavg")
        .await
        .ok()
        .and_then(|t| parse_loadavg(&t));
    let gpus = command_stdout("nvidia-smi", &NVIDIA_SMI_ARGS)
        .await
        .map(|t| parse_nvidia_smi(&t))
        .unwrap_or_default();
    let path = disk_path
        .map(Path::to_path_buf)
        .or_else(|| std::env::var_os("HOME").map(Into::into))
        .unwrap_or_else(|| "/".into());
    let p = path.to_string_lossy().into_owned();
    let disk = command_stdout("df", &["-Pk", &p])
        .await
        .and_then(|t| parse_df(&p, &t));
    ProbeReport {
        helper_version: crate::protocol::HELPER_VERSION.to_owned(),
        arch: std::env::consts::ARCH.to_owned(),
        os: std::env::consts::OS.to_owned(),
        cpus,
        load,
        mem_total_bytes,
        mem_available_bytes,
        gpus,
        disk,
        jobs,
    }
}

/// Health of a host, shown as a badge in `/remote` and on map nodes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Health {
    /// Not probed yet.
    Unknown,
    /// Reachable with resources to spare.
    Healthy,
    /// Reachable but short of something.
    Degraded {
        /// Why.
        reasons: Vec<String>,
    },
    /// Cannot be reached or the helper failed.
    Unreachable {
        /// Why.
        reason: String,
    },
}

impl Health {
    /// Assess a probe outcome against the host's `max_jobs`.
    pub fn assess(result: Result<&ProbeReport, &str>, max_jobs: u32) -> Health {
        let report = match result {
            Ok(r) => r,
            Err(e) => {
                return Health::Unreachable {
                    reason: e.to_owned(),
                };
            }
        };
        let mut reasons = Vec::new();
        if let Some(d) = &report.disk
            && d.available_bytes < LOW_DISK_BYTES
        {
            reasons.push(format!(
                "low disk: {} MiB free",
                d.available_bytes / (1024 * 1024)
            ));
        }
        if let (Some(t), Some(a)) = (report.mem_total_bytes, report.mem_available_bytes)
            && t > 0
            && (a as f64) < (t as f64) * LOW_MEM_FRACTION
        {
            reasons.push(format!("low memory: {} MiB free", a / (1024 * 1024)));
        }
        if let Some([l1, _, _]) = report.load
            && l1 > f64::from(report.cpus) * 2.0
        {
            reasons.push(format!("overloaded: load {l1:.1} on {} cpu", report.cpus));
        }
        if report.jobs >= max_jobs as usize {
            reasons.push(format!("busy: {}/{} jobs", report.jobs, max_jobs));
        }
        if reasons.is_empty() {
            Health::Healthy
        } else {
            Health::Degraded { reasons }
        }
    }

    /// Short badge text: `ok`, `degraded`, `down` or `?`.
    pub fn badge(&self) -> &'static str {
        match self {
            Health::Unknown => "?",
            Health::Healthy => "ok",
            Health::Degraded { .. } => "degraded",
            Health::Unreachable { .. } => "down",
        }
    }

    /// The theme token name (see doc-7) the badge should be drawn with.
    pub fn token(&self) -> &'static str {
        match self {
            Health::Unknown => "muted",
            Health::Healthy => "ok",
            Health::Degraded { .. } => "warn",
            Health::Unreachable { .. } => "error",
        }
    }

    /// Badge plus reasons, for `/remote`.
    pub fn describe(&self) -> String {
        match self {
            Health::Degraded { reasons } => format!("degraded ({})", reasons.join("; ")),
            Health::Unreachable { reason } => format!("down ({reason})"),
            other => other.badge().to_owned(),
        }
    }
}

/// A cached probe outcome.
#[derive(Debug, Clone)]
struct Cached {
    at: Instant,
    result: Result<ProbeReport, String>,
}

/// Probe results per host, valid for a TTL, plus the hosts' last link
/// throughput measurements ([`crate::link::LinkStore`], which do not
/// expire with the TTL).
#[derive(Debug)]
pub struct ProbeCache {
    ttl: Duration,
    entries: Mutex<HashMap<String, Cached>>,
    links: std::sync::Arc<crate::link::LinkStore>,
}

impl Default for ProbeCache {
    fn default() -> Self {
        Self::new(DEFAULT_PROBE_TTL)
    }
}

impl ProbeCache {
    /// A cache whose entries expire after `ttl`.
    pub fn new(ttl: Duration) -> Self {
        Self {
            ttl,
            entries: Mutex::new(HashMap::new()),
            links: Default::default(),
        }
    }

    /// This cache with `links` as its link measurements (e.g. a store
    /// persisted under the data directory).
    pub fn with_links(mut self, links: std::sync::Arc<crate::link::LinkStore>) -> Self {
        self.links = links;
        self
    }

    /// The link measurements.
    pub fn links(&self) -> &std::sync::Arc<crate::link::LinkStore> {
        &self.links
    }

    /// The TTL.
    pub fn ttl(&self) -> Duration {
        self.ttl
    }

    /// Store a probe outcome for `host`.
    pub fn put(&self, host: &str, result: Result<ProbeReport, String>) {
        self.put_at(host, result, Instant::now());
    }

    fn put_at(&self, host: &str, result: Result<ProbeReport, String>, at: Instant) {
        self.lock().insert(host.to_owned(), Cached { at, result });
    }

    /// The fresh outcome for `host`, if any.
    pub fn get(&self, host: &str) -> Option<Result<ProbeReport, String>> {
        self.get_at(host, Instant::now())
    }

    fn get_at(&self, host: &str, now: Instant) -> Option<Result<ProbeReport, String>> {
        let map = self.lock();
        let c = map.get(host)?;
        (now.saturating_duration_since(c.at) < self.ttl).then(|| c.result.clone())
    }

    /// Fresh outcome, or the result of `probe` (which is then cached).
    pub async fn get_or_probe<F, Fut>(&self, host: &str, probe: F) -> Result<ProbeReport, String>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Result<ProbeReport, String>>,
    {
        if let Some(r) = self.get(host) {
            return r;
        }
        let r = probe().await;
        self.put(host, r.clone());
        r
    }

    /// Forget `host` (e.g. after a disconnect).
    pub fn invalidate(&self, host: &str) {
        self.lock().remove(host);
    }

    /// Health for `host` from the cache ([`Health::Unknown`] if stale or missing).
    pub fn health(&self, host: &str, max_jobs: u32) -> Health {
        match self.get(host) {
            None => Health::Unknown,
            Some(Ok(r)) => Health::assess(Ok(&r), max_jobs),
            Some(Err(e)) => Health::assess(Err(&e), max_jobs),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, Cached>> {
        self.entries.lock().unwrap_or_else(|e| e.into_inner())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(crate) fn report() -> ProbeReport {
        ProbeReport {
            helper_version: "0.1.0".into(),
            arch: "x86_64".into(),
            os: "linux".into(),
            cpus: 8,
            load: Some([0.5, 0.4, 0.3]),
            mem_total_bytes: Some(64 << 30),
            mem_available_bytes: Some(40 << 30),
            gpus: vec![],
            disk: Some(Disk {
                path: "/home".into(),
                total_bytes: 500 << 30,
                available_bytes: 100 << 30,
            }),
            jobs: 0,
        }
    }

    #[test]
    fn parses_proc_files() {
        let (t, a) = parse_meminfo(
            "MemTotal:       16303428 kB\nMemFree: 1 kB\nMemAvailable:   8000000 kB\n",
        );
        assert_eq!(t, Some(16303428 * 1024));
        assert_eq!(a, Some(8000000 * 1024));
        assert_eq!(
            parse_loadavg("0.52 0.58 0.59 1/1234 5678"),
            Some([0.52, 0.58, 0.59])
        );
        assert_eq!(parse_loadavg("x"), None);
    }

    #[test]
    fn parses_nvidia_smi() {
        let g = parse_nvidia_smi(
            "NVIDIA A100-SXM4-80GB, 81920, 1024, 37\nNVIDIA L4, 23034, 0, [N/A]\n",
        );
        assert_eq!(g.len(), 2);
        assert_eq!(g[0].name, "NVIDIA A100-SXM4-80GB");
        assert_eq!(g[0].memory_total_mib, 81920);
        assert_eq!(g[0].utilization_pct, Some(37));
        assert_eq!(g[1].utilization_pct, None);
        assert!(parse_nvidia_smi("").is_empty());
    }

    #[test]
    fn parses_df() {
        let d = parse_df(
            "/home",
            "Filesystem 1024-blocks Used Available Capacity Mounted on\n/dev/sda1 1000 400 600 40% /\n",
        )
        .unwrap();
        assert_eq!(d.total_bytes, 1000 * 1024);
        assert_eq!(d.available_bytes, 600 * 1024);
    }

    #[tokio::test]
    async fn collect_reports_this_machine() {
        let r = collect(Some(Path::new("/")), 2).await;
        assert!(r.cpus >= 1);
        assert_eq!(r.jobs, 2);
        assert_eq!(r.arch, std::env::consts::ARCH);
        if cfg!(target_os = "linux") {
            assert!(r.mem_total_bytes.is_some());
            assert!(r.disk.is_some());
        }
    }

    #[test]
    fn health_assessment() {
        let r = report();
        assert_eq!(Health::assess(Ok(&r), 1), Health::Healthy);
        let mut low = r.clone();
        low.disk.as_mut().unwrap().available_bytes = 10 << 20;
        low.jobs = 2;
        let h = Health::assess(Ok(&low), 2);
        let Health::Degraded { reasons } = &h else {
            panic!("{h:?}")
        };
        assert_eq!(reasons.len(), 2);
        assert_eq!(h.badge(), "degraded");
        assert_eq!(h.token(), "warn");
        let down = Health::assess(Err("ssh: cannot reach h"), 1);
        assert_eq!(down.badge(), "down");
        assert!(down.describe().contains("cannot reach"));
    }

    #[test]
    fn cache_expires_after_ttl() {
        let c = ProbeCache::new(Duration::from_secs(10));
        let t0 = Instant::now();
        c.put_at("gpu", Ok(report()), t0);
        assert!(c.get_at("gpu", t0 + Duration::from_secs(5)).is_some());
        assert!(c.get_at("gpu", t0 + Duration::from_secs(11)).is_none());
        assert_eq!(c.health("other", 1), Health::Unknown);
        c.invalidate("gpu");
        assert!(c.get("gpu").is_none());
    }

    #[tokio::test]
    async fn get_or_probe_caches() {
        let c = ProbeCache::new(Duration::from_secs(60));
        let calls = std::sync::atomic::AtomicUsize::new(0);
        for _ in 0..3 {
            let r = c
                .get_or_probe("gpu", || async {
                    calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    Ok(report())
                })
                .await;
            assert!(r.is_ok());
        }
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert_eq!(c.health("gpu", 1), Health::Healthy);
        assert!(report().summary().contains("8 cpu"));
    }
}
