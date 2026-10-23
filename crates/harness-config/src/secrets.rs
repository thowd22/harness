//! Credential resolution (`api_key`, `api_key_cmd`, `api_key_env`) and zeroizing secret holders.
//!
//! - [`Secret`] holds a credential. Its `Debug`, `Display` and `Serialize`
//!   output is `[redacted]` and its memory is zeroized on drop. Read the value
//!   only with [`Secret::expose`], at the point of use (an HTTP header).
//! - [`resolve`] applies the documented precedence: inline `api_key`, then
//!   `api_key_cmd`, then `api_key_env`; the first non-empty value wins.
//! - `api_key_cmd` runs under `sh -c` with a timeout ([`DEFAULT_CMD_TIMEOUT`]),
//!   stdin closed and stderr discarded, so command diagnostics (which may
//!   contain secrets) never reach logs or error messages.
//! - [`insecure_mode`] reports whether a file is readable by group or others.

use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use zeroize::Zeroizing;

/// Text shown in place of a secret.
pub const REDACTED: &str = "[redacted]";

/// Default time limit for `api_key_cmd`.
pub const DEFAULT_CMD_TIMEOUT: Duration = Duration::from_secs(10);

/// Maximum bytes read from `api_key_cmd` stdout.
const MAX_CMD_OUTPUT: u64 = 64 * 1024;

/// A credential whose memory is zeroized on drop and which never prints.
#[derive(Clone)]
pub struct Secret(Zeroizing<String>);

impl Secret {
    /// Wraps `value`.
    pub fn new(value: String) -> Self {
        Self(Zeroizing::new(value))
    }

    /// Returns the secret value. Call only where the value is sent to its
    /// intended recipient; never log or format it.
    pub fn expose(&self) -> &str {
        &self.0
    }

    /// True when the value is empty.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Secret({REDACTED})")
    }
}

impl std::fmt::Display for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(REDACTED)
    }
}

impl serde::Serialize for Secret {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(REDACTED)
    }
}

/// Which source produced a resolved secret.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SecretOrigin {
    /// Inline `api_key`.
    Inline,
    /// Output of `api_key_cmd`.
    Command,
    /// Environment variable named by `api_key_env`.
    Env,
}

impl SecretOrigin {
    /// The `harness.ini` key of this source.
    pub fn ini_key(self) -> &'static str {
        match self {
            Self::Inline => "api_key",
            Self::Command => "api_key_cmd",
            Self::Env => "api_key_env",
        }
    }
}

/// The configured sources for one credential.
#[derive(Debug, Clone, Copy, Default)]
pub struct SecretSources<'a> {
    /// Inline value (`api_key`).
    pub inline: Option<&'a Secret>,
    /// Shell command (`api_key_cmd`).
    pub cmd: Option<&'a str>,
    /// Environment variable name (`api_key_env`).
    pub env: Option<&'a str>,
}

/// Environment variable lookup used by [`resolve`].
pub type EnvLookup = Box<dyn Fn(&str) -> Option<String>>;

/// Knobs for [`resolve`], mainly for tests.
pub struct ResolveOptions {
    /// Time limit for `api_key_cmd`.
    pub cmd_timeout: Duration,
    /// Environment lookup (defaults to the process environment).
    pub env_lookup: EnvLookup,
}

impl Default for ResolveOptions {
    fn default() -> Self {
        Self {
            cmd_timeout: DEFAULT_CMD_TIMEOUT,
            env_lookup: Box::new(|k| std::env::var(k).ok()),
        }
    }
}

/// Errors from resolving a secret. Messages never contain secret values or
/// command stderr.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum SecretError {
    /// `api_key_cmd` could not be started.
    #[error("api_key_cmd could not be started: {0}")]
    Spawn(#[source] std::io::Error),
    /// `api_key_cmd` exceeded its time limit and was killed.
    #[error("api_key_cmd did not finish within {0:?} and was killed")]
    Timeout(Duration),
    /// `api_key_cmd` exited unsuccessfully.
    #[error(
        "api_key_cmd failed ({0}); its stderr is not shown because it may contain secrets, run the command yourself to debug it"
    )]
    Failed(String),
    /// `api_key_cmd` printed something that is not UTF-8.
    #[error("api_key_cmd output is not valid UTF-8")]
    NotUtf8,
}

/// Resolves a credential from `sources` in precedence order.
///
/// Returns `Ok(None)` when no source yields a non-empty value. If
/// `api_key_cmd` fails but `api_key_env` yields a value, the env value is
/// used; if nothing yields a value, the command error is returned.
pub fn resolve(
    sources: &SecretSources<'_>,
    opts: &ResolveOptions,
) -> Result<Option<(Secret, SecretOrigin)>, SecretError> {
    if let Some(s) = sources.inline.filter(|s| !s.is_empty()) {
        tracing::debug!(origin = "api_key", "credential resolved");
        return Ok(Some((s.clone(), SecretOrigin::Inline)));
    }
    let mut cmd_error = None;
    if let Some(cmd) = sources.cmd.filter(|c| !c.trim().is_empty()) {
        match run_secret_command(cmd, opts.cmd_timeout) {
            Ok(s) if !s.is_empty() => {
                tracing::debug!(origin = "api_key_cmd", "credential resolved");
                return Ok(Some((s, SecretOrigin::Command)));
            }
            Ok(_) => tracing::debug!("api_key_cmd printed nothing; trying api_key_env"),
            Err(e) => {
                tracing::warn!(error = %e, "api_key_cmd failed");
                cmd_error = Some(e);
            }
        }
    }
    if let Some(var) = sources.env.filter(|v| !v.is_empty())
        && let Some(v) = (opts.env_lookup)(var).filter(|v| !v.is_empty())
    {
        tracing::debug!(origin = "api_key_env", var, "credential resolved");
        return Ok(Some((Secret::new(v), SecretOrigin::Env)));
    }
    match cmd_error {
        Some(e) => Err(e),
        None => Ok(None),
    }
}

/// Runs `cmd` under `sh -c` and returns its trimmed stdout as a secret.
///
/// Stdin is closed and stderr discarded. The command leads its own process
/// group, and the whole group is killed after `timeout`, so a compound
/// command cannot leave children behind.
pub fn run_secret_command(cmd: &str, timeout: Duration) -> Result<Secret, SecretError> {
    let mut command = Command::new("sh");
    command
        .arg("-c")
        .arg(cmd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    #[cfg(unix)]
    std::os::unix::process::CommandExt::process_group(&mut command, 0);
    let mut child = command.spawn().map_err(SecretError::Spawn)?;
    let deadline = Instant::now() + timeout;
    let stdout = child.stdout.take().expect("stdout is piped");
    let (tx, rx) = mpsc::channel();
    // The reader ends when the pipe closes, which killing the group forces.
    std::thread::spawn(move || {
        let mut buf = Zeroizing::new(Vec::new());
        let res = stdout
            .take(MAX_CMD_OUTPUT)
            .read_to_end(&mut buf)
            .map(|_| buf);
        let _ = tx.send(res);
    });
    let kill = |child: &mut std::process::Child| {
        kill_group(child.id());
        let _ = child.kill();
        let _ = child.wait();
    };
    let output = match rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
        Ok(Ok(buf)) => buf,
        Ok(Err(e)) => {
            kill(&mut child);
            return Err(SecretError::Spawn(e));
        }
        Err(_) => {
            kill(&mut child);
            return Err(SecretError::Timeout(timeout));
        }
    };
    let status = loop {
        match child.try_wait().map_err(SecretError::Spawn)? {
            Some(status) => break status,
            None if Instant::now() >= deadline => {
                kill(&mut child);
                return Err(SecretError::Timeout(timeout));
            }
            None => std::thread::sleep(Duration::from_millis(5)),
        }
    };
    if !status.success() {
        return Err(SecretError::Failed(status.to_string()));
    }
    let text = std::str::from_utf8(&output).map_err(|_| SecretError::NotUtf8)?;
    Ok(Secret::new(text.trim().to_owned()))
}

/// SIGKILLs process group `pgid` (a child started with `process_group(0)`).
/// Ids 0 and 1 are never signalled: as groups they mean the caller's own
/// group and every process the user may signal.
fn kill_group(pgid: u32) {
    #[cfg(unix)]
    if let Some(pg) = i32::try_from(pgid).ok().filter(|&p| p > 1) {
        let _ = nix::sys::signal::killpg(
            nix::unistd::Pid::from_raw(pg),
            nix::sys::signal::Signal::SIGKILL,
        );
    }
    #[cfg(not(unix))]
    let _ = pgid;
}

/// Returns `Some(mode)` (the `0o777` permission bits) when `path` is
/// readable or writable by group or others, `None` when it is private.
/// Always `None` on non-Unix platforms.
pub fn insecure_mode(path: &Path) -> std::io::Result<Option<u32>> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(path)?.permissions().mode() & 0o777;
        Ok((mode & 0o077 != 0).then_some(mode))
    }
    #[cfg(not(unix))]
    {
        let _ = std::fs::metadata(path)?;
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opts(env: &'static [(&'static str, &'static str)]) -> ResolveOptions {
        ResolveOptions {
            cmd_timeout: Duration::from_secs(5),
            env_lookup: Box::new(move |k| {
                env.iter()
                    .find(|(n, _)| *n == k)
                    .map(|(_, v)| (*v).to_owned())
            }),
        }
    }

    #[test]
    fn secret_never_formats() {
        let s = Secret::new("sk-ant-supersecret".into());
        assert_eq!(format!("{s}"), REDACTED);
        assert!(!format!("{s:?}").contains("supersecret"));
        assert!(!format!("{s:#?}").contains("supersecret"));
        assert_eq!(serde_json::to_string(&s).unwrap(), "\"[redacted]\"");
        assert_eq!(s.expose(), "sk-ant-supersecret");
    }

    #[test]
    fn secret_storage_is_zeroizing() {
        // `Secret` stores its value in `Zeroizing<String>`, which wipes the
        // buffer on drop. Check the wrapper type statically and that dropping
        // through `zeroize` clears the contents.
        fn assert_zeroize_on_drop<T: zeroize::ZeroizeOnDrop>() {}
        assert_zeroize_on_drop::<Zeroizing<String>>();
        let mut inner = Zeroizing::new(String::from("sk-wipe-me"));
        zeroize::Zeroize::zeroize(&mut *inner);
        assert!(inner.is_empty());
    }

    #[test]
    fn precedence_inline_then_cmd_then_env() {
        let inline = Secret::new("inline-key".into());
        let all = SecretSources {
            inline: Some(&inline),
            cmd: Some("echo cmd-key"),
            env: Some("K"),
        };
        let env = &[("K", "env-key")];
        let (s, o) = resolve(&all, &opts(env)).unwrap().unwrap();
        assert_eq!((s.expose(), o), ("inline-key", SecretOrigin::Inline));

        let empty = Secret::new(String::new());
        let no_inline = SecretSources {
            inline: Some(&empty),
            ..all
        };
        let (s, o) = resolve(&no_inline, &opts(env)).unwrap().unwrap();
        assert_eq!((s.expose(), o), ("cmd-key", SecretOrigin::Command));

        let empty_cmd = SecretSources {
            cmd: Some("printf ''"),
            ..no_inline
        };
        let (s, o) = resolve(&empty_cmd, &opts(env)).unwrap().unwrap();
        assert_eq!((s.expose(), o), ("env-key", SecretOrigin::Env));

        assert!(resolve(&empty_cmd, &opts(&[("K", "")])).unwrap().is_none());
        assert!(
            resolve(&SecretSources::default(), &opts(&[]))
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn cmd_failure_falls_back_to_env_or_errors() {
        let src = SecretSources {
            cmd: Some("exit 3"),
            env: Some("K"),
            ..Default::default()
        };
        let (s, _) = resolve(&src, &opts(&[("K", "env-key")])).unwrap().unwrap();
        assert_eq!(s.expose(), "env-key");
        let err = resolve(&src, &opts(&[])).unwrap_err();
        assert!(matches!(err, SecretError::Failed(_)));
    }

    #[test]
    fn cmd_times_out() {
        let start = Instant::now();
        let err = run_secret_command("sleep 5", Duration::from_millis(200)).unwrap_err();
        assert!(matches!(err, SecretError::Timeout(_)));
        assert!(start.elapsed() < Duration::from_secs(3));
    }

    /// A timeout kills the whole group: a background child of `sh` must
    /// not outlive the command.
    #[cfg(unix)]
    #[test]
    fn cmd_timeout_kills_background_children() {
        let dir = tempfile::tempdir().unwrap();
        let pid_file = dir.path().join("bg.pid");
        let cmd = format!(
            "sleep 30 & echo $! > {}; sleep 30",
            pid_file.to_string_lossy()
        );
        let err = run_secret_command(&cmd, Duration::from_millis(500)).unwrap_err();
        assert!(matches!(err, SecretError::Timeout(_)));
        let pid = std::fs::read_to_string(&pid_file).unwrap();
        let pid = pid.trim();
        let mut gone = false;
        for _ in 0..50 {
            let alive = Command::new("kill")
                .args(["-0", pid])
                .stderr(Stdio::null())
                .status()
                .unwrap()
                .success();
            if !alive {
                gone = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        assert!(gone, "background child {pid} outlived the timeout");
    }

    #[test]
    fn cmd_stderr_is_not_echoed() {
        let err = run_secret_command(
            "echo leaked-stderr-token >&2; exit 1",
            Duration::from_secs(5),
        )
        .unwrap_err();
        assert!(!err.to_string().contains("leaked-stderr-token"));
        assert!(!format!("{err:?}").contains("leaked-stderr-token"));
        let ok = run_secret_command(
            "echo noise-on-stderr >&2; echo '  the-key  '",
            Duration::from_secs(5),
        )
        .unwrap();
        assert_eq!(ok.expose(), "the-key");
    }

    #[cfg(unix)]
    #[test]
    fn insecure_mode_detects_group_and_world_bits() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("f");
        std::fs::write(&p, "x").unwrap();
        for (mode, expect) in [
            (0o600, None),
            (0o400, None),
            (0o640, Some(0o640)),
            (0o604, Some(0o604)),
        ] {
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(mode)).unwrap();
            assert_eq!(insecure_mode(&p).unwrap(), expect, "mode {mode:o}");
        }
    }
}
