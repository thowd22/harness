//! Process sandboxing for bash (Landlock filesystem rules, network isolation).
//!
//! Policy decides what may run; the sandbox bounds the damage when policy is
//! wrong. [`SandboxedExec`] wraps any [`Exec`] and rewrites each request so
//! the command runs:
//!
//! 1. inside `bwrap --unshare-net` (or `unshare --net`) when
//!    `[sandbox] network = deny` and one of them works here, and
//! 2. under the sandbox launcher ([`launcher_main`]), which applies a
//!    Landlock ruleset that only allows writes beneath the workspace, the
//!    temp dirs and a few devices, then `exec`s the command.
//!
//! When the kernel lacks Landlock or no network tool works, commands still
//! run, but [`SandboxStatus::warnings`] says so (show it in the UI), every
//! sandboxed command prints a `harness sandbox:` warning on stderr, and
//! [`SandboxStatus::doctor_lines`] reports it for `harness doctor`.
//!
//! The launcher is a hidden mode of the main binary (`harness
//! __sandbox-exec ...`, see [`LAUNCHER_ARG`]) or the `harness-sandbox`
//! binary of this crate.

mod launcher;

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;

use async_trait::async_trait;
use harness_tools::exec::{Exec, ExecError, ExecHandle, ExecRequest};

pub use launcher::{PROBE_FLAG, WRITE_FLAG, launcher_main};

/// Hidden first argument that makes the main binary act as the launcher.
pub const LAUNCHER_ARG: &str = "__sandbox-exec";

/// Prefix of warnings the sandbox prints.
pub const WARNING_PREFIX: &str = "harness sandbox:";

/// Devices commands may always write to.
pub const WRITABLE_DEVICES: &[&str] = &[
    "/dev/null",
    "/dev/zero",
    "/dev/full",
    "/dev/tty",
    "/dev/ptmx",
    "/dev/pts",
    "/dev/shm",
];

/// Network policy (`[sandbox] network`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NetworkPolicy {
    /// Network allowed.
    #[default]
    Allow,
    /// Network denied (needs bwrap or unshare).
    Deny,
}

/// What to sandbox.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SandboxConfig {
    /// Apply the Landlock filesystem sandbox (`[sandbox] enabled`).
    pub enabled: bool,
    /// Network policy (`[sandbox] network`).
    pub network: NetworkPolicy,
    /// Directories (and files) commands may write beneath.
    pub writable: Vec<PathBuf>,
}

impl SandboxConfig {
    /// Writable: the workspace `root`, the temp dirs (`$TMPDIR`, `/tmp`,
    /// `/var/tmp`) and [`WRITABLE_DEVICES`].
    pub fn for_workspace(root: impl Into<PathBuf>, enabled: bool, network: NetworkPolicy) -> Self {
        let mut writable = vec![
            root.into(),
            std::env::temp_dir(),
            "/tmp".into(),
            "/var/tmp".into(),
        ];
        writable.extend(WRITABLE_DEVICES.iter().map(PathBuf::from));
        writable.dedup();
        Self {
            enabled,
            network,
            writable,
        }
    }

    /// [`SandboxConfig::for_workspace`] with `enabled` and `network` from
    /// harness.ini `[sandbox]`.
    pub fn from_ini(
        sandbox: &harness_config::ini::config::Sandbox,
        root: impl Into<PathBuf>,
    ) -> Self {
        use harness_config::ini::config::NetworkMode;
        let network = match sandbox.network {
            NetworkMode::Allow => NetworkPolicy::Allow,
            NetworkMode::Deny => NetworkPolicy::Deny,
        };
        Self::for_workspace(root, sandbox.enabled, network)
    }
}

/// How to start the launcher.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Launcher {
    /// Executable.
    pub program: PathBuf,
    /// Arguments before the launcher's own (e.g. [`LAUNCHER_ARG`]).
    pub prefix: Vec<String>,
}

impl Launcher {
    /// The running binary in launcher mode (`<current exe> __sandbox-exec`).
    /// The binary must dispatch [`LAUNCHER_ARG`] to [`launcher_main`].
    pub fn current_exe() -> std::io::Result<Self> {
        Ok(Self {
            program: std::env::current_exe()?,
            prefix: vec![LAUNCHER_ARG.into()],
        })
    }

    /// A standalone launcher binary such as `harness-sandbox`.
    pub fn binary(program: impl Into<PathBuf>) -> Self {
        Self {
            program: program.into(),
            prefix: Vec::new(),
        }
    }
}

/// Landlock support as measured by running the launcher's probe.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum LandlockState {
    /// All requested write restrictions are enforced.
    Full,
    /// Some restrictions are enforced (older Landlock ABI).
    Partial,
    /// Not enforced; `reason` says why.
    Unsupported {
        /// Why.
        reason: String,
    },
    /// `[sandbox] enabled = false`.
    Disabled,
}

/// Tool used for network isolation.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(tag = "tool", content = "path", rename_all = "snake_case")]
pub enum NetTool {
    /// Bubblewrap.
    Bwrap(PathBuf),
    /// util-linux `unshare`.
    Unshare(PathBuf),
}

impl NetTool {
    fn prefix(&self) -> Vec<String> {
        match self {
            NetTool::Bwrap(p) => [
                p.to_string_lossy().as_ref(),
                "--dev-bind",
                "/",
                "/",
                "--unshare-net",
                "--die-with-parent",
                "--",
            ]
            .iter()
            .map(|s| s.to_string())
            .collect(),
            NetTool::Unshare(p) => [
                p.to_string_lossy().as_ref(),
                "--net",
                "--map-root-user",
                "--",
            ]
            .iter()
            .map(|s| s.to_string())
            .collect(),
        }
    }
}

/// What the sandbox can do here.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct SandboxStatus {
    /// Filesystem sandbox.
    pub landlock: LandlockState,
    /// Requested network policy.
    pub network: NetworkPolicy,
    /// Network isolation tool in use, if network is denied and one works.
    pub net_tool: Option<NetTool>,
    /// Problems to show the user.
    pub warnings: Vec<String>,
}

impl SandboxStatus {
    /// Measure support for `config`: runs the launcher's Landlock probe and
    /// test-runs bwrap / unshare when the network is denied.
    pub fn probe(config: &SandboxConfig, launcher: &Launcher) -> Self {
        let landlock = if config.enabled {
            probe_landlock(launcher)
        } else {
            LandlockState::Disabled
        };
        let net_tool = match config.network {
            NetworkPolicy::Allow => None,
            NetworkPolicy::Deny => find_net_tool(),
        };
        let mut warnings = Vec::new();
        match &landlock {
            LandlockState::Unsupported { reason } => warnings.push(format!(
                "{WARNING_PREFIX} Landlock is unavailable ({reason}); bash can write outside the workspace."
            )),
            LandlockState::Partial => warnings.push(format!(
                "{WARNING_PREFIX} this kernel's Landlock ABI enforces only some write restrictions."
            )),
            LandlockState::Disabled => {
                warnings.push(format!("{WARNING_PREFIX} disabled by [sandbox] enabled = false."))
            }
            LandlockState::Full => {}
        }
        if config.network == NetworkPolicy::Deny && net_tool.is_none() {
            warnings.push(format!(
                "{WARNING_PREFIX} network = deny, but neither bwrap nor unshare works here; bash network access is NOT blocked."
            ));
        }
        Self {
            landlock,
            network: config.network,
            net_tool,
            warnings,
        }
    }

    /// Lines for `harness doctor`.
    pub fn doctor_lines(&self) -> Vec<String> {
        let fs = match &self.landlock {
            LandlockState::Full => "filesystem sandbox: landlock, fully enforced".to_string(),
            LandlockState::Partial => {
                "filesystem sandbox: landlock, partially enforced (old kernel ABI)".into()
            }
            LandlockState::Unsupported { reason } => {
                format!("filesystem sandbox: UNAVAILABLE ({reason})")
            }
            LandlockState::Disabled => "filesystem sandbox: disabled in harness.ini".into(),
        };
        let net = match (self.network, &self.net_tool) {
            (NetworkPolicy::Allow, _) => "network: allowed ([sandbox] network = allow)".to_string(),
            (NetworkPolicy::Deny, Some(NetTool::Bwrap(p))) => {
                format!("network: denied via bwrap ({})", p.display())
            }
            (NetworkPolicy::Deny, Some(NetTool::Unshare(p))) => {
                format!("network: denied via unshare ({})", p.display())
            }
            (NetworkPolicy::Deny, None) => {
                "network: deny requested but NOT enforced (install bubblewrap)".into()
            }
        };
        vec![fs, net]
    }
}

fn probe_landlock(launcher: &Launcher) -> LandlockState {
    let out = Command::new(&launcher.program)
        .args(&launcher.prefix)
        .arg(PROBE_FLAG)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output();
    let out = match out {
        Ok(o) => o,
        Err(e) => {
            return LandlockState::Unsupported {
                reason: format!(
                    "cannot run sandbox launcher {}: {e}",
                    launcher.program.display()
                ),
            };
        }
    };
    let text = String::from_utf8_lossy(&out.stdout);
    match text.trim() {
        "landlock=full" => LandlockState::Full,
        "landlock=partial" => LandlockState::Partial,
        other => LandlockState::Unsupported {
            reason: other
                .strip_prefix("landlock=none ")
                .unwrap_or("probe failed")
                .to_string(),
        },
    }
}

fn works(argv: &[String]) -> bool {
    Command::new(&argv[0])
        .args(&argv[1..])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// The first network-isolation tool that actually works here.
pub fn find_net_tool() -> Option<NetTool> {
    let candidates = [
        which::which("bwrap").ok().map(NetTool::Bwrap),
        which::which("unshare").ok().map(NetTool::Unshare),
    ];
    candidates.into_iter().flatten().find(|t| {
        let mut argv = t.prefix();
        argv.push("true".into());
        works(&argv)
    })
}

/// An [`Exec`] that runs every command inside the sandbox.
pub struct SandboxedExec {
    inner: Arc<dyn Exec>,
    launcher: Launcher,
    config: SandboxConfig,
    status: SandboxStatus,
}

impl std::fmt::Debug for SandboxedExec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SandboxedExec")
            .field("launcher", &self.launcher)
            .field("config", &self.config)
            .field("status", &self.status)
            .finish_non_exhaustive()
    }
}

impl SandboxedExec {
    /// Wrap `inner`, probing support once. Warnings are logged and kept in
    /// [`SandboxedExec::status`].
    pub fn new(inner: Arc<dyn Exec>, launcher: Launcher, config: SandboxConfig) -> Self {
        let status = SandboxStatus::probe(&config, &launcher);
        Self::with_status(inner, launcher, config, status)
    }

    /// Wrap `inner` with an already measured status.
    pub fn with_status(
        inner: Arc<dyn Exec>,
        launcher: Launcher,
        config: SandboxConfig,
        status: SandboxStatus,
    ) -> Self {
        for w in &status.warnings {
            tracing::warn!("{w}");
        }
        Self {
            inner,
            launcher,
            config,
            status,
        }
    }

    /// What the sandbox enforces here.
    pub fn status(&self) -> &SandboxStatus {
        &self.status
    }

    /// The rewritten request (exposed for tests and previews).
    pub fn wrap(&self, req: ExecRequest) -> ExecRequest {
        let mut argv: Vec<String> = Vec::new();
        if let Some(t) = &self.status.net_tool {
            argv.extend(t.prefix());
        }
        let fs = !matches!(self.status.landlock, LandlockState::Disabled) && self.config.enabled;
        if fs {
            argv.push(self.launcher.program.to_string_lossy().into_owned());
            argv.extend(self.launcher.prefix.iter().cloned());
            for w in &self.config.writable {
                argv.push(WRITE_FLAG.into());
                argv.push(absolute(w, &req.cwd).to_string_lossy().into_owned());
            }
            argv.push("--".into());
        }
        if argv.is_empty() {
            return req;
        }
        argv.push(req.program.clone());
        argv.extend(req.args.iter().cloned());
        let program = argv.remove(0);
        ExecRequest {
            program,
            args: argv,
            ..req
        }
    }
}

fn absolute(p: &Path, cwd: &Path) -> PathBuf {
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        cwd.join(p)
    }
}

#[async_trait]
impl Exec for SandboxedExec {
    async fn spawn(&self, req: ExecRequest) -> Result<ExecHandle, ExecError> {
        self.inner.spawn(self.wrap(req)).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Never;

    #[async_trait]
    impl Exec for Never {
        async fn spawn(&self, _: ExecRequest) -> Result<ExecHandle, ExecError> {
            unreachable!()
        }
    }

    fn status(landlock: LandlockState, net_tool: Option<NetTool>) -> SandboxStatus {
        SandboxStatus {
            landlock,
            network: if net_tool.is_some() {
                NetworkPolicy::Deny
            } else {
                NetworkPolicy::Allow
            },
            net_tool,
            warnings: vec![],
        }
    }

    fn exec(cfg: SandboxConfig, st: SandboxStatus) -> SandboxedExec {
        SandboxedExec::with_status(Arc::new(Never), Launcher::binary("/bin/launch"), cfg, st)
    }

    fn req() -> ExecRequest {
        ExecRequest::new("bash", vec!["-c".into(), "ls".into()], "/ws")
    }

    #[test]
    fn wraps_with_launcher_and_net_tool() {
        let cfg = SandboxConfig {
            enabled: true,
            network: NetworkPolicy::Deny,
            writable: vec!["/ws".into(), "rel".into()],
        };
        let e = exec(
            cfg.clone(),
            status(
                LandlockState::Full,
                Some(NetTool::Bwrap("/usr/bin/bwrap".into())),
            ),
        );
        let r = e.wrap(req());
        assert_eq!(r.program, "/usr/bin/bwrap");
        assert_eq!(
            r.args,
            [
                "--dev-bind",
                "/",
                "/",
                "--unshare-net",
                "--die-with-parent",
                "--",
                "/bin/launch",
                "--write",
                "/ws",
                "--write",
                "/ws/rel",
                "--",
                "bash",
                "-c",
                "ls"
            ]
        );
        assert_eq!(r.cwd, PathBuf::from("/ws"));
        let e = exec(
            SandboxConfig {
                enabled: false,
                ..cfg.clone()
            },
            status(LandlockState::Disabled, None),
        );
        assert_eq!(e.wrap(req()), req());
        let e = exec(
            cfg,
            status(
                LandlockState::Full,
                Some(NetTool::Unshare("/usr/bin/unshare".into())),
            ),
        );
        assert_eq!(e.wrap(req()).program, "/usr/bin/unshare");
    }

    #[test]
    fn from_ini_section() {
        use harness_config::ini::config::{NetworkMode, Sandbox};
        let c = SandboxConfig::from_ini(
            &Sandbox {
                enabled: false,
                network: NetworkMode::Deny,
            },
            "/ws",
        );
        assert!(!c.enabled);
        assert_eq!(c.network, NetworkPolicy::Deny);
        assert_eq!(c.writable[0], PathBuf::from("/ws"));
    }

    #[test]
    fn workspace_config_includes_temp_and_devices() {
        let c = SandboxConfig::for_workspace("/ws", true, NetworkPolicy::Allow);
        assert!(c.writable.contains(&PathBuf::from("/ws")));
        assert!(c.writable.contains(&PathBuf::from("/tmp")));
        assert!(c.writable.contains(&PathBuf::from("/dev/null")));
    }

    #[test]
    fn warnings_and_doctor_lines() {
        let launcher = Launcher::binary("/nonexistent/harness-sandbox");
        let cfg = SandboxConfig::for_workspace("/ws", true, NetworkPolicy::Allow);
        let s = SandboxStatus::probe(&cfg, &launcher);
        assert!(matches!(s.landlock, LandlockState::Unsupported { .. }));
        assert!(
            s.warnings[0].starts_with(WARNING_PREFIX),
            "{:?}",
            s.warnings
        );
        assert!(s.doctor_lines()[0].contains("UNAVAILABLE"));
        let off = SandboxStatus::probe(
            &SandboxConfig {
                enabled: false,
                ..cfg
            },
            &launcher,
        );
        assert_eq!(off.landlock, LandlockState::Disabled);
        assert!(off.doctor_lines()[0].contains("disabled"));
        assert!(off.doctor_lines()[1].contains("allowed"));
        let denied = SandboxStatus {
            landlock: LandlockState::Full,
            network: NetworkPolicy::Deny,
            net_tool: None,
            warnings: vec![],
        };
        assert!(denied.doctor_lines()[1].contains("NOT enforced"));
    }
}
