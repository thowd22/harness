//! Steps placed on a remote host (`runs_on`) run their tools there.
//!
//! The executor places a step ([`crate::placement::HostPool`]) and passes
//! the host in [`StepRequest::host`]. A [`RemoteStepRunner`] wraps a step
//! type's local runner: without a host it simply delegates; with one it
//!
//! 1. opens a [`RemoteNode`] (connect, record `remote_op` and
//!    `remote_connection` events on the step node tagged with the host,
//!    push the worktree to a fresh run directory per attempt, so a retry
//!    never sees what a failed attempt left behind),
//! 2. runs the step with a runner built for that [`RemotePlace`]: shell and
//!    verify commands run through [`RemoteExec`](harness_remote::workspace::RemoteExec),
//!    agent steps get read/write/edit/bash over the remote workspace while
//!    the model loop, credentials, policy and event log stay local,
//! 3. reads declared shell artifacts back from the host,
//! 4. pulls the changes and applies them locally through the sync gates
//!    (policy and hygiene, all or nothing), then cleans up.
//!
//! Changes that cannot be applied fail the step; the change set is kept as
//! `remote-changes.patch` in the step's artifact directory and the remote
//! run directory is kept for inspection.

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use harness_agent::dispatch::{StandardTools, ToolSet};
use harness_agent::subagents::{ChildHost, GateFactory};
use harness_events::store::RunRecorder;
use harness_remote::node::{HostConnect, RemoteNode};
use harness_remote::sync::{ApplyOptions, ChangeGate, HygieneGate};
use harness_remote::workspace::RemoteEnv;
use harness_tools::bash::EnvScrubber;
use harness_tools::config::ToolsConfig;
use harness_tools::exec::Exec;
use harness_tools::spill::BlobSink;
use harness_tools::workspace::Workspace;

use crate::executor::{StepFailure, StepOutput, StepRequest, StepRunner};
use crate::runners::{AgentStepRunner, ShellStepRunner, VerifyStepRunner};

/// File name of the unapplied change set in the step's artifact directory.
pub const PATCH_FILE: &str = "remote-changes.patch";

/// Where a placed step runs.
#[derive(Clone)]
pub struct RemotePlace {
    /// `[remote.<name>]` name.
    pub host: String,
    /// The pushed worktree on the host.
    pub root: PathBuf,
    /// That worktree.
    pub workspace: Arc<dyn Workspace>,
    /// Processes on the host.
    pub exec: Arc<dyn Exec>,
    /// Directory on the host for shell artifacts (`$ARTIFACTS_DIR`).
    pub artifacts_dir: PathBuf,
}

impl std::fmt::Debug for RemotePlace {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RemotePlace")
            .field("host", &self.host)
            .field("root", &self.root)
            .finish_non_exhaustive()
    }
}

/// Builds a step type's runner for a place.
pub type PlacedRunner = Arc<dyn Fn(&RemotePlace) -> Arc<dyn StepRunner> + Send + Sync>;

/// The gates every returning change passes before it is applied locally.
pub type SyncGates =
    Arc<dyn Fn(&StepRequest) -> Result<Vec<Box<dyn ChangeGate>>, String> + Send + Sync>;

/// The tool gate of agents on a host (`None`: the run's gate).
pub type HostGates = Arc<dyn Fn(&str) -> Option<GateFactory> + Send + Sync>;

/// What every remote step shares: how to reach hosts, the run log, the
/// local worktree, the environment filter and the sync gates.
#[derive(Clone)]
pub struct RemoteSteps {
    hosts: Arc<dyn HostConnect>,
    recorder: RunRecorder,
    local_root: PathBuf,
    env: RemoteEnv,
    gates: Option<SyncGates>,
    host_gates: Option<HostGates>,
}

impl std::fmt::Debug for RemoteSteps {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RemoteSteps")
            .field("local_root", &self.local_root)
            .finish_non_exhaustive()
    }
}

impl RemoteSteps {
    /// Steps reach hosts through `hosts`, record to `recorder` and sync
    /// `local_root`. The environment filter defaults to
    /// [`RemoteEnv::default`] and the sync gate to the hygiene check with
    /// the step's scope.
    pub fn new(
        hosts: Arc<dyn HostConnect>,
        recorder: RunRecorder,
        local_root: impl Into<PathBuf>,
    ) -> Self {
        Self {
            hosts,
            recorder,
            local_root: local_root.into(),
            env: RemoteEnv::default(),
            gates: None,
            host_gates: None,
        }
    }

    /// Filters remote process environments with `env` (build it from the
    /// bash tool's scrubber).
    pub fn with_env(mut self, env: RemoteEnv) -> Self {
        self.env = env;
        self
    }

    /// Checks returning changes with `gates` (e.g. the local policy plus
    /// hygiene with the step's scope).
    pub fn with_gates(mut self, gates: SyncGates) -> Self {
        self.gates = Some(gates);
        self
    }

    /// Gives agents on a host the gate `host_gates(host)` (e.g. the policy
    /// with the host's tighten-only `policy.*` overlay).
    pub fn with_host_gates(mut self, host_gates: HostGates) -> Self {
        self.host_gates = Some(host_gates);
        self
    }

    /// `local` for local steps; `placed(place)` for steps on a host.
    pub fn wrap(&self, local: Arc<dyn StepRunner>, placed: PlacedRunner) -> Arc<dyn StepRunner> {
        Arc::new(RemoteStepRunner {
            steps: self.clone(),
            local,
            placed,
        })
    }

    /// Shell steps: `local`, or `sh -c` on the host, recorded like local
    /// shell steps. On the host the command runs in the remote login
    /// environment plus `$ARTIFACTS_DIR` and `declared` (filtered by the
    /// [`RemoteEnv`]). `declared` is only what the step or workflow
    /// declares, never the local process environment, which must not
    /// reach the host.
    pub fn shell(
        &self,
        local: Arc<dyn StepRunner>,
        declared: Vec<(String, String)>,
    ) -> Arc<dyn StepRunner> {
        let recorder = self.recorder.clone();
        self.wrap(
            local,
            Arc::new(move |p: &RemotePlace| {
                Arc::new(
                    ShellStepRunner::new(p.exec.clone(), p.root.clone())
                        .with_env(declared.clone())
                        .with_recorder(recorder.clone()),
                ) as Arc<dyn StepRunner>
            }),
        )
    }

    /// Verify steps: `local`, or the command on the host in the remote
    /// login environment plus `declared` (as for [`RemoteSteps::shell`]).
    pub fn verify(
        &self,
        local: Arc<dyn StepRunner>,
        declared: Vec<(String, String)>,
    ) -> Arc<dyn StepRunner> {
        let recorder = self.recorder.clone();
        self.wrap(
            local,
            Arc::new(move |p: &RemotePlace| {
                Arc::new(
                    VerifyStepRunner::new(p.exec.clone(), p.root.clone(), recorder.clone())
                        .with_env(declared.clone()),
                ) as Arc<dyn StepRunner>
            }),
        )
    }

    /// Agent steps: `agent` locally, or the same agent with read, write,
    /// edit and bash on the host (`config`, `scrubber` and `spill` as for
    /// the local tools). Its other tools (memory, todo, `spawn_agent`)
    /// stay local, and subagents it spawns use the host too.
    pub fn agent(
        &self,
        agent: AgentStepRunner,
        config: ToolsConfig,
        scrubber: EnvScrubber,
        spill: Arc<dyn BlobSink>,
    ) -> Arc<dyn StepRunner> {
        let local = Arc::new(agent.clone()) as Arc<dyn StepRunner>;
        let host_gates = self.host_gates.clone();
        self.wrap(
            local,
            Arc::new(move |p: &RemotePlace| {
                let place = p.clone();
                let config = config.clone();
                let scrubber = scrubber.clone();
                let spill = spill.clone();
                let host = ChildHost {
                    name: p.host.clone(),
                    root: p.root.clone(),
                    tools: Arc::new(move |_agent, base: ToolSet| {
                        // bash on the host starts from an empty base: the
                        // local environment is never sent (the helper
                        // supplies the remote login `HOME`, `PATH`, ...).
                        ToolSet::standard_with_bash_env(
                            StandardTools {
                                workspace: place.workspace.clone(),
                                exec: place.exec.clone(),
                                cwd: place.root.clone(),
                                config: config.clone(),
                                scrubber: scrubber.clone(),
                                spill: spill.clone(),
                            },
                            Vec::new(),
                        )
                        .fill_from(&base)
                    }),
                    gate: host_gates.as_ref().and_then(|g| g(&p.host)),
                };
                Arc::new(agent.clone().on_host(host)) as Arc<dyn StepRunner>
            }),
        )
    }

    /// Step types that cannot run remotely yet (bug-fix proofs and dream
    /// steps): `local`, and an actionable failure when placed on a host.
    pub fn local_only(&self, kind: &str, local: Arc<dyn StepRunner>) -> Arc<dyn StepRunner> {
        Arc::new(LocalOnly {
            kind: kind.to_owned(),
            local,
        })
    }
}

/// Runs a step on its host (see the module docs).
pub struct RemoteStepRunner {
    steps: RemoteSteps,
    local: Arc<dyn StepRunner>,
    placed: PlacedRunner,
}

impl std::fmt::Debug for RemoteStepRunner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RemoteStepRunner").finish_non_exhaustive()
    }
}

impl RemoteStepRunner {
    fn gates(&self, req: &StepRequest) -> Result<Vec<Box<dyn ChangeGate>>, String> {
        match &self.steps.gates {
            Some(g) => g(req),
            None => HygieneGate::new(&req.scope)
                .map(|g| vec![Box::new(g) as Box<dyn ChangeGate>])
                .map_err(|e| format!("scope: {e}")),
        }
    }
}

#[async_trait]
impl StepRunner for RemoteStepRunner {
    async fn run(&self, req: StepRequest) -> Result<StepOutput, StepFailure> {
        let Some(host) = req.host.clone() else {
            return self.local.run(req).await;
        };
        let s = &self.steps;
        let opened = tokio::select! {
            r = RemoteNode::open(s.hosts.as_ref(), &host, &s.local_root, &s.recorder, &req.node, req.attempt) => r,
            _ = req.cancel.cancelled() => return Err(StepFailure::cancelled()),
        };
        let node = opened.map_err(StepFailure::new)?;
        let place = RemotePlace {
            host: host.clone(),
            root: node.root(),
            workspace: Arc::new(node.workspace()),
            exec: Arc::new(node.exec(s.env.clone())),
            artifacts_dir: node.artifacts_dir(),
        };
        let runner = (self.placed)(&place);
        let mut placed_req = req.clone();
        placed_req.artifacts_dir = place.artifacts_dir.clone();
        let mut result = runner.run(placed_req).await;
        if let Ok(out) = &mut result {
            for a in &req.artifacts {
                if !out.artifacts.contains_key(a)
                    && let Some(data) = node.artifact(a).await
                {
                    out.artifacts.insert(a.clone(), data);
                }
            }
        }
        let gates = match self.gates(&req) {
            Ok(g) => g,
            Err(e) => {
                let _ = node.finish(false, &[], ApplyOptions::default()).await;
                return Err(StepFailure::new(e));
            }
        };
        let refs: Vec<&dyn ChangeGate> = gates.iter().map(AsRef::as_ref).collect();
        // `ask` rules were settled when the tool calls ran (agent steps go
        // through the policy gate); denials still block the apply.
        let finished = node
            .finish(result.is_ok(), &refs, ApplyOptions { approved: true })
            .await;
        let out = match result {
            Ok(out) => out,
            Err(mut f) => {
                if !f.cancelled {
                    f.message
                        .push_str(&format!("\n(on {host}; its run directory is kept)"));
                }
                return Err(f);
            }
        };
        let fail = |message: String| StepFailure {
            message,
            cancelled: false,
            cost_usd: out.cost_usd,
            tokens: out.tokens,
        };
        match finished {
            Err(e) => Err(fail(e)),
            Ok(ret) if !ret.applied() => {
                let patch = req.artifacts_dir.join(PATCH_FILE);
                let saved = std::fs::create_dir_all(&req.artifacts_dir)
                    .and_then(|()| std::fs::write(&patch, ret.changes.to_patch(&s.local_root)));
                let kept = match saved {
                    Ok(()) => format!("the change set is in {}", patch.display()),
                    Err(e) => format!("the change set could not be saved: {e}"),
                };
                Err(fail(format!(
                    "the changes from {host} were not applied ({}); {kept}",
                    ret.report.problems().join("; ")
                )))
            }
            Ok(_) => Ok(out),
        }
    }
}

struct LocalOnly {
    kind: String,
    local: Arc<dyn StepRunner>,
}

#[async_trait]
impl StepRunner for LocalOnly {
    async fn run(&self, req: StepRequest) -> Result<StepOutput, StepFailure> {
        if let Some(host) = &req.host {
            return Err(StepFailure::new(format!(
                "`{}` steps cannot run on a remote host yet (placed on {host}); remove `runs_on` from step `{}`",
                self.kind, req.step
            )));
        }
        self.local.run(req).await
    }
}
