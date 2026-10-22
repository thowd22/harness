//! One node (workflow step or agent) running its tools on a remote host.
//!
//! [`RemoteNode::open`] connects to the host through a [`HostConnect`]
//! (production: [`SshHosts`], over OpenSSH with on-demand helper install),
//! records every remote operation and connection change on the node's run
//! log (`remote_op` / `remote_connection`, both tagged with the host),
//! and pushes the local worktree to a fresh run directory. The node's
//! tools then use [`RemoteNode::workspace`] and [`RemoteNode::exec`].
//! [`RemoteNode::finish`] pulls the changes back, applies them locally
//! through the [`ChangeGate`]s (all or nothing), cleans up and closes the
//! connection.
//!
//! Only file and process operations cross the wire: the model loop,
//! credentials, policy and the event log stay local.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;
use harness_events::schema::{EventKind, NodeId};
use harness_events::store::{EventDraft, RunRecorder};
use harness_tools::workspace::Workspace;

use crate::audit::{AuditSink, RecorderAudit, record_connection_on};
use crate::client::{ClientOptions, Connector, RemoteClient, SshConnector};
use crate::hosts::HostRegistry;
use crate::install::{HelperBinaries, connect_with_upgrade};
use crate::protocol::HELPER_VERSION;
use crate::shell::SshShell;
use crate::ssh::SshOptions;
use crate::sync::{
    ApplyOptions, ApplyReport, ChangeGate, ChangeSet, CleanupPolicy, Pusher, RemoteRun, SyncError,
};
use crate::workspace::{RemoteEnv, RemoteExec, RemoteWorkspace};

/// A connected host, ready for a node.
pub struct HostSession {
    /// The connection (its audit sink is the one passed to
    /// [`HostConnect::connect`]).
    pub client: RemoteClient,
    /// `[remote.<name>] workdir` (`None`: the default).
    pub workdir: Option<String>,
    /// How the worktree is copied there.
    pub pusher: Pusher,
    /// What happens to the run directory afterwards.
    pub cleanup: CleanupPolicy,
}

impl std::fmt::Debug for HostSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HostSession")
            .field("host", &self.client.host())
            .field("workdir", &self.workdir)
            .field("pusher", &self.pusher)
            .finish_non_exhaustive()
    }
}

/// Opens connections to declared hosts.
#[async_trait]
pub trait HostConnect: Send + Sync {
    /// Connect to `host`, sending one audit event per operation to `audit`.
    async fn connect(&self, host: &str, audit: Arc<dyn AuditSink>) -> Result<HostSession, String>;
}

/// [`HostConnect`] over OpenSSH for the `[remote.*]` hosts of harness.ini:
/// installs or upgrades the helper when needed, guards the resolved
/// credentials and syncs with the host's `sync` method.
///
/// SSH compression follows each host's `compression` setting. With
/// `auto`, the host's last link measurement (younger than
/// [`crate::link::LINK_MAX_AGE`]) decides; without one, the link is
/// measured over an uncompressed connection first and the result stored.
/// The decision is audited as op `link.compression`.
pub struct SshHosts {
    registry: HostRegistry,
    bins: HelperBinaries,
    secrets: Vec<String>,
    links: Arc<crate::link::LinkStore>,
}

impl std::fmt::Debug for SshHosts {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SshHosts")
            .field("hosts", &self.registry.names())
            .finish_non_exhaustive()
    }
}

impl SshHosts {
    /// Hosts of `registry`, helper builds from `bins`; `secrets` (the
    /// resolved credentials) are never sent.
    pub fn new(registry: HostRegistry, bins: HelperBinaries, secrets: Vec<String>) -> Self {
        Self {
            registry,
            bins,
            secrets,
            links: Default::default(),
        }
    }

    /// Use `links` for link measurements (default: in memory only).
    pub fn with_links(mut self, links: Arc<crate::link::LinkStore>) -> Self {
        self.links = links;
        self
    }

    /// The compression decision for `h`, measuring the link first when
    /// `compression = auto` has no fresh measurement.
    async fn compression(
        &self,
        h: &crate::hosts::RemoteHost,
        audit: &dyn AuditSink,
    ) -> crate::link::CompressionDecision {
        use harness_config::ini::config::SshCompression;
        if h.config.compression != SshCompression::Auto {
            return h.compression(None);
        }
        if let Some(l) = self.links.fresh(&h.name, crate::link::LINK_MAX_AGE) {
            return h.compression(Some(l));
        }
        let shell = SshShell {
            target: h.ssh_target(SshOptions::default()).with_compression(false),
        };
        let at = std::time::SystemTime::now();
        let t0 = Instant::now();
        let measured = crate::hosts::measure_host_link(h, &shell, &self.links).await;
        audit.record(&crate::audit::AuditEvent {
            host: h.name.clone(),
            op: "link.measure".into(),
            target: measured.as_ref().ok().map(|l| l.describe()),
            ok: measured.is_ok(),
            error: measured.as_ref().err().cloned(),
            duration: t0.elapsed(),
            at,
            bytes_saved: 0,
        });
        h.compression(measured.ok())
    }
}

#[async_trait]
impl HostConnect for SshHosts {
    async fn connect(&self, host: &str, audit: Arc<dyn AuditSink>) -> Result<HostSession, String> {
        let h = self.registry.get(host).map_err(|e| e.to_string())?;
        let decision = self.compression(h, audit.as_ref()).await;
        audit.record(&crate::audit::AuditEvent {
            host: h.name.clone(),
            op: "link.compression".into(),
            target: Some(decision.describe()),
            ok: true,
            error: None,
            duration: std::time::Duration::ZERO,
            at: std::time::SystemTime::now(),
            bytes_saved: 0,
        });
        let target = h
            .ssh_target(SshOptions::default())
            .with_compression(decision.active);
        let shell = SshShell {
            target: target.clone(),
        };
        let opts = ClientOptions::new(h.name.clone())
            .with_secrets(self.secrets.iter().cloned())
            .with_audit(audit);
        let (client, _) = connect_with_upgrade(
            &shell,
            &self.bins,
            HELPER_VERSION,
            true,
            |installed| {
                Arc::new(SshConnector::new(target.clone(), installed.path.clone()))
                    as Arc<dyn Connector>
            },
            opts,
        )
        .await
        .map_err(|e| format!("cannot connect to remote host {host}: {e}"))?;
        let pusher = match h.config.sync {
            harness_config::ini::config::SyncMethod::Git => Pusher::GitBundle(Arc::new(shell)),
            _ => Pusher::Rsync(Some(target)),
        };
        Ok(HostSession {
            client,
            workdir: h.config.workdir.clone(),
            pusher,
            cleanup: h.cleanup(),
        })
    }
}

/// Makes `s` safe as one path component.
fn component(s: &str) -> String {
    let c: String = s
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') {
                c
            } else {
                '-'
            }
        })
        .collect();
    c.trim_matches(|c| c == '.' || c == '-').to_owned()
}

/// What came back from a node.
#[derive(Debug, Clone, Default)]
pub struct NodeReturn {
    /// Every file the node added, changed or deleted on the host.
    pub changes: ChangeSet,
    /// What was applied locally (empty when the node failed).
    pub report: ApplyReport,
    /// Whether the remote run directory was removed.
    pub removed: bool,
}

impl NodeReturn {
    /// True when the changes (if any) were applied.
    pub fn applied(&self) -> bool {
        self.report.is_applied()
    }
}

/// A node's synced workspace on a remote host.
pub struct RemoteNode {
    host: String,
    client: RemoteClient,
    run: RemoteRun,
    artifacts: String,
    cleanup: CleanupPolicy,
    recorder: RunRecorder,
    node: NodeId,
    connection: tokio::task::JoinHandle<()>,
}

impl std::fmt::Debug for RemoteNode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RemoteNode")
            .field("host", &self.host)
            .field("root", &self.run.remote_dir())
            .field("node", &self.node)
            .finish_non_exhaustive()
    }
}

impl RemoteNode {
    /// Connect to `host` for `node`, record its remote operations on
    /// `recorder`, and push `local_root` to
    /// `<workdir>/<project-key>/<run-id>-<node>-a<attempt>`. Each attempt
    /// gets its own run (and artifact) directory, so a retry starts from a
    /// fresh copy and the failed attempt's directory stays for inspection.
    pub async fn open(
        hosts: &dyn HostConnect,
        host: &str,
        local_root: &Path,
        recorder: &RunRecorder,
        node: &NodeId,
        attempt: u32,
    ) -> Result<RemoteNode, String> {
        let emit_state = |state: &str, detail: Option<String>| {
            let _ = recorder.emit(EventDraft::node(
                node.clone(),
                EventKind::RemoteConnection {
                    host: host.to_owned(),
                    state: state.to_owned(),
                    detail,
                },
            ));
        };
        emit_state("connecting", None);
        let audit: Arc<dyn AuditSink> =
            Arc::new(RecorderAudit::new(recorder.clone(), Some(node.clone())));
        let started = Instant::now();
        let session = match hosts.connect(host, audit).await {
            Ok(s) => s,
            Err(e) => {
                emit_state("failed", Some(e.clone()));
                return Err(e);
            }
        };
        emit_state(
            "connected",
            Some(format!("{} ms", started.elapsed().as_millis())),
        );
        let connection =
            record_connection_on(&session.client, recorder.clone(), Some(node.clone()));
        let run_id = format!(
            "{}-{}-a{attempt}",
            recorder.run_id(),
            component(node.as_str()).to_lowercase()
        );
        let run = match RemoteRun::push(
            &session.client,
            local_root,
            session.workdir.as_deref(),
            &run_id,
            &session.pusher,
        )
        .await
        {
            Ok(r) => r,
            Err(e) => {
                session.client.close();
                connection.abort();
                return Err(format!("cannot sync the worktree to {host}: {e}"));
            }
        };
        let artifacts = format!("{}/{run_id}.artifacts", run.project_dir());
        // Creates the directory (writes create their parents).
        let ws = RemoteWorkspace::new(session.client.clone(), &artifacts);
        if let Err(e) = ws.write(Path::new(".keep"), b"").await {
            session.client.close();
            connection.abort();
            return Err(format!(
                "cannot prepare the artifact directory on {host}: {e}"
            ));
        }
        Ok(RemoteNode {
            host: host.to_owned(),
            client: session.client,
            run,
            artifacts,
            cleanup: session.cleanup,
            recorder: recorder.clone(),
            node: node.clone(),
            connection,
        })
    }

    /// `[remote.<name>]` name.
    pub fn host(&self) -> &str {
        &self.host
    }

    /// The connection.
    pub fn client(&self) -> &RemoteClient {
        &self.client
    }

    /// The node's workspace root on the host (the pushed worktree).
    pub fn root(&self) -> PathBuf {
        PathBuf::from(self.run.remote_dir())
    }

    /// A directory on the host for the node's artifacts, outside the
    /// worktree (so they are not pulled back as changes).
    pub fn artifacts_dir(&self) -> PathBuf {
        PathBuf::from(&self.artifacts)
    }

    /// The worktree as a [`Workspace`].
    pub fn workspace(&self) -> RemoteWorkspace {
        RemoteWorkspace::new(self.client.clone(), self.run.remote_dir())
    }

    /// Processes on the host, with `env` filtering their environment.
    pub fn exec(&self, env: RemoteEnv) -> RemoteExec {
        RemoteExec::new(self.client.clone(), env)
    }

    /// Reads `name` from [`RemoteNode::artifacts_dir`], if the node wrote it.
    pub async fn artifact(&self, name: &str) -> Option<Vec<u8>> {
        RemoteWorkspace::new(self.client.clone(), &self.artifacts)
            .read(Path::new(name))
            .await
            .ok()
    }

    /// Finish the node: when it `succeeded`, pull its changes and apply
    /// them locally through `gates` (all or nothing); then clean up the
    /// run directory per the host's policy (kept when the node failed or
    /// its changes were not applied) and close the connection.
    pub async fn finish(
        self,
        succeeded: bool,
        gates: &[&dyn ChangeGate],
        opts: ApplyOptions,
    ) -> Result<NodeReturn, String> {
        let mut ret = NodeReturn::default();
        let result = async {
            if succeeded {
                ret.changes = self.run.pull().await.map_err(|e| self.sync_err(e))?;
                let started = Instant::now();
                let applied = self.run.apply(&ret.changes, gates, opts);
                let error = match &applied {
                    Ok(r) if r.is_applied() => None,
                    Ok(r) => Some(r.problems().join("; ")),
                    Err(e) => Some(e.to_string()),
                };
                self.client
                    .record_audit("sync.apply", Some(self.run.remote_dir()), started, error);
                ret.report = applied.map_err(|e| format!("cannot apply the changes: {e}"))?;
            }
            Ok::<(), String>(())
        }
        .await;
        let ok = succeeded && result.is_ok() && ret.applied();
        match self.run.cleanup(self.cleanup, ok).await {
            Ok(removed) => {
                ret.removed = removed;
                if removed {
                    let _ = self
                        .client
                        .remove_run_dir(&self.artifacts, self.run.project_dir())
                        .await;
                }
            }
            Err(e) => {
                tracing::warn!(target: "harness::remote", host = %self.host, error = %e, "remote cleanup failed")
            }
        }
        self.client.close();
        let _ = self.recorder.emit(EventDraft::node(
            self.node.clone(),
            EventKind::RemoteConnection {
                host: self.host.clone(),
                state: "disconnected".into(),
                detail: Some("node finished".into()),
            },
        ));
        self.connection.abort();
        result.map(|()| ret)
    }

    fn sync_err(&self, e: SyncError) -> String {
        format!("cannot pull the changes from {}: {e}", self.host)
    }
}
