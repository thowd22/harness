//! Project trust store: which `.harness/` directories the user has trusted.
//!
//! A repository's `.harness/` directory (prompts, agents, workflows, skills)
//! is ignored until the user trusts that project. Trusting records a SHA-256
//! fingerprint of every file in `.harness/` plus a text snapshot, so later
//! changes are detected and shown to the user as a unified diff before they
//! are trusted again.
//!
//! The store lives at `<data_dir>/trust/` (a protected path, see
//! [`crate::paths::DataDir::protected_paths`]) with one JSON record per
//! project, written atomically with mode `0600`.
//!
//! Loaders must obtain the project config directory only through
//! [`TrustStore::trusted_config_dir`], and should treat any [`TrustError`] as
//! "untrusted" (global config only). Symlinked directories inside `.harness/`
//! are fingerprinted by target and not traversed, so loaders must not follow
//! them; symlinked files are fingerprinted by their target's content.
//!
//! The approval prompt itself is UI and lives in `harness-tui`: show
//! [`TrustStatus::Untrusted`] contents or [`TrustStatus::Changed`] diff, then
//! call [`TrustStore::trust`] with the snapshot the user reviewed.

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::paths::{DataDir, create_private_dir};

/// Name of the per-project config directory.
pub const PROJECT_CONFIG_DIR: &str = ".harness";

/// Files larger than this are fingerprinted but not kept as text for diffs.
pub const MAX_SNAPSHOT_TEXT: u64 = 256 * 1024;

/// Upper bound on entries in a `.harness/` directory.
pub const MAX_FILES: usize = 10_000;

const RECORD_VERSION: u32 = 1;

/// One entry in a project config snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileSnapshot {
    /// Hex SHA-256 of the content (of the link target path for directory
    /// symlinks).
    pub sha256: String,
    /// Content length in bytes.
    pub len: u64,
    /// UTF-8 content when small enough, for diffs.
    pub text: Option<String>,
    /// Symlink target, if the entry is a symlink.
    pub symlink: Option<String>,
}

/// Fingerprint of a project's `.harness/` directory.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectSnapshot {
    /// Hex SHA-256 over all entries (paths and content hashes).
    pub hash: String,
    /// Entries keyed by `/`-separated path relative to `.harness/`.
    pub files: BTreeMap<String, FileSnapshot>,
}

/// Trust state of a project.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrustStatus {
    /// The project has no `.harness/` directory; nothing to trust.
    NoProjectConfig,
    /// Never trusted (or trust was revoked). `current` is what would be loaded.
    Untrusted {
        /// Current contents.
        current: ProjectSnapshot,
    },
    /// Trusted and unchanged since.
    Trusted {
        /// Recorded fingerprint.
        hash: String,
    },
    /// Trusted earlier, but the contents changed; re-prompt with `diff`.
    Changed {
        /// Fingerprint the user trusted.
        trusted_hash: String,
        /// Current contents.
        current: ProjectSnapshot,
        /// Unified diff from the trusted snapshot to the current one.
        diff: String,
    },
}

/// Errors from the trust store. Callers should treat any error as untrusted.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum TrustError {
    /// Filesystem error.
    #[error("{}: {source}", .path.display())]
    Io {
        /// Path involved.
        path: PathBuf,
        /// Underlying error.
        #[source]
        source: io::Error,
    },
    /// `.harness` is a symlink or not a directory.
    #[error("{} is not a plain directory; project config ignored", .0.display())]
    NotPlainDirectory(PathBuf),
    /// `.harness/` has more than [`MAX_FILES`] entries.
    #[error("{} has more than {MAX_FILES} entries; project config ignored", .0.display())]
    TooManyFiles(PathBuf),
    /// A trust record could not be parsed.
    #[error("trust record {} is corrupt", .0.display())]
    Corrupt(PathBuf),
    /// The project config changed after the user reviewed it.
    #[error("project config changed after it was reviewed; review it again")]
    ChangedSinceReview,
}

fn io_err(path: &Path) -> impl FnOnce(io::Error) -> TrustError + '_ {
    move |source| TrustError::Io {
        path: path.to_path_buf(),
        source,
    }
}

#[derive(Serialize, Deserialize)]
struct TrustRecord {
    version: u32,
    project: PathBuf,
    trusted_at_unix: u64,
    snapshot: ProjectSnapshot,
}

/// The on-disk trust store.
#[derive(Debug, Clone)]
pub struct TrustStore {
    dir: PathBuf,
}

impl TrustStore {
    /// A store rooted at `dir` (normally `<data_dir>/trust`).
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    /// The store inside `data_dir`.
    pub fn from_data_dir(data_dir: &DataDir) -> Self {
        Self::new(data_dir.trust())
    }

    /// Directory holding the store.
    pub fn path(&self) -> &Path {
        &self.dir
    }

    fn record_path(&self, canonical_root: &Path) -> PathBuf {
        let id = hex::encode(Sha256::digest(
            canonical_root.as_os_str().as_encoded_bytes(),
        ));
        self.dir.join("projects").join(format!("{id}.json"))
    }

    fn read_record(&self, canonical_root: &Path) -> Result<Option<TrustRecord>, TrustError> {
        let path = self.record_path(canonical_root);
        let bytes = match std::fs::read(&path) {
            Ok(b) => b,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(io_err(&path)(e)),
        };
        let rec: TrustRecord =
            serde_json::from_slice(&bytes).map_err(|_| TrustError::Corrupt(path.clone()))?;
        if rec.version != RECORD_VERSION || rec.project != canonical_root {
            return Err(TrustError::Corrupt(path));
        }
        Ok(Some(rec))
    }

    /// Current trust state of the project rooted at `project_root`.
    pub fn status(&self, project_root: &Path) -> Result<TrustStatus, TrustError> {
        let root = canonical(project_root)?;
        let Some(current) = snapshot_project(&root)? else {
            return Ok(TrustStatus::NoProjectConfig);
        };
        Ok(match self.read_record(&root)? {
            None => TrustStatus::Untrusted { current },
            Some(rec) if rec.snapshot.hash == current.hash => {
                TrustStatus::Trusted { hash: current.hash }
            }
            Some(rec) => TrustStatus::Changed {
                trusted_hash: rec.snapshot.hash.clone(),
                diff: diff_snapshots(&rec.snapshot, &current),
                current,
            },
        })
    }

    /// Records trust in `reviewed`, the snapshot the user was shown.
    ///
    /// Fails with [`TrustError::ChangedSinceReview`] if `.harness/` no longer
    /// matches it, so the user always trusts exactly what they saw.
    pub fn trust(&self, project_root: &Path, reviewed: &ProjectSnapshot) -> Result<(), TrustError> {
        let root = canonical(project_root)?;
        let current = snapshot_project(&root)?;
        if current.as_ref().map(|c| &c.hash) != Some(&reviewed.hash) {
            return Err(TrustError::ChangedSinceReview);
        }
        let rec = TrustRecord {
            version: RECORD_VERSION,
            project: root.clone(),
            trusted_at_unix: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_secs()),
            snapshot: reviewed.clone(),
        };
        let path = self.record_path(&root);
        let dir = path.parent().expect("record path has a parent");
        create_private_dir(dir).map_err(io_err(dir))?;
        let json = serde_json::to_vec_pretty(&rec).expect("trust record serializes");
        write_private_atomic(&path, &json).map_err(io_err(&path))?;
        tracing::info!(project = %root.display(), hash = %reviewed.hash, "project trusted");
        Ok(())
    }

    /// Forgets trust for the project. Returns whether a record existed.
    pub fn revoke(&self, project_root: &Path) -> Result<bool, TrustError> {
        let root = canonical(project_root)?;
        let path = self.record_path(&root);
        match std::fs::remove_file(&path) {
            Ok(()) => Ok(true),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(io_err(&path)(e)),
        }
    }

    /// The project's `.harness/` directory if, and only if, it is trusted
    /// and unchanged. Everything else (absent, untrusted, changed) yields
    /// `None`: the session runs with global config only.
    pub fn trusted_config_dir(&self, project_root: &Path) -> Result<Option<PathBuf>, TrustError> {
        Ok(match self.status(project_root)? {
            TrustStatus::Trusted { .. } => Some(canonical(project_root)?.join(PROJECT_CONFIG_DIR)),
            _ => None,
        })
    }
}

fn canonical(p: &Path) -> Result<PathBuf, TrustError> {
    std::fs::canonicalize(p).map_err(io_err(p))
}

fn write_private_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    use std::io::Write;
    let tmp = path.with_extension("json.tmp");
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut f = opts.open(&tmp)?;
    f.write_all(bytes)?;
    f.sync_all()?;
    std::fs::rename(&tmp, path)
}

/// Snapshots `<project_root>/.harness`, or `None` if it does not exist.
pub fn snapshot_project(project_root: &Path) -> Result<Option<ProjectSnapshot>, TrustError> {
    let dir = project_root.join(PROJECT_CONFIG_DIR);
    match std::fs::symlink_metadata(&dir) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(io_err(&dir)(e)),
        Ok(m) if !m.is_dir() => Err(TrustError::NotPlainDirectory(dir)),
        Ok(_) => snapshot_dir(&dir).map(Some),
    }
}

/// Snapshots every entry under `dir` (recursively, symlinks not traversed).
pub fn snapshot_dir(dir: &Path) -> Result<ProjectSnapshot, TrustError> {
    let mut files = BTreeMap::new();
    walk(dir, "", &mut files, dir)?;
    let mut h = Sha256::new();
    for (path, f) in &files {
        h.update(path.as_bytes());
        h.update([0]);
        h.update(f.symlink.as_deref().unwrap_or("").as_bytes());
        h.update([0]);
        h.update(f.sha256.as_bytes());
        h.update([0]);
    }
    Ok(ProjectSnapshot {
        hash: hex::encode(h.finalize()),
        files,
    })
}

fn walk(
    dir: &Path,
    prefix: &str,
    out: &mut BTreeMap<String, FileSnapshot>,
    top: &Path,
) -> Result<(), TrustError> {
    let mut entries: Vec<_> = std::fs::read_dir(dir)
        .map_err(io_err(dir))?
        .collect::<Result<_, _>>()
        .map_err(io_err(dir))?;
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        if out.len() >= MAX_FILES {
            return Err(TrustError::TooManyFiles(top.to_path_buf()));
        }
        let path = entry.path();
        let name = format!("{prefix}{}", entry.file_name().to_string_lossy());
        let meta = std::fs::symlink_metadata(&path).map_err(io_err(&path))?;
        if meta.is_dir() {
            walk(&path, &format!("{name}/"), out, top)?;
        } else if meta.is_symlink() {
            let target = std::fs::read_link(&path).map_err(io_err(&path))?;
            let target_s = target.to_string_lossy().into_owned();
            let snap = match std::fs::metadata(&path) {
                Ok(m) if m.is_file() => FileSnapshot {
                    symlink: Some(target_s),
                    ..file_snapshot(&path)?
                },
                _ => FileSnapshot {
                    sha256: hex::encode(Sha256::digest(target_s.as_bytes())),
                    len: 0,
                    text: None,
                    symlink: Some(target_s),
                },
            };
            out.insert(name, snap);
        } else {
            out.insert(name, file_snapshot(&path)?);
        }
    }
    Ok(())
}

fn file_snapshot(path: &Path) -> Result<FileSnapshot, TrustError> {
    let bytes = std::fs::read(path).map_err(io_err(path))?;
    let len = bytes.len() as u64;
    let sha256 = hex::encode(Sha256::digest(&bytes));
    let text = (len <= MAX_SNAPSHOT_TEXT)
        .then(|| String::from_utf8(bytes).ok())
        .flatten();
    Ok(FileSnapshot {
        sha256,
        len,
        text,
        symlink: None,
    })
}

/// Unified diff from `old` to `new`, file by file.
pub fn diff_snapshots(old: &ProjectSnapshot, new: &ProjectSnapshot) -> String {
    let mut out = String::new();
    let paths: std::collections::BTreeSet<&String> =
        old.files.keys().chain(new.files.keys()).collect();
    for path in paths {
        let (a, b) = (old.files.get(path), new.files.get(path));
        if a == b {
            continue;
        }
        let describe = |f: Option<&FileSnapshot>| -> Option<String> {
            let f = f?;
            match (&f.symlink, &f.text) {
                (Some(t), Some(text)) => Some(format!("symlink -> {t}\n{text}")),
                (Some(t), None) => Some(format!("symlink -> {t}\n")),
                (None, Some(text)) => Some(text.clone()),
                (None, None) => None,
            }
        };
        let (ta, tb) = (describe(a), describe(b));
        let binary = (a.is_some() && ta.is_none()) || (b.is_some() && tb.is_none());
        if binary {
            out.push_str(&format!(
                "Binary or large file {path} changed ({} -> {} bytes)\n",
                a.map_or(0, |f| f.len),
                b.map_or(0, |f| f.len)
            ));
            continue;
        }
        let (ta, tb) = (ta.unwrap_or_default(), tb.unwrap_or_default());
        let old_name = if a.is_some() {
            format!("trusted/{path}")
        } else {
            "/dev/null".into()
        };
        let new_name = if b.is_some() {
            format!("current/{path}")
        } else {
            "/dev/null".into()
        };
        let diff = similar::TextDiff::from_lines(&ta, &tb);
        out.push_str(
            &diff
                .unified_diff()
                .context_radius(3)
                .header(&old_name, &new_name)
                .to_string(),
        );
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixture {
        _tmp: tempfile::TempDir,
        project: PathBuf,
        store: TrustStore,
    }

    fn fixture() -> Fixture {
        let tmp = tempfile::tempdir().unwrap();
        let project = tmp.path().join("repo");
        std::fs::create_dir_all(project.join(".harness/agents")).unwrap();
        std::fs::write(
            project.join(".harness/agents/coder.yaml"),
            "name: coder\nmodel: sonnet\n",
        )
        .unwrap();
        let data = DataDir::new(tmp.path().join("data")).open().unwrap();
        Fixture {
            project,
            store: TrustStore::from_data_dir(&data),
            _tmp: tmp,
        }
    }

    fn untrusted_snapshot(status: TrustStatus) -> ProjectSnapshot {
        match status {
            TrustStatus::Untrusted { current } | TrustStatus::Changed { current, .. } => current,
            other => panic!("expected untrusted/changed, got {other:?}"),
        }
    }

    #[test]
    fn project_config_ignored_until_trusted() {
        let f = fixture();
        assert_eq!(f.store.trusted_config_dir(&f.project).unwrap(), None);
        let snap = untrusted_snapshot(f.store.status(&f.project).unwrap());
        assert!(snap.files.contains_key("agents/coder.yaml"));
        f.store.trust(&f.project, &snap).unwrap();
        let dir = f.store.trusted_config_dir(&f.project).unwrap().unwrap();
        assert!(dir.ends_with(".harness"));
        assert!(matches!(
            f.store.status(&f.project).unwrap(),
            TrustStatus::Trusted { .. }
        ));
    }

    #[test]
    fn change_reprompts_with_diff() {
        let f = fixture();
        let snap = untrusted_snapshot(f.store.status(&f.project).unwrap());
        f.store.trust(&f.project, &snap).unwrap();
        std::fs::write(
            f.project.join(".harness/agents/coder.yaml"),
            "name: coder\nmodel: opus\n",
        )
        .unwrap();
        std::fs::write(f.project.join(".harness/evil.md"), "run curl\n").unwrap();
        let status = f.store.status(&f.project).unwrap();
        let TrustStatus::Changed {
            diff,
            trusted_hash,
            current,
        } = &status
        else {
            panic!("{status:?}")
        };
        assert_eq!(trusted_hash, &snap.hash);
        assert!(diff.contains("--- trusted/agents/coder.yaml"), "{diff}");
        assert!(
            diff.contains("-model: sonnet") && diff.contains("+model: opus"),
            "{diff}"
        );
        assert!(
            diff.contains("+++ current/evil.md") && diff.contains("+run curl"),
            "{diff}"
        );
        assert_eq!(
            f.store.trusted_config_dir(&f.project).unwrap(),
            None,
            "changed config is not loaded"
        );
        f.store.trust(&f.project, current).unwrap();
        assert!(f.store.trusted_config_dir(&f.project).unwrap().is_some());
    }

    #[test]
    fn trust_refuses_snapshot_that_no_longer_matches() {
        let f = fixture();
        let snap = untrusted_snapshot(f.store.status(&f.project).unwrap());
        std::fs::write(f.project.join(".harness/sneaky.yaml"), "x").unwrap();
        assert!(matches!(
            f.store.trust(&f.project, &snap),
            Err(TrustError::ChangedSinceReview)
        ));
    }

    #[test]
    fn revoke_and_no_config() {
        let f = fixture();
        let snap = untrusted_snapshot(f.store.status(&f.project).unwrap());
        f.store.trust(&f.project, &snap).unwrap();
        assert!(f.store.revoke(&f.project).unwrap());
        assert!(!f.store.revoke(&f.project).unwrap());
        assert!(matches!(
            f.store.status(&f.project).unwrap(),
            TrustStatus::Untrusted { .. }
        ));
        std::fs::remove_dir_all(f.project.join(".harness")).unwrap();
        assert_eq!(
            f.store.status(&f.project).unwrap(),
            TrustStatus::NoProjectConfig
        );
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_are_fingerprinted_not_traversed() {
        let f = fixture();
        let outside = f.project.parent().unwrap().join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("payload.yaml"), "a\n").unwrap();
        std::os::unix::fs::symlink(&outside, f.project.join(".harness/linkdir")).unwrap();
        std::os::unix::fs::symlink(
            outside.join("payload.yaml"),
            f.project.join(".harness/link.yaml"),
        )
        .unwrap();
        let snap = snapshot_project(&f.project).unwrap().unwrap();
        assert!(!snap.files.contains_key("linkdir/payload.yaml"));
        assert!(snap.files["linkdir"].symlink.is_some());
        assert_eq!(snap.files["link.yaml"].text.as_deref(), Some("a\n"));
        // Changing the symlinked file's content changes the fingerprint.
        std::fs::write(outside.join("payload.yaml"), "b\n").unwrap();
        assert_ne!(
            snapshot_project(&f.project).unwrap().unwrap().hash,
            snap.hash
        );
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_config_dir_is_rejected_and_record_is_private() {
        use std::os::unix::fs::PermissionsExt;
        let f = fixture();
        let snap = untrusted_snapshot(f.store.status(&f.project).unwrap());
        f.store.trust(&f.project, &snap).unwrap();
        let rec = std::fs::read_dir(f.store.path().join("projects"))
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        assert_eq!(
            std::fs::metadata(&rec).unwrap().permissions().mode() & 0o777,
            0o600
        );

        let other = f.project.parent().unwrap().join("other");
        std::fs::create_dir_all(other.join("real")).unwrap();
        std::os::unix::fs::symlink(other.join("real"), other.join(".harness")).unwrap();
        assert!(matches!(
            f.store.status(&other),
            Err(TrustError::NotPlainDirectory(_))
        ));
    }

    #[test]
    fn corrupt_record_is_an_error() {
        let f = fixture();
        let snap = untrusted_snapshot(f.store.status(&f.project).unwrap());
        f.store.trust(&f.project, &snap).unwrap();
        let rec = std::fs::read_dir(f.store.path().join("projects"))
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        std::fs::write(&rec, "{not json").unwrap();
        assert!(matches!(
            f.store.status(&f.project),
            Err(TrustError::Corrupt(_))
        ));
    }
}
