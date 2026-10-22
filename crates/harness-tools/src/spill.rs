//! Output budget: outputs over a size limit are stored as blobs and replaced
//! with head + tail and a `spill:<id>` pointer the model can page with read.
//!
//! Storage goes through the small [`BlobSink`] trait so the run store (in
//! `harness-events`) can provide it; [`FileBlobSink`] and [`MemoryBlobSink`]
//! are simple built-in sinks.

use std::collections::HashMap;
use std::io;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use crate::config::ToolsConfig;
use crate::tool::{SpillRecord, ToolContext, ToolEvent, estimate_tokens};
use crate::tracker::content_hash;

/// Prefix of spill pointers accepted by the read tool's `path` argument.
pub const SPILL_SCHEME: &str = "spill:";

/// Content-addressed blob storage for spilled outputs.
pub trait BlobSink: Send + Sync {
    /// Store `data` and return its id (see [`blob_id`]).
    fn put(&self, data: &[u8]) -> io::Result<String>;
    /// Fetch a blob by id; `Ok(None)` when it does not exist.
    fn get(&self, id: &str) -> io::Result<Option<Vec<u8>>>;
}

/// The id used for `data`: the first 16 hex digits of its sha256.
pub fn blob_id(data: &[u8]) -> String {
    let mut h = content_hash(data);
    h.truncate(16);
    h
}

fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 64 && id.bytes().all(|b| b.is_ascii_hexdigit())
}

/// [`BlobSink`] that writes one file per blob into a directory.
#[derive(Debug, Clone)]
pub struct FileBlobSink {
    dir: PathBuf,
}

impl FileBlobSink {
    /// Store blobs under `dir` (created on first write).
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }
}

impl BlobSink for FileBlobSink {
    fn put(&self, data: &[u8]) -> io::Result<String> {
        let id = blob_id(data);
        let path = self.dir.join(&id);
        if !path.exists() {
            std::fs::create_dir_all(&self.dir)?;
            let mut tmp = tempfile::NamedTempFile::new_in(&self.dir)?;
            io::Write::write_all(&mut tmp, data)?;
            tmp.persist(&path).map_err(|e| e.error)?;
        }
        Ok(id)
    }

    fn get(&self, id: &str) -> io::Result<Option<Vec<u8>>> {
        if !valid_id(id) {
            return Ok(None);
        }
        match std::fs::read(self.dir.join(id)) {
            Ok(d) => Ok(Some(d)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }
}

/// In-memory [`BlobSink`], for tests and headless runs.
#[derive(Debug, Default)]
pub struct MemoryBlobSink {
    blobs: Mutex<HashMap<String, Vec<u8>>>,
}

impl MemoryBlobSink {
    /// An empty sink.
    pub fn new() -> Self {
        Self::default()
    }
}

impl BlobSink for MemoryBlobSink {
    fn put(&self, data: &[u8]) -> io::Result<String> {
        let id = blob_id(data);
        self.blobs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(id.clone(), data.to_vec());
        Ok(id)
    }

    fn get(&self, id: &str) -> io::Result<Option<Vec<u8>>> {
        Ok(self
            .blobs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(id)
            .cloned())
    }
}

/// Applies the output budget to tool results.
#[derive(Clone)]
pub struct Spiller {
    sink: Arc<dyn BlobSink>,
    over_bytes: usize,
    head_lines: usize,
    tail_lines: usize,
}

impl std::fmt::Debug for Spiller {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Spiller")
            .field("over_bytes", &self.over_bytes)
            .field("head_lines", &self.head_lines)
            .field("tail_lines", &self.tail_lines)
            .finish_non_exhaustive()
    }
}

/// Longest prefix of `s` with at most `max_lines` lines and `max_bytes` bytes.
fn take_head(s: &str, max_lines: usize, max_bytes: usize) -> &str {
    let mut end = 0;
    for (i, line) in s.split_inclusive('\n').enumerate() {
        if i >= max_lines || end + line.len() > max_bytes {
            if i == 0 {
                end = s.floor_char_boundary(max_bytes);
            }
            break;
        }
        end += line.len();
    }
    &s[..end]
}

/// Longest suffix of `s` with at most `max_lines` lines and `max_bytes` bytes.
fn take_tail(s: &str, max_lines: usize, max_bytes: usize) -> &str {
    let mut start = s.len();
    for (i, line) in s.split_inclusive('\n').rev().enumerate() {
        if i >= max_lines || (s.len() - start) + line.len() > max_bytes {
            if i == 0 {
                start = s.ceil_char_boundary(s.len().saturating_sub(max_bytes));
            }
            break;
        }
        start -= line.len();
    }
    &s[start..]
}

impl Spiller {
    /// A spiller storing into `sink` with limits from `config`
    /// (`spill_over_bytes`, `spill_head_lines`, `spill_tail_lines`).
    pub fn new(sink: Arc<dyn BlobSink>, config: &ToolsConfig) -> Self {
        Self {
            sink,
            over_bytes: config.spill_over_bytes,
            head_lines: config.spill_head_lines,
            tail_lines: config.spill_tail_lines,
        }
    }

    /// The configured threshold in bytes.
    pub fn over_bytes(&self) -> usize {
        self.over_bytes
    }

    /// Return `text` unchanged if it fits the budget. Otherwise store it as a
    /// blob, emit [`ToolEvent::OutputSpilled`] with the token savings, and
    /// return its head and tail around a notice with the `spill:<id>` pointer.
    ///
    /// If the blob cannot be stored the output is still truncated, and the
    /// notice says the full output is unavailable.
    pub fn apply(&self, ctx: &ToolContext, tool: &str, text: String) -> String {
        if text.len() <= self.over_bytes {
            return text;
        }
        let blob = match self.sink.put(text.as_bytes()) {
            Ok(id) => Some(id),
            Err(err) => {
                tracing::warn!(%err, tool, "failed to store spilled output");
                None
            }
        };
        let side_budget = self.over_bytes * 2 / 5;
        let head = take_head(&text, self.head_lines, side_budget);
        let tail = take_tail(&text[head.len()..], self.tail_lines, side_budget);
        let total_lines = text.lines().count();
        let head_lines = head.matches('\n').count();
        let shown_lines = head_lines + tail.lines().count();
        let omitted_bytes = text.len() - head.len() - tail.len();
        let next_line = head_lines + 1;
        let pointer = match &blob {
            Some(id) => format!(
                "Full output saved as {SPILL_SCHEME}{id}; page through it with the read tool, \
                 e.g. path=\"{SPILL_SCHEME}{id}\" offset={next_line}."
            ),
            None => "The full output could not be saved; rerun the command with narrower output \
                     (e.g. pipe through grep, head or tail)."
                .to_string(),
        };
        let mut out = String::with_capacity(head.len() + tail.len() + 300);
        out.push_str(head);
        if !out.is_empty() && !out.ends_with('\n') {
            out.push('\n');
        }
        out.push_str(&format!(
            "[... output truncated: {} of {total_lines} lines ({omitted_bytes} of {} bytes) not shown. {pointer} ...]\n",
            total_lines.saturating_sub(shown_lines),
            text.len(),
        ));
        out.push_str(tail);
        if let Some(id) = blob {
            ctx.emit(ToolEvent::OutputSpilled(SpillRecord {
                tool: tool.to_string(),
                blob_id: id,
                original_bytes: text.len(),
                kept_bytes: out.len(),
                original_tokens: estimate_tokens(text.len()),
                kept_tokens: estimate_tokens(out.len()),
            }));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool::RecordingEvents;

    fn spiller(sink: Arc<dyn BlobSink>, over: usize) -> Spiller {
        Spiller::new(
            sink,
            &ToolsConfig {
                spill_over_bytes: over,
                spill_head_lines: 3,
                spill_tail_lines: 2,
                ..Default::default()
            },
        )
    }

    fn numbered(n: usize) -> String {
        (1..=n).map(|i| format!("line {i}\n")).collect()
    }

    #[test]
    fn small_output_is_untouched() {
        let s = spiller(Arc::new(MemoryBlobSink::new()), 1000);
        let ctx = ToolContext::new("c");
        assert_eq!(s.apply(&ctx, "bash", "hi\n".into()), "hi\n");
    }

    #[test]
    fn large_output_is_spilled_with_pointer_and_event() {
        let sink = Arc::new(MemoryBlobSink::new());
        let events = Arc::new(RecordingEvents::new());
        let ctx = ToolContext::new("call-1").with_events(events.clone());
        let s = spiller(sink.clone(), 200);
        let text = numbered(100);
        let out = s.apply(&ctx, "bash", text.clone());
        assert!(out.starts_with("line 1\nline 2\nline 3\n[... output truncated: 95 of 100 lines"));
        assert!(out.ends_with("line 99\nline 100\n"), "{out}");
        let id = blob_id(text.as_bytes());
        assert!(out.contains(&format!("spill:{id}")), "{out}");
        assert!(out.contains("offset=4"), "{out}");
        assert_eq!(sink.get(&id).unwrap().unwrap(), text.as_bytes());
        let evs = events.events();
        let [(call, ToolEvent::OutputSpilled(rec))] = evs.as_slice() else {
            panic!("{evs:?}")
        };
        assert_eq!(call, "call-1");
        assert_eq!(rec.original_bytes, text.len());
        assert_eq!(rec.kept_bytes, out.len());
        assert_eq!(
            rec.tokens_saved(),
            estimate_tokens(text.len()) - estimate_tokens(out.len())
        );
        assert!(rec.tokens_saved() > 0);
    }

    #[test]
    fn single_huge_line_is_cut_at_char_boundaries() {
        let s = spiller(Arc::new(MemoryBlobSink::new()), 100);
        let text = "é".repeat(500);
        let out = s.apply(&ToolContext::new("c"), "bash", text);
        assert!(out.len() < 400, "{}", out.len());
        assert!(out.contains("output truncated"));
    }

    struct FailingSink;
    impl BlobSink for FailingSink {
        fn put(&self, _: &[u8]) -> io::Result<String> {
            Err(io::Error::other("disk full"))
        }
        fn get(&self, _: &str) -> io::Result<Option<Vec<u8>>> {
            Ok(None)
        }
    }

    #[test]
    fn failed_store_still_truncates() {
        let events = Arc::new(RecordingEvents::new());
        let ctx = ToolContext::new("c").with_events(events.clone());
        let out = spiller(Arc::new(FailingSink), 100).apply(&ctx, "bash", numbered(100));
        assert!(out.contains("could not be saved"));
        assert!(events.events().is_empty());
    }

    #[test]
    fn file_sink_roundtrip_and_rejects_bad_ids() {
        let dir = tempfile::tempdir().unwrap();
        let sink = FileBlobSink::new(dir.path().join("blobs"));
        let id = sink.put(b"data").unwrap();
        assert_eq!(id, blob_id(b"data"));
        assert_eq!(sink.put(b"data").unwrap(), id, "idempotent");
        assert_eq!(sink.get(&id).unwrap().unwrap(), b"data");
        assert_eq!(sink.get("0000000000000000").unwrap(), None);
        assert_eq!(sink.get("../../etc/passwd").unwrap(), None);
    }
}
