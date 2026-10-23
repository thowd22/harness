//! The MCP OAuth token store: one JSON file per server,
//! `<data_dir>/auth/mcp/<name>.json`, written atomically with mode `0600`
//! in `0700` directories. The directory lies inside the data dir's
//! protected `auth/` directory (agent tools can never read it) and is never
//! synced to remote hosts.
//!
//! A record is bound to the server URL it was authorized for and to the
//! issuer of its client registration: tokens are only ever sent to that
//! URL, and a registration is only reused with that issuer.

use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use super::{AuthError, Token};

/// How the client id was obtained.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Registration {
    /// `oauth_client_id` from harness.ini.
    PreRegistered,
    /// `oauth_client_metadata_url` (a Client ID Metadata Document).
    MetadataDocument,
    /// Dynamic Client Registration (RFC 7591).
    Dynamic,
}

impl Registration {
    /// For messages.
    pub fn label(self) -> &'static str {
        match self {
            Registration::PreRegistered => "pre-registered client",
            Registration::MetadataDocument => "client metadata document",
            Registration::Dynamic => "dynamic client registration",
        }
    }
}

/// The client harness uses with one authorization server.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientRegistration {
    /// The client id.
    pub client_id: String,
    /// A client secret, if the server issued one with registration.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_secret: Option<Token>,
    /// How it was obtained.
    pub kind: Registration,
    /// Dynamic registration only: the registered loopback redirect URI.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub redirect_uri: Option<String>,
}

/// Stored tokens.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpTokens {
    /// Bearer token for MCP requests.
    pub access_token: Token,
    /// Refresh token, if one was issued.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh_token: Option<Token>,
    /// When the access token expires (Unix seconds); `None` when the server
    /// did not say.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<u64>,
    /// Granted scopes.
    #[serde(default)]
    pub scopes: Vec<String>,
}

/// One server's authorization.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpAuthRecord {
    /// The canonical MCP server URL the tokens are for.
    pub resource: String,
    /// The `resource` parameter sent to the authorization server (the
    /// protected resource metadata's `resource`).
    pub resource_indicator: String,
    /// The authorization server's issuer.
    pub issuer: String,
    /// Its token endpoint (for refreshes).
    pub token_endpoint: String,
    /// The client registration for this issuer.
    pub client: ClientRegistration,
    /// Scopes requested at the last authorization.
    #[serde(default)]
    pub scopes: Vec<String>,
    /// Scopes the server asked for at runtime (`insufficient_scope`); the
    /// next authorization adds them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub step_up_scopes: Vec<String>,
    /// Tokens, when authorized.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tokens: Option<McpTokens>,
}

/// The token store directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpTokenStore {
    dir: PathBuf,
}

fn store_err(path: &Path, e: impl std::fmt::Display) -> AuthError {
    AuthError::Store(format!("{}: {e}", path.display()))
}

impl McpTokenStore {
    /// The store in `dir` (normally `harness_config::paths::DataDir::mcp_auth`).
    /// Nothing is created until something is saved.
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    /// The directory.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// The file for `server` (server names are letters, digits, `_`, `-`).
    pub fn path(&self, server: &str) -> PathBuf {
        let safe: String = server
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        self.dir.join(format!("{safe}.json"))
    }

    /// The record for `server`, if any.
    pub fn load(&self, server: &str) -> Result<Option<McpAuthRecord>, AuthError> {
        let path = self.path(server);
        match std::fs::read(&path) {
            Ok(b) => {
                let b = Zeroizing::new(b);
                serde_json::from_slice(&b).map(Some).map_err(|e| {
                    store_err(
                        &path,
                        format!("unreadable ({e}); delete it and run `harness login mcp {server}`"),
                    )
                })
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(store_err(&path, e)),
        }
    }

    /// Replaces the record for `server`.
    pub fn save(&self, server: &str, record: &McpAuthRecord) -> Result<(), AuthError> {
        let bytes = Zeroizing::new(serde_json::to_vec_pretty(record).expect("serializable"));
        self.write(&self.path(server), &bytes)
    }

    /// Deletes the tokens and keeps the registration. Returns true when
    /// tokens were stored.
    pub fn clear_tokens(&self, server: &str) -> Result<bool, AuthError> {
        let Some(mut r) = self.load(server)? else {
            return Ok(false);
        };
        let had = r.tokens.take().is_some();
        if had {
            self.save(server, &r)?;
        }
        Ok(had)
    }

    /// Deletes everything stored for `server` (logout). Returns true when
    /// there was a file.
    pub fn delete(&self, server: &str) -> Result<bool, AuthError> {
        let path = self.path(server);
        match std::fs::remove_file(&path) {
            Ok(()) => {
                super::session::forget_cached(&self.dir, server);
                Ok(true)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(store_err(&path, e)),
        }
    }

    /// True when `server` has stored tokens.
    pub fn has_tokens(&self, server: &str) -> bool {
        self.load(server)
            .is_ok_and(|r| r.is_some_and(|r| r.tokens.is_some()))
    }

    /// Every stored token and client secret, for redactors.
    pub fn secrets(&self) -> Vec<String> {
        let Ok(entries) = std::fs::read_dir(&self.dir) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for e in entries.flatten() {
            let path = e.path();
            if path.extension().and_then(|x| x.to_str()) != Some("json") {
                continue;
            }
            let Ok(b) = std::fs::read(&path) else {
                continue;
            };
            let b = Zeroizing::new(b);
            let Ok(r) = serde_json::from_slice::<McpAuthRecord>(&b) else {
                continue;
            };
            if let Some(t) = &r.tokens {
                out.push(t.access_token.expose().to_owned());
                out.extend(t.refresh_token.as_ref().map(|t| t.expose().to_owned()));
            }
            out.extend(
                r.client
                    .client_secret
                    .as_ref()
                    .map(|t| t.expose().to_owned()),
            );
        }
        out
    }

    /// Writes `bytes` to `path` atomically, mode `0600`, creating the
    /// directories with mode `0700`.
    fn write(&self, path: &Path, bytes: &[u8]) -> Result<(), AuthError> {
        create_private_dir(&self.dir).map_err(|e| store_err(&self.dir, e))?;
        let tmp = self.dir.join(format!(
            ".{}.{}.tmp",
            path.file_name().and_then(|n| n.to_str()).unwrap_or("auth"),
            std::process::id()
        ));
        let result = (|| -> std::io::Result<()> {
            let _ = std::fs::remove_file(&tmp);
            let mut opts = std::fs::OpenOptions::new();
            opts.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                opts.mode(0o600);
            }
            let mut f = opts.open(&tmp)?;
            f.write_all(bytes)?;
            f.sync_all()?;
            std::fs::rename(&tmp, path)
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(&tmp);
        }
        result.map_err(|e| store_err(path, e))
    }
}

fn create_private_dir(dir: &Path) -> std::io::Result<()> {
    let mut b = std::fs::DirBuilder::new();
    b.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        b.mode(0o700);
    }
    b.create(dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record() -> McpAuthRecord {
        McpAuthRecord {
            resource: "https://mcp.example.com/mcp".into(),
            resource_indicator: "https://mcp.example.com/mcp".into(),
            issuer: "https://auth.example.com".into(),
            token_endpoint: "https://auth.example.com/token".into(),
            client: ClientRegistration {
                client_id: "dyn-1".into(),
                client_secret: Some(Token::new("cs-secret")),
                kind: Registration::Dynamic,
                redirect_uri: Some("http://127.0.0.1:5/callback".into()),
            },
            scopes: vec!["read".into()],
            step_up_scopes: Vec::new(),
            tokens: Some(McpTokens {
                access_token: Token::new("at-secret"),
                refresh_token: Some(Token::new("rt-secret")),
                expires_at: Some(100),
                scopes: vec!["read".into()],
            }),
        }
    }

    #[test]
    fn records_are_private_and_never_debug_printed() {
        let tmp = tempfile::tempdir().unwrap();
        let s = McpTokenStore::new(tmp.path().join("auth/mcp"));
        assert!(s.load("docs").unwrap().is_none());
        s.save("docs", &record()).unwrap();
        let r = s.load("docs").unwrap().unwrap();
        assert_eq!(r, record());
        assert!(s.has_tokens("docs"));
        let dbg = format!("{r:?}");
        assert!(!dbg.contains("-secret"), "{dbg}");
        let mut secrets = s.secrets();
        secrets.sort();
        assert_eq!(secrets, ["at-secret", "cs-secret", "rt-secret"]);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode(&s.path("docs")), 0o600);
            assert_eq!(mode(s.dir()), 0o700);
            assert_eq!(mode(s.dir().parent().unwrap()), 0o700);
        }
        assert!(s.clear_tokens("docs").unwrap());
        assert!(!s.clear_tokens("docs").unwrap());
        let r = s.load("docs").unwrap().unwrap();
        assert_eq!(r.client.client_id, "dyn-1", "registration kept");
        assert!(
            !std::fs::read_to_string(s.path("docs"))
                .unwrap()
                .contains("at-secret")
        );
        assert!(s.delete("docs").unwrap());
        assert!(!s.delete("docs").unwrap());
        let leftovers: Vec<_> = std::fs::read_dir(s.dir())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");
    }

    #[test]
    fn corrupt_file_is_a_clear_error_and_names_are_safe() {
        let tmp = tempfile::tempdir().unwrap();
        let s = McpTokenStore::new(tmp.path());
        std::fs::write(s.path("x"), "{").unwrap();
        let e = s.load("x").unwrap_err().to_string();
        assert!(e.contains("harness login mcp x"), "{e}");
        assert_eq!(s.path("../evil"), tmp.path().join("___evil.json"));
    }
}
