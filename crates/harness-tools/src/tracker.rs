//! Stale-read detection: remembers the content hash of every file the model
//! has read, so write and edit can refuse to act on an outdated view.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

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

/// Read records held back from the session's [`ReadTracker`].
///
/// A speculative tool call (started before the model's message ended) must
/// not change what write and edit believe the model has seen until its
/// result is actually given to the model. Such a call runs with a
/// [`crate::tool::ToolContext`] carrying a `StagedReads`; read records go
/// here instead, and [`StagedReads::commit`] applies them when the result is
/// used. Dropping it without committing discards them.
#[derive(Debug, Default)]
pub struct StagedReads {
    pending: Mutex<Vec<(Arc<ReadTracker>, PathBuf, String)>>,
}

impl StagedReads {
    /// An empty stage.
    pub fn new() -> Self {
        Self::default()
    }

    /// Hold back the record `path` -> `hash` destined for `tracker`.
    pub fn record(&self, tracker: &Arc<ReadTracker>, path: &Path, hash: String) {
        self.lock()
            .push((tracker.clone(), path.to_path_buf(), hash));
    }

    /// Number of held-back records.
    pub fn len(&self) -> usize {
        self.lock().len()
    }

    /// True when nothing is held back.
    pub fn is_empty(&self) -> bool {
        self.lock().is_empty()
    }

    /// Apply every held-back record to its tracker, in record order, and
    /// empty the stage.
    pub fn commit(&self) {
        for (tracker, path, hash) in std::mem::take(&mut *self.lock()) {
            tracker.record(&path, hash);
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Vec<(Arc<ReadTracker>, PathBuf, String)>> {
        self.pending.lock().unwrap_or_else(|e| e.into_inner())
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

    #[test]
    fn staged_reads_apply_only_on_commit() {
        let t = Arc::new(ReadTracker::new());
        let stage = StagedReads::new();
        let p = Path::new("/a");
        stage.record(&t, p, "h1".into());
        stage.record(&t, p, "h2".into());
        assert_eq!(stage.len(), 2);
        assert_eq!(t.get(p), None);
        stage.commit();
        assert!(stage.is_empty());
        assert_eq!(t.get(p).as_deref(), Some("h2"));

        let dropped = StagedReads::new();
        dropped.record(&t, Path::new("/b"), "x".into());
        drop(dropped);
        assert_eq!(t.get(Path::new("/b")), None);
    }
}
