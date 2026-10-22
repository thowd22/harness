//! Stale-read detection: remembers the content hash of every file the model
//! has read, so write and edit can refuse to act on an outdated view.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use sha2::{Digest, Sha256};

/// sha256 of `data` as lowercase hex.
pub fn content_hash(data: &[u8]) -> String {
    hex::encode(Sha256::digest(data))
}

/// Map from resolved path to the hash of the full file content the model
/// last saw (via read, or via its own write/edit).
///
/// One tracker is shared by the read, write and edit tools of a session.
#[derive(Debug, Default)]
pub struct ReadTracker {
    seen: Mutex<HashMap<PathBuf, String>>,
}

impl ReadTracker {
    /// An empty tracker.
    pub fn new() -> Self {
        Self::default()
    }

    /// Record that the model has seen `path` with content hash `hash`.
    pub fn record(&self, path: &Path, hash: String) {
        self.lock().insert(path.to_path_buf(), hash);
    }

    /// The hash last recorded for `path`, if any.
    pub fn get(&self, path: &Path) -> Option<String> {
        self.lock().get(path).cloned()
    }

    /// Forget `path` (e.g. after it was deleted).
    pub fn forget(&self, path: &Path) {
        self.lock().remove(path);
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<PathBuf, String>> {
        self.seen.lock().unwrap_or_else(|e| e.into_inner())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_is_sha256_hex() {
        assert_eq!(
            content_hash(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn record_get_forget() {
        let t = ReadTracker::new();
        let p = Path::new("/a");
        assert_eq!(t.get(p), None);
        t.record(p, "h".into());
        assert_eq!(t.get(p).as_deref(), Some("h"));
        t.forget(p);
        assert_eq!(t.get(p), None);
    }
}
