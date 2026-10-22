//! Line-preserving credential edits to `harness.ini`, used by
//! `harness login` / `harness logout` and the TUI `/login` flow.
//!
//! - [`set_credential`] writes one of `api_key`, `api_key_cmd` or
//!   `api_key_env` into `[provider.<id>]` and removes the other two from that
//!   section, so the chosen source is the one in effect. Every other line,
//!   comment, blank line and section stays byte-for-byte as it was.
//!   [`Credential::ChatGpt`] instead writes `auth = chatgpt` (OpenAI only)
//!   and keeps the API key lines; storing an API key removes `auth`.
//! - [`remove_credentials`] deletes all three keys and `auth` from the
//!   section and leaves the rest of it (`base_url`, comments) in place.
//! - [`write_private`] replaces the file atomically with mode `0600`,
//!   following a symlinked `harness.ini` to its target.
//! - [`save_credential`] and [`remove_credential_file`] combine the two.
//!
//! Values are written raw (the INI dialect has no quoting), so a value
//! containing a line break or a ` ;` inline-comment marker is refused rather
//! than silently truncated on the next load.

use std::io::Write;
use std::path::{Path, PathBuf};

use super::config::PROVIDER_IDS;
use super::document::parse;
use crate::secrets::Secret;

/// The keys that hold a provider credential, in precedence order.
pub const CREDENTIAL_KEYS: [&str; 3] = ["api_key", "api_key_cmd", "api_key_env"];

/// The key selecting the auth method (`[provider.openai] auth`).
pub const AUTH_KEY: &str = "auth";

/// Every key `harness logout` removes: the credential keys and `auth`.
pub const LOGOUT_KEYS: [&str; 4] = ["api_key", "api_key_cmd", "api_key_env", AUTH_KEY];

/// A credential to store in `[provider.<id>]`.
#[derive(Clone)]
pub enum Credential {
    /// Inline key (`api_key`). The file must be private (`0600`).
    ApiKey(Secret),
    /// Shell command printing the key (`api_key_cmd`).
    Command(String),
    /// Environment variable holding the key (`api_key_env`).
    Env(String),
    /// Sign in with ChatGPT (`auth = chatgpt`, OpenAI only). The tokens
    /// live in the protected token store under the data dir, never in
    /// `harness.ini`; API keys in the section are kept.
    ChatGpt,
}

impl std::fmt::Debug for Credential {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ApiKey(_) => f.write_str("ApiKey([redacted])"),
            Self::Command(c) => f.debug_tuple("Command").field(c).finish(),
            Self::Env(v) => f.debug_tuple("Env").field(v).finish(),
            Self::ChatGpt => f.write_str("ChatGpt"),
        }
    }
}

impl Credential {
    /// The INI key this credential is stored under.
    pub fn ini_key(&self) -> &'static str {
        match self {
            Self::ApiKey(_) => "api_key",
            Self::Command(_) => "api_key_cmd",
            Self::Env(_) => "api_key_env",
            Self::ChatGpt => AUTH_KEY,
        }
    }

    /// A description safe to print: never contains an inline key.
    pub fn describe(&self) -> String {
        match self {
            Self::ApiKey(_) => "api_key (stored in harness.ini)".into(),
            Self::Command(c) => format!("api_key_cmd = {c}"),
            Self::Env(v) => format!("api_key_env = {v}"),
            Self::ChatGpt => "auth = chatgpt".into(),
        }
    }

    fn value(&self) -> &str {
        match self {
            Self::ApiKey(s) => s.expose(),
            Self::Command(c) => c,
            Self::Env(v) => v,
            Self::ChatGpt => "chatgpt",
        }
    }

    /// Keys this credential replaces in the section: storing an API key
    /// also drops `auth`, so the key is the method in effect; storing
    /// `auth = chatgpt` keeps API keys.
    fn competing(&self) -> &'static [&'static str] {
        match self {
            Self::ChatGpt => &[],
            _ => &LOGOUT_KEYS,
        }
    }

    /// Checks that the value can be written to `harness.ini` and read back
    /// unchanged. Error messages never contain an inline key.
    pub fn validate(&self) -> Result<(), EditError> {
        let v = self.value();
        let what = self.ini_key();
        let bad = |why: &str| Err(EditError::InvalidValue(format!("{what} {why}")));
        if v.trim().is_empty() {
            return bad("is empty");
        }
        if v.trim() != v {
            return bad("has leading or trailing whitespace");
        }
        if v.contains(['\n', '\r']) {
            return bad("contains a line break");
        }
        if v.chars().any(|c| c.is_control()) {
            return bad("contains control characters");
        }
        let b = v.as_bytes();
        if b.windows(2)
            .any(|w| w[0].is_ascii_whitespace() && w[1] == b';')
        {
            return bad("contains ` ;`, which harness.ini reads as the start of a comment");
        }
        if let Self::Env(name) = self {
            let mut chars = name.chars();
            let first_ok = chars
                .next()
                .is_some_and(|c| c.is_ascii_alphabetic() || c == '_');
            if !first_ok || !chars.all(|c| c.is_ascii_alphanumeric() || c == '_') {
                return Err(EditError::InvalidValue(format!(
                    "api_key_env must be an environment variable name such as ANTHROPIC_API_KEY, got `{name}`"
                )));
            }
        }
        Ok(())
    }
}

/// Errors from editing `harness.ini`. Never contain secret values.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum EditError {
    /// The provider id is not one of [`PROVIDER_IDS`].
    #[error("unknown provider `{0}` (expected one of: anthropic, openai, openrouter)")]
    UnknownProvider(String),
    /// The value cannot be stored.
    #[error("{0}")]
    InvalidValue(String),
    /// The existing file has INI syntax errors.
    #[error(
        "{path}: cannot edit a file with syntax errors; run `harness config check` and fix them first"
    )]
    Unparseable {
        /// The file.
        path: PathBuf,
    },
    /// Reading or writing the file failed.
    #[error("{path}: {source}")]
    Io {
        /// The file.
        path: PathBuf,
        /// The cause.
        #[source]
        source: std::io::Error,
    },
}

fn check_provider(provider: &str) -> Result<String, EditError> {
    if PROVIDER_IDS.contains(&provider) {
        Ok(format!("provider.{provider}"))
    } else {
        Err(EditError::UnknownProvider(provider.to_owned()))
    }
}

/// Section header name on `line`, using the lexer's rules.
fn header_name(line: &str) -> Option<&str> {
    let line = line.trim_start_matches('\u{feff}').trim();
    let rest = line.strip_prefix('[')?;
    let rest = match rest.find(" ;").or_else(|| rest.find("\t;")) {
        Some(i) => &rest[..i],
        None => rest,
    };
    Some(rest.trim_end().strip_suffix(']')?.trim())
}

/// Key of the `key = value` entry on `line`, if it is one.
fn entry_key(line: &str) -> Option<&str> {
    let t = line.trim();
    if t.is_empty() || t.starts_with(';') || t.starts_with('#') || t.starts_with('[') {
        return None;
    }
    t.split_once('=').map(|(k, _)| k.trim())
}

/// Splits `text` into lines that keep their terminators.
fn lines(text: &str) -> Vec<&str> {
    text.split_inclusive('\n').collect()
}

fn newline_of(text: &str) -> &'static str {
    if text.contains("\r\n") { "\r\n" } else { "\n" }
}

/// Line range `[header, end)` of section `name`.
fn section_range(lines: &[&str], name: &str) -> Option<(usize, usize)> {
    let start = lines.iter().position(|l| header_name(l) == Some(name))?;
    let end = lines[start + 1..]
        .iter()
        .position(|l| header_name(l).is_some())
        .map_or(lines.len(), |i| start + 1 + i);
    Some((start, end))
}

/// Returns `text` with `credential` stored in `[provider.<provider>]`.
///
/// An existing line for the same key is replaced in place (keeping its
/// indentation); the other two credential keys are removed from the
/// section; a missing section is appended at the end of the file.
pub fn set_credential(
    text: &str,
    provider: &str,
    credential: &Credential,
) -> Result<String, EditError> {
    let section = check_provider(provider)?;
    credential.validate()?;
    if matches!(credential, Credential::ChatGpt) && provider != "openai" {
        return Err(EditError::InvalidValue(format!(
            "Sign in with ChatGPT is only for openai, not {provider}"
        )));
    }
    let nl = newline_of(text);
    let key = credential.ini_key();
    let new_line = |indent: &str| format!("{indent}{key} = {}{nl}", credential.value());
    let ls = lines(text);
    let Some((start, end)) = section_range(&ls, &section) else {
        let mut out = text.to_owned();
        if !out.is_empty() && !out.ends_with('\n') {
            out.push_str(nl);
        }
        if !out.trim().is_empty() {
            out.push_str(nl);
        }
        out.push_str(&format!("[{section}]{nl}"));
        out.push_str(&new_line(""));
        return Ok(out);
    };
    let mut out = String::with_capacity(text.len() + 64);
    let mut written = false;
    let mut last_entry = start;
    for (i, l) in ls.iter().enumerate().take(end).skip(start + 1) {
        if entry_key(l).is_some() {
            last_entry = i;
        }
    }
    let insert = !ls[start + 1..end].iter().any(|l| entry_key(l) == Some(key));
    for (i, l) in ls.iter().enumerate() {
        let in_section = i > start && i < end;
        match entry_key(l) {
            Some(k) if in_section && k == key => {
                if !written {
                    let indent = &l[..l.len() - l.trim_start().len()];
                    out.push_str(&new_line(indent));
                    written = true;
                }
            }
            Some(k) if in_section && credential.competing().contains(&k) => {}
            _ => out.push_str(l),
        }
        if i == last_entry && insert {
            if !out.ends_with('\n') {
                out.push_str(nl);
            }
            out.push_str(&new_line(""));
            written = true;
        }
    }
    Ok(out)
}

/// Returns `text` without any credential key (or `auth`) in `[provider.<provider>]`,
/// plus the keys that were removed (empty when there were none).
pub fn remove_credentials(
    text: &str,
    provider: &str,
) -> Result<(String, Vec<&'static str>), EditError> {
    let section = check_provider(provider)?;
    let ls = lines(text);
    let Some((start, end)) = section_range(&ls, &section) else {
        return Ok((text.to_owned(), Vec::new()));
    };
    let mut removed = Vec::new();
    let mut out = String::with_capacity(text.len());
    for (i, l) in ls.iter().enumerate() {
        if i > start
            && i < end
            && let Some(k) = entry_key(l)
            && let Some(known) = LOGOUT_KEYS.iter().find(|c| **c == k)
        {
            removed.push(*known);
            continue;
        }
        out.push_str(l);
    }
    Ok((out, removed))
}

/// Reads `path`, treating a missing file as empty.
fn read_or_empty(path: &Path) -> Result<String, EditError> {
    match std::fs::read_to_string(path) {
        Ok(t) => Ok(t),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(source) => Err(EditError::Io {
            path: path.to_path_buf(),
            source,
        }),
    }
}

fn ensure_parses(text: &str, path: &Path) -> Result<(), EditError> {
    parse(text, Some(path))
        .map(|_| ())
        .map_err(|_| EditError::Unparseable {
            path: path.to_path_buf(),
        })
}

/// Stores `credential` for `provider` in the file at `path` (created, with
/// private parent directories, when missing) and makes the file `0600`.
pub fn save_credential(
    path: &Path,
    provider: &str,
    credential: &Credential,
) -> Result<(), EditError> {
    let old = read_or_empty(path)?;
    ensure_parses(&old, path)?;
    let new = set_credential(&old, provider, credential)?;
    ensure_parses(&new, path)?;
    write_private(path, &new).map_err(|source| EditError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    tracing::info!(provider, key = credential.ini_key(), path = %path.display(), "credential saved");
    Ok(())
}

/// Removes every credential key for `provider` from the file at `path`.
/// Returns the removed keys; the file is not rewritten when there were none.
pub fn remove_credential_file(path: &Path, provider: &str) -> Result<Vec<&'static str>, EditError> {
    let old = read_or_empty(path)?;
    ensure_parses(&old, path)?;
    let (new, removed) = remove_credentials(&old, provider)?;
    if !removed.is_empty() {
        write_private(path, &new).map_err(|source| EditError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        tracing::info!(provider, path = %path.display(), "credentials removed");
    }
    Ok(removed)
}

/// The credential keys currently set for `provider` in `text`.
pub fn credential_keys(text: &str, provider: &str) -> Vec<&'static str> {
    let Ok(section) = check_provider(provider) else {
        return Vec::new();
    };
    let ls = lines(text);
    let Some((start, end)) = section_range(&ls, &section) else {
        return Vec::new();
    };
    ls[start + 1..end]
        .iter()
        .filter_map(|l| entry_key(l))
        .filter_map(|k| LOGOUT_KEYS.iter().find(|c| **c == k).copied())
        .collect()
}

/// Writes `text` to `path` atomically with mode `0600`.
///
/// A symlinked `path` is resolved so the link's target is replaced, not the
/// link. Missing parent directories are created with mode `0700`. The data
/// goes to a private temporary file in the same directory, is synced, then
/// renamed over the target.
pub fn write_private(path: &Path, text: &str) -> std::io::Result<()> {
    let target = match std::fs::canonicalize(path) {
        Ok(p) => p,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => path.to_path_buf(),
        Err(e) => return Err(e),
    };
    let dir = match target.parent() {
        Some(d) if !d.as_os_str().is_empty() => d.to_path_buf(),
        _ => PathBuf::from("."),
    };
    if !dir.exists() {
        crate::paths::create_private_dir(&dir)?;
    }
    let mut tmp = tempfile::Builder::new()
        .prefix(".harness.ini.")
        .tempfile_in(&dir)?;
    set_private(tmp.path())?;
    tmp.write_all(text.as_bytes())?;
    tmp.as_file().sync_all()?;
    tmp.persist(&target).map_err(|e| e.error)?;
    set_private(&target)
}

fn set_private(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(v: &str) -> Credential {
        Credential::ApiKey(Secret::new(v.into()))
    }

    const FILE: &str = "\
; my harness config
[general]
default_model = sonnet   ; alias

[provider.anthropic]
# a comment inside
api_key_env = ANTHROPIC_API_KEY
base_url = https://api.anthropic.com
; api_key_cmd = pass show anthropic

[ui]
mouse = false
";

    #[test]
    fn replaces_competing_keys_and_keeps_everything_else() {
        let out = set_credential(FILE, "anthropic", &key("sk-ant-new")).unwrap();
        assert_eq!(
            out,
            FILE.replace(
                "api_key_env = ANTHROPIC_API_KEY\nbase_url = https://api.anthropic.com\n",
                "base_url = https://api.anthropic.com\napi_key = sk-ant-new\n"
            )
        );
        let cfg = crate::ini::load_str(&out, None).unwrap();
        let p = &cfg.providers["anthropic"];
        assert_eq!(p.api_key.as_ref().unwrap().expose(), "sk-ant-new");
        assert!(p.api_key_env.is_none());
    }

    #[test]
    fn replaces_the_same_key_in_place_with_its_indentation() {
        let text = "[provider.openai]\n  api_key_cmd = old\nbase_url = x\n";
        let out =
            set_credential(text, "openai", &Credential::Command("pass show oa".into())).unwrap();
        assert_eq!(
            out,
            "[provider.openai]\n  api_key_cmd = pass show oa\nbase_url = x\n"
        );
    }

    #[test]
    fn appends_a_missing_section_and_handles_empty_files() {
        let out = set_credential("", "openrouter", &Credential::Env("OR_KEY".into())).unwrap();
        assert_eq!(out, "[provider.openrouter]\napi_key_env = OR_KEY\n");
        let out = set_credential("[ui]\nmouse = false", "openai", &key("sk-x")).unwrap();
        assert_eq!(
            out,
            "[ui]\nmouse = false\n\n[provider.openai]\napi_key = sk-x\n"
        );
        let out = set_credential("[provider.openai]\n", "openai", &key("sk-x")).unwrap();
        assert_eq!(out, "[provider.openai]\napi_key = sk-x\n");
        // Header without a trailing newline.
        let out = set_credential("[provider.openai]", "openai", &key("sk-x")).unwrap();
        assert_eq!(out, "[provider.openai]\napi_key = sk-x\n");
    }

    #[test]
    fn keeps_crlf_line_endings() {
        let text = "[provider.openai]\r\nbase_url = x\r\n[ui]\r\nmouse = true\r\n";
        let out = set_credential(text, "openai", &key("sk-x")).unwrap();
        assert_eq!(
            out,
            "[provider.openai]\r\nbase_url = x\r\napi_key = sk-x\r\n[ui]\r\nmouse = true\r\n"
        );
    }

    #[test]
    fn refuses_values_that_would_not_round_trip() {
        for bad in [
            key(""),
            key(" sk"),
            key("sk\nx"),
            key("sk\u{7}"),
            Credential::Command("pass show x ; rm".into()),
            Credential::Env("1BAD".into()),
            Credential::Env("A-B".into()),
        ] {
            let e = set_credential("", "openai", &bad).unwrap_err();
            assert!(matches!(e, EditError::InvalidValue(_)), "{bad:?}");
        }
        // `;` without leading whitespace is part of the value.
        set_credential("", "openai", &Credential::Command("a;b".into())).unwrap();
        assert!(matches!(
            set_credential("", "gemini", &key("k")).unwrap_err(),
            EditError::UnknownProvider(_)
        ));
        let e = set_credential("", "openai", &key("sk-secret value\n")).unwrap_err();
        assert!(!e.to_string().contains("sk-secret"), "{e}");
    }

    #[test]
    fn removes_only_credential_keys_of_that_section() {
        let text = "[provider.anthropic]\napi_key = sk-a\napi_key_cmd = c\nbase_url = u\n; api_key = old\n[provider.openai]\napi_key = sk-o\n";
        let (out, removed) = remove_credentials(text, "anthropic").unwrap();
        assert_eq!(removed, ["api_key", "api_key_cmd"]);
        assert_eq!(
            out,
            "[provider.anthropic]\nbase_url = u\n; api_key = old\n[provider.openai]\napi_key = sk-o\n"
        );
        let (same, none) = remove_credentials(&out, "anthropic").unwrap();
        assert!(none.is_empty());
        assert_eq!(same, out);
        assert_eq!(credential_keys(text, "openai"), ["api_key"]);
        assert!(credential_keys(text, "openrouter").is_empty());
    }

    #[test]
    fn chatgpt_sets_auth_and_api_keys_reset_it() {
        let text = "[provider.openai]\napi_key_env = OPENAI_API_KEY\nbase_url = u\n";
        let out = set_credential(text, "openai", &Credential::ChatGpt).unwrap();
        assert_eq!(
            out,
            "[provider.openai]\napi_key_env = OPENAI_API_KEY\nbase_url = u\nauth = chatgpt\n"
        );
        assert_eq!(credential_keys(&out, "openai"), ["api_key_env", "auth"]);
        let back = set_credential(&out, "openai", &Credential::Env("OA".into())).unwrap();
        assert_eq!(back, "[provider.openai]\napi_key_env = OA\nbase_url = u\n");
        assert!(set_credential("", "anthropic", &Credential::ChatGpt).is_err());
        let (gone, removed) = remove_credentials(&out, "openai").unwrap();
        assert_eq!(removed, ["api_key_env", "auth"]);
        assert_eq!(gone, "[provider.openai]\nbase_url = u\n");
        assert_eq!(Credential::ChatGpt.ini_key(), "auth");
    }

    #[test]
    fn credential_debug_hides_inline_keys() {
        let d = format!("{:?}", key("sk-hidden"));
        assert!(!d.contains("sk-hidden"), "{d}");
        assert!(!key("sk-hidden").describe().contains("sk-hidden"));
    }

    #[cfg(unix)]
    #[test]
    fn save_writes_mode_0600_and_follows_symlinks() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("dotfiles").join("harness.ini");
        std::fs::create_dir_all(real.parent().unwrap()).unwrap();
        std::fs::write(&real, FILE).unwrap();
        std::fs::set_permissions(&real, std::fs::Permissions::from_mode(0o644)).unwrap();
        let link = dir.path().join("harness.ini");
        std::os::unix::fs::symlink(&real, &link).unwrap();

        save_credential(&link, "anthropic", &key("sk-ant-saved")).unwrap();
        assert!(
            std::fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        let mode = std::fs::metadata(&real).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        let cfg = crate::ini::load_file(&link).unwrap();
        assert_eq!(
            cfg.providers["anthropic"]
                .api_key
                .as_ref()
                .unwrap()
                .expose(),
            "sk-ant-saved"
        );
        // No temp files left behind.
        let names: Vec<_> = std::fs::read_dir(real.parent().unwrap())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names, [std::ffi::OsString::from("harness.ini")]);

        let removed = remove_credential_file(&link, "anthropic").unwrap();
        assert_eq!(removed, ["api_key"]);
        let text = std::fs::read_to_string(&real).unwrap();
        assert!(!text.contains("sk-ant-saved"));
        assert!(text.contains("base_url = https://api.anthropic.com"));
    }

    #[cfg(unix)]
    #[test]
    fn save_creates_missing_file_and_private_dirs() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cfg").join("harness").join("harness.ini");
        save_credential(&path, "openai", &Credential::Env("OPENAI_API_KEY".into())).unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "[provider.openai]\napi_key_env = OPENAI_API_KEY\n"
        );
        let fmode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        let dmode = std::fs::metadata(path.parent().unwrap())
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!((fmode, dmode), (0o600, 0o700));
    }

    #[test]
    fn saving_and_removing_never_log_the_key() {
        use std::sync::{Arc, Mutex};
        #[derive(Clone, Default)]
        struct Buf(Arc<Mutex<Vec<u8>>>);
        impl std::io::Write for Buf {
            fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(b);
                Ok(b.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let buf = Buf::default();
        let sink = buf.clone();
        let subscriber = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::TRACE)
            .with_writer(move || sink.clone())
            .finish();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("harness.ini");
        // tracing caches each callsite's interest when it is first hit. With
        // only one live dispatcher, a callsite first hit on another test's
        // thread is judged by *that* thread's (empty) default and cached as
        // "never", which silently drops this test's events. So: register the
        // callsites here first, keep a second dispatcher alive so interest is
        // computed over every dispatcher, and rebuild the cache once this
        // thread's subscriber is installed.
        let warm = dir.path().join("warm.ini");
        save_credential(&warm, "openai", &key("sk-warm")).unwrap();
        remove_credential_file(&warm, "openai").unwrap();
        let _other = tracing::Dispatch::new(tracing::subscriber::NoSubscriber::default());
        {
            let _guard = tracing::subscriber::set_default(subscriber);
            tracing::callsite::rebuild_interest_cache();
            save_credential(&path, "openai", &key("sk-never-logged-42")).unwrap();
            let _ = save_credential(&path, "openai", &key("sk-never-logged-42\n"));
            remove_credential_file(&path, "openai").unwrap();
        }
        let logs = String::from_utf8(buf.0.lock().unwrap().clone()).unwrap();
        assert!(logs.contains("credential saved"), "{logs}");
        assert!(!logs.contains("sk-never-logged-42"), "{logs}");
    }

    #[test]
    fn refuses_to_edit_unparseable_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("harness.ini");
        std::fs::write(&path, "[ui\n").unwrap();
        let e = save_credential(&path, "openai", &key("sk")).unwrap_err();
        assert!(matches!(e, EditError::Unparseable { .. }), "{e}");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "[ui\n");
    }
}
