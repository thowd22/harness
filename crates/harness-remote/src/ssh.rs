//! System OpenSSH transport with ControlMaster multiplexing.
//!
//! harness shells out to the system `ssh` binary instead of embedding a
//! Rust SSH client, so `~/.ssh/config`, `ssh-agent`, `ProxyJump`, hardware
//! keys and `known_hosts` behave exactly as they do for the user (the
//! decision is recorded in `docs/remote.md`).
//!
//! Every invocation built here:
//! - multiplexes over one master connection per host (`ControlMaster=auto`)
//!   whose socket lives in a private directory under `$XDG_RUNTIME_DIR/harness/`;
//! - requires a known host key (`StrictHostKeyChecking=yes`, `BatchMode=yes`),
//!   so an unknown host fails with [`SshError::UnknownHost`] instead of a
//!   trust-on-first-use prompt in the middle of an agent run;
//! - disables agent, X11 and port forwarding and sends no environment
//!   variables (`SendEnv=-*`);
//! - sends keepalives (`ServerAliveInterval`), so a dead network is noticed
//!   and reported as a disconnect (see [`crate::client`] for reconnecting).

use std::io;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use harness_config::ini::Remote;

/// Default `ControlPersist`: how long an idle master connection stays up.
pub const DEFAULT_CONTROL_PERSIST: Duration = Duration::from_secs(60);
/// Default `ServerAliveInterval`.
pub const DEFAULT_KEEPALIVE_INTERVAL: Duration = Duration::from_secs(15);
/// Default `ServerAliveCountMax`.
pub const DEFAULT_KEEPALIVE_COUNT: u32 = 3;
/// Default `ConnectTimeout`.
pub const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// `ControlPath` file name pattern (`%C` is a hash of host, port and user,
/// which keeps the socket path short).
pub const CONTROL_PATH_PATTERN: &str = "cm-%C";
/// `ControlPath` file name pattern for compressed connections. A mux
/// client cannot change the master's compression, so compressed and
/// uncompressed connections to the same host use separate masters.
pub const CONTROL_PATH_COMPRESSED_PATTERN: &str = "cmz-%C";

/// Settings shared by every SSH invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SshOptions {
    /// The `ssh` program (overridable for tests).
    pub program: String,
    /// Directory holding ControlMaster sockets (created with mode 0700).
    pub control_dir: PathBuf,
    /// `ControlPersist`.
    pub control_persist: Duration,
    /// `ServerAliveInterval`.
    pub keepalive_interval: Duration,
    /// `ServerAliveCountMax`.
    pub keepalive_count: u32,
    /// `ConnectTimeout`.
    pub connect_timeout: Duration,
}

impl Default for SshOptions {
    fn default() -> Self {
        Self::new(default_control_dir())
    }
}

impl SshOptions {
    /// Defaults with sockets in `control_dir`.
    pub fn new(control_dir: impl Into<PathBuf>) -> Self {
        Self {
            program: "ssh".into(),
            control_dir: control_dir.into(),
            control_persist: DEFAULT_CONTROL_PERSIST,
            keepalive_interval: DEFAULT_KEEPALIVE_INTERVAL,
            keepalive_count: DEFAULT_KEEPALIVE_COUNT,
            connect_timeout: DEFAULT_CONNECT_TIMEOUT,
        }
    }
}

/// Size of `sockaddr_un.sun_path` on this platform: 104 bytes on macOS and
/// the BSDs, 108 on Linux. A ControlPath must be shorter than this.
pub const SUN_PATH_MAX: usize = if cfg!(any(
    target_os = "macos",
    target_os = "ios",
    target_os = "freebsd",
    target_os = "openbsd",
    target_os = "netbsd",
    target_os = "dragonfly"
)) {
    104
} else {
    108
};

/// Longest socket path OpenSSH builds under a control dir: the longer
/// pattern (`cmz-`), `%C` expanded to 40 hex characters, and the
/// `.<16 hex>` suffix ssh appends while binding a new master socket.
const CONTROL_SOCKET_TAIL: usize = 1 + "cmz-".len() + 40 + 17;

/// True if sockets in `dir` stay under the `sun_path` limit `max`.
fn control_dir_fits(dir: &Path, max: usize) -> bool {
    dir.as_os_str().len() + CONTROL_SOCKET_TAIL < max
}

/// `$XDG_RUNTIME_DIR/harness`, else `$TMPDIR/harness-<user>`, else
/// `/tmp/harness-<user>`: the first one whose ControlMaster socket paths fit
/// in `sun_path`. macOS has no runtime dir and a long per-user `$TMPDIR`
/// (`/var/folders/…/T/`), so there the sockets live under `/tmp`.
pub fn default_control_dir() -> PathBuf {
    control_dir_with(|k| std::env::var_os(k))
}

/// [`default_control_dir`] with an injectable environment.
pub fn control_dir_with(env: impl Fn(&str) -> Option<std::ffi::OsString>) -> PathBuf {
    control_dir_for(env, SUN_PATH_MAX)
}

/// [`control_dir_with`] for a given `sun_path` size.
fn control_dir_for(env: impl Fn(&str) -> Option<std::ffi::OsString>, max: usize) -> PathBuf {
    let user = env("USER")
        .filter(|u| !u.is_empty())
        .map(|u| u.to_string_lossy().into_owned())
        .unwrap_or_else(|| "user".into());
    let leaf = format!("harness-{user}");
    let short = Path::new("/tmp").join(&leaf);
    let mut candidates = Vec::new();
    if let Some(dir) = env("XDG_RUNTIME_DIR").filter(|v| !v.is_empty()) {
        candidates.push(PathBuf::from(dir).join("harness"));
    }
    if let Some(dir) = env("TMPDIR").filter(|v| !v.is_empty()) {
        candidates.push(PathBuf::from(dir).join(&leaf));
    }
    candidates
        .into_iter()
        .find(|d| control_dir_fits(d, max))
        .unwrap_or(short)
}

/// Create `dir` (and parents) with mode 0700, and tighten an existing one.
///
/// The directory may sit in a shared `/tmp`, so this refuses a symlink and
/// a directory owned by someone else (only the owner can change its mode).
pub fn create_private_dir(dir: &Path) -> io::Result<()> {
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)?;
    if !std::fs::symlink_metadata(dir)?.is_dir() {
        return Err(io::Error::other(format!(
            "{} is not a directory (a symlink?); remove it and retry",
            dir.display()
        )));
    }
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
}

/// One SSH destination built from a `[remote.<name>]` section.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SshTarget {
    /// The `[remote.<name>]` name.
    pub name: String,
    /// Host name or address (may be a `~/.ssh/config` alias).
    pub host: String,
    /// Login user.
    pub user: Option<String>,
    /// Port.
    pub port: u16,
    /// Private key.
    pub identity_file: Option<PathBuf>,
    /// Shared options.
    pub options: SshOptions,
    /// SSH channel compression (`Compression=yes`, on its own master).
    pub compression: bool,
}

impl SshTarget {
    /// A target for host `name` configured by `remote`. Compression is on
    /// only for `compression = yes`; for `auto`, decide with
    /// [`crate::link::decide`] and set it with
    /// [`SshTarget::with_compression`].
    pub fn new(name: impl Into<String>, remote: &Remote, options: SshOptions) -> Self {
        Self {
            name: name.into(),
            host: remote.host.clone(),
            user: remote.user.clone(),
            port: remote.port,
            identity_file: remote.identity_file.clone(),
            options,
            compression: remote.compression == harness_config::ini::config::SshCompression::Yes,
        }
    }

    /// This target with SSH channel compression on or off.
    pub fn with_compression(mut self, on: bool) -> Self {
        self.compression = on;
        self
    }

    /// The `ControlPath` value (a separate socket when compressed).
    pub fn control_path(&self) -> PathBuf {
        let pattern = if self.compression {
            CONTROL_PATH_COMPRESSED_PATTERN
        } else {
            CONTROL_PATH_PATTERN
        };
        self.options.control_dir.join(pattern)
    }

    /// Every option argument, without the program name or destination.
    pub fn option_args(&self) -> Vec<String> {
        let o = &self.options;
        let mut args: Vec<String> = Vec::new();
        let mut opt = |kv: String| {
            args.push("-o".into());
            args.push(kv);
        };
        opt("BatchMode=yes".into());
        opt("StrictHostKeyChecking=yes".into());
        opt("ControlMaster=auto".into());
        opt(format!("ControlPath={}", self.control_path().display()));
        opt(format!(
            "ControlPersist={}",
            o.control_persist.as_secs().max(1)
        ));
        opt(format!(
            "ServerAliveInterval={}",
            o.keepalive_interval.as_secs().max(1)
        ));
        opt(format!("ServerAliveCountMax={}", o.keepalive_count.max(1)));
        opt(format!(
            "ConnectTimeout={}",
            o.connect_timeout.as_secs().max(1)
        ));
        opt("ForwardAgent=no".into());
        opt("ForwardX11=no".into());
        opt("ClearAllForwardings=yes".into());
        opt("PermitLocalCommand=no".into());
        opt("SendEnv=-*".into());
        opt(format!(
            "Compression={}",
            if self.compression { "yes" } else { "no" }
        ));
        opt("LogLevel=ERROR".into());
        if self.identity_file.is_some() {
            opt("IdentitiesOnly=yes".into());
        }
        // -a: no agent forwarding, -x: no X11, -T: no tty.
        args.extend(["-a".into(), "-x".into(), "-T".into()]);
        args.extend(["-p".into(), self.port.to_string()]);
        if let Some(user) = &self.user {
            args.extend(["-l".into(), user.clone()]);
        }
        if let Some(id) = &self.identity_file {
            args.extend(["-i".into(), id.display().to_string()]);
        }
        args
    }

    /// Full argument list to run `remote` (argv, quoted for the remote
    /// shell) on the host.
    pub fn command_args(&self, remote: &[String]) -> Vec<String> {
        let mut args = self.option_args();
        args.push("--".into());
        args.push(self.host.clone());
        if !remote.is_empty() {
            args.push(remote_command_line(remote));
        }
        args
    }

    /// A ready-to-spawn command running `remote` on the host, with piped
    /// stdio and `kill_on_drop`.
    pub fn command(&self, remote: &[String]) -> tokio::process::Command {
        let mut cmd = tokio::process::Command::new(&self.options.program);
        cmd.args(self.command_args(remote))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        cmd
    }

    /// Like [`SshTarget::command`], but with a remote command line that is
    /// already quoted for the remote shell.
    pub fn command_line(&self, line: &str) -> tokio::process::Command {
        let mut args = self.option_args();
        args.push("--".into());
        args.push(self.host.clone());
        args.push(line.to_owned());
        let mut cmd = tokio::process::Command::new(&self.options.program);
        cmd.args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        cmd
    }

    /// The value for `rsync -e`: the ssh program and options, quoted.
    pub fn rsync_shell(&self) -> String {
        std::iter::once(self.options.program.clone())
            .chain(self.option_args())
            .map(|a| shell_quote(&a))
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// Create the private ControlMaster socket directory.
    pub fn prepare(&self) -> io::Result<()> {
        create_private_dir(&self.options.control_dir)
    }

    /// True if a master connection is up (`ssh -O check`).
    pub async fn master_running(&self) -> bool {
        self.control(&["-O", "check"]).await.unwrap_or(false)
    }

    /// Close the master connection (`ssh -O exit`).
    pub async fn close_master(&self) -> io::Result<bool> {
        self.control(&["-O", "exit"]).await
    }

    async fn control(&self, op: &[&str]) -> io::Result<bool> {
        let mut args: Vec<String> = op.iter().map(|s| s.to_string()).collect();
        args.extend(self.option_args());
        args.push("--".into());
        args.push(self.host.clone());
        let status = tokio::process::Command::new(&self.options.program)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await?;
        Ok(status.success())
    }

    /// Run `remote` to completion with `stdin`, returning its output.
    ///
    /// ssh-level failures (exit 255) are classified into [`SshError`].
    pub async fn run(&self, remote: &[String], stdin: &[u8]) -> Result<RunOutput, SshError> {
        use tokio::io::AsyncWriteExt;
        self.prepare().map_err(|e| SshError::Spawn {
            program: self.options.program.clone(),
            source: e,
        })?;
        let mut child = self.command(remote).spawn().map_err(|e| SshError::Spawn {
            program: self.options.program.clone(),
            source: e,
        })?;
        if let Some(mut si) = child.stdin.take() {
            let data = stdin.to_vec();
            tokio::spawn(async move {
                let _ = si.write_all(&data).await;
            });
        }
        let out = child
            .wait_with_output()
            .await
            .map_err(|e| SshError::Spawn {
                program: self.options.program.clone(),
                source: e,
            })?;
        let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
        if out.status.code() == Some(255) {
            return Err(classify(&self.host, out.status.code(), &stderr));
        }
        Ok(RunOutput {
            code: out.status.code(),
            stdout: out.stdout,
            stderr,
        })
    }
}

/// Output of [`SshTarget::run`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunOutput {
    /// Remote exit code.
    pub code: Option<i32>,
    /// stdout bytes.
    pub stdout: Vec<u8>,
    /// stderr text.
    pub stderr: String,
}

/// Quote `s` for a POSIX shell (single quotes; safe words pass unchanged).
pub fn shell_quote(s: &str) -> String {
    let safe = !s.is_empty()
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"@%+=:,./_-".contains(&b));
    if safe {
        s.to_owned()
    } else {
        format!("'{}'", s.replace('\'', r"'\''"))
    }
}

/// Quote a remote path, keeping a leading `~/` expandable
/// (`"$HOME"/rest`).
pub fn quote_remote_path(p: &str) -> String {
    match p.strip_prefix("~/") {
        Some(rest) => format!("\"$HOME\"/{}", shell_quote(rest)),
        None => shell_quote(p),
    }
}

/// Join argv into one remote command line (ssh passes a single string to
/// the remote login shell).
pub fn remote_command_line(args: &[String]) -> String {
    args.iter()
        .map(|a| shell_quote(a))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Why an ssh invocation failed, with a message the user can act on.
#[derive(Debug, thiserror::Error)]
pub enum SshError {
    /// The host key is not in `known_hosts`.
    #[error(
        "ssh: the host key for {host} is not in known_hosts. harness never accepts unknown \
         host keys: verify the fingerprint, then add it (for example `ssh {host}` once, or \
         `ssh-keyscan {host} >> ~/.ssh/known_hosts` after checking it)"
    )]
    UnknownHost {
        /// Destination.
        host: String,
    },
    /// The host key changed (possible man-in-the-middle).
    #[error(
        "ssh: the host key for {host} has CHANGED. This can mean a man-in-the-middle attack. \
         harness will not connect; if the change is expected, remove the old key with \
         `ssh-keygen -R {host}` and verify the new one"
    )]
    HostKeyChanged {
        /// Destination.
        host: String,
    },
    /// Authentication failed.
    #[error(
        "ssh: authentication to {host} failed ({detail}); check the user, identity_file and ssh-agent"
    )]
    Auth {
        /// Destination.
        host: String,
        /// ssh's message.
        detail: String,
    },
    /// The host name did not resolve.
    #[error("ssh: could not resolve host {host}; check `host` in [remote.<name>] or ~/.ssh/config")]
    Resolve {
        /// Destination.
        host: String,
    },
    /// The host could not be reached.
    #[error("ssh: cannot reach {host}: {detail}")]
    Unreachable {
        /// Destination.
        host: String,
        /// ssh's message.
        detail: String,
    },
    /// The ssh program could not be started.
    #[error("cannot run {program}: {source}")]
    Spawn {
        /// Program.
        program: String,
        /// Underlying error.
        #[source]
        source: io::Error,
    },
    /// Any other failure.
    #[error("ssh to {host} failed (exit {code:?}): {stderr}")]
    Failed {
        /// Destination.
        host: String,
        /// Exit code.
        code: Option<i32>,
        /// Trimmed stderr.
        stderr: String,
    },
}

/// Classify a failed ssh run from its exit code and stderr.
pub fn classify(host: &str, code: Option<i32>, stderr: &str) -> SshError {
    let host = host.to_owned();
    let lower = stderr.to_ascii_lowercase();
    let detail = stderr.trim().lines().last().unwrap_or("").trim().to_owned();
    if lower.contains("remote host identification has changed")
        || lower.contains("host key for") && lower.contains("has changed")
    {
        SshError::HostKeyChanged { host }
    } else if lower.contains("host key verification failed")
        || lower.contains("no ") && lower.contains("host key is known")
    {
        SshError::UnknownHost { host }
    } else if lower.contains("permission denied") || lower.contains("too many authentication") {
        SshError::Auth { host, detail }
    } else if lower.contains("could not resolve hostname")
        || lower.contains("name or service not known")
    {
        SshError::Resolve { host }
    } else if lower.contains("connection refused")
        || lower.contains("timed out")
        || lower.contains("no route to host")
        || lower.contains("network is unreachable")
        || lower.contains("connection closed")
        || lower.contains("broken pipe")
    {
        SshError::Unreachable { host, detail }
    } else {
        SshError::Failed {
            host,
            code,
            stderr: stderr.trim().to_owned(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn remote() -> Remote {
        Remote {
            host: "10.0.0.5".into(),
            user: Some("ci".into()),
            port: 2222,
            identity_file: Some("/home/me/.ssh/id_gpu".into()),
            labels: vec!["gpu".into()],
            workdir: None,
            max_jobs: 1,
            sync: harness_config::ini::config::SyncMethod::Rsync,
            policy_bash_deny: vec![],
            policy_bash_ask: vec![],
            compression: harness_config::ini::config::SshCompression::Auto,
            compression_below_mbps: 20.0,
        }
    }

    fn target() -> SshTarget {
        SshTarget::new("gpu", &remote(), SshOptions::new("/run/user/1000/harness"))
    }

    fn has_opt(args: &[String], kv: &str) -> bool {
        args.windows(2).any(|w| w[0] == "-o" && w[1] == kv)
    }

    #[test]
    fn multiplexing_uses_runtime_control_path() {
        let a = target().option_args();
        assert!(has_opt(&a, "ControlMaster=auto"));
        assert!(has_opt(&a, "ControlPath=/run/user/1000/harness/cm-%C"));
        assert!(has_opt(&a, "ControlPersist=60"));
    }

    fn target_with(compression: harness_config::ini::config::SshCompression) -> SshTarget {
        let r = Remote {
            compression,
            ..remote()
        };
        SshTarget::new("gpu", &r, SshOptions::new("/run/user/1000/harness"))
    }

    #[test]
    fn compression_yes_compresses_on_its_own_master() {
        use harness_config::ini::config::SshCompression;
        let t = target_with(SshCompression::Yes);
        assert!(t.compression);
        let a = t.option_args();
        assert!(has_opt(&a, "Compression=yes"));
        assert!(!has_opt(&a, "Compression=no"));
        assert!(has_opt(&a, "ControlPath=/run/user/1000/harness/cmz-%C"));
        assert!(t.rsync_shell().contains("Compression=yes"));
    }

    #[test]
    fn compression_no_and_unmeasured_auto_do_not_compress() {
        use harness_config::ini::config::SshCompression;
        for c in [SshCompression::No, SshCompression::Auto] {
            let t = target_with(c);
            assert!(!t.compression, "{c}");
            let a = t.option_args();
            assert!(has_opt(&a, "Compression=no"), "{c}");
            assert!(has_opt(&a, "ControlPath=/run/user/1000/harness/cm-%C"));
        }
    }

    #[test]
    fn auto_decision_sets_compression() {
        use crate::link::{LinkSample, decide};
        use harness_config::ini::config::SshCompression;
        let r = Remote {
            compression: SshCompression::Auto,
            ..remote()
        };
        let slow = decide(&r, Some(LinkSample::now(250_000.0))); // 2 Mbit/s
        let t = target_with(SshCompression::Auto).with_compression(slow.active);
        assert!(has_opt(&t.option_args(), "Compression=yes"));
        assert!(has_opt(
            &t.option_args(),
            "ControlPath=/run/user/1000/harness/cmz-%C"
        ));
        let fast = decide(&r, Some(LinkSample::now(125_000_000.0))); // 1 Gbit/s
        let t = target_with(SshCompression::Auto).with_compression(fast.active);
        assert!(has_opt(&t.option_args(), "Compression=no"));
    }

    #[test]
    fn strict_host_keys_and_no_prompts() {
        let a = target().option_args();
        assert!(has_opt(&a, "StrictHostKeyChecking=yes"));
        assert!(has_opt(&a, "BatchMode=yes"));
    }

    #[test]
    fn keepalives_are_on() {
        let a = target().option_args();
        assert!(has_opt(&a, "ServerAliveInterval=15"));
        assert!(has_opt(&a, "ServerAliveCountMax=3"));
        assert!(has_opt(&a, "ConnectTimeout=10"));
    }

    #[test]
    fn no_forwarding_of_agent_env_or_ports() {
        let a = target().option_args();
        assert!(has_opt(&a, "ForwardAgent=no"));
        assert!(has_opt(&a, "ForwardX11=no"));
        assert!(has_opt(&a, "ClearAllForwardings=yes"));
        assert!(has_opt(&a, "SendEnv=-*"));
        assert!(a.contains(&"-a".to_string()));
        assert!(a.contains(&"-x".to_string()));
        assert!(!a.iter().any(|x| x == "-A" || x.starts_with("SetEnv")));
    }

    #[test]
    fn user_port_identity() {
        let a = target().option_args();
        let s = a.join(" ");
        assert!(s.contains("-p 2222"));
        assert!(s.contains("-l ci"));
        assert!(s.contains("-i /home/me/.ssh/id_gpu"));
        assert!(has_opt(&a, "IdentitiesOnly=yes"));
    }

    #[test]
    fn command_quotes_remote_argv() {
        let args = target().command_args(&["echo".into(), "a b".into(), "it's".into()]);
        let n = args.len();
        assert_eq!(args[n - 3], "--");
        assert_eq!(args[n - 2], "10.0.0.5");
        assert_eq!(args[n - 1], r"echo 'a b' 'it'\''s'");
    }

    #[test]
    fn remote_paths_keep_home() {
        assert_eq!(quote_remote_path("~/.cache/x y"), "\"$HOME\"/'.cache/x y'");
        assert_eq!(quote_remote_path("/opt/h"), "/opt/h");
    }

    #[test]
    fn quoting() {
        assert_eq!(shell_quote("abc/d.e"), "abc/d.e");
        assert_eq!(shell_quote(""), "''");
        assert_eq!(shell_quote("$HOME"), "'$HOME'");
        assert_eq!(shell_quote("a'b"), r"'a'\''b'");
    }

    #[test]
    fn control_dir_prefers_runtime_dir() {
        let d = control_dir_with(|k| (k == "XDG_RUNTIME_DIR").then(|| "/run/user/7".into()));
        assert_eq!(d, PathBuf::from("/run/user/7/harness"));
        let d = control_dir_with(|k| (k == "USER").then(|| "bob".into()));
        assert_eq!(d, PathBuf::from("/tmp/harness-bob"));
        let d = control_dir_with(|k| match k {
            "USER" => Some("bob".into()),
            "TMPDIR" => Some("/var/tmp".into()),
            _ => None,
        });
        assert_eq!(d, PathBuf::from("/var/tmp/harness-bob"));
    }

    /// macOS has no XDG_RUNTIME_DIR and a long per-user TMPDIR; sockets
    /// under it would exceed the 104-byte sun_path and ssh would refuse
    /// with "ControlPath too long".
    #[test]
    fn control_dir_fits_sun_path_with_a_macos_tmpdir() {
        let mac_tmp = "/var/folders/24/8k48jl6d249_n_qfxwsl6xvm0000gn/T/";
        let env = |k: &str| match k {
            "USER" => Some("runner".into()),
            "TMPDIR" => Some(mac_tmp.into()),
            _ => None,
        };
        for max in [104, 108] {
            let d = control_dir_for(env, max);
            let longest = d.join(format!("cmz-{}.{}", "f".repeat(40), "0".repeat(16)));
            assert!(
                longest.as_os_str().len() < max,
                "{} is {} bytes, sun_path is {max}",
                longest.display(),
                longest.as_os_str().len()
            );
        }
        assert_eq!(
            control_dir_for(env, 104),
            PathBuf::from("/tmp/harness-runner")
        );
        // A long runtime dir falls back too.
        let long = format!("/run/{}", "x".repeat(60));
        let d = control_dir_for(
            |k| (k == "XDG_RUNTIME_DIR").then(|| long.clone().into()),
            108,
        );
        assert_eq!(d, PathBuf::from("/tmp/harness-user"));
    }

    #[test]
    fn private_dir_refuses_a_symlink() {
        let t = tempfile::tempdir().unwrap();
        let real = t.path().join("real");
        std::fs::create_dir(&real).unwrap();
        let link = t.path().join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        assert!(create_private_dir(&link).is_err());
    }

    #[test]
    fn private_dir_is_0700() {
        use std::os::unix::fs::PermissionsExt;
        let t = tempfile::tempdir().unwrap();
        let d = t.path().join("a/b");
        create_private_dir(&d).unwrap();
        let mode = std::fs::metadata(&d).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700);
    }

    #[test]
    fn unknown_host_has_clear_message() {
        let e = classify("gpu", Some(255), "Host key verification failed.\r\n");
        assert!(matches!(e, SshError::UnknownHost { .. }));
        let msg = e.to_string();
        assert!(msg.contains("not in known_hosts"), "{msg}");
        assert!(msg.contains("never accepts unknown host keys"), "{msg}");
        let e = classify(
            "gpu",
            Some(255),
            "No ED25519 host key is known for gpu and you have requested strict checking.\nHost key verification failed.",
        );
        assert!(matches!(e, SshError::UnknownHost { .. }));
    }

    #[test]
    fn other_failures_are_classified() {
        let e = classify(
            "h",
            Some(255),
            "@@@@\n@ WARNING: REMOTE HOST IDENTIFICATION HAS CHANGED! @\n",
        );
        assert!(matches!(e, SshError::HostKeyChanged { .. }));
        assert!(matches!(
            classify("h", Some(255), "ci@h: Permission denied (publickey)."),
            SshError::Auth { .. }
        ));
        assert!(matches!(
            classify(
                "h",
                Some(255),
                "ssh: Could not resolve hostname h: Name or service not known"
            ),
            SshError::Resolve { .. }
        ));
        assert!(matches!(
            classify(
                "h",
                Some(255),
                "ssh: connect to host h port 22: Connection refused"
            ),
            SshError::Unreachable { .. }
        ));
        assert!(matches!(
            classify("h", Some(255), "weird"),
            SshError::Failed { .. }
        ));
    }

    /// A fake `ssh` that fails like OpenSSH does for an unknown host.
    #[tokio::test]
    async fn run_reports_unknown_host_from_fake_ssh() {
        let t = tempfile::tempdir().unwrap();
        // Written as a plain file and made executable by a child process
        // (`install`): if this multi-threaded test process held a write
        // descriptor on the executable, a concurrent fork could inherit it
        // and the exec would fail with ETXTBSY ("Text file busy").
        let src = t.path().join("ssh.src");
        std::fs::write(
            &src,
            "#!/bin/sh\necho 'Host key verification failed.' >&2\nexit 255\n",
        )
        .unwrap();
        let fake = t.path().join("ssh");
        let st = std::process::Command::new("install")
            .args(["-m", "755"])
            .arg(&src)
            .arg(&fake)
            .status()
            .unwrap();
        assert!(st.success(), "install failed: {st}");
        let mut opts = SshOptions::new(t.path().join("cm"));
        opts.program = fake.display().to_string();
        let tg = SshTarget::new("gpu", &remote(), opts);
        let err = tg.run(&["true".into()], b"").await.unwrap_err();
        assert!(matches!(err, SshError::UnknownHost { .. }), "{err}");
    }

    /// Real OpenSSH against a host key that is not known. Needs a local
    /// sshd on 127.0.0.1:22.
    #[tokio::test]
    #[ignore = "needs a local sshd"]
    async fn real_ssh_rejects_unknown_host() {
        let t = tempfile::tempdir().unwrap();
        let mut r = remote();
        r.host = "127.0.0.1".into();
        r.port = 22;
        r.user = None;
        r.identity_file = None;
        let mut tg = SshTarget::new("local", &r, SshOptions::new(t.path().join("cm")));
        tg.options.program = "ssh".into();
        let mut args = tg.option_args();
        args.extend([
            "-o".into(),
            format!("UserKnownHostsFile={}", t.path().join("kh").display()),
            "--".into(),
            "127.0.0.1".into(),
            "true".into(),
        ]);
        let out = tokio::process::Command::new("ssh")
            .args(args)
            .output()
            .await
            .unwrap();
        let e = classify(
            "127.0.0.1",
            out.status.code(),
            &String::from_utf8_lossy(&out.stderr),
        );
        assert!(matches!(e, SshError::UnknownHost { .. }), "{e}");
    }
}
