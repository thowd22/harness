//! On-disk cache of live model lists, one JSON file per provider under
//! `<data_dir>/model-lists/`.
//!
//! A cached list records when it was fetched and from which base URL. It
//! is *fresh* while younger than the TTL and fetched from the same base URL;
//! a stale list is still used (offline, or until the background refresh
//! finishes), it is just refetched when possible.

use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::live::LiveModel;

/// Version of the cache file format. Files with another version are ignored.
pub const CACHE_FORMAT: u32 = 1;

/// Name of the cache directory inside the data directory.
pub const CACHE_DIR: &str = "model-lists";

/// Default TTL for cached lists (`[general] model_list_ttl`).
pub const DEFAULT_TTL: Duration = Duration::from_secs(24 * 3600);

/// One provider's live model list as fetched at `fetched_at`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LiveList {
    /// Cache file format ([`CACHE_FORMAT`]).
    pub format: u32,
    /// Provider id.
    pub provider: String,
    /// Base URL the list came from.
    pub base_url: String,
    /// Fetch time, seconds since the Unix epoch.
    pub fetched_at: u64,
    /// The models.
    pub models: Vec<LiveModel>,
}

impl LiveList {
    /// A list fetched now-ish (`fetched_at`).
    pub fn new(
        provider: impl Into<String>,
        base_url: impl Into<String>,
        fetched_at: u64,
        models: Vec<LiveModel>,
    ) -> Self {
        Self {
            format: CACHE_FORMAT,
            provider: provider.into(),
            base_url: base_url.into(),
            fetched_at,
            models,
        }
    }

    /// True when the list is younger than `ttl` at `now` and came from
    /// `base_url`.
    pub fn is_fresh(&self, now: u64, ttl: Duration, base_url: &str) -> bool {
        self.base_url == base_url && now.saturating_sub(self.fetched_at) < ttl.as_secs()
    }
}

/// The cache directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelListCache {
    dir: PathBuf,
}

impl ModelListCache {
    /// A cache in `dir` (usually `<data_dir>/model-lists`). Nothing is
    /// created until the first [`ModelListCache::store`].
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    /// The cache under a data directory root.
    pub fn in_data_dir(data_dir: &Path) -> Self {
        Self::new(data_dir.join(CACHE_DIR))
    }

    /// The cache directory.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// The file holding `provider`'s list.
    pub fn path(&self, provider: &str) -> PathBuf {
        let safe: String = provider
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        self.dir.join(format!("{safe}.json"))
    }

    /// The cached list for `provider`, or `None` when missing, unreadable,
    /// of another format or for another provider.
    pub fn load(&self, provider: &str) -> Option<LiveList> {
        let text = std::fs::read_to_string(self.path(provider)).ok()?;
        let list: LiveList = serde_json::from_str(&text)
            .map_err(
                |e| tracing::debug!(provider, error = %e, "ignoring unreadable model list cache"),
            )
            .ok()?;
        (list.format == CACHE_FORMAT && list.provider == provider).then_some(list)
    }

    /// Writes `list` atomically (temp file and rename), creating the
    /// directory with mode `0700` when missing.
    pub fn store(&self, list: &LiveList) -> io::Result<()> {
        let mut b = std::fs::DirBuilder::new();
        b.recursive(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            b.mode(0o700);
        }
        b.create(&self.dir)?;
        let path = self.path(&list.provider);
        let tmp = path.with_extension(format!("json.{}.tmp", std::process::id()));
        let json = serde_json::to_vec_pretty(list).map_err(io::Error::other)?;
        std::fs::write(&tmp, json)?;
        std::fs::rename(&tmp, &path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn store_load_round_trip_and_freshness() {
        let tmp = tempfile::tempdir().unwrap();
        let cache = ModelListCache::in_data_dir(tmp.path());
        assert!(cache.load("openai").is_none());
        let list = LiveList::new(
            "openai",
            "https://api.openai.com/v1",
            1_000,
            vec![LiveModel {
                id: "gpt-5.5".into(),
                ..LiveModel::default()
            }],
        );
        cache.store(&list).unwrap();
        assert_eq!(cache.load("openai").unwrap(), list);
        assert!(cache.load("anthropic").is_none());
        let ttl = Duration::from_secs(100);
        assert!(list.is_fresh(1_099, ttl, "https://api.openai.com/v1"));
        assert!(!list.is_fresh(1_100, ttl, "https://api.openai.com/v1"));
        assert!(!list.is_fresh(1_000, ttl, "http://localhost:1/v1"));
        std::fs::write(cache.path("openai"), "{not json").unwrap();
        assert!(cache.load("openai").is_none());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(cache.dir()).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o700);
        }
    }
}
