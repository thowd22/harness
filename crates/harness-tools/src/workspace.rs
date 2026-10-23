//! The [`Workspace`] trait: every file operation a tool performs goes through it.
//!
//! Tools never touch `std::fs` directly. This gives local and remote (SSH)
//! hosts one interface and gives policy and change tracking a single
//! interception point. [`crate::local::LocalWorkspace`] is the local
//! implementation; `crate::conformance` holds the shared test suite every
//! implementation must pass.

use std::io;
use std::path::{Component, Path, PathBuf};
use std::time::SystemTime;

use async_trait::async_trait;

/// What kind of filesystem object a path refers to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileKind {
    /// A regular file.
    File,
    /// A directory.
    Dir,
    /// A symbolic link (only reported by [`Workspace::list`], which does not follow links).
    Symlink,
    /// Anything else (socket, fifo, device).
    Other,
}

/// Metadata about one path, as returned by [`Workspace::stat`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileStat {
    /// Kind of object (links are followed, so never [`FileKind::Symlink`]).
    pub kind: FileKind,
    /// Size in bytes.
    pub len: u64,
    /// Unix permission bits (`st_mode & 0o7777`), when the host has them.
    pub mode: Option<u32>,
    /// Last modification time, when known.
    pub modified: Option<SystemTime>,
}

/// One entry of a directory listing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirEntry {
    /// File name (no directory part).
    pub name: String,
    /// Kind of object; symbolic links are reported as [`FileKind::Symlink`].
    pub kind: FileKind,
    /// Size in bytes (of the link itself for symlinks).
    pub len: u64,
}

/// Errors from [`Workspace`] operations.
///
/// Messages are written for the model: they say what went wrong and, where
/// possible, what to do next.
#[derive(Debug, thiserror::Error)]
pub enum WsError {
    /// The path does not exist.
    #[error("{0}: no such file or directory")]
    NotFound(PathBuf),
    /// The host refused access.
    #[error("{0}: permission denied")]
    PermissionDenied(PathBuf),
    /// A file was expected but the path is a directory.
    #[error("{0} is a directory; use a file path (bash `ls` lists directories)")]
    IsDirectory(PathBuf),
    /// A directory was expected but the path is not one.
    #[error("{0} is not a directory")]
    NotADirectory(PathBuf),
    /// A glob, regex or file-type pattern could not be parsed.
    #[error("invalid pattern {pattern:?}: {reason}")]
    InvalidPattern {
        /// The pattern as given.
        pattern: String,
        /// Parser message.
        reason: String,
    },
    /// Any other I/O failure.
    #[error("{path}: {source}")]
    Io {
        /// Path the operation was acting on.
        path: PathBuf,
        /// Underlying error.
        #[source]
        source: io::Error,
    },
}

impl WsError {
    /// Classify an [`io::Error`] for `path` into the matching variant.
    pub fn from_io(path: impl Into<PathBuf>, err: io::Error) -> Self {
        let path = path.into();
        match err.kind() {
            io::ErrorKind::NotFound => WsError::NotFound(path),
            io::ErrorKind::PermissionDenied => WsError::PermissionDenied(path),
            io::ErrorKind::IsADirectory => WsError::IsDirectory(path),
            io::ErrorKind::NotADirectory => WsError::NotADirectory(path),
            _ => WsError::Io { path, source: err },
        }
    }
}

/// Result alias for workspace operations.
pub type WsResult<T> = Result<T, WsError>;

/// File operations on one host, rooted at a project directory.
///
/// Relative paths are resolved against [`Workspace::root`]; absolute paths
/// are used as given (access control is the policy layer's job, not this
/// trait's). Implementations must be cheap to share behind an `Arc`.
#[async_trait]
pub trait Workspace: Send + Sync {
    /// The directory relative paths are resolved against.
    fn root(&self) -> &Path;

    /// Resolve `path` against the root and normalise `.`/`..` lexically.
    ///
    /// The result is the key used for stale-read tracking, so two spellings
    /// of the same path resolve to the same value.
    fn resolve(&self, path: &Path) -> PathBuf {
        normalize(&self.root().join(path))
    }

    /// Read the whole file.
    async fn read(&self, path: &Path) -> WsResult<Vec<u8>>;

    /// Create or truncate the file and write `data`, creating parent
    /// directories as needed. Not atomic; prefer [`Workspace::replace_atomic`]
    /// for tool writes.
    async fn write(&self, path: &Path, data: &[u8]) -> WsResult<()>;

    /// Atomically replace (or create) the file with `data`.
    ///
    /// Readers see either the old or the new content, never a mix. An
    /// existing file keeps its permission bits, and a symlink is followed so
    /// the link target is replaced and the link survives. Parent directories
    /// are created as needed.
    async fn replace_atomic(&self, path: &Path, data: &[u8]) -> WsResult<()>;

    /// Set the Unix permission bits of `path` (like `chmod`).
    async fn set_mode(&self, path: &Path, mode: u32) -> WsResult<()>;

    /// Metadata for `path`, following symlinks.
    async fn stat(&self, path: &Path) -> WsResult<FileStat>;

    /// Entries of the directory at `path`, sorted by name.
    async fn list(&self, path: &Path) -> WsResult<Vec<DirEntry>>;

    /// Files under the root matching `pattern` (globset syntax, e.g.
    /// `src/**/*.rs`), as root-relative paths sorted lexically.
    ///
    /// `.gitignore` rules are honoured, hidden files are included and the
    /// `.git` directory is skipped.
    async fn glob(&self, pattern: &str) -> WsResult<Vec<PathBuf>>;

    /// Search files under (or at) `query.base` for the grep and glob tools;
    /// see [`crate::search`] for the walk rules.
    ///
    /// The default returns an [`io::ErrorKind::Unsupported`] error, which
    /// the tools turn into a hint to use bash on this host.
    async fn find(&self, query: &crate::search::FindQuery) -> WsResult<crate::search::FindResult> {
        Err(WsError::Io {
            path: query.base.clone(),
            source: io::Error::new(
                io::ErrorKind::Unsupported,
                "file search is not available on this host",
            ),
        })
    }
}

/// True when `e` says the host does not support the operation (e.g.
/// [`Workspace::find`] on a host without it).
pub fn is_unsupported(e: &WsError) -> bool {
    matches!(e, WsError::Io { source, .. } if source.kind() == io::ErrorKind::Unsupported)
}

/// Lexically normalise a path: drop `.` components and fold `..` into its
/// parent. Does not touch the filesystem or follow links.
pub fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for comp in path.components() {
        match comp {
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    out.push(comp);
                }
            }
            other => out.push(other),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_folds_dots() {
        assert_eq!(normalize(Path::new("/a/./b/../c")), PathBuf::from("/a/c"));
        assert_eq!(normalize(Path::new("/a/b/..")), PathBuf::from("/a"));
        assert_eq!(normalize(Path::new("../x")), PathBuf::from("../x"));
    }

    #[test]
    fn io_errors_are_classified() {
        let e = WsError::from_io("f", io::Error::from(io::ErrorKind::NotFound));
        assert!(matches!(e, WsError::NotFound(_)));
        let e = WsError::from_io("f", io::Error::from(io::ErrorKind::PermissionDenied));
        assert!(matches!(e, WsError::PermissionDenied(_)));
        let e = WsError::from_io("f", io::Error::other("boom"));
        assert!(matches!(e, WsError::Io { .. }));
    }
}
