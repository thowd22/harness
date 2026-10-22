//! Project keys: `<slug>-<hash>` derived from the git remote URL, or from the
//! canonical path when the project has no remote.
//!
//! Remote URLs are normalised first, so `git@github.com:Me/App.git`,
//! `https://github.com/me/app` and `ssh://git@github.com/me/app.git` give the
//! same key, and clones of one repository share their project notes.

use std::path::Path;
use std::process::{Command, Stdio};

use sha2::{Digest, Sha256};

/// Number of hex characters of the hash in a project key.
pub const KEY_HASH_LEN: usize = 8;
/// Longest slug part of a project key.
pub const MAX_SLUG_LEN: usize = 40;

/// Where a project key came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeySource {
    /// The normalised URL of the `origin` remote (or the first remote).
    Remote(String),
    /// The canonical project path.
    Path(String),
}

/// A project key and its source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectKey {
    /// The key, e.g. `my-app-1a2b3c4d`. Safe as a single path component.
    pub key: String,
    /// What it was derived from.
    pub source: KeySource,
}

impl ProjectKey {
    /// The key string.
    pub fn as_str(&self) -> &str {
        &self.key
    }
}

/// Derives the project key for `project_root`: from its git remote URL when
/// it has one, otherwise from its canonical path.
pub fn project_key(project_root: &Path) -> ProjectKey {
    match remote_url(project_root) {
        Some(url) => key_from_remote(&url),
        None => {
            let canon = project_root
                .canonicalize()
                .unwrap_or_else(|_| project_root.to_path_buf());
            key_from_path(&canon)
        }
    }
}

/// The key for a remote URL.
pub fn key_from_remote(url: &str) -> ProjectKey {
    let norm = normalize_remote(url);
    let name = norm.rsplit('/').next().unwrap_or(&norm);
    ProjectKey {
        key: make_key(name, &norm),
        source: KeySource::Remote(norm),
    }
}

/// The key for a canonical path.
pub fn key_from_path(path: &Path) -> ProjectKey {
    let s = path.to_string_lossy().into_owned();
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "root".to_owned());
    ProjectKey {
        key: make_key(&name, &s),
        source: KeySource::Path(s),
    }
}

/// Normalises a git remote URL to `host/path` in lower case, without scheme,
/// user, port, trailing slash or `.git` suffix.
pub fn normalize_remote(url: &str) -> String {
    let u = url.trim();
    let rest = match u.split_once("://") {
        Some((_, r)) => r.to_owned(),
        // scp-like `user@host:path`.
        None => match u.split_once(':') {
            Some((host, path)) if !host.contains('/') => format!("{host}/{path}"),
            _ => u.to_owned(),
        },
    };
    let rest = rest.rsplit_once('@').map_or(rest.as_str(), |(_, r)| r);
    let (host, path) = rest.split_once('/').unwrap_or((rest, ""));
    let host = host.split(':').next().unwrap_or(host);
    let path = path.trim_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);
    let joined = if path.is_empty() {
        host.to_owned()
    } else {
        format!("{host}/{path}")
    };
    joined.to_lowercase()
}

fn make_key(name: &str, identity: &str) -> String {
    let mut slug = String::new();
    for c in name.chars() {
        if c.is_ascii_alphanumeric() {
            slug.push(c.to_ascii_lowercase());
        } else if !slug.ends_with('-') {
            slug.push('-');
        }
    }
    let mut slug = slug.trim_matches('-').to_owned();
    slug.truncate(MAX_SLUG_LEN);
    let slug = slug.trim_end_matches('-');
    let slug = if slug.is_empty() { "project" } else { slug };
    let hash = hex::encode(Sha256::digest(identity.as_bytes()));
    format!("{slug}-{}", &hash[..KEY_HASH_LEN])
}

fn remote_url(root: &Path) -> Option<String> {
    let git = |args: &[&str]| -> Option<String> {
        let out = Command::new("git")
            .current_dir(root)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .args(args)
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .ok()?;
        out.status
            .success()
            .then(|| String::from_utf8_lossy(&out.stdout).trim().to_owned())
            .filter(|s| !s.is_empty())
    };
    git(&["remote", "get-url", "origin"]).or_else(|| {
        let first = git(&["remote"])?.lines().next()?.to_owned();
        git(&["remote", "get-url", &first])
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn equivalent_remote_urls_share_a_key() {
        let a = key_from_remote("git@github.com:Me/My-App.git");
        let b = key_from_remote("https://github.com/me/my-app");
        let c = key_from_remote("ssh://git@github.com:22/me/my-app.git/");
        assert_eq!(a, b);
        assert_eq!(a, c);
        assert!(a.key.starts_with("my-app-"), "{}", a.key);
        assert_eq!(a.key.len(), "my-app-".len() + KEY_HASH_LEN);
        assert_eq!(a.source, KeySource::Remote("github.com/me/my-app".into()));
    }

    #[test]
    fn different_remotes_differ() {
        assert_ne!(
            key_from_remote("git@github.com:me/app.git").key,
            key_from_remote("git@github.com:you/app.git").key
        );
    }

    #[test]
    fn path_key_is_slugged_and_hashed() {
        let k = key_from_path(Path::new("/home/me/Code/Weird Name!"));
        assert!(k.key.starts_with("weird-name-"), "{}", k.key);
        assert!(!k.key.contains('/'));
    }
}
