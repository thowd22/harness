//! Remote host configuration and scheduling.
//!
//! Hosts come only from `[remote.<name>]` sections of the user's
//! `harness.ini` ([`HostRegistry::from_config`]); there is deliberately no
//! way to add a host from project YAML. `runs_on` can only name or select
//! hosts that are already declared.
//!
//! This module also provides what the `/remote` command shows: one line per
//! host ([`HostRegistry::list_lines`]) and the result of its "test" action
//! ([`test_host`]: connectivity, helper status and a fresh probe).

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use harness_config::ini::{Config, Remote};
use harness_policy::rules::PolicyOverlay;

use crate::client::{ClientError, RemoteClient};
use crate::link::{CompressionDecision, LinkSample, LinkStore, RsyncCompression};
use crate::probe::{Health, ProbeCache, ProbeReport};
use crate::ssh::{SshOptions, SshTarget};
use crate::sync::{CleanupPolicy, DEFAULT_WORKDIR};

/// One declared remote host.
#[derive(Debug, Clone)]
pub struct RemoteHost {
    /// `[remote.<name>]` name.
    pub name: String,
    /// Its settings.
    pub config: Remote,
}

impl RemoteHost {
    /// The SSH destination.
    pub fn ssh_target(&self, options: SshOptions) -> SshTarget {
        SshTarget::new(&self.name, &self.config, options)
    }

    /// The SSH destination with compression decided from the host's
    /// `compression` setting and its last link measurement in `links`.
    pub fn ssh_target_with_links(&self, options: SshOptions, links: &LinkStore) -> SshTarget {
        let decision = self.compression(links.get(&self.name));
        self.ssh_target(options).with_compression(decision.active)
    }

    /// Whether this host's SSH connection is compressed given its last
    /// link measurement (see [`crate::link::decide`]).
    pub fn compression(&self, link: Option<LinkSample>) -> CompressionDecision {
        crate::link::decide(&self.config, link)
    }

    /// Remote workdir (default [`DEFAULT_WORKDIR`]).
    pub fn workdir(&self) -> &str {
        self.config.workdir.as_deref().unwrap_or(DEFAULT_WORKDIR)
    }

    /// The host's extra policy as a tighten-only overlay, to be applied with
    /// `PolicyEngine::with_overlay(self.policy_origin(), &overlay)` on top
    /// of the global policy for nodes running here. Overlays can only add
    /// `deny`/`ask` rules, so a host can never loosen the global policy.
    pub fn policy_overlay(&self) -> PolicyOverlay {
        PolicyOverlay {
            bash_deny: self.config.policy_bash_deny.clone(),
            bash_ask: self.config.policy_bash_ask.clone(),
            paths_protected: Vec::new(),
            outward_ask: Vec::new(),
        }
    }

    /// Origin label for the host overlay (`harness.ini [remote.<name>]`).
    pub fn policy_origin(&self) -> String {
        format!("harness.ini [remote.{}]", self.name)
    }

    /// True if the host has every label in `labels`.
    pub fn has_labels(&self, labels: &[String]) -> bool {
        labels.iter().all(|l| self.config.labels.contains(l))
    }

    /// Cleanup policy for run dirs (currently always the default,
    /// [`CleanupPolicy::OnSuccess`]).
    pub fn cleanup(&self) -> CleanupPolicy {
        CleanupPolicy::default()
    }

    /// `name  user@host:port  [labels]  max_jobs=N  sync=rsync`.
    pub fn describe(&self) -> String {
        let c = &self.config;
        let dest = match &c.user {
            Some(u) => format!("{u}@{}", c.host),
            None => c.host.clone(),
        };
        let labels = if c.labels.is_empty() {
            String::new()
        } else {
            format!("  [{}]", c.labels.join(", "))
        };
        format!(
            "{}  {dest}:{}{labels}  max_jobs={}  sync={}",
            self.name, c.port, c.max_jobs, c.sync
        )
    }
}

/// Errors looking up hosts.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum HostError {
    /// The name is not declared in harness.ini.
    #[error("unknown remote host `{name}`: hosts must be declared as [remote.{name}] in harness.ini{}", known_hint(.known))]
    Unknown {
        /// Requested name.
        name: String,
        /// Declared names.
        known: Vec<String>,
    },
    /// No declared host has the labels.
    #[error("no remote host has labels [{}]", .labels.join(", "))]
    NoMatch {
        /// Requested labels.
        labels: Vec<String>,
    },
}

fn known_hint(known: &[String]) -> String {
    if known.is_empty() {
        " (none are declared)".into()
    } else {
        format!(" (declared: {})", known.join(", "))
    }
}

/// Every host declared in harness.ini.
#[derive(Debug, Clone, Default)]
pub struct HostRegistry {
    hosts: BTreeMap<String, RemoteHost>,
}

impl HostRegistry {
    /// The hosts of `config.remotes`. This is the only constructor: hosts
    /// cannot come from project YAML.
    pub fn from_config(config: &Config) -> Self {
        Self {
            hosts: config
                .remotes
                .iter()
                .map(|(name, r)| {
                    (
                        name.clone(),
                        RemoteHost {
                            name: name.clone(),
                            config: r.clone(),
                        },
                    )
                })
                .collect(),
        }
    }

    /// True when no host is declared.
    pub fn is_empty(&self) -> bool {
        self.hosts.is_empty()
    }

    /// Hosts sorted by name.
    pub fn iter(&self) -> impl Iterator<Item = &RemoteHost> {
        self.hosts.values()
    }

    /// Host names.
    pub fn names(&self) -> Vec<String> {
        self.hosts.keys().cloned().collect()
    }

    /// Look a host up by name.
    pub fn get(&self, name: &str) -> Result<&RemoteHost, HostError> {
        self.hosts.get(name).ok_or_else(|| HostError::Unknown {
            name: name.to_owned(),
            known: self.names(),
        })
    }

    /// Hosts carrying every label in `labels`.
    pub fn with_labels(&self, labels: &[String]) -> Result<Vec<&RemoteHost>, HostError> {
        let v: Vec<_> = self.iter().filter(|h| h.has_labels(labels)).collect();
        if v.is_empty() {
            Err(HostError::NoMatch {
                labels: labels.to_vec(),
            })
        } else {
            Ok(v)
        }
    }

    /// Lines for `/remote`: one per host with its health badge from `cache`
    /// and whether its SSH connection is compressed
    /// (`compression=on (auto: link 3.2 Mbit/s < 20.0 Mbit/s)`).
    pub fn list_lines(&self, cache: &ProbeCache) -> Vec<String> {
        if self.is_empty() {
            return vec!["no remote hosts: add a [remote.<name>] section to harness.ini".into()];
        }
        self.iter()
            .map(|h| {
                let health = cache.health(&h.name, h.config.max_jobs);
                let summary = match cache.get(&h.name) {
                    Some(Ok(r)) => format!("  {}", r.summary()),
                    _ => String::new(),
                };
                let compression = h.compression(cache.links().get(&h.name)).describe();
                format!(
                    "[{}] {}  compression={compression}{summary}",
                    health.describe(),
                    h.describe()
                )
            })
            .collect()
    }
}

/// What the helper looks like on a host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HelperStatus {
    /// Running with a matching version.
    Ok {
        /// Helper version.
        version: String,
        /// Architecture.
        arch: String,
    },
    /// Running but with another version.
    Mismatch {
        /// Helper version.
        remote: String,
        /// Our version.
        local: String,
    },
    /// Not reachable / not started.
    Unavailable {
        /// Why.
        reason: String,
    },
}

/// Result of the `/remote` test action.
#[derive(Debug, Clone)]
pub struct HostTestReport {
    /// Host name.
    pub host: String,
    /// Round-trip of a ping, or the connection error.
    pub connectivity: Result<Duration, String>,
    /// Helper status.
    pub helper: HelperStatus,
    /// Fresh probe, if it worked.
    pub probe: Option<ProbeReport>,
    /// Health from the probe.
    pub health: Health,
    /// The link throughput measurement: `Some(Ok)` when measured,
    /// `Some(Err)` when the measurement failed, `None` when not run.
    pub link: Option<Result<LinkSample, String>>,
    /// Whether the host's SSH connection is compressed.
    pub compression: CompressionDecision,
    /// The rsync compression for pushes to the host, when checked.
    pub rsync: Option<RsyncCompression>,
}

impl HostTestReport {
    /// True when everything works.
    pub fn ok(&self) -> bool {
        self.connectivity.is_ok() && matches!(self.helper, HelperStatus::Ok { .. })
    }

    /// Human-readable lines.
    pub fn lines(&self) -> Vec<String> {
        let mut v = vec![format!("{}: {}", self.host, self.health.describe())];
        v.push(match &self.connectivity {
            Ok(d) => format!("  connection: ok ({} ms)", d.as_millis()),
            Err(e) => format!("  connection: FAILED: {e}"),
        });
        v.push(match &self.helper {
            HelperStatus::Ok { version, arch } => format!("  helper: {version} ({arch})"),
            HelperStatus::Mismatch { remote, local } => {
                format!("  helper: version {remote}, harness is {local} (will upgrade)")
            }
            HelperStatus::Unavailable { reason } => format!("  helper: unavailable: {reason}"),
        });
        if let Some(p) = &self.probe {
            v.push(format!("  resources: {}", p.summary()));
        }
        match &self.link {
            Some(Ok(l)) => v.push(format!("  link: {}", l.describe())),
            Some(Err(e)) => v.push(format!("  link: not measured: {e}")),
            None => {}
        }
        v.push(format!("  compression: {}", self.compression.describe()));
        if let Some(r) = self.rsync {
            v.push(format!("  rsync compression: {}", r.name()));
        }
        v
    }
}

/// Run the `/remote` test action for `host` given the outcome of
/// connecting to it, and refresh `cache`. The compression shown comes from
/// the host's setting and the last link measurement in
/// [`ProbeCache::links`]; [`measure_host_link`] refreshes that first.
pub async fn test_host(
    host: &RemoteHost,
    connected: Result<&RemoteClient, &ClientError>,
    cache: &ProbeCache,
) -> HostTestReport {
    let client = match connected {
        Ok(c) => c,
        Err(e) => {
            let reason = e.to_string();
            cache.put(&host.name, Err(reason.clone()));
            let helper = match e {
                ClientError::VersionMismatch { local, remote, .. } => HelperStatus::Mismatch {
                    remote: remote.clone(),
                    local: local.clone(),
                },
                _ => HelperStatus::Unavailable {
                    reason: reason.clone(),
                },
            };
            let link = cache.links().get(&host.name);
            return HostTestReport {
                host: host.name.clone(),
                connectivity: Err(reason.clone()),
                helper,
                probe: None,
                health: Health::Unreachable { reason },
                link: link.map(Ok),
                compression: host.compression(link),
                rsync: None,
            };
        }
    };
    let t0 = Instant::now();
    let connectivity = client
        .ping()
        .await
        .map(|_| t0.elapsed())
        .map_err(|e| e.to_string());
    let helper = match client.hello() {
        Some(h) => HelperStatus::Ok {
            version: h.version,
            arch: h.arch,
        },
        None => HelperStatus::Unavailable {
            reason: "no handshake".into(),
        },
    };
    let probe = client
        .probe(Some(host.workdir()))
        .await
        .map_err(|e| e.to_string());
    cache.put(&host.name, probe.clone());
    let health = match &probe {
        Ok(p) => Health::assess(Ok(p), host.config.max_jobs),
        Err(e) => Health::assess(Err(e), host.config.max_jobs),
    };
    let link = cache.links().get(&host.name);
    HostTestReport {
        host: host.name.clone(),
        connectivity,
        helper,
        probe: probe.ok(),
        health,
        link: link.map(Ok),
        compression: host.compression(link),
        rsync: None,
    }
}

/// Measure the link to `host` through `shell` (part of the remote probe)
/// and record it in `links`. A failure keeps the previous measurement.
pub async fn measure_host_link(
    host: &RemoteHost,
    shell: &dyn crate::shell::RemoteShell,
    links: &LinkStore,
) -> Result<LinkSample, String> {
    let sample = crate::link::measure_link(
        shell,
        crate::link::DEFAULT_SAMPLE_BYTES,
        crate::link::MEASURE_TIMEOUT,
    )
    .await?;
    links.put(&host.name, sample);
    Ok(sample)
}

#[cfg(test)]
mod tests {
    use super::*;
    use harness_policy::rules::{Action, Decision, Mode, PolicyConfig, PolicyEngine};

    fn config(text: &str) -> Config {
        harness_config::ini::load_str(text, None).unwrap()
    }

    const INI: &str = "\
[policy]
bash.deny = rm -rf *
[remote.gpu-box]
host = 10.0.0.5
user = ci
port = 2222
identity_file = /keys/id_gpu
labels = gpu, cuda
workdir = /scratch/harness
max_jobs = 2
sync = git
policy.bash.deny = shutdown *
policy.bash.ask = docker *
[remote.cpu]
host = cpu.example
";

    #[test]
    fn hosts_come_from_ini_with_all_keys() {
        let reg = HostRegistry::from_config(&config(INI));
        assert_eq!(reg.names(), vec!["cpu", "gpu-box"]);
        let h = reg.get("gpu-box").unwrap();
        let c = &h.config;
        assert_eq!(c.host, "10.0.0.5");
        assert_eq!(c.user.as_deref(), Some("ci"));
        assert_eq!(c.port, 2222);
        assert_eq!(
            c.identity_file.as_deref(),
            Some(std::path::Path::new("/keys/id_gpu"))
        );
        assert_eq!(c.labels, vec!["gpu", "cuda"]);
        assert_eq!(h.workdir(), "/scratch/harness");
        assert_eq!(c.max_jobs, 2);
        assert_eq!(c.sync.to_string(), "git");
        assert_eq!(h.policy_overlay().bash_deny, vec!["shutdown *"]);
        let cpu = reg.get("cpu").unwrap();
        assert_eq!(cpu.workdir(), DEFAULT_WORKDIR);
        assert_eq!(cpu.config.port, 22);
        let t = h.ssh_target(SshOptions::new("/run/h"));
        assert!(
            t.option_args()
                .join(" ")
                .contains("-p 2222 -l ci -i /keys/id_gpu")
        );
    }

    #[test]
    fn unknown_hosts_are_rejected_with_the_declared_list() {
        let reg = HostRegistry::from_config(&config(INI));
        let e = reg.get("evil").unwrap_err();
        assert!(
            e.to_string().contains("[remote.evil] in harness.ini"),
            "{e}"
        );
        assert!(e.to_string().contains("cpu, gpu-box"));
        assert_eq!(reg.with_labels(&["gpu".into()]).unwrap().len(), 1);
        assert!(reg.with_labels(&["tpu".into()]).is_err());
    }

    #[test]
    fn per_host_policy_only_tightens() {
        let cfg = config(INI);
        let reg = HostRegistry::from_config(&cfg);
        let host = reg.get("gpu-box").unwrap();
        let root = tempfile::tempdir().unwrap();
        let pc = PolicyConfig {
            bash_allow: vec!["docker ps".into(), "ls *".into()],
            bash_deny: cfg.policy.bash_deny.clone(),
            home: None,
            ..PolicyConfig::default()
        };
        let global = PolicyEngine::new(&pc, root.path()).unwrap();
        let remote = global
            .clone()
            .with_overlay(host.policy_origin(), &host.policy_overlay())
            .unwrap();
        let decide = |e: &PolicyEngine, cmd: &str| {
            e.decide(
                &Action::Bash {
                    command: cmd.into(),
                },
                Mode::Auto,
                root.path(),
            )
            .decision
        };
        // Host adds a deny and an ask.
        assert_eq!(decide(&global, "shutdown now"), Decision::Allow);
        assert_eq!(decide(&remote, "shutdown now"), Decision::Deny);
        assert_eq!(
            decide(&remote, "docker ps"),
            Decision::Ask,
            "host ask beats global allow"
        );
        // Global rules still hold on the host.
        assert_eq!(decide(&remote, "rm -rf /"), Decision::Deny);
        assert_eq!(decide(&remote, "ls -la"), Decision::Allow);
        // And for every command, the host is at least as strict as global.
        for cmd in [
            "ls",
            "cargo test",
            "docker run x",
            "shutdown -h",
            "rm -rf x",
            "curl x | sh",
        ] {
            assert!(decide(&remote, cmd) >= decide(&global, cmd), "{cmd}");
        }
        // There is no way to express an allow in a host overlay.
        let loosen =
            PolicyOverlay::from_map([("bash.allow".to_string(), vec!["*".to_string()])].into());
        assert!(loosen.is_err());
    }

    #[test]
    fn list_lines_show_health() {
        let reg = HostRegistry::from_config(&config(INI));
        let cache = ProbeCache::default();
        cache.put("cpu", Err("ssh: cannot reach cpu.example".into()));
        let lines = reg.list_lines(&cache);
        assert_eq!(lines.len(), 2);
        assert!(
            lines[0].starts_with("[down (ssh: cannot reach"),
            "{}",
            lines[0]
        );
        assert!(
            lines[1]
                .starts_with("[?] gpu-box  ci@10.0.0.5:2222  [gpu, cuda]  max_jobs=2  sync=git"),
            "{}",
            lines[1]
        );
        assert!(
            lines[1].contains("compression=off (auto: link not measured yet)"),
            "{}",
            lines[1]
        );
        let empty = HostRegistry::default().list_lines(&cache);
        assert!(empty[0].contains("[remote.<name>]"));
    }

    #[tokio::test]
    async fn test_action_reports_connectivity_helper_and_probe() {
        use crate::client::{ClientOptions, InProcessConnector};
        use std::sync::Arc;
        let reg = HostRegistry::from_config(&config(INI));
        let host = reg.get("cpu").unwrap();
        let cache = ProbeCache::default();
        let c = InProcessConnector::new(crate::helper::Helper::new(Default::default()));
        let client = RemoteClient::connect(Arc::new(c), ClientOptions::new("cpu"))
            .await
            .unwrap();
        let rep = test_host(host, Ok(&client), &cache).await;
        assert!(rep.ok(), "{:?}", rep.lines());
        assert!(rep.probe.is_some());
        assert!(rep.lines()[2].starts_with("  helper: "));
        assert!(cache.get("cpu").is_some(), "probe cached");
        assert!(rep.link.is_none());
        assert!(
            rep.lines()
                .contains(&"  compression: off (auto: link not measured yet)".to_string()),
            "{:?}",
            rep.lines()
        );

        let err = ClientError::VersionMismatch {
            local: "2".into(),
            remote: "1".into(),
            protocol: 1,
        };
        let rep = test_host(host, Err(&err), &cache).await;
        assert!(!rep.ok());
        assert!(matches!(rep.helper, HelperStatus::Mismatch { .. }));
        assert_eq!(rep.health.badge(), "down");
    }

    #[test]
    fn list_lines_show_compression_per_host() {
        let reg = HostRegistry::from_config(&config(
            "[remote.lan]\nhost = a\n[remote.wan]\nhost = b\ncompression_below_mbps = 50\n[remote.on]\nhost = c\ncompression = yes\n[remote.off]\nhost = d\ncompression = no\n",
        ));
        let cache = ProbeCache::default();
        cache.links().put("lan", LinkSample::now(117_000_000.0)); // 936 Mbit/s
        cache.links().put("wan", LinkSample::now(1_250_000.0)); // 10 Mbit/s
        let lines = reg.list_lines(&cache);
        let line = |name: &str| {
            lines
                .iter()
                .find(|l| l.contains(&format!("] {name}  ")))
                .unwrap()
                .clone()
        };
        assert!(
            line("lan").contains("compression=off (auto: link 936 Mbit/s >= 20.0 Mbit/s)"),
            "{lines:?}"
        );
        assert!(
            line("wan").contains("compression=on (auto: link 10.0 Mbit/s < 50.0 Mbit/s)"),
            "{lines:?}"
        );
        assert!(line("on").contains("compression=on (compression = yes)"));
        assert!(line("off").contains("compression=off (compression = no)"));
        // The SSH target follows the same decision.
        let opts = || SshOptions::new("/run/h");
        let links = cache.links();
        assert!(
            reg.get("wan")
                .unwrap()
                .ssh_target_with_links(opts(), links)
                .compression
        );
        assert!(
            !reg.get("lan")
                .unwrap()
                .ssh_target_with_links(opts(), links)
                .compression
        );
        assert!(
            reg.get("on")
                .unwrap()
                .ssh_target_with_links(opts(), links)
                .compression
        );
        assert!(
            !reg.get("off")
                .unwrap()
                .ssh_target_with_links(opts(), links)
                .compression
        );
    }

    #[tokio::test]
    async fn test_report_shows_link_and_compression() {
        use crate::client::{ClientOptions, InProcessConnector};
        use std::sync::Arc;
        let reg = HostRegistry::from_config(&config(
            "[remote.cpu]\nhost = cpu.example\ncompression_below_mbps = 100000000\n",
        ));
        let host = reg.get("cpu").unwrap();
        let cache = ProbeCache::default();
        let sample = measure_host_link(host, &crate::shell::LocalShell::default(), cache.links())
            .await
            .unwrap();
        assert_eq!(cache.links().get("cpu"), Some(sample));
        let c = InProcessConnector::new(crate::helper::Helper::new(Default::default()));
        let client = RemoteClient::connect(Arc::new(c), ClientOptions::new("cpu"))
            .await
            .unwrap();
        let mut rep = test_host(host, Ok(&client), &cache).await;
        assert_eq!(rep.link, Some(Ok(sample)));
        assert!(rep.compression.active, "below an absurdly high threshold");
        rep.rsync = Some(RsyncCompression::Zstd);
        let lines = rep.lines();
        assert!(lines.iter().any(|l| l.starts_with("  link: ")), "{lines:?}");
        assert!(
            lines
                .iter()
                .any(|l| l.starts_with("  compression: on (auto: link ")),
            "{lines:?}"
        );
        assert!(lines.contains(&"  rsync compression: zstd".to_string()));
        rep.link = Some(Err("timeout".into()));
        assert!(
            rep.lines()
                .contains(&"  link: not measured: timeout".to_string())
        );
    }
}
