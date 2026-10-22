//! The ChatGPT token store: two JSON files in a private directory under
//! the data dir (`<data_dir>/auth/`, a built-in protected path):
//!
//! - `chatgpt-host.json`: the `ext_agent_host_id`, created once and kept
//!   across sign-ins, logouts and account switches.
//! - `chatgpt-auth.json`: the issued `client_id` and, when signed in, the
//!   tokens, their expiry, the granted scopes and the account.
//!
//! Files are written atomically with mode `0600` in a `0700` directory.
//! Tokens are [`Token`]s: zeroized on drop and never shown by `Debug`.

use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use super::{ChatGptError, PLAN_SCOPE};

/// Host id file name.
pub const HOST_FILE: &str = "chatgpt-host.json";
/// Client id and token file name.
pub const AUTH_FILE: &str = "chatgpt-auth.json";

/// A token: zeroized on drop, `[redacted]` in `Debug`.
#[derive(Clone, PartialEq, Eq)]
pub struct Token(Zeroizing<String>);

impl Serialize for Token {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for Token {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        String::deserialize(d).map(Self::new)
    }
}

impl Token {
    /// Wraps `s`.
    pub fn new(s: impl Into<String>) -> Self {
        Self(Zeroizing::new(s.into()))
    }

    /// The token text. Never log or display it.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for Token {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("[redacted]")
    }
}

/// The signed-in account (from the verified ID token).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Account {
    /// Account identity (`sub`).
    pub sub: String,
    /// Email address, if the ID token had one.
    #[serde(default)]
    pub email: Option<String>,
}

impl Account {
    /// The email address, else the account id, for messages.
    pub fn label(&self) -> &str {
        self.email.as_deref().unwrap_or(&self.sub)
    }
}

/// Stored tokens.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredTokens {
    /// Bearer token for API calls.
    pub access_token: Token,
    /// Rotating refresh token (`offline_access`).
    #[serde(default)]
    pub refresh_token: Option<Token>,
    /// The last ID token (for `id_token_hint`).
    #[serde(default)]
    pub id_token: Option<Token>,
    /// When the access token expires (Unix seconds).
    pub expires_at: u64,
    /// Granted scopes.
    #[serde(default)]
    pub scopes: Vec<String>,
    /// The account.
    pub account: Account,
}

impl StoredTokens {
    /// True when plan usage was granted.
    pub fn plan_usage(&self) -> bool {
        self.scopes.iter().any(|s| s == PLAN_SCOPE)
    }
}

/// `chatgpt-auth.json`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthRecord {
    /// The client id OpenAI issued on the first sign-in.
    #[serde(default)]
    pub client_id: Option<String>,
    /// Tokens, when signed in.
    #[serde(default)]
    pub tokens: Option<StoredTokens>,
}

#[derive(Serialize, Deserialize)]
struct HostRecord {
    ext_agent_host_id: String,
}

/// The token store directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokenStore {
    dir: PathBuf,
}

fn store_err(path: &Path, e: impl std::fmt::Display) -> ChatGptError {
    ChatGptError::Store(format!("{}: {e}", path.display()))
}

impl TokenStore {
    /// The store in `dir` (usually `harness_config::paths::DataDir::auth`).
    /// Nothing is created until something is saved.
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    /// The directory.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// `chatgpt-auth.json`.
    pub fn auth_path(&self) -> PathBuf {
        self.dir.join(AUTH_FILE)
    }

    /// `chatgpt-host.json`.
    pub fn host_path(&self) -> PathBuf {
        self.dir.join(HOST_FILE)
    }

    fn read<T: for<'de> Deserialize<'de>>(&self, path: &Path) -> Result<Option<T>, ChatGptError> {
        match std::fs::read(path) {
            Ok(b) => serde_json::from_slice(&b).map(Some).map_err(|e| {
                store_err(
                    path,
                    format!("unreadable ({e}); delete it and sign in again"),
                )
            }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(store_err(path, e)),
        }
    }

    /// The `ext_agent_host_id`, created and saved on first use.
    pub fn host_id(&self) -> Result<String, ChatGptError> {
        let path = self.host_path();
        if let Some(h) = self.read::<HostRecord>(&path)?
            && !h.ext_agent_host_id.is_empty()
        {
            return Ok(h.ext_agent_host_id);
        }
        let id = super::pkce::new_host_id();
        self.write(
            &path,
            &serde_json::to_vec_pretty(&HostRecord {
                ext_agent_host_id: id.clone(),
            })
            .expect("serializable"),
        )?;
        Ok(id)
    }

    /// The saved record (empty when nothing was saved).
    pub fn load(&self) -> Result<AuthRecord, ChatGptError> {
        Ok(self.read(&self.auth_path())?.unwrap_or_default())
    }

    /// Replaces the record.
    pub fn save(&self, record: &AuthRecord) -> Result<(), ChatGptError> {
        let bytes = Zeroizing::new(serde_json::to_vec_pretty(record).expect("serializable"));
        self.write(&self.auth_path(), &bytes)
    }

    /// Saves the issued client id, keeping any tokens.
    pub fn save_client_id(&self, client_id: &str) -> Result<(), ChatGptError> {
        let mut r = self.load()?;
        r.client_id = Some(client_id.to_owned());
        self.save(&r)
    }

    /// Deletes the tokens and keeps the registration (client id and host
    /// id), as OpenAI recommends. Returns true when tokens were stored.
    pub fn clear_tokens(&self) -> Result<bool, ChatGptError> {
        let mut r = self.load()?;
        let had = r.tokens.take().is_some();
        if had {
            self.save(&r)?;
        }
        Ok(had)
    }

    /// Writes `bytes` to `path` atomically, mode `0600`, creating the
    /// directory with mode `0700`.
    fn write(&self, path: &Path, bytes: &[u8]) -> Result<(), ChatGptError> {
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

    fn tokens() -> StoredTokens {
        StoredTokens {
            access_token: Token::new("at-secret"),
            refresh_token: Some(Token::new("rt-secret")),
            id_token: None,
            expires_at: 100,
            scopes: vec![PLAN_SCOPE.into()],
            account: Account {
                sub: "u".into(),
                email: Some("a@example.com".into()),
            },
        }
    }

    #[test]
    fn host_id_is_created_once_and_kept() {
        let tmp = tempfile::tempdir().unwrap();
        let s = TokenStore::new(tmp.path().join("auth"));
        let id = s.host_id().unwrap();
        assert_eq!(s.host_id().unwrap(), id);
        s.save_client_id("oaiapp_1").unwrap();
        s.clear_tokens().unwrap();
        assert_eq!(TokenStore::new(s.dir()).host_id().unwrap(), id);
    }

    #[test]
    fn tokens_are_private_and_never_debug_printed() {
        let tmp = tempfile::tempdir().unwrap();
        let s = TokenStore::new(tmp.path().join("auth"));
        s.save(&AuthRecord {
            client_id: Some("oaiapp_1".into()),
            tokens: Some(tokens()),
        })
        .unwrap();
        let r = s.load().unwrap();
        assert_eq!(
            r.tokens.as_ref().unwrap().access_token.expose(),
            "at-secret"
        );
        assert!(r.tokens.as_ref().unwrap().plan_usage());
        let dbg = format!("{r:?}");
        assert!(!dbg.contains("secret"), "{dbg}");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode(&s.auth_path()), 0o600);
            assert_eq!(mode(s.dir()), 0o700);
        }
        assert!(s.clear_tokens().unwrap());
        assert!(!s.clear_tokens().unwrap());
        let r = s.load().unwrap();
        assert_eq!(
            r.client_id.as_deref(),
            Some("oaiapp_1"),
            "registration kept"
        );
        assert!(r.tokens.is_none());
        assert!(
            !std::fs::read_to_string(s.auth_path())
                .unwrap()
                .contains("secret")
        );
        let leftovers: Vec<_> = std::fs::read_dir(s.dir())
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .filter(|n| n.ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");
    }

    #[test]
    fn corrupt_file_is_a_clear_error() {
        let tmp = tempfile::tempdir().unwrap();
        let s = TokenStore::new(tmp.path());
        std::fs::write(s.auth_path(), "{").unwrap();
        let e = s.load().unwrap_err().to_string();
        assert!(e.contains("sign in again"), "{e}");
        assert_eq!(
            TokenStore::new(tmp.path().join("none")).load().unwrap(),
            AuthRecord::default()
        );
    }
}
