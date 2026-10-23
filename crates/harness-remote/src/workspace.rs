//! Remote implementation of the `Workspace` and `Exec` traits.
//!
//! [`RemoteWorkspace`] and [`RemoteExec`] forward every operation to the
//! helper through a [`RemoteClient`], so the harness tools (read, write,
//! edit, bash) run unchanged on a remote host while the model loop, policy,
//! redaction, change tracking and credentials stay local: tools compute
//! diffs, stale-read checks and policy decisions locally and only the raw
//! file or process operation crosses the wire.
//!
//! Environment handling for remote processes ([`RemoteEnv`]):
//! - secret variables (harness credentials, `HARNESS_*`, configured
//!   `api_key_env` names and any value containing a resolved key) are
//!   removed before the request leaves this machine;
//! - host-specific variables (`HOME`, `PATH`, `USER`, `SSH_*`, `XDG_*`,
//!   locale, ...) are never forwarded; for the basic ones
//!   ([`HOST_ENV_FROM_REMOTE`]) the helper uses the remote login values
//!   instead, the rest (agent sockets, display) are dropped. A process
//!   started with a clean environment always gets those basics from the
//!   remote login, even when the caller sent no base environment (bash on
//!   a host never sends the local one).

use std::io;
use std::path::{Path, PathBuf};

use async_trait::async_trait;
use harness_tools::bash::EnvScrubber;
use harness_tools::exec::{Exec, ExecError, ExecHandle, ExecRequest};
use harness_tools::search::{FindQuery, FindResult};
use harness_tools::workspace::{DirEntry, FileStat, Workspace, WsError, WsResult};
use serde_json::Value;
use tokio::sync::oneshot;

use crate::client::{ClientError, RemoteClient};
use crate::protocol::{
    ExecStartParams, GlobParams, GlobResult, JobParams, ListResult, PathParams, SetModeParams,
    WireStat, codes, methods,
};

/// Variables that describe the local machine and are never forwarded.
pub const HOST_SPECIFIC_VARS: &[&str] = &[
    "HOME",
    "PATH",
    "USER",
    "LOGNAME",
    "SHELL",
    "PWD",
    "OLDPWD",
    "TMPDIR",
    "MAIL",
    "HOSTNAME",
    "LANG",
    "LANGUAGE",
    "DISPLAY",
    "WAYLAND_DISPLAY",
    "KRB5CCNAME",
    "GPG_AGENT_INFO",
    "DESKTOP_SESSION",
];

/// Prefixes of host-specific variables (`SSH_AUTH_SOCK`, `XDG_*`, `LC_*`, ...).
pub const HOST_SPECIFIC_PREFIXES: &[&str] = &["SSH_", "XDG_", "LC_", "DBUS_"];

/// Host-specific variables whose *remote* values the helper sets when the
/// request named them (the rest are simply dropped).
pub const HOST_ENV_FROM_REMOTE: &[&str] = &[
    "HOME", "PATH", "USER", "LOGNAME", "SHELL", "TMPDIR", "LANG", "LANGUAGE",
];

/// True if `name` describes the local machine.
pub fn is_host_specific(name: &str) -> bool {
    HOST_SPECIFIC_VARS.contains(&name) || HOST_SPECIFIC_PREFIXES.iter().any(|p| name.starts_with(p))
}

/// Filters the environment of remote processes.
#[derive(Debug, Clone)]
pub struct RemoteEnv {
    scrubber: EnvScrubber,
}

impl Default for RemoteEnv {
    /// Scrubs the built-in secret names (`ANTHROPIC_API_KEY`, ...) and
    /// `HARNESS_*`; use [`RemoteEnv::new`] to add configured names and
    /// resolved values.
    fn default() -> Self {
        Self::new(EnvScrubber::new(Vec::new(), Vec::new()))
    }
}

impl RemoteEnv {
    /// Scrub with `scrubber` (build it from the configured secret names and
    /// resolved values, as for the local bash tool).
    pub fn new(scrubber: EnvScrubber) -> Self {
        Self { scrubber }
    }

    /// Split `env` into the variables to send and the host-specific names
    /// the helper should take from the remote environment.
    pub fn prepare(&self, env: Vec<(String, String)>) -> (Vec<(String, String)>, Vec<String>) {
        let mut send = Vec::new();
        let mut host = Vec::new();
        for (k, v) in env {
            if self.scrubber.is_secret(&k, &v) {
                continue;
            }
            if is_host_specific(&k) {
                let from_remote =
                    HOST_ENV_FROM_REMOTE.contains(&k.as_str()) || k.starts_with("LC_");
                if from_remote && !host.contains(&k) {
                    host.push(k);
                }
            } else {
                send.push((k, v));
            }
        }
        (send, host)
    }
}

fn path_str(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

fn ws_err(host: &str, path: &Path, e: ClientError) -> WsError {
    match e {
        ClientError::Rpc(r) => r.into_ws(path),
        ClientError::SecretBlocked { .. } => WsError::Io {
            path: path.to_path_buf(),
            source: io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!("not sent to remote host {host}: the data contains a harness credential"),
            ),
        },
        other => WsError::Io {
            path: path.to_path_buf(),
            source: io::Error::other(format!("remote host {host}: {other}")),
        },
    }
}

/// A [`Workspace`] on a remote host.
#[derive(Debug, Clone)]
pub struct RemoteWorkspace {
    client: RemoteClient,
    root: PathBuf,
}

impl RemoteWorkspace {
    /// A workspace rooted at `root` (a path on the remote host; a leading
    /// `~/` is made absolute with the helper's home).
    pub fn new(client: RemoteClient, root: impl AsRef<str>) -> Self {
        let root = PathBuf::from(client.expand_home(root.as_ref()));
        Self { client, root }
    }

    /// The host this workspace lives on.
    pub fn host(&self) -> &str {
        self.client.host()
    }

    /// The client.
    pub fn client(&self) -> &RemoteClient {
        &self.client
    }

    /// Send `data` to `path` with `method` (`fs.write` or
    /// `fs.replace_atomic`). The bytes travel as the binary payload (raw on
    /// protocol 2), or as their hash when the helper already holds them
    /// ([`RemoteClient::write_file`]); the secret guard refuses them before
    /// encoding either way.
    async fn send(&self, method: &str, path: &Path, data: &[u8]) -> WsResult<()> {
        let p = self.resolve(path);
        let s = path_str(&p);
        self.client
            .write_file(method, &s, data)
            .await
            .map_err(|e| ws_err(self.host(), &p, e))
    }
}

#[async_trait]
impl Workspace for RemoteWorkspace {
    fn root(&self) -> &Path {
        &self.root
    }

    async fn read(&self, path: &Path) -> WsResult<Vec<u8>> {
        let p = self.resolve(path);
        let s = path_str(&p);
        // Through the content cache: unchanged content comes back as a hash.
        self.client
            .read_file(&s)
            .await
            .map_err(|e| ws_err(self.host(), &p, e))
    }

    async fn write(&self, path: &Path, data: &[u8]) -> WsResult<()> {
        self.send(methods::FS_WRITE, path, data).await
    }

    async fn replace_atomic(&self, path: &Path, data: &[u8]) -> WsResult<()> {
        self.send(methods::FS_REPLACE, path, data).await
    }

    async fn set_mode(&self, path: &Path, mode: u32) -> WsResult<()> {
        let p = self.resolve(path);
        let s = path_str(&p);
        self.client
            .call::<_, Value>(
                methods::FS_SET_MODE,
                Some(&s),
                SetModeParams {
                    path: s.clone(),
                    mode,
                },
            )
            .await
            .map(|_| ())
            .map_err(|e| ws_err(self.host(), &p, e))
    }

    async fn stat(&self, path: &Path) -> WsResult<FileStat> {
        let p = self.resolve(path);
        let s = path_str(&p);
        self.client
            .call::<_, WireStat>(methods::FS_STAT, Some(&s), PathParams { path: s.clone() })
            .await
            .map(FileStat::from)
            .map_err(|e| ws_err(self.host(), &p, e))
    }

    async fn list(&self, path: &Path) -> WsResult<Vec<DirEntry>> {
        let p = self.resolve(path);
        let s = path_str(&p);
        self.client
            .call::<_, ListResult>(methods::FS_LIST, Some(&s), PathParams { path: s.clone() })
            .await
            .map(|r| r.entries.into_iter().map(DirEntry::from).collect())
            .map_err(|e| ws_err(self.host(), &p, e))
    }

    async fn glob(&self, pattern: &str) -> WsResult<Vec<PathBuf>> {
        let root = path_str(&self.root);
        self.client
            .call::<_, GlobResult>(
                methods::FS_GLOB,
                Some(pattern),
                GlobParams {
                    root,
                    pattern: pattern.to_owned(),
                },
            )
            .await
            .map(|r| r.paths.into_iter().map(PathBuf::from).collect())
            .map_err(|e| match e {
                ClientError::Rpc(r) if r.code == codes::INVALID_PATTERN => {
                    WsError::InvalidPattern {
                        pattern: pattern.to_owned(),
                        reason: r.message,
                    }
                }
                e => ws_err(self.host(), Path::new(pattern), e),
            })
    }

    async fn find(&self, query: &FindQuery) -> WsResult<FindResult> {
        let mut q = query.clone();
        q.base = self.resolve(&query.base);
        let s = path_str(&q.base);
        self.client
            .call::<_, FindResult>(methods::FS_FIND, Some(&s), &q)
            .await
            .map_err(|e| match e {
                // A helper that predates fs.find: the tools fall back.
                ClientError::Rpc(r) if r.code == codes::METHOD_NOT_FOUND => WsError::Io {
                    path: q.base.clone(),
                    source: io::Error::new(
                        io::ErrorKind::Unsupported,
                        format!("the helper on {} does not support file search", self.host()),
                    ),
                },
                e => ws_err(self.host(), &q.base, e),
            })
    }
}

/// An [`Exec`] on a remote host.
#[derive(Debug, Clone)]
pub struct RemoteExec {
    client: RemoteClient,
    env: RemoteEnv,
}

impl RemoteExec {
    /// Run processes through `client`, filtering the environment with `env`.
    pub fn new(client: RemoteClient, env: RemoteEnv) -> Self {
        Self { client, env }
    }

    /// The host processes run on.
    pub fn host(&self) -> &str {
        self.client.host()
    }
}

#[async_trait]
impl Exec for RemoteExec {
    async fn spawn(&self, req: ExecRequest) -> Result<ExecHandle, ExecError> {
        let job = uuid::Uuid::now_v7().simple().to_string();
        let (output, status, liveness) = self.client.register_job(&job);
        let (env, mut host_env) = self.env.prepare(req.env);
        if req.clear_env {
            // A clean environment still gets the remote login basics, even
            // when the caller started from an empty base (bash on a host
            // never sends the local environment).
            for name in HOST_ENV_FROM_REMOTE {
                if !host_env.iter().any(|h| h == name) && !env.iter().any(|(k, _)| k == name) {
                    host_env.push((*name).to_owned());
                }
            }
        }
        let params = ExecStartParams {
            job: job.clone(),
            program: req.program.clone(),
            args: req.args,
            cwd: self.client.expand_home(&path_str(&req.cwd)),
            env,
            clear_env: req.clear_env,
            host_env,
            timeout_ms: req.timeout.map(|t| t.as_millis() as u64),
        };
        if let Err(e) = self
            .client
            .call::<_, Value>(methods::EXEC_START, Some(&req.program), params)
            .await
        {
            self.client.forget_job(&job);
            let kind = match e.rpc_code() {
                Some(codes::SPAWN) => io::ErrorKind::NotFound,
                _ => io::ErrorKind::Other,
            };
            return Err(ExecError::Spawn {
                program: req.program,
                source: io::Error::new(kind, format!("on {}: {e}", self.host())),
            });
        }
        let (kill_tx, kill_rx) = oneshot::channel::<()>();
        let client = self.client.clone();
        tokio::spawn(async move {
            // Fires on kill() and on drop of the handle.
            let _ = kill_rx.await;
            if client.job_active(&job) {
                let _ = client
                    .call::<_, Value>(
                        methods::EXEC_KILL,
                        Some(&job),
                        JobParams { job: job.clone() },
                    )
                    .await;
            }
        });
        Ok(ExecHandle::new(output, kill_tx, status).with_liveness(liveness))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_is_scrubbed_and_host_vars_are_not_forwarded() {
        let env = RemoteEnv::new(EnvScrubber::new(
            ["MY_KEY_ENV".to_string()],
            ["sk-live-0123456789".to_string()],
        ));
        let (send, host) = env.prepare(vec![
            ("FOO".into(), "bar".into()),
            ("PATH".into(), "/local/bin".into()),
            ("HOME".into(), "/home/me".into()),
            ("SSH_AUTH_SOCK".into(), "/tmp/agent".into()),
            ("XDG_RUNTIME_DIR".into(), "/run/user/1".into()),
            ("ANTHROPIC_API_KEY".into(), "x".into()),
            ("MY_KEY_ENV".into(), "x".into()),
            ("HARNESS_TOKEN".into(), "x".into()),
            ("SNEAKY".into(), "Bearer sk-live-0123456789".into()),
        ]);
        assert_eq!(send, vec![("FOO".to_string(), "bar".to_string())]);
        assert_eq!(
            host,
            vec!["PATH", "HOME"],
            "agent socket and runtime dirs are dropped"
        );
    }
}
