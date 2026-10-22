//! `harness doctor`: one command to diagnose configuration and environment
//! problems, each with an actionable fix.
//!
//! Checks, in order: config validity, credential file permissions,
//! credential resolution and provider connectivity, sandbox support,
//! remote reachability, prompt lint and shadowed definitions. Anything
//! that touches the network or spawns the sandbox launcher goes through
//! [`Probes`], so tests use fakes and [`LiveProbes`] does the real thing.

use std::fmt;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use harness_config::agents::ValidationContext;
use harness_config::discovery::{Catalog, Roots};
use harness_config::ini::{Config, load_location};
use harness_config::lint::{self, LintOptions, Severity};
use harness_config::paths::{ConfigLocation, DataDir, config_location};
use harness_config::trust::TrustStore;
use harness_policy::sandbox::{LandlockState, Launcher, SandboxConfig, SandboxStatus};
use harness_remote::hosts::{HostRegistry, RemoteHost};
use harness_remote::ssh::SshOptions;

/// Outcome of one check.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Status {
    /// Fine.
    Ok,
    /// Not checked (nothing to check, or not possible here).
    Skip,
    /// Works, but should be improved.
    Warn,
    /// Broken.
    Fail,
}

impl Status {
    fn tag(self) -> &'static str {
        match self {
            Status::Ok => "ok",
            Status::Skip => "skip",
            Status::Warn => "warn",
            Status::Fail => "FAIL",
        }
    }
}

/// One finding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Check {
    /// Area (`config`, `credentials`, `provider anthropic`, ...).
    pub area: String,
    /// Outcome.
    pub status: Status,
    /// What was found.
    pub message: String,
    /// What to do about it.
    pub fix: Option<String>,
}

impl Check {
    fn new(area: impl Into<String>, status: Status, message: impl Into<String>) -> Self {
        Self {
            area: area.into(),
            status,
            message: message.into(),
            fix: None,
        }
    }

    fn fix(mut self, fix: impl Into<String>) -> Self {
        self.fix = Some(fix.into());
        self
    }
}

impl fmt::Display for Check {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "[{:<4}] {}: {}",
            self.status.tag(),
            self.area,
            self.message
        )?;
        if let Some(fix) = &self.fix {
            write!(f, "\n       fix: {fix}")?;
        }
        Ok(())
    }
}

/// Everything doctor found.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Report {
    /// Findings in check order.
    pub checks: Vec<Check>,
}

impl Report {
    /// The worst status.
    pub fn worst(&self) -> Status {
        self.checks
            .iter()
            .map(|c| c.status)
            .max()
            .unwrap_or(Status::Ok)
    }

    /// Process exit code: 1 when any check failed.
    pub fn exit_code(&self) -> i32 {
        i32::from(self.worst() == Status::Fail)
    }

    /// Findings in `area`.
    pub fn area(&self, area: &str) -> Vec<&Check> {
        self.checks.iter().filter(|c| c.area == area).collect()
    }

    /// Terminal text.
    pub fn render(&self) -> String {
        let mut s = String::new();
        for c in &self.checks {
            s.push_str(&c.to_string());
            s.push('\n');
        }
        let (fails, warns) = self.checks.iter().fold((0, 0), |(f, w), c| match c.status {
            Status::Fail => (f + 1, w),
            Status::Warn => (f, w + 1),
            _ => (f, w),
        });
        s.push_str(&format!("{fails} problem(s), {warns} warning(s)\n"));
        s
    }
}

/// Network and process probes.
#[async_trait]
pub trait Probes: Send + Sync {
    /// Connects to a provider endpoint; returns the connect time.
    async fn provider(&self, name: &str, base_url: &str) -> Result<Duration, String>;
    /// Runs a trivial command on a remote host; returns the round trip.
    async fn remote(&self, host: &RemoteHost) -> Result<Duration, String>;
    /// Probes sandbox support (`None` = cannot probe here).
    fn sandbox(&self, cfg: &Config, root: &Path) -> Option<SandboxStatus>;
}

/// The real probes: TCP connect to providers, `ssh host true` for remotes,
/// and the sandbox launcher.
#[derive(Debug, Clone)]
pub struct LiveProbes {
    /// The sandbox launcher (the `harness` binary); `None` skips the probe.
    pub launcher: Option<Launcher>,
    /// Per-probe timeout.
    pub timeout: Duration,
}

impl Default for LiveProbes {
    fn default() -> Self {
        Self {
            launcher: Launcher::current_exe().ok(),
            timeout: Duration::from_secs(5),
        }
    }
}

/// `host:port` of an http(s) URL.
pub fn host_port(url: &str) -> Option<String> {
    let (scheme, rest) = url.split_once("://")?;
    let authority = rest.split(['/', '?', '#']).next()?;
    let authority = authority.rsplit('@').next()?;
    if authority.is_empty() {
        return None;
    }
    if authority.contains(':') && !authority.ends_with(']') {
        return Some(authority.to_owned());
    }
    let port = if scheme == "http" { 80 } else { 443 };
    Some(format!("{authority}:{port}"))
}

#[async_trait]
impl Probes for LiveProbes {
    async fn provider(&self, _name: &str, base_url: &str) -> Result<Duration, String> {
        let addr =
            host_port(base_url).ok_or_else(|| format!("`{base_url}` is not an http(s) URL"))?;
        let started = Instant::now();
        match tokio::time::timeout(self.timeout, tokio::net::TcpStream::connect(&addr)).await {
            Ok(Ok(_)) => Ok(started.elapsed()),
            Ok(Err(e)) => Err(format!("cannot connect to {addr}: {e}")),
            Err(_) => Err(format!(
                "connecting to {addr} timed out after {}s",
                self.timeout.as_secs()
            )),
        }
    }

    async fn remote(&self, host: &RemoteHost) -> Result<Duration, String> {
        let target = host.ssh_target(SshOptions::default());
        let started = Instant::now();
        match tokio::time::timeout(self.timeout * 4, target.run(&["true".into()], b"")).await {
            Ok(Ok(out)) if out.code == Some(0) => Ok(started.elapsed()),
            Ok(Ok(out)) => Err(format!(
                "`ssh {} true` exited with {:?}",
                host.name, out.code
            )),
            Ok(Err(e)) => Err(e.to_string()),
            Err(_) => Err("ssh timed out".into()),
        }
    }

    fn sandbox(&self, cfg: &Config, root: &Path) -> Option<SandboxStatus> {
        let launcher = self.launcher.as_ref()?;
        Some(SandboxStatus::probe(
            &SandboxConfig::from_ini(&cfg.sandbox, root),
            launcher,
        ))
    }
}

/// What to diagnose.
#[derive(Debug, Clone, Default)]
pub struct Input {
    /// Explicit config file (`None`: `$HARNESS_CONFIG` or the default path).
    pub config: Option<PathBuf>,
    /// Project directory (for project YAML and the sandbox probe).
    pub project: Option<PathBuf>,
    /// Global config directory override (tests); `None` = the real one.
    pub global_dir: Option<PathBuf>,
}

fn default_base_url(provider: &str) -> Option<&'static str> {
    match provider {
        harness_providers::anthropic::NAME => Some(harness_providers::anthropic::DEFAULT_BASE_URL),
        harness_providers::openai::PROVIDER_NAME => {
            Some(harness_providers::openai::DEFAULT_BASE_URL)
        }
        harness_providers::openrouter::PROVIDER_NAME => {
            Some(harness_providers::openrouter::DEFAULT_BASE_URL)
        }
        _ => None,
    }
}

fn config_checks(input: &Input, out: &mut Vec<Check>) -> Option<Config> {
    let location = match &input.config {
        Some(p) => ConfigLocation::Explicit(p.clone()),
        None => config_location(|k| std::env::var_os(k)),
    };
    let path = location.path().to_path_buf();
    if !location.is_explicit() && !path.exists() {
        out.push(Check::new(
            "config",
            Status::Ok,
            format!("{} not found; built-in defaults apply", path.display()),
        ));
        return Some(Config::defaults());
    }
    match load_location(&location) {
        Ok(cfg) => {
            out.push(Check::new(
                "config",
                Status::Ok,
                format!("{} is valid", path.display()),
            ));
            // Permissions.
            match harness_config::secrets::insecure_mode(&path) {
                Ok(Some(mode)) => {
                    let (status, what) = if cfg.has_inline_secrets() {
                        (
                            Status::Fail,
                            "contains api_key values and is readable by others",
                        )
                    } else {
                        (Status::Warn, "is readable by other users")
                    };
                    out.push(
                        Check::new(
                            "credentials",
                            status,
                            format!("{} (mode {mode:o}) {what}", path.display()),
                        )
                        .fix(format!("chmod 600 {}", path.display())),
                    );
                }
                Ok(None) => out.push(Check::new(
                    "credentials",
                    Status::Ok,
                    format!("{} is private to you", path.display()),
                )),
                Err(e) => out.push(Check::new(
                    "credentials",
                    Status::Warn,
                    format!("cannot stat {}: {e}", path.display()),
                )),
            }
            Some(cfg)
        }
        Err(errors) => {
            for e in errors.errors() {
                out.push(
                    Check::new("config", Status::Fail, e.to_string()).fix(format!(
                        "edit {} (see docs/config.md), then run `harness config check`",
                        path.display()
                    )),
                );
            }
            if errors
                .errors()
                .iter()
                .any(|e| matches!(e, harness_config::ini::IniError::InsecurePermissions { .. }))
            {
                out.push(
                    Check::new(
                        "credentials",
                        Status::Fail,
                        "inline api_key in a file others can read",
                    )
                    .fix(format!("chmod 600 {}", path.display())),
                );
            }
            None
        }
    }
}

async fn provider_checks(cfg: &Config, probes: &dyn Probes, out: &mut Vec<Check>) {
    if cfg.providers.is_empty() {
        out.push(
            Check::new("providers", Status::Warn, "no [provider.*] section is configured")
                .fix("add [provider.anthropic] with api_key_env = ANTHROPIC_API_KEY (see docs/config.md)"),
        );
        return;
    }
    for (name, p) in &cfg.providers {
        let area = format!("provider {name}");
        let auth = match p.resolve_api_key_with_origin() {
            Ok(Some((_, origin))) => p.auth_description(origin),
            Ok(None) => {
                out.push(
                    Check::new(&area, Status::Fail, "auth: none (no API key)").fix(format!(
                        "run `harness login {name}`, or set api_key_env, api_key_cmd or api_key in [provider.{name}]"
                    )),
                );
                continue;
            }
            Err(e) => {
                out.push(
                    Check::new(
                        &area,
                        Status::Fail,
                        format!("cannot resolve the API key: {e}"),
                    )
                    .fix(format!(
                        "check api_key_cmd / api_key_env in [provider.{name}]"
                    )),
                );
                continue;
            }
        };
        let Some(url) = p.base_url.as_deref().or_else(|| default_base_url(name)) else {
            out.push(Check::new(
                &area,
                Status::Skip,
                "unknown provider; connectivity not checked",
            ));
            continue;
        };
        match probes.provider(name, url).await {
            Ok(d) => out.push(Check::new(
                &area,
                Status::Ok,
                format!(
                    "auth: {auth}; key resolves; {url} reachable in {} ms",
                    d.as_millis()
                ),
            )),
            Err(e) => out.push(
                Check::new(&area, Status::Fail, format!("auth: {auth}; {e}")).fix(format!(
                    "check your network or proxy, or base_url in [provider.{name}]"
                )),
            ),
        }
    }
}

fn sandbox_checks(cfg: &Config, root: &Path, probes: &dyn Probes, out: &mut Vec<Check>) {
    if !cfg.sandbox.enabled {
        out.push(
            Check::new(
                "sandbox",
                Status::Warn,
                "the bash sandbox is disabled ([sandbox] enabled = false)",
            )
            .fix("set [sandbox] enabled = true"),
        );
        return;
    }
    let Some(status) = probes.sandbox(cfg, root) else {
        out.push(Check::new(
            "sandbox",
            Status::Skip,
            "cannot probe the sandbox launcher here",
        ));
        return;
    };
    let level = match (&status.landlock, status.warnings.is_empty()) {
        (LandlockState::Unsupported { .. }, _) => Status::Warn,
        (_, false) => Status::Warn,
        _ => Status::Ok,
    };
    let mut c = Check::new("sandbox", level, status.doctor_lines().join("; "));
    if level == Status::Warn {
        c = c.fix("use a kernel with Landlock (5.13+) and install bwrap for network isolation, or accept the reduced isolation");
    }
    out.push(c);
}

async fn remote_checks(cfg: &Config, probes: &dyn Probes, out: &mut Vec<Check>) {
    let reg = HostRegistry::from_config(cfg);
    for host in reg.iter() {
        let area = format!("remote {}", host.name);
        match probes.remote(host).await {
            Ok(d) => out.push(Check::new(&area, Status::Ok, format!("reachable over ssh in {} ms", d.as_millis()))),
            Err(e) => out.push(
                Check::new(&area, Status::Fail, e).fix(format!(
                    "check `ssh {}` works without a password prompt (keys in ssh-agent, host in known_hosts), or fix [remote.{}]",
                    host.config.host, host.name
                )),
            ),
        }
    }
}

fn catalog_checks(cfg: &Config, input: &Input, out: &mut Vec<Check>) {
    let data = DataDir::from_config(cfg);
    let trust = TrustStore::from_data_dir(&data);
    let builtins = harness_config::builtins::BUILTINS;
    let roots = match &input.global_dir {
        Some(g) => Roots::resolve_with(builtins, Some(g.clone()), &trust, input.project.as_deref()),
        None => Roots::resolve(builtins, &trust, input.project.as_deref()),
    };
    for n in &roots.notes {
        out.push(Check::new("project", Status::Skip, n.clone()));
    }
    let catalog = Catalog::load(&roots);
    let report = lint::lint_catalog(
        &catalog,
        Some(&ValidationContext::from_config(cfg)),
        &[],
        &LintOptions::default(),
    );
    if report.findings.is_empty() {
        out.push(Check::new(
            "lint",
            Status::Ok,
            "prompts, agents and skills pass prompt lint",
        ));
    }
    for f in &report.findings {
        let status = match f.severity {
            Severity::Error => Status::Fail,
            Severity::Warning => Status::Warn,
        };
        let mut c = Check::new("lint", status, format!("{}: {}", f.loc, f.message));
        if let Some(s) = &f.suggestion {
            c = c.fix(s.clone());
        }
        out.push(c);
    }
    let shadowed = catalog.shadowed();
    if shadowed.is_empty() {
        out.push(Check::new(
            "definitions",
            Status::Ok,
            "no shadowed definitions",
        ));
    }
    for s in shadowed {
        out.push(
            Check::new("definitions", Status::Warn, s.to_string())
                .fix("rename one of them if both should exist; otherwise nothing to do"),
        );
    }
}

/// Runs every check.
pub async fn diagnose(input: &Input, probes: &dyn Probes) -> Report {
    let mut checks = Vec::new();
    let Some(cfg) = config_checks(input, &mut checks) else {
        return Report { checks };
    };
    let root = input
        .project
        .clone()
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."));
    provider_checks(&cfg, probes, &mut checks).await;
    sandbox_checks(&cfg, &root, probes, &mut checks);
    remote_checks(&cfg, probes, &mut checks).await;
    catalog_checks(&cfg, input, &mut checks);
    Report { checks }
}

/// Entry point for `harness doctor`: prints the report, returns the exit
/// code (1 when something is broken).
pub async fn run(input: &Input, probes: &dyn Probes, out: &mut dyn Write) -> i32 {
    let report = diagnose(input, probes).await;
    let _ = write!(out, "{}", report.render());
    report.exit_code()
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fake {
        provider_ok: bool,
        remote_ok: bool,
        sandbox: Option<SandboxStatus>,
    }

    #[async_trait]
    impl Probes for Fake {
        async fn provider(&self, _n: &str, url: &str) -> Result<Duration, String> {
            if self.provider_ok {
                Ok(Duration::from_millis(3))
            } else {
                Err(format!("cannot connect to {url}"))
            }
        }
        async fn remote(&self, h: &RemoteHost) -> Result<Duration, String> {
            if self.remote_ok {
                Ok(Duration::from_millis(9))
            } else {
                Err(format!("{}: connection refused", h.name))
            }
        }
        fn sandbox(&self, _c: &Config, _r: &Path) -> Option<SandboxStatus> {
            self.sandbox.clone()
        }
    }

    fn write_ini(dir: &Path, text: &str, mode: u32) -> PathBuf {
        let p = dir.join("harness.ini");
        std::fs::write(&p, text).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(mode)).unwrap();
        }
        p
    }

    fn input(dir: &Path, cfg: PathBuf) -> Input {
        Input {
            config: Some(cfg),
            project: Some(dir.to_owned()),
            global_dir: Some(dir.join("global")),
        }
    }

    const GOOD: &str = "[general]\ndata_dir = DATA\n[provider.anthropic]\napi_key_env = DOCTOR_TEST_KEY_UNSET_X\n[remote.gpu]\nhost = 10.0.0.9\n[sandbox]\nenabled = true\n";

    #[tokio::test]
    async fn reports_problems_with_fixes() {
        let dir = tempfile::tempdir().unwrap();
        let ini = GOOD.replace("DATA", &dir.path().join("data").display().to_string());
        let p = write_ini(dir.path(), &ini, 0o644);
        let fake = Fake {
            provider_ok: false,
            remote_ok: false,
            sandbox: Some(SandboxStatus {
                landlock: LandlockState::Unsupported {
                    reason: "kernel too old".into(),
                },
                network: harness_policy::sandbox::NetworkPolicy::Allow,
                net_tool: None,
                warnings: vec!["landlock unavailable".into()],
            }),
        };
        let r = diagnose(&input(dir.path(), p.clone()), &fake).await;
        let text = r.render();
        assert_eq!(r.area("config")[0].status, Status::Ok);
        let perm = r.area("credentials")[0];
        assert_eq!(perm.status, Status::Warn, "{text}");
        assert!(perm.fix.as_deref().unwrap().starts_with("chmod 600"));
        let prov = r.area("provider anthropic")[0];
        assert_eq!(prov.status, Status::Fail);
        assert!(prov.message.contains("no API key"), "{text}");
        assert!(prov.fix.as_deref().unwrap().contains("api_key_env"));
        let sb = r.area("sandbox")[0];
        assert_eq!(sb.status, Status::Warn);
        assert!(sb.fix.is_some());
        let rem = r.area("remote gpu")[0];
        assert_eq!(rem.status, Status::Fail);
        assert!(rem.fix.as_deref().unwrap().contains("ssh 10.0.0.9"));
        assert_eq!(
            r.area("lint")[0].status,
            Status::Ok,
            "built-in defaults lint clean"
        );
        assert_eq!(r.area("definitions")[0].status, Status::Ok);
        assert_eq!(r.exit_code(), 1);
        assert!(text.contains("fix: chmod 600"));
    }

    #[tokio::test]
    async fn healthy_setup_passes() {
        let dir = tempfile::tempdir().unwrap();
        let ini = GOOD
            .replace("DATA", &dir.path().join("data").display().to_string())
            .replace(
                "api_key_env = DOCTOR_TEST_KEY_UNSET_X",
                "api_key_cmd = echo test-key-value",
            );
        let p = write_ini(dir.path(), &ini, 0o600);
        let fake = Fake {
            provider_ok: true,
            remote_ok: true,
            sandbox: Some(SandboxStatus {
                landlock: LandlockState::Full,
                network: harness_policy::sandbox::NetworkPolicy::Allow,
                net_tool: None,
                warnings: vec![],
            }),
        };
        let r = diagnose(&input(dir.path(), p), &fake).await;
        assert_eq!(r.worst(), Status::Ok, "{}", r.render());
        assert_eq!(r.exit_code(), 0);
        let prov = r.area("provider anthropic")[0];
        assert!(
            prov.message
                .starts_with("auth: API key (api_key_cmd); key resolves"),
            "{}",
            prov.message
        );
    }

    #[tokio::test]
    async fn shows_the_auth_method_in_use_per_provider() {
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().join("data").display().to_string();
        let ini = format!(
            "[general]\ndata_dir = {data}\n[provider.anthropic]\napi_key = sk-inline-doctor\n[provider.openai]\napi_key_env = PATH\n[provider.openrouter]\napi_key_cmd = printf ''\napi_key_env = DOCTOR_TEST_AUTH_UNSET_Y\n"
        );
        let p = write_ini(dir.path(), &ini, 0o600);
        let fake = Fake {
            provider_ok: true,
            remote_ok: true,
            sandbox: None,
        };
        let r = diagnose(&input(dir.path(), p), &fake).await;
        let text = r.render();
        let msg = |a: &str| r.area(a)[0].message.clone();
        assert!(
            msg("provider anthropic").starts_with("auth: API key (api_key in harness.ini);"),
            "{text}"
        );
        assert!(
            msg("provider openai").starts_with("auth: API key (api_key_env PATH);"),
            "{text}"
        );
        assert_eq!(
            msg("provider openrouter"),
            "auth: none (no API key)",
            "{text}"
        );
        assert!(
            r.area("provider openrouter")[0]
                .fix
                .as_deref()
                .unwrap()
                .contains("harness login openrouter")
        );
        assert!(!text.contains("sk-inline-doctor"), "{text}");
    }

    #[tokio::test]
    async fn invalid_config_and_lint_and_shadowing() {
        let dir = tempfile::tempdir().unwrap();
        let p = write_ini(dir.path(), "[general]\nbogus = 1\n", 0o600);
        let fake = Fake {
            provider_ok: true,
            remote_ok: true,
            sandbox: None,
        };
        let r = diagnose(&input(dir.path(), p), &fake).await;
        assert_eq!(r.checks[0].status, Status::Fail);
        assert!(r.checks[0].message.contains("bogus"));
        assert!(
            r.checks[0]
                .fix
                .as_deref()
                .unwrap()
                .contains("harness config check")
        );

        // A global prompt with a blanket prohibition that shadows a built-in.
        let g = dir.path().join("global/prompts");
        std::fs::create_dir_all(&g).unwrap();
        std::fs::write(
            g.join("coder.yaml"),
            "name: coder\nlayers:\n  - text: Do not modify the tests.\n",
        )
        .unwrap();
        let ini = format!(
            "[general]\ndata_dir = {}\n",
            dir.path().join("data").display()
        );
        let p = write_ini(dir.path(), &ini, 0o600);
        let r = diagnose(&input(dir.path(), p), &fake).await;
        let lint = r.area("lint");
        assert!(
            lint.iter()
                .any(|c| c.status == Status::Fail && c.message.contains("blanket prohibition")),
            "{}",
            r.render()
        );
        assert!(lint.iter().any(|c| c.fix.is_some()));
        let defs = r.area("definitions");
        assert!(
            defs.iter()
                .any(|c| c.status == Status::Warn && c.message.contains("coder")),
            "{}",
            r.render()
        );
        assert_eq!(r.area("providers")[0].status, Status::Warn);
        assert_eq!(r.area("sandbox")[0].status, Status::Skip);
    }

    #[tokio::test]
    async fn live_provider_probe_connects_locally() {
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = l.local_addr().unwrap().port();
        let probes = LiveProbes {
            launcher: None,
            timeout: Duration::from_secs(2),
        };
        assert!(
            probes
                .provider("x", &format!("http://127.0.0.1:{port}/v1"))
                .await
                .is_ok()
        );
        drop(l);
        assert!(probes.provider("x", "not a url").await.is_err());
        assert_eq!(
            host_port("https://api.anthropic.com"),
            Some("api.anthropic.com:443".into())
        );
        assert_eq!(
            host_port("http://localhost:8080/x"),
            Some("localhost:8080".into())
        );
    }
}
