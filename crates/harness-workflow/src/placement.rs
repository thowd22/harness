//! Host placement for `runs_on` (agents and workflow steps).
//!
//! [`HostPool`] knows the `[remote.<name>]` hosts from harness.ini
//! ([`HostPool::from_registry`]), their labels, `max_jobs` and the latest
//! probe results. For each step it:
//!
//! - runs `runs_on: local` (or no `runs_on`) locally;
//! - waits for a free job slot on a named host (`runs_on: gpu-box`);
//! - for a selector (`runs_on: { labels: [gpu], min_mem_gb: 64, fallback: local }`)
//!   keeps the hosts that have every label, are reachable and meet the
//!   resource minimums from the probe (cpus, memory, GPUs), then picks the
//!   least busy one (running jobs / `max_jobs`, then load per CPU). When all
//!   matching hosts are busy it waits. When none matches it uses the
//!   `fallback`, or fails with an explanation.
//!
//! A [`HostLease`] holds one job slot and frees it on drop.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use harness_remote::hosts::HostRegistry;
use harness_remote::probe::{Health, ProbeCache, ProbeReport};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

use crate::schema::{Fallback, HostSelector, RunsOn};

/// What the pool knows about one host.
#[derive(Debug, Clone, PartialEq)]
pub struct HostInfo {
    /// `[remote.<name>]`.
    pub name: String,
    /// Labels.
    pub labels: Vec<String>,
    /// Concurrent jobs allowed.
    pub max_jobs: u32,
    /// Latest probe, if any.
    pub probe: Option<ProbeReport>,
    /// False when the last probe failed.
    pub reachable: bool,
}

impl HostInfo {
    /// A reachable, unprobed host.
    pub fn new(name: impl Into<String>, labels: Vec<String>, max_jobs: u32) -> Self {
        Self {
            name: name.into(),
            labels,
            max_jobs: max_jobs.max(1),
            probe: None,
            reachable: true,
        }
    }

    /// Adds probe results.
    pub fn with_probe(mut self, probe: ProbeReport) -> Self {
        self.probe = Some(probe);
        self
    }

    /// Why the host does not satisfy `sel`, or `None` when it does.
    pub fn mismatch(&self, sel: &HostSelector) -> Option<String> {
        if !self.reachable {
            return Some("unreachable".into());
        }
        let missing: Vec<&str> = sel
            .labels
            .iter()
            .filter(|l| !self.labels.contains(l))
            .map(String::as_str)
            .collect();
        if !missing.is_empty() {
            return Some(format!("missing labels {}", missing.join(", ")));
        }
        let needs_probe =
            sel.min_mem_gb.is_some() || sel.min_cpus.is_some() || sel.min_gpus.is_some();
        let Some(p) = &self.probe else {
            return needs_probe.then(|| "no probe results for resource minimums".into());
        };
        if let Some(min) = sel.min_mem_gb
            && p.mem_gb().unwrap_or(0) < u64::from(min)
        {
            return Some(format!("{} GB memory < {min} GB", p.mem_gb().unwrap_or(0)));
        }
        if let Some(min) = sel.min_cpus
            && p.cpus < min
        {
            return Some(format!("{} cpus < {min}", p.cpus));
        }
        if let Some(min) = sel.min_gpus
            && (p.gpus.len() as u64) < u64::from(min)
        {
            return Some(format!("{} gpus < {min}", p.gpus.len()));
        }
        None
    }

    fn load_per_cpu(&self) -> f64 {
        match &self.probe {
            Some(p) => p.load.map_or(0.0, |l| l[0]) / f64::from(p.cpus.max(1)),
            None => 0.0,
        }
    }
}

/// Where a step runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Placement {
    /// This machine.
    Local,
    /// A remote host.
    Host(String),
}

impl Placement {
    /// The host name, if remote.
    pub fn host(&self) -> Option<&str> {
        match self {
            Self::Local => None,
            Self::Host(h) => Some(h),
        }
    }
}

/// No host can run the step.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PlacementError {
    /// The host is not in harness.ini.
    #[error("unknown host `{0}`: declare it as [remote.{0}] in harness.ini")]
    UnknownHost(String),
    /// No host matches and there is no fallback.
    #[error("no host matches runs_on ({0}) and there is no fallback")]
    NoMatch(String),
    /// Cancelled while waiting.
    #[error("cancelled while waiting for a host")]
    Cancelled,
}

enum Pick {
    Now(Placement),
    Wait,
}

/// Hosts with job counting.
#[derive(Debug)]
pub struct HostPool {
    hosts: Mutex<Vec<HostInfo>>,
    running: Mutex<BTreeMap<String, u32>>,
    freed: Notify,
}

/// One job slot on a host (or a local placement). Frees the slot on drop.
#[derive(Debug)]
pub struct HostLease {
    pool: Option<Arc<HostPool>>,
    placement: Placement,
}

impl HostLease {
    /// Where to run.
    pub fn placement(&self) -> &Placement {
        &self.placement
    }
}

impl Drop for HostLease {
    fn drop(&mut self) {
        if let (Some(pool), Placement::Host(h)) = (&self.pool, &self.placement) {
            let mut r = pool.running.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(n) = r.get_mut(h) {
                *n = n.saturating_sub(1);
            }
            drop(r);
            pool.freed.notify_waiters();
        }
    }
}

impl HostPool {
    /// A pool of `hosts`.
    pub fn new(hosts: Vec<HostInfo>) -> Self {
        Self {
            hosts: Mutex::new(hosts),
            running: Mutex::new(BTreeMap::new()),
            freed: Notify::new(),
        }
    }

    /// Hosts from harness.ini with the cached probe results.
    pub fn from_registry(registry: &HostRegistry, cache: &ProbeCache) -> Self {
        let hosts = registry
            .iter()
            .map(|h| {
                let mut info =
                    HostInfo::new(h.name.clone(), h.config.labels.clone(), h.config.max_jobs);
                match cache.get(&h.name) {
                    Some(Ok(p)) => info.probe = Some(p),
                    Some(Err(_)) => info.reachable = false,
                    None => {}
                }
                if matches!(
                    cache.health(&h.name, h.config.max_jobs),
                    Health::Unreachable { .. }
                ) {
                    info.reachable = false;
                }
                info
            })
            .collect();
        Self::new(hosts)
    }

    /// Replaces a host's probe result (`None` = probe failed).
    pub fn update_probe(&self, name: &str, probe: Option<ProbeReport>) {
        let mut hosts = self.hosts.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(h) = hosts.iter_mut().find(|h| h.name == name) {
            h.reachable = probe.is_some();
            h.probe = probe;
        }
        drop(hosts);
        self.freed.notify_waiters();
    }

    /// Jobs this pool has placed on `name` and not yet released.
    pub fn running(&self, name: &str) -> u32 {
        self.running
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(name)
            .copied()
            .unwrap_or(0)
    }

    /// Hosts that satisfy `sel` (ignoring how busy they are).
    pub fn candidates(&self, sel: &HostSelector) -> Vec<String> {
        self.hosts
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .filter(|h| h.mismatch(sel).is_none())
            .map(|h| h.name.clone())
            .collect()
    }

    fn try_pick(&self, runs_on: &RunsOn) -> Result<Pick, PlacementError> {
        let hosts = self.hosts.lock().unwrap_or_else(|e| e.into_inner());
        let mut running = self.running.lock().unwrap_or_else(|e| e.into_inner());
        let free = |h: &HostInfo, running: &BTreeMap<String, u32>| {
            running.get(&h.name).copied().unwrap_or(0) < h.max_jobs
        };
        let take = |name: &str, running: &mut BTreeMap<String, u32>| {
            *running.entry(name.to_owned()).or_insert(0) += 1;
            Pick::Now(Placement::Host(name.to_owned()))
        };
        match runs_on {
            RunsOn::Local => Ok(Pick::Now(Placement::Local)),
            RunsOn::Host(name) => {
                let h = hosts
                    .iter()
                    .find(|h| h.name == *name)
                    .ok_or_else(|| PlacementError::UnknownHost(name.clone()))?;
                Ok(if free(h, &running) {
                    take(name, &mut running)
                } else {
                    Pick::Wait
                })
            }
            RunsOn::Select(sel) => {
                let matching: Vec<&HostInfo> =
                    hosts.iter().filter(|h| h.mismatch(sel).is_none()).collect();
                if matching.is_empty() {
                    return match &sel.fallback {
                        Some(Fallback::Local) => Ok(Pick::Now(Placement::Local)),
                        Some(Fallback::Host(name)) => {
                            let h = hosts
                                .iter()
                                .find(|h| h.name == *name)
                                .ok_or_else(|| PlacementError::UnknownHost(name.clone()))?;
                            Ok(if free(h, &running) {
                                take(name, &mut running)
                            } else {
                                Pick::Wait
                            })
                        }
                        None => {
                            let why: Vec<String> = hosts
                                .iter()
                                .filter_map(|h| h.mismatch(sel).map(|m| format!("{}: {m}", h.name)))
                                .collect();
                            Err(PlacementError::NoMatch(if why.is_empty() {
                                "no hosts configured".into()
                            } else {
                                why.join("; ")
                            }))
                        }
                    };
                }
                let best = matching
                    .iter()
                    .filter(|h| free(h, &running))
                    .min_by(|a, b| {
                        let ra = f64::from(running.get(&a.name).copied().unwrap_or(0))
                            / f64::from(a.max_jobs);
                        let rb = f64::from(running.get(&b.name).copied().unwrap_or(0))
                            / f64::from(b.max_jobs);
                        ra.total_cmp(&rb)
                            .then(a.load_per_cpu().total_cmp(&b.load_per_cpu()))
                            .then(a.name.cmp(&b.name))
                    })
                    .map(|h| h.name.clone());
                Ok(match best {
                    Some(name) => take(&name, &mut running),
                    None => Pick::Wait,
                })
            }
        }
    }

    /// Places a step, waiting for a job slot when every suitable host is
    /// busy. `on_wait` runs once before the first wait.
    pub async fn acquire(
        self: &Arc<Self>,
        runs_on: &RunsOn,
        cancel: &CancellationToken,
        on_wait: impl FnOnce(),
    ) -> Result<HostLease, PlacementError> {
        let mut on_wait = Some(on_wait);
        loop {
            let notified = self.freed.notified();
            match self.try_pick(runs_on)? {
                Pick::Now(placement) => {
                    return Ok(HostLease {
                        pool: Some(self.clone()),
                        placement,
                    });
                }
                Pick::Wait => {
                    if let Some(f) = on_wait.take() {
                        f();
                    }
                    tokio::select! {
                        _ = notified => {}
                        _ = cancel.cancelled() => return Err(PlacementError::Cancelled),
                    }
                }
            }
        }
    }
}

/// An agent's `runs_on` (harness-config) as a placement request, so
/// subagents and agent steps are placed by the same [`HostPool`].
pub fn from_agent(r: &harness_config::agents::RunsOn) -> RunsOn {
    use harness_config::agents::RunsOn as A;
    match r {
        A::Local => RunsOn::Local,
        A::Remote(name) => RunsOn::Host(name.clone()),
        A::Labels {
            labels,
            min_mem_gb,
            min_cpus,
            min_gpus,
            fallback,
        } => RunsOn::Select(HostSelector {
            labels: labels.clone(),
            min_mem_gb: *min_mem_gb,
            min_cpus: *min_cpus,
            min_gpus: *min_gpus,
            fallback: fallback.as_deref().map(|f| match f {
                "local" => crate::schema::Fallback::Local,
                host => crate::schema::Fallback::Host(host.to_owned()),
            }),
        }),
    }
}

/// A local lease (no pool).
pub fn local_lease() -> HostLease {
    HostLease {
        pool: None,
        placement: Placement::Local,
    }
}

#[cfg(test)]
mod tests;
