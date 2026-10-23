//! Managed MCP servers: lazy start, restart with backoff, per-request
//! timeouts and clean shutdown.
//!
//! A [`ServerHandle`] owns one `[mcp.<name>]` server. Nothing starts until
//! the first request (or [`ServerHandle::start`], which [`Servers`] calls
//! for `startup = eager`). When a stdio server crashes, the requests in
//! flight fail with [`McpError::Exited`] (they are not retried, since tool
//! calls may have side effects) and the next request restarts it after an
//! exponential backoff. After [`ClientOptions::max_failures`] consecutive
//! failures the server is given up for the session.
//!
//! Tool-list changes: when a connected server announces
//! `capabilities.tools.listChanged`, the handle asks for change
//! notifications ([`Connection::listen_for_changes`]) and bumps a
//! generation counter on every `notifications/tools/list_changed` (and when
//! the server restarts, since its tools may differ). Callers that cache
//! tool lists compare [`ServerHandle::tools_generation`].
//!
//! Status changes (starting, connected, failed, stopped, disabled for the
//! session, and prompt or resource list changes) bump a second counter,
//! [`ServerHandle::watch_status`], which the TUI's `/mcp` panel and status
//! indicator follow. [`ServerHandle::status`] never waits for a start in
//! progress: it reports [`Status::Starting`].

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::Duration;

use harness_config::ini::{Config, McpSecret, McpServer, McpStartup, McpTransport};
use harness_config::secrets::Secret;
use harness_events::redact::Redactor;
use serde_json::Value;
use tokio::time::Instant;

use crate::auth::{McpTokenStore, OAuthSession};
use crate::client::{Connection, Limits, Negotiated, Transport};
use crate::error::{McpError, Result};
use crate::http::HttpTransport;
use crate::prompts::{GetPromptResult, Prompt, ReadResourceResult, Resource};
use crate::protocol::{CallToolResult, Tool};
use crate::stdio::{SpawnSpec, StderrTail, StdioTransport};

/// Client-wide knobs (not in harness.ini; tests shorten them).
#[derive(Debug, Clone)]
pub struct ClientOptions {
    /// Credentials already known to harness (resolved provider keys);
    /// added to each server's redactor along with its own `env.*` /
    /// `header.*` values, and inherited variables whose value contains one
    /// are removed from a stdio server's environment.
    pub known_secrets: Vec<String>,
    /// Variables removed from a stdio server's inherited environment
    /// (normally [`Config::secret_env_vars`]), on top of the bash tool's
    /// always-scrubbed names (provider keys, `HARNESS_*`).
    pub scrub_env: Vec<String>,
    /// stdio `server/discover` probe limit before assuming a legacy server
    /// (capped by the server's `startup_timeout`).
    pub probe_timeout: Duration,
    /// First restart delay; doubles per consecutive failure.
    pub backoff_base: Duration,
    /// Longest restart delay.
    pub backoff_max: Duration,
    /// Consecutive failures after which the server is given up.
    pub max_failures: u32,
    /// How long shutdown waits after closing stdin, and after SIGTERM.
    pub shutdown_grace: Duration,
    /// The OAuth token store directory for `auth = oauth` servers
    /// (normally `harness_config::paths::DataDir::mcp_auth`). `None`: such
    /// servers fail with a message saying to authorize.
    pub auth_dir: Option<std::path::PathBuf>,
}

impl Default for ClientOptions {
    fn default() -> Self {
        Self {
            known_secrets: Vec::new(),
            scrub_env: Vec::new(),
            probe_timeout: Duration::from_secs(10),
            backoff_base: Duration::from_millis(500),
            backoff_max: Duration::from_secs(30),
            max_failures: 5,
            shutdown_grace: Duration::from_secs(2),
            auth_dir: None,
        }
    }
}

impl ClientOptions {
    /// Options for `cfg`: scrubs every configured credential variable,
    /// knows every provider key `cfg` resolves (running `api_key_cmd` where
    /// set; unresolvable keys are skipped) and every stored MCP OAuth
    /// token, and uses the data dir's MCP token store. Callers with more
    /// credentials (such as stored login tokens) add them to
    /// [`known_secrets`](Self::known_secrets).
    pub fn from_config(cfg: &Config) -> Self {
        let auth_dir = harness_config::paths::DataDir::from_config(cfg).mcp_auth();
        let mut known_secrets: Vec<String> = cfg
            .providers
            .values()
            .filter_map(|p| p.resolve_api_key().ok().flatten())
            .map(|s| s.expose().to_owned())
            .collect();
        known_secrets.extend(McpTokenStore::new(&auth_dir).secrets());
        Self {
            known_secrets,
            scrub_env: cfg.secret_env_vars(),
            auth_dir: Some(auth_dir),
            ..Self::default()
        }
    }

    /// The delay before restart attempt number `failures` (1-based).
    pub fn backoff(&self, failures: u32) -> Duration {
        let shift = failures.saturating_sub(1).min(16);
        self.backoff_base
            .saturating_mul(1u32 << shift)
            .min(self.backoff_max)
    }
}

/// What a server is doing, for status displays and doctor.
#[derive(Debug, Clone, PartialEq)]
pub enum Status {
    /// Disabled in harness.ini.
    Disabled,
    /// Disabled by the user for this session (`/mcp`).
    DisabledForSession,
    /// Not started yet (lazy).
    Idle,
    /// Starting (or waiting to restart after a failure).
    Starting,
    /// Connected.
    Running(Negotiated),
    /// Crashed or failed to start; will retry on next use.
    Failing {
        /// Consecutive failures.
        failures: u32,
        /// The last error.
        last: String,
    },
    /// Given up for this session.
    GaveUp {
        /// The last error.
        last: String,
    },
}

#[derive(Default)]
struct State {
    conn: Option<Arc<Connection>>,
    retry_at: Option<Instant>,
    last_error: Option<String>,
}

/// One configured MCP server.
pub struct ServerHandle {
    name: String,
    config: McpServer,
    opts: Arc<ClientOptions>,
    state: tokio::sync::Mutex<State>,
    failures: AtomicU32,
    stderr: StderrTail,
    /// Bumped on every tool-list change (and restart).
    tools_gen: tokio::sync::watch::Sender<u64>,
    /// Connections opened so far.
    connects: AtomicU32,
    /// The redactor of the last connection (credentials of this server).
    redactor: std::sync::Mutex<Arc<Redactor>>,
    /// Notification forwarding and change-stream tasks of the connection.
    watchers: std::sync::Mutex<Vec<tokio::task::JoinHandle<()>>>,
    /// Bumped on every status change (and prompt/resource list change).
    status_gen: tokio::sync::watch::Sender<u64>,
    /// A start (or the backoff before one) is in progress.
    starting: AtomicBool,
    /// Disabled by the user for this session.
    session_disabled: AtomicBool,
}

impl Drop for ServerHandle {
    fn drop(&mut self) {
        self.stop_watchers();
    }
}

impl ServerHandle {
    /// A handle for `[mcp.<name>]`; nothing starts yet.
    pub fn new(name: impl Into<String>, config: McpServer, opts: Arc<ClientOptions>) -> Self {
        Self {
            name: name.into(),
            config,
            state: tokio::sync::Mutex::default(),
            failures: AtomicU32::new(0),
            stderr: StderrTail::default(),
            tools_gen: tokio::sync::watch::channel(0).0,
            connects: AtomicU32::new(0),
            redactor: std::sync::Mutex::new(Arc::new(Redactor::new(
                opts.known_secrets.iter().map(String::as_str),
            ))),
            opts,
            watchers: std::sync::Mutex::default(),
            status_gen: tokio::sync::watch::channel(0).0,
            starting: AtomicBool::new(false),
            session_disabled: AtomicBool::new(false),
        }
    }

    /// A receiver that sees every status change: starting, connected,
    /// failed, stopped, disabled or enabled for the session, a crash, and
    /// tool, prompt or resource list changes.
    pub fn watch_status(&self) -> tokio::sync::watch::Receiver<u64> {
        self.status_gen.subscribe()
    }

    /// The current status-change counter (see [`watch_status`](Self::watch_status)).
    pub fn status_generation(&self) -> u64 {
        *self.status_gen.borrow()
    }

    fn bump_status(&self) {
        self.status_gen.send_modify(|g| *g += 1);
    }

    /// How many connections were opened so far (a new one may offer other
    /// tools, prompts and resources).
    pub fn connect_count(&self) -> u32 {
        self.connects.load(Ordering::SeqCst)
    }

    /// True when the user disabled the server for this session.
    pub fn is_disabled_for_session(&self) -> bool {
        self.session_disabled.load(Ordering::SeqCst)
    }

    /// Disables the server for the rest of the session and stops it.
    /// Requests fail with [`McpError::DisabledForSession`] until
    /// [`enable_for_session`](Self::enable_for_session).
    pub async fn disable_for_session(&self) {
        self.session_disabled.store(true, Ordering::SeqCst);
        tracing::info!(server = %self.name, "MCP server disabled for this session");
        self.shutdown().await;
        self.bump_status();
    }

    /// Undoes [`disable_for_session`](Self::disable_for_session); the
    /// server starts again on next use, with a fresh failure count.
    pub async fn enable_for_session(&self) {
        self.session_disabled.store(false, Ordering::SeqCst);
        self.reset_failures().await;
        tracing::info!(server = %self.name, "MCP server enabled for this session");
        self.bump_status();
    }

    async fn reset_failures(&self) {
        let mut st = self.state.lock().await;
        self.failures.store(0, Ordering::SeqCst);
        st.retry_at = None;
        st.last_error = None;
    }

    /// Stops the server, forgets its failures (a given-up server gets
    /// another chance) and starts it again.
    pub async fn restart(&self) -> Result<Negotiated> {
        self.shutdown().await;
        self.reset_failures().await;
        tracing::info!(server = %self.name, "MCP server restart requested");
        self.start().await
    }

    /// Changes whenever the server's tool list may have changed: on
    /// `notifications/tools/list_changed` and when the server restarts.
    pub fn tools_generation(&self) -> u64 {
        *self.tools_gen.borrow()
    }

    /// A receiver that sees every [`tools_generation`](Self::tools_generation) change.
    pub fn watch_tools(&self) -> tokio::sync::watch::Receiver<u64> {
        self.tools_gen.subscribe()
    }

    /// Redacts this server's credentials (and harness's) from `text`.
    pub fn redact(&self, text: &str) -> String {
        let r = self
            .redactor
            .lock()
            .map(|r| r.clone())
            .unwrap_or_else(|e| e.into_inner().clone());
        r.redact(text).into_owned()
    }

    fn bump_tools(&self) {
        self.tools_gen.send_modify(|g| *g += 1);
        self.bump_status();
    }

    fn stop_watchers(&self) {
        let tasks = match self.watchers.lock() {
            Ok(mut w) => std::mem::take(&mut *w),
            Err(e) => std::mem::take(&mut *e.into_inner()),
        };
        for t in tasks {
            t.abort();
        }
    }

    /// Follows tool-list changes of a new connection.
    fn watch(&self, conn: &Arc<Connection>) {
        self.stop_watchers();
        if self.connects.fetch_add(1, Ordering::SeqCst) > 0 {
            // A restarted server may offer different tools.
            self.bump_tools();
        }
        let mut tasks = Vec::new();
        let mut rx = conn.notifications();
        let gen_tx = self.tools_gen.clone();
        let status_tx = self.status_gen.clone();
        let name = self.name.clone();
        tasks.push(tokio::spawn(async move {
            use tokio::sync::broadcast::error::RecvError;
            loop {
                match rx.recv().await {
                    Ok(n) if n.method == crate::protocol::TOOLS_LIST_CHANGED => {
                        tracing::info!(server = %name, "MCP server changed its tools");
                        gen_tx.send_modify(|g| *g += 1);
                        status_tx.send_modify(|g| *g += 1);
                    }
                    Ok(n)
                        if n.method == crate::prompts::PROMPTS_LIST_CHANGED
                            || n.method == crate::prompts::RESOURCES_LIST_CHANGED =>
                    {
                        tracing::info!(server = %name, method = %n.method, "MCP server changed its prompts or resources");
                        status_tx.send_modify(|g| *g += 1);
                    }
                    Ok(_) => {}
                    Err(RecvError::Lagged(_)) => {
                        gen_tx.send_modify(|g| *g += 1);
                        status_tx.send_modify(|g| *g += 1);
                    }
                    Err(RecvError::Closed) => {
                        // The connection ended (a crash shows as failing).
                        status_tx.send_modify(|g| *g += 1);
                        break;
                    }
                }
            }
        }));
        if conn.announces_tool_changes() {
            let weak = Arc::downgrade(conn);
            let gen_tx = self.tools_gen.clone();
            let name = self.name.clone();
            tasks.push(tokio::spawn(async move {
                let mut delay = Duration::from_secs(1);
                let mut opened = 0u32;
                loop {
                    let Some(c) = weak.upgrade() else { break };
                    if opened > 0 {
                        // Changes may have been missed while the stream was down.
                        gen_tx.send_modify(|g| *g += 1);
                    }
                    opened += 1;
                    let r = c.listen_for_changes().await;
                    drop(c);
                    match r {
                        Ok(false) => break,
                        Ok(true) => delay = Duration::from_secs(1),
                        Err(McpError::Http {
                            status: Some(404 | 405),
                            ..
                        }) => {
                            tracing::debug!(server = %name, "MCP server has no notification stream");
                            break;
                        }
                        Err(e) => {
                            tracing::debug!(server = %name, error = %e, "MCP notification stream ended");
                        }
                    }
                    tokio::time::sleep(delay).await;
                    delay = (delay * 2).min(Duration::from_secs(30));
                }
            }));
        }
        if let Ok(mut w) = self.watchers.lock() {
            *w = tasks;
        }
    }

    /// The server name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The server's configuration.
    pub fn config(&self) -> &McpServer {
        &self.config
    }

    /// The last lines the server wrote to stderr (redacted).
    pub fn stderr_tail(&self) -> Vec<String> {
        self.stderr.lines()
    }

    /// The current status. Never waits for a start in progress.
    pub async fn status(&self) -> Status {
        if !self.config.enabled {
            return Status::Disabled;
        }
        if self.is_disabled_for_session() {
            return Status::DisabledForSession;
        }
        if self.starting.load(Ordering::SeqCst) {
            return Status::Starting;
        }
        // The lock is held only briefly unless a start is under way.
        let Ok(st) = tokio::time::timeout(Duration::from_millis(200), self.state.lock()).await
        else {
            return Status::Starting;
        };
        let failures = self.failures.load(Ordering::SeqCst);
        let last = st.last_error.clone().unwrap_or_default();
        match &st.conn {
            Some(c) if c.is_alive() => Status::Running(c.negotiated().clone()),
            _ if failures >= self.opts.max_failures => Status::GaveUp { last },
            Some(_) => Status::Failing {
                failures: failures + 1,
                last: "the server exited".into(),
            },
            None if failures > 0 => Status::Failing { failures, last },
            None => Status::Idle,
        }
    }

    /// The process group of a running stdio server (for tests and status).
    pub async fn pgid(&self) -> Option<i32> {
        let st = self.state.lock().await;
        match st.conn.as_deref().map(Connection::transport) {
            Some(Transport::Stdio(t)) => Some(t.pgid()),
            _ => None,
        }
    }

    /// Starts the server and completes the handshake if it is not running.
    pub async fn start(&self) -> Result<Negotiated> {
        Ok(self.connection().await?.negotiated().clone())
    }

    async fn open(&self) -> Result<Connection> {
        let limits = Limits {
            request: self.config.timeout,
            probe: self.opts.probe_timeout.min(self.config.startup_timeout),
        };
        let env = resolve_secrets(&self.name, &self.config.env, "env.").await?;
        let headers = resolve_secrets(&self.name, &self.config.headers, "header.").await?;
        let redactor = Arc::new(Redactor::new(
            self.opts
                .known_secrets
                .iter()
                .map(String::as_str)
                .chain(env.iter().chain(&headers).map(|(_, v)| v.expose())),
        ));
        if let Ok(mut r) = self.redactor.lock() {
            *r = redactor.clone();
        }
        let transport = match &self.config.transport {
            McpTransport::Stdio { command, args, cwd } => {
                Transport::Stdio(StdioTransport::spawn(SpawnSpec {
                    server: self.name.clone(),
                    command: command.clone(),
                    args: args.clone(),
                    cwd: cwd.clone(),
                    env,
                    scrub_env: self.opts.scrub_env.clone(),
                    scrub_values: self.opts.known_secrets.clone(),
                    redactor,
                    stderr_tail: self.stderr.clone(),
                })?)
            }
            McpTransport::Http { url } => {
                let t = HttpTransport::new(&self.name, url, headers, redactor)?;
                if self.config.uses_oauth() {
                    let Some(dir) = &self.opts.auth_dir else {
                        return Err(McpError::Auth {
                            server: self.name.clone(),
                            message: crate::auth::AuthError::NotAuthorized {
                                server: self.name.clone(),
                                reason: "no token store is configured".into(),
                            }
                            .to_string(),
                        });
                    };
                    let session =
                        OAuthSession::shared(McpTokenStore::new(dir.clone()), &self.name, url);
                    Transport::Http(t.with_oauth(session))
                } else {
                    Transport::Http(t)
                }
            }
        };
        Connection::open(&self.name, transport, limits).await
    }

    /// The live connection, starting or restarting the server as needed.
    async fn connection(&self) -> Result<Arc<Connection>> {
        if !self.config.enabled {
            return Err(McpError::Disabled(self.name.clone()));
        }
        if self.is_disabled_for_session() {
            return Err(McpError::DisabledForSession(self.name.clone()));
        }
        let mut st = self.state.lock().await;
        if let Some(c) = &st.conn {
            if c.is_alive() {
                return Ok(c.clone());
            }
            let why = c
                .death()
                .map_or_else(|| "the server exited".to_owned(), |e| e.to_string());
            tracing::warn!(server = %self.name, error = %why, "MCP server crashed");
            st.conn = None;
            self.record_failure(&mut st, why);
        }
        let failures = self.failures.load(Ordering::SeqCst);
        if failures >= self.opts.max_failures {
            return Err(McpError::GaveUp {
                server: self.name.clone(),
                failures,
                last: st.last_error.clone().unwrap_or_default(),
            });
        }
        self.starting.store(true, Ordering::SeqCst);
        self.bump_status();
        let _starting = StartingGuard(self);
        if let Some(at) = st.retry_at.take() {
            tokio::time::sleep_until(at).await;
        }
        let limit = self.config.startup_timeout;
        let opened = tokio::time::timeout(limit, self.open()).await;
        let result = match opened {
            Ok(r) => r,
            Err(_) => Err(McpError::StartupTimeout {
                server: self.name.clone(),
                limit,
                stderr: self.stderr.summary(": "),
            }),
        };
        match result {
            Ok(c) => {
                let c = Arc::new(c);
                st.conn = Some(c.clone());
                self.watch(&c);
                Ok(c)
            }
            Err(e) => {
                self.record_failure(&mut st, e.to_string());
                Err(e)
            }
        }
    }

    fn record_failure(&self, st: &mut State, why: String) {
        let n = self.failures.fetch_add(1, Ordering::SeqCst) + 1;
        st.retry_at = Some(Instant::now() + self.opts.backoff(n));
        st.last_error = Some(why);
    }

    /// Runs `op` on the connection. A successful answer resets the failure
    /// count; an expired legacy HTTP session is re-established once.
    async fn with_conn<T, F, Fut>(&self, op: F) -> Result<T>
    where
        F: Fn(Arc<Connection>) -> Fut,
        Fut: std::future::Future<Output = Result<T>>,
    {
        let conn = self.connection().await?;
        let result = match op(conn.clone()).await {
            Err(McpError::SessionExpired(_)) => {
                tracing::info!(server = %self.name, "MCP session expired; reconnecting");
                {
                    let mut st = self.state.lock().await;
                    if st.conn.as_ref().is_some_and(|c| Arc::ptr_eq(c, &conn)) {
                        st.conn = None;
                    }
                }
                op(self.connection().await?).await
            }
            other => other,
        };
        match &result {
            Ok(_) => self.failures.store(0, Ordering::SeqCst),
            Err(e) if !e.is_connection_lost() => self.failures.store(0, Ordering::SeqCst),
            Err(_) => {}
        }
        result
    }

    /// Checks the server answers.
    pub async fn ping(&self) -> Result<()> {
        self.with_conn(|c| async move { c.ping().await }).await
    }

    /// Lists the server's tools.
    pub async fn list_tools(&self) -> Result<Vec<Tool>> {
        self.with_conn(|c| async move { c.list_tools().await })
            .await
    }

    /// Calls tool `name`.
    pub async fn call_tool(&self, name: &str, arguments: Value) -> Result<CallToolResult> {
        self.with_conn(|c| {
            let arguments = arguments.clone();
            async move { c.call_tool(name, arguments).await }
        })
        .await
    }

    /// Sends any request (with the era's metadata) and returns the result.
    pub async fn request(&self, method: &str, params: Value) -> Result<Value> {
        self.with_conn(|c| {
            let params = params.clone();
            async move { c.call(method, params).await }
        })
        .await
    }

    /// Lists the server's prompts (none without the `prompts` capability).
    pub async fn list_prompts(&self) -> Result<Vec<Prompt>> {
        self.with_conn(|c| async move { c.list_prompts().await })
            .await
    }

    /// Gets prompt `name` with `arguments`.
    pub async fn get_prompt(
        &self,
        name: &str,
        arguments: &BTreeMap<String, String>,
    ) -> Result<GetPromptResult> {
        self.with_conn(|c| async move { c.get_prompt(name, arguments).await })
            .await
    }

    /// Lists the server's resources (none without the `resources`
    /// capability).
    pub async fn list_resources(&self) -> Result<Vec<Resource>> {
        self.with_conn(|c| async move { c.list_resources().await })
            .await
    }

    /// Reads resource `uri`.
    pub async fn read_resource(&self, uri: &str) -> Result<ReadResourceResult> {
        self.with_conn(|c| async move { c.read_resource(uri).await })
            .await
    }

    /// Stops the server. The next request starts it again.
    pub async fn shutdown(&self) {
        self.stop_watchers();
        let conn = self.state.lock().await.conn.take();
        if let Some(c) = conn {
            c.shutdown(self.opts.shutdown_grace).await;
            self.bump_status();
        }
    }
}

/// Clears [`ServerHandle`]'s `starting` flag when a start ends (also when
/// the start is cancelled).
struct StartingGuard<'a>(&'a ServerHandle);

impl Drop for StartingGuard<'_> {
    fn drop(&mut self) {
        self.0.starting.store(false, Ordering::SeqCst);
        self.0.bump_status();
    }
}

/// Resolves `env.*` / `header.*` values (commands run off the async
/// threads).
async fn resolve_secrets(
    server: &str,
    map: &BTreeMap<String, McpSecret>,
    prefix: &'static str,
) -> Result<Vec<(String, Secret)>> {
    let map = map.clone();
    let server = server.to_owned();
    let srv = server.clone();
    tokio::task::spawn_blocking(move || {
        let mut out = Vec::new();
        for (key, source) in &map {
            let fail = |message: String| McpError::Secret {
                server: server.clone(),
                key: format!("{prefix}{key}"),
                message,
            };
            match source.resolve() {
                Ok(Some(v)) => out.push((key.clone(), v)),
                Ok(None) => {
                    return Err(fail(format!(
                        "no value (the command printed nothing or the environment variable is unset); fix {prefix}{key}, {prefix}{key}_cmd or {prefix}{key}_env"
                    )));
                }
                Err(e) => {
                    return Err(fail(
                        e.to_string()
                            .replace("api_key_cmd", &format!("{prefix}{key}_cmd")),
                    ));
                }
            }
        }
        Ok(out)
    })
    .await
    .unwrap_or_else(|_| {
        Err(McpError::Secret {
            server: srv,
            key: prefix.trim_end_matches('.').to_owned(),
            message: "resolving the values panicked".into(),
        })
    })
}

/// Every `[mcp.*]` server of a config.
pub struct Servers {
    servers: BTreeMap<String, Arc<ServerHandle>>,
}

impl Servers {
    /// Handles for every server in `cfg` (disabled ones included, so they
    /// can be reported); nothing starts.
    pub fn from_config(cfg: &Config, opts: ClientOptions) -> Self {
        let opts = Arc::new(opts);
        Self {
            servers: cfg
                .mcp
                .iter()
                .map(|(name, s)| {
                    (
                        name.clone(),
                        Arc::new(ServerHandle::new(name, s.clone(), opts.clone())),
                    )
                })
                .collect(),
        }
    }

    /// The server called `name`.
    pub fn get(&self, name: &str) -> Option<&Arc<ServerHandle>> {
        self.servers.get(name)
    }

    /// All servers, by name.
    pub fn iter(&self) -> impl Iterator<Item = &Arc<ServerHandle>> {
        self.servers.values()
    }

    /// Starts every enabled `startup = eager` server in the background.
    pub fn start_eager(&self) {
        for s in self.servers.values() {
            if s.config.enabled && s.config.startup == McpStartup::Eager {
                let s = s.clone();
                tokio::spawn(async move {
                    if let Err(e) = s.start().await {
                        tracing::warn!(server = %s.name, error = %e, "MCP server failed to start");
                    }
                });
            }
        }
    }

    /// Stops every server.
    pub async fn shutdown_all(&self) {
        futures::future::join_all(self.servers.values().map(|s| s.shutdown())).await;
    }
}
