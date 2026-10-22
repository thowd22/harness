//! Request-body compression (`Content-Encoding: gzip`).
//!
//! Compression only changes the bytes on the wire: the body is serialized
//! exactly as it would be uncompressed and then gzipped as one member with
//! `mtime` 0, so the decompressed bytes are identical, billed tokens and
//! prompt-cache keys are unchanged, and the output is deterministic.
//!
//! Servers that do not understand the encoding answer with an error that
//! [`is_encoding_rejection`] recognises. The transport then resends the raw
//! body once and trips the shared [`GzipLatch`], so every later request in
//! the process goes out uncompressed.

use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};

use flate2::{Compression, GzBuilder};
use reqwest::header::HeaderMap;
use serde_json::Value;

/// Bodies smaller than this many bytes are never compressed.
pub const MIN_GZIP_BYTES: usize = 4096;

/// Default gzip level: cheap on CPU and already shrinks JSON several times.
pub const DEFAULT_GZIP_LEVEL: u32 = 3;

/// Error code some gateways return for an unsupported `Content-Encoding`.
pub const ENCODING_UNSUPPORTED_CODE: &str = "request_body_encoding_unsupported";

/// A one-way switch that disables compression once a server rejected it.
///
/// Clones share the switch. [`GzipLatch::process`] is the switch shared by
/// the whole process; [`GzipLatch::new`] makes an independent one (tests).
#[derive(Debug, Clone, Default)]
pub struct GzipLatch(Arc<AtomicBool>);

impl GzipLatch {
    /// A fresh, untripped latch.
    pub fn new() -> Self {
        Self::default()
    }

    /// The process-wide latch.
    pub fn process() -> Self {
        static PROCESS: OnceLock<GzipLatch> = OnceLock::new();
        PROCESS.get_or_init(GzipLatch::new).clone()
    }

    /// True once compression has been disabled.
    pub fn is_tripped(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }

    /// Disables compression. Returns true if this call tripped the latch
    /// (false if it already was).
    pub fn trip(&self) -> bool {
        !self.0.swap(true, Ordering::Relaxed)
    }
}

impl PartialEq for GzipLatch {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

/// Gzip settings for request bodies sent by one [`super::HttpClient`].
#[derive(Debug, Clone, PartialEq)]
pub struct RequestCompression {
    /// gzip level, 1 (fastest) to 9 (smallest).
    pub level: u32,
    /// Bodies smaller than this are sent raw.
    pub min_bytes: usize,
    /// Request paths whose bodies may be compressed, e.g. `/v1/messages`.
    pub paths: Vec<String>,
    /// Shared switch tripped by a rejection.
    pub latch: GzipLatch,
}

impl RequestCompression {
    /// gzip at `level` for `paths`, with the [`MIN_GZIP_BYTES`] threshold
    /// and the process-wide latch.
    pub fn gzip(level: u32, paths: impl IntoIterator<Item = impl Into<String>>) -> Self {
        Self {
            level: level.clamp(1, 9),
            min_bytes: MIN_GZIP_BYTES,
            paths: paths.into_iter().map(Into::into).collect(),
            latch: GzipLatch::process(),
        }
    }

    /// Uses `latch` instead of the process-wide one.
    pub fn with_latch(mut self, latch: GzipLatch) -> Self {
        self.latch = latch;
        self
    }

    /// Whether a `len`-byte body for `path` is compressed.
    pub fn applies(&self, path: &str, len: usize) -> bool {
        len >= self.min_bytes && !self.latch.is_tripped() && self.paths.iter().any(|p| p == path)
    }
}

/// `raw` as a single gzip member with `mtime` 0 at `level` (clamped to
/// 1..=9).
pub fn gzip(raw: &[u8], level: u32) -> Vec<u8> {
    let mut enc = GzBuilder::new().mtime(0).write(
        Vec::with_capacity(raw.len() / 4 + 64),
        Compression::new(level.clamp(1, 9)),
    );
    // Writing to a Vec cannot fail.
    enc.write_all(raw).expect("gzip into memory");
    enc.finish().expect("gzip into memory")
}

/// What was sent for one request body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BodyStats {
    /// Serialized JSON size in bytes.
    pub raw_bytes: u64,
    /// Bytes actually sent (equal to `raw_bytes` when not compressed).
    pub wire_bytes: u64,
    /// `Content-Encoding` used, e.g. `gzip`; `None` when sent raw.
    pub encoding: Option<String>,
}

impl BodyStats {
    /// Stats for an uncompressed body of `len` bytes.
    pub fn raw(len: usize) -> Self {
        Self {
            raw_bytes: len as u64,
            wire_bytes: len as u64,
            encoding: None,
        }
    }
}

/// Whether the response came from Anthropic itself rather than a proxy or
/// gateway in between: Anthropic sets `request-id` and answers errors with
/// `{"type":"error","error":{"type":...}}`.
pub fn is_anthropic_hop(headers: &HeaderMap, body: &str) -> bool {
    if headers.contains_key("request-id") || headers.contains_key("anthropic-organization-id") {
        return true;
    }
    serde_json::from_str::<Value>(body).is_ok_and(|v| {
        v.get("type").and_then(Value::as_str) == Some("error")
            && v.pointer("/error/type").is_some_and(Value::is_string)
    })
}

/// Whether an error answer to a gzip-encoded request means "this encoding
/// is not accepted": any 415, a 400 naming
/// [`ENCODING_UNSUPPORTED_CODE`] or saying the request body is not valid
/// JSON (the server read the compressed bytes as JSON), or a 400/403 from a hop that
/// is not Anthropic.
pub fn is_encoding_rejection(status: u16, headers: &HeaderMap, body: &str) -> bool {
    match status {
        415 => true,
        400 => {
            let lower = super::error_message(body).to_ascii_lowercase();
            body.contains(ENCODING_UNSUPPORTED_CODE)
                || (lower.contains("json")
                    && (lower.contains("body") || lower.contains("request"))
                    && ["invalid", "parse", "parsing", "malformed", "not valid"]
                        .iter()
                        .any(|w| lower.contains(w)))
                || !is_anthropic_hop(headers, body)
        }
        403 => !is_anthropic_hop(headers, body),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use std::io::Read;

    use reqwest::header::HeaderValue;

    use super::*;

    fn gunzip(b: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        flate2::read::GzDecoder::new(b)
            .read_to_end(&mut out)
            .unwrap();
        out
    }

    #[test]
    fn gzip_is_one_deterministic_member_with_zero_mtime() {
        let raw = br#"{"messages":[{"role":"user","content":"hello hello hello"}]}"#.repeat(200);
        let a = gzip(&raw, 3);
        assert_eq!(a, gzip(&raw, 3), "deterministic");
        assert_eq!(&a[..3], &[0x1f, 0x8b, 8], "gzip magic + deflate");
        assert_eq!(&a[4..8], &[0, 0, 0, 0], "mtime 0");
        assert_eq!(gunzip(&a), raw);
        // Exactly one member: a multi-member decoder consumes it all and a
        // single-member decoder leaves nothing behind.
        let mut single = flate2::bufread::GzDecoder::new(&a[..]);
        let mut out = Vec::new();
        single.read_to_end(&mut out).unwrap();
        assert!(single.into_inner().is_empty());
        assert!(a.len() * 5 < raw.len());
        assert!(gzip(&raw, 9).len() <= a.len());
        assert_eq!(gunzip(&gzip(&raw, 0)), raw, "level clamps to 1");
    }

    #[test]
    fn latch_is_shared_by_clones_and_trips_once() {
        let l = GzipLatch::new();
        let c = l.clone();
        assert!(!c.is_tripped());
        assert!(l.trip());
        assert!(!c.trip());
        assert!(c.is_tripped());
        assert_ne!(l, GzipLatch::new());
        assert_eq!(GzipLatch::process(), GzipLatch::process());
    }

    #[test]
    fn applies_only_to_listed_paths_above_threshold_until_tripped() {
        let c = RequestCompression::gzip(3, ["/v1/messages"]).with_latch(GzipLatch::new());
        assert!(c.applies("/v1/messages", MIN_GZIP_BYTES));
        assert!(!c.applies("/v1/messages", MIN_GZIP_BYTES - 1));
        assert!(!c.applies("/v1/models", 1 << 20));
        c.latch.trip();
        assert!(!c.applies("/v1/messages", 1 << 20));
    }

    #[test]
    fn rejection_classification() {
        let none = HeaderMap::new();
        let mut anth = HeaderMap::new();
        anth.insert("request-id", HeaderValue::from_static("req_1"));
        let anth_err = r#"{"type":"error","error":{"type":"invalid_request_error","message":"max_tokens: required"}}"#;
        assert!(is_encoding_rejection(415, &anth, anth_err));
        assert!(is_encoding_rejection(
            400,
            &anth,
            r#"{"type":"error","error":{"type":"request_body_encoding_unsupported","message":"zstd"}}"#
        ));
        assert!(is_encoding_rejection(
            400,
            &anth,
            r#"{"type":"error","error":{"type":"invalid_request_error","message":"There was an error parsing the body as JSON"}}"#
        ));
        assert!(is_encoding_rejection(
            400,
            &none,
            "<html>Bad Request</html>"
        ));
        assert!(is_encoding_rejection(403, &none, "Forbidden by proxy"));
        // Ordinary Anthropic errors are not rejections of the encoding.
        assert!(!is_encoding_rejection(400, &anth, anth_err));
        assert!(!is_encoding_rejection(400, &none, anth_err));
        assert!(!is_encoding_rejection(
            403,
            &none,
            r#"{"type":"error","error":{"type":"permission_error","message":"no"}}"#
        ));
        assert!(!is_encoding_rejection(
            400,
            &anth,
            r#"{"type":"error","error":{"type":"invalid_request_error","message":"tool input is not valid JSON"}}"#
        ));
        assert!(!is_encoding_rejection(401, &none, ""));
        assert!(!is_encoding_rejection(500, &none, ""));
    }
}
