//! Connecting to `[remote.*]` hosts for `/remote test`.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use harness_remote::client::{ClientError, ClientOptions, Connector, SshConnector};
use harness_remote::hosts::{HostTestReport, RemoteHost, test_host};
use harness_remote::install::{HelperBinaries, connect_with_upgrade};
use harness_remote::link::{LinkStore, RsyncCompression};
use harness_remote::probe::ProbeCache;
use harness_remote::protocol::HELPER_VERSION;
use harness_remote::shell::SshShell;
use harness_remote::ssh::SshOptions;

/// How long a connection test may take.
pub const TEST_TIMEOUT: Duration = Duration::from_secs(20);

/// A probe cache whose link measurements persist in the data directory
/// (`remote-links.json`), shared by `/remote` and remote workflow steps.
pub fn probe_cache(data_root: &Path) -> ProbeCache {
    ProbeCache::default().with_links(Arc::new(LinkStore::in_data_dir(data_root)))
}

/// Connects to `host` (installing or upgrading the helper when needed) and
/// reports connectivity, helper status, health, link throughput, whether
/// SSH compression is active and the rsync compression.
pub async fn test(
    host: &RemoteHost,
    data_root: &Path,
    secrets: &[String],
    cache: &ProbeCache,
) -> HostTestReport {
    // The probe's throughput measurement decides `compression = auto`.
    let probe_shell = SshShell {
        target: host.ssh_target_with_links(SshOptions::default(), cache.links()),
    };
    let link = tokio::time::timeout(
        TEST_TIMEOUT,
        harness_remote::hosts::measure_host_link(host, &probe_shell, cache.links()),
    )
    .await
    .unwrap_or_else(|_| Err("timed out".into()));
    let target = host.ssh_target_with_links(SshOptions::default(), cache.links());
    let shell = SshShell {
        target: target.clone(),
    };
    let bins = HelperBinaries::from_env_or(data_root);
    let opts = ClientOptions::new(host.name.clone()).with_secrets(secrets.iter().cloned());
    let connect = connect_with_upgrade(
        &shell,
        &bins,
        HELPER_VERSION,
        true,
        |installed| {
            Arc::new(SshConnector::new(target.clone(), installed.path.clone()))
                as Arc<dyn Connector>
        },
        opts,
    );
    match tokio::time::timeout(TEST_TIMEOUT, connect).await {
        Ok(Ok((client, _))) => {
            let mut report = test_host(host, Ok(&client), cache).await;
            client.close();
            if let Err(e) = link {
                report.link = Some(Err(e));
            }
            if host.config.sync == harness_config::ini::config::SyncMethod::Rsync {
                let local = harness_remote::link::local_rsync_version().await;
                let remote = harness_remote::link::remote_rsync_version(&shell).await;
                report.rsync = Some(RsyncCompression::choose(&local, &remote));
            }
            report
        }
        Ok(Err(e)) => {
            let err = ClientError::Connect {
                host: host.name.clone(),
                message: e.to_string(),
            };
            test_host(host, Err(&err), cache).await
        }
        Err(_) => {
            let err = ClientError::Timeout(format!("connect to {}", host.name));
            test_host(host, Err(&err), cache).await
        }
    }
}

/// What workflow steps placed on `[remote.*]` hosts share: OpenSSH
/// connections (helper installed on demand, credentials guarded), the
/// remote environment filter, the sync gates (local policy plus hygiene
/// with the step's scope) and, for agents, the run's policy with each
/// host's tighten-only `policy.*` overlay.
pub struct StepWiring<'a> {
    /// harness.ini.
    pub cfg: &'a harness_config::ini::Config,
    /// Data directory root (helper builds).
    pub data_root: &'a Path,
    /// Project directory (synced to the hosts).
    pub cwd: &'a Path,
    /// Resolved credentials (never sent).
    pub secrets: &'a [String],
    /// The run log.
    pub recorder: &'a harness_events::store::RunRecorder,
    /// The bash tool's scrubber.
    pub scrubber: &'a harness_tools::bash::EnvScrubber,
    /// The run's approval broker.
    pub broker: Arc<harness_policy::approvals::ApprovalBroker>,
    /// The run's session grants.
    pub grants: Arc<harness_policy::approvals::SessionGrants>,
    /// Autonomy mode of agent steps.
    pub mode: harness_policy::rules::Mode,
}

/// The [`harness_workflow::remote::RemoteSteps`] for `w`.
pub fn steps(w: StepWiring<'_>) -> Result<harness_workflow::remote::RemoteSteps, String> {
    use harness_policy::rules::PolicyEngine;
    use harness_remote::sync::{ChangeGate, HygieneGate, PolicyGate as SyncPolicy};
    use harness_workflow::remote::{HostGates, RemoteSteps, SyncGates};

    let registry = harness_remote::hosts::HostRegistry::from_config(w.cfg);
    let policy = crate::core::policy_config(w.cfg, None);
    let gates = host_gates(&w, &registry)?;
    let host_gates: HostGates = Arc::new(move |h: &str| gates.get(h).cloned());
    let sync_gates: SyncGates = {
        let cwd = w.cwd.to_path_buf();
        let mode = w.mode;
        Arc::new(move |req| {
            let engine = PolicyEngine::new(&policy, &cwd).map_err(|e| format!("[policy]: {e}"))?;
            let hygiene = HygieneGate::new(&req.scope).map_err(|e| format!("scope: {e}"))?;
            Ok(vec![
                Box::new(SyncPolicy::new(engine, mode)) as Box<dyn ChangeGate>,
                Box::new(hygiene),
            ])
        })
    };
    let hosts = harness_remote::node::SshHosts::new(
        registry,
        HelperBinaries::from_env_or(w.data_root),
        w.secrets.to_vec(),
    )
    .with_links(Arc::new(LinkStore::in_data_dir(w.data_root)));
    Ok(RemoteSteps::new(Arc::new(hosts), w.recorder.clone(), w.cwd)
        .with_env(harness_remote::workspace::RemoteEnv::new(
            w.scrubber.clone(),
        ))
        .with_gates(sync_gates)
        .with_host_gates(host_gates))
}

/// Per host: the run's policy plus the host's tighten-only `policy.*`
/// overlay (sharing the run's approvals), capped at each agent's `mode:`.
fn host_gates(
    w: &StepWiring<'_>,
    registry: &harness_remote::hosts::HostRegistry,
) -> Result<std::collections::BTreeMap<String, harness_agent::subagents::GateFactory>, String> {
    let policy = crate::core::policy_config(w.cfg, None);
    let mut gates = std::collections::BTreeMap::new();
    for host in registry.iter() {
        let engine = harness_policy::rules::PolicyEngine::new(&policy, w.cwd)
            .map_err(|e| format!("[policy]: {e}"))?
            .with_grants(w.grants.clone())
            .with_overlay(host.policy_origin(), &host.policy_overlay())
            .map_err(|e| format!("{}: {e}", host.policy_origin()))?;
        let gate = Arc::new(
            harness_agent::dispatch::PolicyGate::new(engine, w.broker.clone(), w.mode)
                .with_recorder(w.recorder.clone()),
        );
        gates.insert(host.name.clone(), crate::core::agent_gates(gate));
    }
    Ok(gates)
}

#[cfg(test)]
mod tests {
    use super::*;
    use harness_agent::dispatch::{GateCall, GateDecision};
    use harness_config::discovery::Artifact;

    #[tokio::test]
    async fn agents_on_a_host_get_its_tighter_policy() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = harness_config::ini::load_str(
            "[remote.gpu-box]\nhost = 10.0.0.5\npolicy.bash.deny = make deploy*\n\n[remote.cpu-box]\nhost = 10.0.0.6\n",
            None,
        )
        .unwrap();
        let recorder = harness_events::store::RunRecorder::create(
            dir.path(),
            harness_events::schema::RunId::new(),
            Arc::new(harness_events::redact::Redactor::default()),
            harness_events::store::RecorderOptions::default(),
        )
        .await
        .unwrap();
        let grants = Arc::new(harness_policy::approvals::SessionGrants::default());
        let broker = Arc::new(harness_policy::approvals::ApprovalBroker::new(
            grants.clone(),
            Arc::new(crate::headless::HeadlessUi::new(
                crate::cli::Approvals::Deny,
            )),
            dir.path(),
        ));
        let scrubber = harness_tools::bash::EnvScrubber::new(Vec::new(), Vec::new());
        let w = StepWiring {
            cfg: &cfg,
            data_root: dir.path(),
            cwd: dir.path(),
            secrets: &[],
            recorder: &recorder,
            scrubber: &scrubber,
            broker,
            grants,
            mode: harness_policy::rules::Mode::Auto,
        };
        let registry = harness_remote::hosts::HostRegistry::from_config(&cfg);
        let gates = host_gates(&w, &registry).unwrap();
        let agent =
            harness_config::agents::Agent::load(&harness_config::discovery::SourceFile::inline(
                harness_config::discovery::Scope::Global,
                harness_config::discovery::Kind::Agent,
                "agents/x.yaml",
                "name: x\nprompt_text: x\n",
            ))
            .unwrap();
        let call = |cmd: &str| GateCall {
            call_id: "c".into(),
            tool: "bash".into(),
            args: serde_json::json!({"command": cmd}),
        };
        let gpu = gates["gpu-box"](&agent);
        let d = gpu.review(None, &[call("make deploy"), call("ls")]).await;
        assert!(
            matches!(&d[0], GateDecision::Deny { reason } if reason.contains("remote.gpu-box")),
            "{d:?}"
        );
        assert_eq!(d[1], GateDecision::Allow);
        let cpu = gates["cpu-box"](&agent);
        assert_eq!(
            cpu.review(None, &[call("make deploy")]).await,
            vec![GateDecision::Allow]
        );
        assert!(steps(w).is_ok());
    }
}
