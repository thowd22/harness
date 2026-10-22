//! Content-addressed blob store for large payloads.
//!
//! Each run has its own store at `<run_dir>/blobs/`. A blob is named by the
//! lowercase hex SHA-256 of its bytes, so storing the same content twice
//! writes it once. Writes go to a temporary file first and are renamed into
//! place, so a crash never leaves a half-written blob under its final name.

use std::io::{self, Write};
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::schema::{BlobEncoding, BlobRef, Payload};

/// Directory name of the blob store inside a run directory.
pub const BLOBS_DIR: &str = "blobs";

/// A content-addressed store rooted at one directory.
#[derive(Debug, Clone)]
pub struct BlobStore {
    dir: PathBuf,
}

/// Lowercase hex SHA-256 of `bytes`.
pub fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

impl BlobStore {
    /// A store rooted at `dir` (created lazily on first write).
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    /// The store's root directory.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Path of the blob with the given hash.
    pub fn path_for(&self, sha256: &str) -> PathBuf {
        self.dir.join(sha256)
    }

    /// Stores `bytes` and returns its reference. Content already present is
    /// not rewritten.
    pub fn put(&self, bytes: &[u8], encoding: BlobEncoding) -> io::Result<BlobRef> {
        let sha256 = sha256_hex(bytes);
        let path = self.path_for(&sha256);
        if !path.exists() {
            std::fs::create_dir_all(&self.dir)?;
            let mut tmp = tempfile_in(&self.dir, &sha256)?;
            tmp.1.write_all(bytes)?;
            tmp.1.sync_all()?;
            drop(tmp.1);
            std::fs::rename(&tmp.0, &path)?;
        }
        Ok(BlobRef {
            sha256,
            size: bytes.len() as u64,
            encoding,
        })
    }

    /// Reads a blob and checks its hash.
    pub fn get(&self, blob: &BlobRef) -> io::Result<Vec<u8>> {
        let bytes = std::fs::read(self.path_for(&blob.sha256))?;
        if sha256_hex(&bytes) != blob.sha256 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("blob {} is corrupt", blob.sha256),
            ));
        }
        Ok(bytes)
    }

    /// Moves an inline payload larger than `threshold` bytes into the store,
    /// replacing it with a [`Payload::Blob`]. Smaller payloads are untouched.
    pub fn spill(&self, payload: &mut Payload, threshold: usize) -> io::Result<()> {
        let (bytes, encoding) = match payload {
            Payload::Text(s) if s.len() > threshold => (s.as_bytes().to_vec(), BlobEncoding::Text),
            Payload::Json(v) => {
                let bytes = serde_json::to_vec(v)?;
                if bytes.len() <= threshold {
                    return Ok(());
                }
                (bytes, BlobEncoding::Json)
            }
            _ => return Ok(()),
        };
        *payload = Payload::Blob(self.put(&bytes, encoding)?);
        Ok(())
    }

    /// Returns `payload` with any blob reference loaded back inline.
    pub fn resolve(&self, payload: &Payload) -> io::Result<Payload> {
        match payload {
            Payload::Blob(blob) => {
                let bytes = self.get(blob)?;
                Ok(match blob.encoding {
                    BlobEncoding::Text => Payload::Text(
                        String::from_utf8(bytes)
                            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?,
                    ),
                    BlobEncoding::Json => Payload::Json(serde_json::from_slice(&bytes)?),
                })
            }
            inline => Ok(inline.clone()),
        }
    }
}

fn tempfile_in(dir: &Path, sha: &str) -> io::Result<(PathBuf, std::fs::File)> {
    let path = dir.join(format!(".{sha}.{}.tmp", uuid::Uuid::now_v7().simple()));
    let file = std::fs::File::create(&path)?;
    Ok((path, file))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identical_content_is_stored_once() {
        let tmp = tempfile::tempdir().unwrap();
        let store = BlobStore::new(tmp.path().join(BLOBS_DIR));
        let a = store.put(b"hello world", BlobEncoding::Text).unwrap();
        let b = store.put(b"hello world", BlobEncoding::Text).unwrap();
        let c = store.put(b"other", BlobEncoding::Text).unwrap();
        assert_eq!(a, b);
        assert_ne!(a.sha256, c.sha256);
        assert_eq!(a.size, 11);
        assert_eq!(std::fs::read_dir(store.dir()).unwrap().count(), 2);
        assert_eq!(store.get(&a).unwrap(), b"hello world");
    }

    #[test]
    fn corrupt_blob_is_detected() {
        let tmp = tempfile::tempdir().unwrap();
        let store = BlobStore::new(tmp.path());
        let r = store.put(b"data", BlobEncoding::Text).unwrap();
        std::fs::write(store.path_for(&r.sha256), b"tampered").unwrap();
        assert_eq!(
            store.get(&r).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
    }

    #[test]
    fn spill_respects_threshold_and_resolves_back() {
        let tmp = tempfile::tempdir().unwrap();
        let store = BlobStore::new(tmp.path());
        let mut small = Payload::text("tiny");
        store.spill(&mut small, 10).unwrap();
        assert_eq!(small, Payload::text("tiny"));

        let original = Payload::Json(serde_json::json!({"k": "v".repeat(50)}));
        let mut big = original.clone();
        store.spill(&mut big, 10).unwrap();
        assert!(matches!(&big, Payload::Blob(b) if b.encoding == BlobEncoding::Json));
        assert_eq!(store.resolve(&big).unwrap(), original);
    }
}
