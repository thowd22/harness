//! `[mcp.<name>]` sections: MCP (Model Context Protocol) servers.
//!
//! Servers are user-granted, so they exist only in `harness.ini`; project
//! YAML may reference a server by name but never define one. A section
//! defines either a local stdio server (`command`, `args`, `env.*`, `cwd`)
//! or a remote streamable-HTTP server (`url`, `header.*`), plus `enabled`,
//! `timeout`, `startup` and `startup_timeout`.
//!
//! Every `env.<NAME>` and `header.<Name>` value is a credential and uses the
//! same three sources as a provider key: `env.<NAME>` (inline, like
//! `api_key`), `env.<NAME>_cmd` (a command printing it, like `api_key_cmd`)
//! and `env.<NAME>_env` (an environment variable holding it, like
//! `api_key_env`). The values are [`Secret`]s: they never print, and the MCP
//! client adds them to its redactor. See
//! `backlog/docs/doc-2 - harness.ini-Reference.md`.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use serde::Serialize;

use super::document::Entry;
use super::value::{SectionReader, config_enum};
use crate::secrets::{self, ResolveOptions, Secret, SecretError, SecretOrigin, SecretSources};

/// Default per-request time limit (`timeout`).
pub const DEFAULT_MCP_TIMEOUT: Duration = Duration::from_secs(60);

/// Default limit for starting a server and completing the handshake
/// (`startup_timeout`).
pub const DEFAULT_MCP_STARTUP_TIMEOUT: Duration = Duration::from_secs(30);

config_enum!(
    /// When a server is started (`[mcp.<name>] startup`).
    McpStartup { /// On first use (the default).
        Lazy = "lazy", /// When the session starts.
        Eager = "eager" }
);

config_enum!(
    /// How harness authorizes with a remote server (`[mcp.<name>] auth`).
    McpAuth { /// Only the configured `header.*` values (the default).
        None = "none", /// OAuth per the MCP authorization spec: tokens from
        /// `harness login mcp <name>`, kept in the protected token store.
        OAuth = "oauth" }
);

/// How harness talks to an MCP server.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(untagged)]
pub enum McpTransport {
    /// A local subprocess speaking newline-delimited JSON-RPC on stdin and
    /// stdout.
    Stdio {
        /// Program to run (`command`), looked up on `PATH`.
        command: String,
        /// Arguments (`args`, comma-separated).
        args: Vec<String>,
        /// Working directory (`cwd`, `~` expanded); `None` = harness's own.
        cwd: Option<PathBuf>,
    },
    /// A remote server reached with the streamable HTTP transport.
    Http {
        /// The MCP endpoint (`url`), `http://` or `https://`.
        url: String,
    },
}

/// One credential value from `key`, `key_cmd` or `key_env`.
#[derive(Debug, Clone, Default, Serialize)]
pub struct McpSecret {
    /// Inline value. Serialized as `[redacted]`.
    pub inline: Option<Secret>,
    /// Shell command printing the value (`<key>_cmd`).
    pub cmd: Option<String>,
    /// Environment variable holding the value (`<key>_env`).
    pub env: Option<String>,
}

impl McpSecret {
    /// Resolves the value: inline, then `_cmd`, then `_env`; the first
    /// non-empty value wins. `Ok(None)` when no source yields a value.
    pub fn resolve(&self) -> Result<Option<Secret>, SecretError> {
        self.resolve_with(&ResolveOptions::default())
            .map(|r| r.map(|(s, _)| s))
    }

    /// Like [`resolve`](Self::resolve) with explicit options, also saying
    /// which source supplied the value.
    pub fn resolve_with(
        &self,
        opts: &ResolveOptions,
    ) -> Result<Option<(Secret, SecretOrigin)>, SecretError> {
        secrets::resolve(
            &SecretSources {
                inline: self.inline.as_ref(),
                cmd: self.cmd.as_deref(),
                env: self.env.as_deref(),
            },
            opts,
        )
    }
}

/// `[mcp.<name>]`: one MCP server.
#[derive(Debug, Clone, Serialize)]
pub struct McpServer {
    /// stdio (`command`) or streamable HTTP (`url`).
    #[serde(flatten)]
    pub transport: McpTransport,
    /// stdio only: extra environment for the server (`env.<NAME>`,
    /// `env.<NAME>_cmd`, `env.<NAME>_env`), keyed by variable name.
    #[serde(serialize_with = "ser_secrets")]
    pub env: BTreeMap<String, McpSecret>,
    /// HTTP only: extra request headers (`header.<Name>`,
    /// `header.<Name>_cmd`, `header.<Name>_env`), keyed by header name.
    #[serde(rename = "header", serialize_with = "ser_secrets")]
    pub headers: BTreeMap<String, McpSecret>,
    /// Use this server. Default true.
    pub enabled: bool,
    /// Per-request time limit. Default [`DEFAULT_MCP_TIMEOUT`].
    #[serde(serialize_with = "ser_duration")]
    pub timeout: Duration,
    /// When to start the server. Default [`McpStartup::Lazy`].
    pub startup: McpStartup,
    /// Limit for starting the server and completing the handshake.
    /// Default [`DEFAULT_MCP_STARTUP_TIMEOUT`].
    #[serde(serialize_with = "ser_duration")]
    pub startup_timeout: Duration,
    /// HTTP only: `none` (the default) or `oauth`.
    pub auth: McpAuth,
    /// HTTP only, OAuth: a client id registered with the authorization
    /// server ahead of time (`oauth_client_id`); wins over any other
    /// registration.
    pub oauth_client_id: Option<String>,
    /// HTTP only, OAuth: an `https://` URL of a Client ID Metadata Document
    /// that lists harness's loopback redirect (`oauth_client_metadata_url`),
    /// used when the authorization server supports such documents.
    pub oauth_client_metadata_url: Option<String>,
    /// HTTP only, OAuth: scopes to request (`oauth_scopes`, comma
    /// separated) instead of the ones the server asks for.
    pub oauth_scopes: Option<Vec<String>>,
}

impl McpServer {
    /// A stdio server running `command` with default settings.
    pub fn stdio(command: impl Into<String>, args: Vec<String>) -> Self {
        Self::with_transport(McpTransport::Stdio {
            command: command.into(),
            args,
            cwd: None,
        })
    }

    /// A streamable-HTTP server at `url` with default settings.
    pub fn http(url: impl Into<String>) -> Self {
        Self::with_transport(McpTransport::Http { url: url.into() })
    }

    fn with_transport(transport: McpTransport) -> Self {
        Self {
            transport,
            env: BTreeMap::new(),
            headers: BTreeMap::new(),
            enabled: true,
            timeout: DEFAULT_MCP_TIMEOUT,
            startup: McpStartup::Lazy,
            startup_timeout: DEFAULT_MCP_STARTUP_TIMEOUT,
            auth: McpAuth::None,
            oauth_client_id: None,
            oauth_client_metadata_url: None,
            oauth_scopes: None,
        }
    }

    /// True for a remote server with `auth = oauth`.
    pub fn uses_oauth(&self) -> bool {
        self.auth == McpAuth::OAuth && matches!(self.transport, McpTransport::Http { .. })
    }

    /// True when any `env.*` or `header.*` value is written inline.
    pub fn has_inline_secrets(&self) -> bool {
        self.env
            .values()
            .chain(self.headers.values())
            .any(|s| s.inline.is_some())
    }

    /// Environment variable names read by `env.*_env` / `header.*_env`.
    pub fn secret_env_vars(&self) -> impl Iterator<Item = &str> {
        self.env
            .values()
            .chain(self.headers.values())
            .filter_map(|s| s.env.as_deref())
    }
}

/// Serializes `env` / `header` maps with their ini key suffixes
/// (`NAME`, `NAME_cmd`, `NAME_env`); inline values print as `[redacted]`.
fn ser_secrets<S: serde::Serializer>(
    map: &BTreeMap<String, McpSecret>,
    s: S,
) -> Result<S::Ok, S::Error> {
    use serde::ser::SerializeMap;
    let mut m = s.serialize_map(None)?;
    for (name, v) in map {
        if let Some(i) = &v.inline {
            m.serialize_entry(name, i)?;
        }
        if let Some(c) = &v.cmd {
            m.serialize_entry(&format!("{name}_cmd"), c)?;
        }
        if let Some(e) = &v.env {
            m.serialize_entry(&format!("{name}_env"), e)?;
        }
    }
    m.end()
}

fn ser_duration<S: serde::Serializer>(d: &Duration, s: S) -> Result<S::Ok, S::Error> {
    s.serialize_f64(d.as_secs_f64())
}

/// True for a valid server name: letters, digits, `_` and `-`.
pub fn valid_server_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// True for an HTTP header field name (RFC 9110 `token`).
fn valid_header_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b))
}

/// True for a portable environment variable name.
fn valid_env_name(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with(|c: char| c.is_ascii_digit())
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Which source a `env.*` / `header.*` key sets.
enum Source {
    Inline,
    Cmd,
    Env,
}

/// Splits `NAME`, `NAME_cmd` or `NAME_env` into the name and its source.
fn split_source(key: &str) -> (&str, Source) {
    if let Some(n) = key.strip_suffix("_cmd") {
        (n, Source::Cmd)
    } else if let Some(n) = key.strip_suffix("_env") {
        (n, Source::Env)
    } else {
        (key, Source::Inline)
    }
}

/// Reads the `env.*` or `header.*` entries of a section.
fn read_secrets<'a>(
    r: &mut SectionReader<'a>,
    prefix: &str,
    valid: fn(&str) -> bool,
    what: &str,
) -> BTreeMap<String, McpSecret> {
    let mut out: BTreeMap<String, McpSecret> = BTreeMap::new();
    let section = r.section();
    for e in &section.entries {
        let Some(rest) = e.key.strip_prefix(prefix) else {
            continue;
        };
        let entry: &'a Entry = r.entry(&e.key).expect("entry exists");
        let (name, source) = split_source(rest);
        if !valid(name) {
            r.invalid(entry, format!("`{name}` is not a valid {what} name"));
            continue;
        }
        let slot = out.entry(name.to_owned()).or_default();
        match source {
            Source::Inline => slot.inline = Some(Secret::new(entry.value.clone())),
            Source::Cmd if entry.value.trim().is_empty() => {
                r.invalid(entry, "expected a shell command that prints the value")
            }
            Source::Cmd => slot.cmd = Some(entry.value.clone()),
            Source::Env if !valid_env_name(&entry.value) => r.invalid(
                entry,
                "expected the name of an environment variable holding the value",
            ),
            Source::Env => slot.env = Some(entry.value.clone()),
        }
    }
    out
}

/// Reads one `[mcp.<name>]` section. Problems are recorded on `r`;
/// `None` when the section cannot describe a server.
pub(crate) fn read_mcp(r: &mut SectionReader<'_>) -> Option<McpServer> {
    let command = r.nonempty_string("command");
    let args = r.list("args").unwrap_or_default();
    let cwd = r.path("cwd");
    let url = r.nonempty_string("url");
    let enabled = r.bool("enabled").unwrap_or(true);
    let timeout = positive_duration(r, "timeout").unwrap_or(DEFAULT_MCP_TIMEOUT);
    let startup = r.enumerated("startup").unwrap_or(McpStartup::Lazy);
    let startup_timeout =
        positive_duration(r, "startup_timeout").unwrap_or(DEFAULT_MCP_STARTUP_TIMEOUT);
    let env = read_secrets(r, "env.", valid_env_name, "environment variable");
    let headers = read_secrets(r, "header.", valid_header_name, "header");
    let auth = r.enumerated("auth").unwrap_or(McpAuth::None);
    let oauth_client_id = r.nonempty_string("oauth_client_id");
    let oauth_client_metadata_url = r.nonempty_string("oauth_client_metadata_url");
    let oauth_scopes = r.list("oauth_scopes");

    let section = r.section();
    let transport = match (command, url) {
        (Some(_), Some(_)) => {
            let entry = section.get("url").expect("url was read");
            r.invalid(
                entry,
                "set either `command` (a local stdio server) or `url` (a remote server), not both",
            );
            return None;
        }
        (Some(command), None) => {
            if let Some(e) = section
                .entries
                .iter()
                .find(|e| e.key.starts_with("header."))
            {
                r.invalid(
                    e,
                    "headers apply only to remote servers (`url`); use `env.*` for a stdio server",
                );
            }
            for key in OAUTH_KEYS {
                if let Some(e) = section.get(key) {
                    r.invalid(e, format!("`{key}` applies only to remote servers (`url`)"));
                }
            }
            McpTransport::Stdio { command, args, cwd }
        }
        (None, Some(url)) => {
            for key in ["args", "cwd"] {
                if let Some(e) = section.get(key) {
                    r.invalid(
                        e,
                        format!("`{key}` applies only to stdio servers (`command`)"),
                    );
                }
            }
            check_oauth_keys(r, auth, oauth_client_metadata_url.as_deref());
            if auth == McpAuth::OAuth && url.starts_with("http://") && !loopback_http(&url) {
                let entry = section.get("url").expect("url was read");
                r.invalid(
                    entry,
                    "`auth = oauth` needs an https:// url (http:// only on a loopback host such as 127.0.0.1), because OAuth bearer tokens must never travel unencrypted",
                );
            }
            if let Some(e) = section.entries.iter().find(|e| e.key.starts_with("env.")) {
                r.invalid(
                    e,
                    "`env.*` applies only to stdio servers (`command`); use `header.*` for a remote server",
                );
            }
            if !(url.starts_with("http://") || url.starts_with("https://")) {
                let entry = section.get("url").expect("url was read");
                r.invalid(entry, "expected an http:// or https:// URL");
                return None;
            }
            McpTransport::Http { url }
        }
        (None, None) => {
            if section.get("command").is_none() && section.get("url").is_none() {
                r.missing("command` or `url");
            }
            return None;
        }
    };
    Some(McpServer {
        transport,
        env,
        headers,
        enabled,
        timeout,
        startup,
        startup_timeout,
        auth,
        oauth_client_id,
        oauth_client_metadata_url,
        oauth_scopes,
    })
}

/// The OAuth keys, which only remote servers take.
const OAUTH_KEYS: [&str; 4] = [
    "auth",
    "oauth_client_id",
    "oauth_client_metadata_url",
    "oauth_scopes",
];

/// True when `url` is `http://` on a loopback host (`127.0.0.1`,
/// `localhost` or `[::1]`), the only place plain HTTP may carry OAuth
/// tokens (RFC 6750 section 5.3, OAuth 2.1 section 1.5).
fn loopback_http(url: &str) -> bool {
    let Some(rest) = url
        .get(..7)
        .filter(|p| p.eq_ignore_ascii_case("http://"))
        .map(|_| &url[7..])
    else {
        return false;
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    let host_port = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    let host = if host_port.starts_with('[') {
        host_port.find(']').map_or(host_port, |i| &host_port[..=i])
    } else {
        host_port.split(':').next().unwrap_or_default()
    };
    ["127.0.0.1", "localhost", "[::1]"]
        .iter()
        .any(|h| host.eq_ignore_ascii_case(h))
}

/// Checks the OAuth keys of a remote server.
fn check_oauth_keys(r: &mut SectionReader<'_>, auth: McpAuth, metadata_url: Option<&str>) {
    let section = r.section();
    if auth == McpAuth::OAuth {
        if let Some(e) = section.entries.iter().find(|e| {
            e.key
                .strip_prefix("header.")
                .is_some_and(|k| split_source(k).0.eq_ignore_ascii_case("authorization"))
        }) {
            r.invalid(
                e,
                "`auth = oauth` sends its own Authorization header; remove header.Authorization or set auth = none",
            );
        }
    } else {
        for key in &OAUTH_KEYS[1..] {
            if let Some(e) = section.get(key) {
                r.invalid(e, format!("`{key}` needs `auth = oauth`"));
            }
        }
    }
    if let Some(u) = metadata_url {
        let path_ok = u
            .strip_prefix("https://")
            .and_then(|rest| rest.split_once('/'))
            .is_some_and(|(host, path)| !host.is_empty() && !path.is_empty());
        if !path_ok {
            let entry = section
                .get("oauth_client_metadata_url")
                .expect("key was read");
            r.invalid(
                entry,
                "expected an https:// URL with a path (a Client ID Metadata Document)",
            );
        }
    }
}

/// Reads a duration that must be greater than zero.
fn positive_duration(r: &mut SectionReader<'_>, key: &'static str) -> Option<Duration> {
    let d = r.duration(key)?;
    if d.is_zero() {
        let entry = r.section().get(key).expect("key was just read");
        r.invalid(entry, "must be greater than 0");
        return None;
    }
    Some(d)
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use crate::ini::config::Config;
    use crate::ini::document::parse;
    use crate::ini::error::IniError;

    fn load(text: &str) -> Result<Config, Vec<IniError>> {
        let doc = parse(text, Some(Path::new("/cfg/harness.ini")))?;
        Config::from_document(
            &doc,
            Config::defaults_with_data_dir("/data".into()),
            Some(Path::new("/home/me")),
        )
    }

    fn errors(text: &str) -> Vec<String> {
        load(text)
            .unwrap_err()
            .iter()
            .map(ToString::to_string)
            .collect()
    }

    #[test]
    fn stdio_server_with_all_settings() {
        let c = load(
            "[mcp.github]\ncommand = npx\nargs = -y, @modelcontextprotocol/server-github\n\
             cwd = ~/src\nenv.GITHUB_TOKEN_cmd = gh auth token\nenv.LOG_LEVEL = debug\n\
             env.OTHER_env = OTHER_TOKEN\nenabled = false\ntimeout = 5s\nstartup = eager\n\
             startup_timeout = 2m\n",
        )
        .unwrap();
        let s = &c.mcp["github"];
        assert_eq!(
            s.transport,
            McpTransport::Stdio {
                command: "npx".into(),
                args: vec!["-y".into(), "@modelcontextprotocol/server-github".into()],
                cwd: Some(PathBuf::from("/home/me/src")),
            }
        );
        assert!(!s.enabled);
        assert_eq!(s.timeout, Duration::from_secs(5));
        assert_eq!(s.startup, McpStartup::Eager);
        assert_eq!(s.startup_timeout, Duration::from_secs(120));
        assert_eq!(s.env["GITHUB_TOKEN"].cmd.as_deref(), Some("gh auth token"));
        assert_eq!(
            s.env["LOG_LEVEL"].inline.as_ref().unwrap().expose(),
            "debug"
        );
        assert_eq!(s.env["OTHER"].env.as_deref(), Some("OTHER_TOKEN"));
        assert!(s.has_inline_secrets());
        assert!(c.has_inline_secrets());
        assert!(c.secret_env_vars().contains(&"OTHER_TOKEN".to_owned()));
    }

    #[test]
    fn defaults_apply() {
        let c = load("[mcp.fs]\ncommand = mcp-fs\n").unwrap();
        let s = &c.mcp["fs"];
        assert!(s.enabled);
        assert_eq!(s.timeout, DEFAULT_MCP_TIMEOUT);
        assert_eq!(s.startup, McpStartup::Lazy);
        assert_eq!(s.startup_timeout, DEFAULT_MCP_STARTUP_TIMEOUT);
        assert!(!c.has_inline_secrets());
    }

    #[test]
    fn remote_server_with_headers() {
        let c = load(
            "[mcp.docs]\nurl = https://mcp.example.com/mcp\nheader.Authorization_env = DOCS_TOKEN\nheader.X-Team = blue\n",
        )
        .unwrap();
        let s = &c.mcp["docs"];
        assert_eq!(
            s.transport,
            McpTransport::Http {
                url: "https://mcp.example.com/mcp".into()
            }
        );
        assert_eq!(
            s.headers["Authorization"].env.as_deref(),
            Some("DOCS_TOKEN")
        );
        assert!(s.headers["X-Team"].inline.is_some());
    }

    #[test]
    fn secrets_resolve_like_provider_keys() {
        let c = load(
            "[mcp.a]\ncommand = x\nenv.A = inline-value\nenv.A_cmd = printf from-cmd\nenv.B_cmd = printf from-cmd\nenv.C_env = HARNESS_TEST_MCP_C\n",
        )
        .unwrap();
        let s = &c.mcp["a"];
        let opts = || ResolveOptions {
            env_lookup: Box::new(|k| (k == "HARNESS_TEST_MCP_C").then(|| "from-env".to_owned())),
            ..ResolveOptions::default()
        };
        let get = |k: &str| {
            s.env[k]
                .resolve_with(&opts())
                .unwrap()
                .map(|(v, o)| (v.expose().to_owned(), o))
        };
        assert_eq!(
            get("A"),
            Some(("inline-value".into(), SecretOrigin::Inline))
        );
        assert_eq!(get("B"), Some(("from-cmd".into(), SecretOrigin::Command)));
        assert_eq!(get("C"), Some(("from-env".into(), SecretOrigin::Env)));
    }

    #[test]
    fn secrets_never_print() {
        let c = load("[mcp.a]\nurl = http://h/mcp\nheader.Authorization = Bearer sk-very-secret\n")
            .unwrap();
        let debug = format!("{c:?}");
        let json = serde_json::to_string(&c).unwrap();
        assert!(!debug.contains("sk-very-secret"), "{debug}");
        assert!(!json.contains("sk-very-secret"), "{json}");
        assert!(json.contains("\"url\":\"http://h/mcp\""), "{json}");
        assert!(
            json.contains("\"header\":{\"Authorization\":\"[redacted]\"}"),
            "{json}"
        );
    }

    #[test]
    fn oauth_keys_on_remote_servers() {
        let c = load(
            "[mcp.docs]\nurl = https://mcp.example.com/mcp\nauth = oauth\noauth_client_id = harness-123\n\
             oauth_client_metadata_url = https://example.com/harness.json\noauth_scopes = read, write\n",
        )
        .unwrap();
        let s = &c.mcp["docs"];
        assert_eq!(s.auth, McpAuth::OAuth);
        assert!(s.uses_oauth());
        assert_eq!(s.oauth_client_id.as_deref(), Some("harness-123"));
        assert_eq!(
            s.oauth_client_metadata_url.as_deref(),
            Some("https://example.com/harness.json")
        );
        assert_eq!(
            s.oauth_scopes.as_deref(),
            Some(&["read".to_owned(), "write".to_owned()][..])
        );
        let plain = load("[mcp.docs]\nurl = https://h/mcp\n").unwrap();
        assert_eq!(plain.mcp["docs"].auth, McpAuth::None);
        assert!(!plain.mcp["docs"].uses_oauth());
    }

    #[test]
    fn oauth_keys_are_checked() {
        let e = errors("[mcp.a]\ncommand = x\nauth = oauth\n");
        assert!(e[0].contains("only to remote servers"), "{e:?}");
        let e = errors("[mcp.a]\nurl = https://h/mcp\noauth_client_id = x\n");
        assert!(e[0].contains("needs `auth = oauth`"), "{e:?}");
        let e =
            errors("[mcp.a]\nurl = https://h/mcp\nauth = oauth\nheader.Authorization_env = T\n");
        assert!(e[0].contains("remove header.Authorization"), "{e:?}");
        let e = errors(
            "[mcp.a]\nurl = https://h/mcp\nauth = oauth\noauth_client_metadata_url = http://h/x.json\n",
        );
        assert!(e[0].contains("https:// URL with a path"), "{e:?}");
        let e = errors("[mcp.a]\nurl = https://h/mcp\nauth = basic\n");
        assert!(e[0].contains("oauth"), "{e:?}");
    }

    #[test]
    fn oauth_needs_tls_except_on_loopback() {
        for url in [
            "http://example.com/mcp",
            "http://mcp.example.com:8080/mcp",
            "http://127.0.0.1.evil.example/mcp",
            "http://localhost@evil.example/mcp",
            "http://10.0.0.5/mcp",
        ] {
            let e = errors(&format!("[mcp.a]\nurl = {url}\nauth = oauth\n"));
            assert!(e[0].contains("needs an https:// url"), "{url}: {e:?}");
        }
        for url in [
            "https://mcp.example.com/mcp",
            "http://127.0.0.1:8123/mcp",
            "http://localhost:9/mcp",
            "http://[::1]:9/mcp",
            "http://user@127.0.0.1/mcp",
        ] {
            let c = load(&format!("[mcp.a]\nurl = {url}\nauth = oauth\n")).unwrap();
            assert!(c.mcp["a"].uses_oauth(), "{url}");
        }
        // Without OAuth, plain http stays allowed (header.* is the user's call).
        load("[mcp.a]\nurl = http://example.com/mcp\n").unwrap();
    }

    #[test]
    fn transport_must_be_exactly_one() {
        let e = errors("[mcp.a]\nenabled = true\n");
        assert!(
            e[0].contains("missing required key `command` or `url`"),
            "{e:?}"
        );
        let e = errors("[mcp.a]\ncommand = x\nurl = http://h\n");
        assert!(e[0].contains("not both"), "{e:?}");
    }

    #[test]
    fn transport_specific_keys_are_checked() {
        let e = errors("[mcp.a]\nurl = http://h\nargs = x\nenv.X = 1\n");
        assert_eq!(e.len(), 2, "{e:?}");
        assert!(e[0].contains("`args` applies only to stdio servers"));
        assert!(e[1].contains("use `header.*` for a remote server"));
        let e = errors("[mcp.a]\ncommand = x\nheader.Auth = 1\n");
        assert!(
            e[0].contains("headers apply only to remote servers"),
            "{e:?}"
        );
        let e = errors("[mcp.a]\nurl = ftp://h\n");
        assert!(e[0].contains("http:// or https://"), "{e:?}");
    }

    #[test]
    fn invalid_values_and_unknown_keys() {
        let e = errors(
            "[mcp.a]\ncommand = x\ntimeout = 0\nstartup = later\nenv.1BAD = x\nenv.X_env = not a var\ncomand = y\n",
        );
        assert_eq!(e.len(), 5, "{e:#?}");
        assert!(
            e[0].contains(":3: invalid value for `timeout` in [mcp.a]: must be greater than 0")
        );
        assert!(e[1].contains("expected one of lazy | eager"));
        assert!(e[2].contains("`1BAD` is not a valid environment variable name"));
        assert!(e[3].contains("name of an environment variable"));
        assert!(e[4].contains("unknown key `comand` in [mcp.a] (did you mean `command`?)"));
    }

    #[test]
    fn server_names_are_restricted() {
        let e = errors("[mcp.my.server]\ncommand = x\n");
        assert!(e[0].contains("MCP server name"), "{e:?}");
        let e = errors("[mcp]\ncommand = x\n");
        assert!(e[0].contains("unknown section [mcp]"), "{e:?}");
        assert!(load("[mcp.my_server-2]\ncommand = x\n").is_ok());
    }
}
