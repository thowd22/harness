//! The `read` tool: line-numbered, size-capped file reads.
//!
//! - `offset` (1-based line) and `limit` select a window; output lines are
//!   numbered like `cat -n`.
//! - Output is capped by [`ToolsConfig`] (lines, bytes, line length) and ends
//!   with a hint naming the `offset` to continue from.
//! - Binary files are detected and not dumped; images are returned as
//!   [`ToolOutput::Image`] when the model supports image input.
//! - Every read of a workspace file records the full content hash in the
//!   shared [`ReadTracker`], which write and edit use to refuse stale edits.
//! - `path` may be a `spill:<id>` pointer from a truncated tool output, which
//!   is paged the same way.

use std::path::Path;
use std::sync::Arc;

use serde::Deserialize;
use serde_json::{Value, json};

use crate::config::ToolsConfig;
use crate::spill::{BlobSink, SPILL_SCHEME};
use crate::tool::{ToolContext, ToolError, ToolErrorKind, ToolOutput, parse_args};
use crate::tracker::{ReadTracker, content_hash};
use crate::workspace::{FileKind, Workspace};

/// Tool name.
pub const NAME: &str = "read";

/// Largest image returned to the model, in bytes.
pub const IMAGE_MAX_BYTES: usize = 5 * 1024 * 1024;

/// How many leading bytes are checked for NUL when sniffing binary content.
const SNIFF_BYTES: usize = 8192;

/// Arguments of the read tool.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReadArgs {
    /// File path (relative to the project root) or a `spill:<id>` pointer.
    pub path: String,
    /// First line to show, 1-based. Defaults to 1.
    #[serde(default)]
    pub offset: Option<usize>,
    /// Maximum number of lines to show. Capped by `read_max_lines`.
    #[serde(default)]
    pub limit: Option<usize>,
}

/// JSON schema of the read tool's arguments.
pub fn schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "path": {"type": "string", "description": "File path, or a spill:<id> pointer from truncated output"},
            "offset": {"type": "integer", "minimum": 1, "description": "1-based first line"},
            "limit": {"type": "integer", "minimum": 1, "description": "Max lines"}
        },
        "required": ["path"],
        "additionalProperties": false
    })
}

/// One-line description shown to the model.
pub const DESCRIPTION: &str =
    "Read a text file with line numbers (or view an image). Use offset/limit to page long files.";

/// The read tool.
#[derive(Clone)]
pub struct ReadTool {
    ws: Arc<dyn Workspace>,
    tracker: Arc<ReadTracker>,
    config: ToolsConfig,
    spill: Option<Arc<dyn BlobSink>>,
}

impl std::fmt::Debug for ReadTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReadTool")
            .field("root", &self.ws.root())
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

/// Recognise common image formats by magic number.
pub fn sniff_image(data: &[u8]) -> Option<&'static str> {
    if data.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else if data.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some("image/jpeg")
    } else if data.starts_with(b"GIF87a") || data.starts_with(b"GIF89a") {
        Some("image/gif")
    } else if data.len() >= 12 && &data[..4] == b"RIFF" && &data[8..12] == b"WEBP" {
        Some("image/webp")
    } else {
        None
    }
}

/// True if `data` looks binary (contains NUL in its first 8 KiB).
pub fn is_binary(data: &[u8]) -> bool {
    data[..data.len().min(SNIFF_BYTES)].contains(&0)
}

impl ReadTool {
    /// A read tool over `ws`, recording hashes in `tracker`.
    pub fn new(ws: Arc<dyn Workspace>, tracker: Arc<ReadTracker>, config: ToolsConfig) -> Self {
        Self {
            ws,
            tracker,
            config,
            spill: None,
        }
    }

    /// Allow reading `spill:<id>` pointers from `sink`.
    pub fn with_spill(mut self, sink: Arc<dyn BlobSink>) -> Self {
        self.spill = Some(sink);
        self
    }

    /// Run with raw JSON arguments.
    pub async fn call(&self, ctx: &ToolContext, args: Value) -> Result<ToolOutput, ToolError> {
        self.run(ctx, parse_args(NAME, args)?).await
    }

    /// Run with typed arguments.
    pub async fn run(&self, ctx: &ToolContext, args: ReadArgs) -> Result<ToolOutput, ToolError> {
        if let Some(id) = args.path.strip_prefix(SPILL_SCHEME) {
            return self.read_spill(id, &args);
        }
        let path = self.ws.resolve(Path::new(&args.path));
        let stat = self.ws.stat(&path).await?;
        if stat.kind == FileKind::Dir {
            return Err(ToolError::new(
                ToolErrorKind::Unsupported,
                format!(
                    "{} is a directory. Use bash `ls` to list it, then read a file inside it.",
                    args.path
                ),
            ));
        }
        if stat.len > self.config.read_max_file_bytes {
            return Err(ToolError::new(
                ToolErrorKind::Unsupported,
                format!(
                    "{} is too large to read ({} bytes, limit {}). Use bash with rg, head, tail or `sed -n 'START,ENDp'` to view the part you need.",
                    args.path, stat.len, self.config.read_max_file_bytes
                ),
            ));
        }
        let data = self.ws.read(&path).await?;
        ctx.record_read(&self.tracker, &path, content_hash(&data));

        if let Some(media_type) = sniff_image(&data) {
            return Ok(self.image(ctx, &args.path, media_type, data));
        }
        if is_binary(&data) {
            return Ok(ToolOutput::Text(format!(
                "{} is a binary file ({} bytes); its contents are not shown. Use bash (e.g. `file`, `xxd | head`) if you need to inspect it.",
                args.path,
                data.len()
            )));
        }
        let mut out = render(&String::from_utf8_lossy(&data), &args, &self.config)?;
        if std::str::from_utf8(&data).is_err() {
            out.push_str("\n[note: the file is not valid UTF-8; invalid bytes are shown as U+FFFD and edit cannot modify it.]");
        }
        Ok(ToolOutput::Text(out))
    }

    fn image(&self, ctx: &ToolContext, path: &str, media_type: &str, data: Vec<u8>) -> ToolOutput {
        let len = data.len();
        if !ctx.supports_images {
            return ToolOutput::Text(format!(
                "{path} is an image ({media_type}, {len} bytes). The current model cannot view images."
            ));
        }
        if len > IMAGE_MAX_BYTES {
            return ToolOutput::Text(format!(
                "{path} is an image ({media_type}, {len} bytes) larger than the {IMAGE_MAX_BYTES}-byte limit for image input. Resize it first (e.g. with bash `convert`) and read the smaller copy."
            ));
        }
        ToolOutput::Image {
            media_type: media_type.to_string(),
            data,
            caption: format!("{path} ({media_type}, {len} bytes)"),
        }
    }

    fn read_spill(&self, id: &str, args: &ReadArgs) -> Result<ToolOutput, ToolError> {
        let missing = || {
            ToolError::new(
                ToolErrorKind::NotFound,
                format!(
                    "{} was not found. Spill pointers are only valid in the session that produced them; rerun the command if you need the output again.",
                    args.path
                ),
            )
        };
        let sink = self.spill.as_ref().ok_or_else(missing)?;
        let data = sink
            .get(id)
            .map_err(|e| ToolError::new(ToolErrorKind::Io, format!("reading {}: {e}", args.path)))?
            .ok_or_else(missing)?;
        render(&String::from_utf8_lossy(&data), args, &self.config).map(ToolOutput::Text)
    }
}

fn truncate_line(line: &str, max_chars: usize) -> std::borrow::Cow<'_, str> {
    match line.char_indices().nth(max_chars) {
        None => line.into(),
        Some((cut, _)) => {
            let rest = line[cut..].chars().count();
            format!("{}… [line truncated: {rest} more characters]", &line[..cut]).into()
        }
    }
}

/// Render `text` as numbered lines for the window in `args`, applying the
/// caps in `config` and appending a continuation hint when lines remain.
pub fn render(text: &str, args: &ReadArgs, config: &ToolsConfig) -> Result<String, ToolError> {
    let total = text.lines().count();
    if total == 0 {
        return Ok(format!("[{} is empty]", args.path));
    }
    let offset = args.offset.unwrap_or(1).max(1);
    if offset > total {
        return Err(ToolError::invalid_args(format!(
            "offset {offset} is past the end of {}, which has {total} lines. Use an offset between 1 and {total}.",
            args.path
        )));
    }
    let limit = args
        .limit
        .unwrap_or(config.read_max_lines)
        .clamp(1, config.read_max_lines.max(1));
    let mut out = String::new();
    let mut last = offset - 1;
    let mut byte_capped = false;
    for (idx, line) in text.lines().enumerate().skip(offset - 1).take(limit) {
        let rendered = format!(
            "{:>6}\t{}\n",
            idx + 1,
            truncate_line(line, config.read_max_line_chars)
        );
        if !out.is_empty() && out.len() + rendered.len() > config.read_max_bytes {
            byte_capped = true;
            break;
        }
        out.push_str(&rendered);
        last = idx + 1;
    }
    if last < total {
        let why = if byte_capped {
            format!(" (output capped at {} bytes)", config.read_max_bytes)
        } else {
            String::new()
        };
        out.push_str(&format!(
            "\n[Showing lines {offset}-{last} of {total}{why}. To read more, call read with offset={}.]",
            last + 1
        ));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::local::LocalWorkspace;
    use crate::spill::{MemoryBlobSink, Spiller};

    struct Fixture {
        _dir: tempfile::TempDir,
        ws: Arc<LocalWorkspace>,
        tracker: Arc<ReadTracker>,
        tool: ReadTool,
    }

    fn fixture(config: ToolsConfig) -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let ws = Arc::new(LocalWorkspace::new(dir.path()));
        let tracker = Arc::new(ReadTracker::new());
        let tool = ReadTool::new(ws.clone(), tracker.clone(), config);
        Fixture {
            _dir: dir,
            ws,
            tracker,
            tool,
        }
    }

    fn args(path: &str, offset: Option<usize>, limit: Option<usize>) -> Value {
        let mut v = json!({"path": path});
        if let Some(o) = offset {
            v["offset"] = o.into();
        }
        if let Some(l) = limit {
            v["limit"] = l.into();
        }
        v
    }

    async fn text(f: &Fixture, a: Value) -> String {
        match f.tool.call(&ToolContext::new("t"), a).await.unwrap() {
            ToolOutput::Text(t) => t,
            other => panic!("{other:?}"),
        }
    }

    fn lines(n: usize) -> String {
        (1..=n).map(|i| format!("l{i}\n")).collect()
    }

    #[tokio::test]
    async fn offset_and_limit_with_line_numbers() {
        let f = fixture(ToolsConfig::default());
        f.ws.write(Path::new("a.txt"), lines(10).as_bytes())
            .await
            .unwrap();
        let out = text(&f, args("a.txt", Some(3), Some(2))).await;
        assert_eq!(
            out,
            "     3\tl3\n     4\tl4\n\n[Showing lines 3-4 of 10. To read more, call read with offset=5.]"
        );
        let all = text(&f, args("a.txt", None, None)).await;
        assert!(all.starts_with("     1\tl1\n"));
        assert!(
            all.ends_with("    10\tl10\n"),
            "no hint when everything shown"
        );
    }

    #[tokio::test]
    async fn crlf_lines_are_shown_without_carriage_returns() {
        let f = fixture(ToolsConfig::default());
        f.ws.write(Path::new("w.txt"), b"a\r\nb\r\n").await.unwrap();
        assert_eq!(
            text(&f, args("w.txt", None, None)).await,
            "     1\ta\n     2\tb\n"
        );
    }

    #[tokio::test]
    async fn caps_lines_bytes_and_line_length() {
        let cfg = ToolsConfig {
            read_max_lines: 5,
            read_max_bytes: 40,
            read_max_line_chars: 4,
            ..Default::default()
        };
        let f = fixture(cfg);
        f.ws.write(Path::new("a.txt"), lines(100).as_bytes())
            .await
            .unwrap();
        let out = text(&f, args("a.txt", None, Some(50))).await;
        assert!(out.contains("[Showing lines 1-4 of 100 (output capped at 40 bytes). To read more, call read with offset=5.]"), "{out}");
        let f2 = fixture(ToolsConfig {
            read_max_lines: 5,
            ..Default::default()
        });
        f2.ws
            .write(Path::new("a.txt"), lines(100).as_bytes())
            .await
            .unwrap();
        let out = text(&f2, args("a.txt", None, Some(50))).await;
        assert!(
            out.ends_with("[Showing lines 1-5 of 100. To read more, call read with offset=6.]"),
            "{out}"
        );
        f.ws.write(Path::new("long.txt"), b"abcdefgh\n")
            .await
            .unwrap();
        let out = text(&f, args("long.txt", None, None)).await;
        assert_eq!(out, "     1\tabcd… [line truncated: 4 more characters]\n");
    }

    #[tokio::test]
    async fn offset_past_end_and_too_large_files_explain_recovery() {
        let f = fixture(ToolsConfig {
            read_max_file_bytes: 10,
            ..Default::default()
        });
        f.ws.write(Path::new("a.txt"), b"x\ny\n").await.unwrap();
        let err = f
            .tool
            .call(&ToolContext::new("t"), args("a.txt", Some(9), None))
            .await
            .unwrap_err();
        assert_eq!(err.kind, ToolErrorKind::InvalidArgs);
        assert!(err.message.contains("between 1 and 2"), "{err}");
        f.ws.write(Path::new("big.txt"), &[b'a'; 50]).await.unwrap();
        let err = f
            .tool
            .call(&ToolContext::new("t"), args("big.txt", None, None))
            .await
            .unwrap_err();
        assert_eq!(err.kind, ToolErrorKind::Unsupported);
        assert!(err.message.contains("sed -n"), "{err}");
        let err = f
            .tool
            .call(&ToolContext::new("t"), args("missing", None, None))
            .await
            .unwrap_err();
        assert_eq!(err.kind, ToolErrorKind::NotFound);
        let err = f
            .tool
            .call(&ToolContext::new("t"), json!({"file": "a"}))
            .await
            .unwrap_err();
        assert_eq!(err.kind, ToolErrorKind::InvalidArgs);
    }

    #[tokio::test]
    async fn binary_files_are_not_dumped() {
        let f = fixture(ToolsConfig::default());
        f.ws.write(Path::new("b.bin"), b"\x7fELF\x00\x01\x02")
            .await
            .unwrap();
        let out = text(&f, args("b.bin", None, None)).await;
        assert!(out.contains("binary file (7 bytes)"), "{out}");
    }

    const PNG: &[u8] = b"\x89PNG\r\n\x1a\n\x00\x00\x00\rIHDR";

    #[tokio::test]
    async fn images_for_capable_models_only() {
        let f = fixture(ToolsConfig::default());
        f.ws.write(Path::new("p.png"), PNG).await.unwrap();
        let ctx = ToolContext::new("t").with_images(true);
        let out = f.tool.call(&ctx, args("p.png", None, None)).await.unwrap();
        let ToolOutput::Image {
            media_type, data, ..
        } = out
        else {
            panic!("{out:?}")
        };
        assert_eq!(media_type, "image/png");
        assert_eq!(data, PNG);
        let out = text(&f, args("p.png", None, None)).await;
        assert!(out.contains("cannot view images"), "{out}");
    }

    #[test]
    fn sniffs_image_formats() {
        assert_eq!(sniff_image(&[0xFF, 0xD8, 0xFF, 0xE0]), Some("image/jpeg"));
        assert_eq!(sniff_image(b"GIF89a.."), Some("image/gif"));
        assert_eq!(sniff_image(b"RIFF\0\0\0\0WEBPVP8 "), Some("image/webp"));
        assert_eq!(sniff_image(b"hello"), None);
    }

    #[tokio::test]
    async fn records_full_content_hash() {
        let f = fixture(ToolsConfig::default());
        let content = lines(10);
        f.ws.write(Path::new("a.txt"), content.as_bytes())
            .await
            .unwrap();
        text(&f, args("./a.txt", Some(5), Some(1))).await;
        let key = f.ws.resolve(Path::new("a.txt"));
        assert_eq!(f.tracker.get(&key), Some(content_hash(content.as_bytes())));
    }

    #[tokio::test]
    async fn pages_spilled_output() {
        let sink = Arc::new(MemoryBlobSink::new());
        let mut f = fixture(ToolsConfig::default());
        f.tool = f.tool.clone().with_spill(sink.clone());
        let spiller = Spiller::new(
            sink,
            &ToolsConfig {
                spill_over_bytes: 100,
                ..Default::default()
            },
        );
        let full = lines(500);
        let short = spiller.apply(&ToolContext::new("t"), "bash", full);
        let id = short.split("spill:").nth(1).unwrap()[..16].to_string();
        let out = text(&f, args(&format!("spill:{id}"), Some(250), Some(2))).await;
        assert!(out.starts_with("   250\tl250\n   251\tl251\n"), "{out}");
        assert!(out.contains("offset=252"));
        let err = f
            .tool
            .call(&ToolContext::new("t"), args("spill:00", None, None))
            .await
            .unwrap_err();
        assert_eq!(err.kind, ToolErrorKind::NotFound);
    }

    #[tokio::test]
    async fn staged_context_holds_back_the_read_record() {
        let f = fixture(ToolsConfig::default());
        f.ws.write(Path::new("a.txt"), b"one\n").await.unwrap();
        let stage = Arc::new(crate::tracker::StagedReads::new());
        let ctx = ToolContext::new("c").with_staged_reads(Some(stage.clone()));
        f.tool.call(&ctx, json!({"path": "a.txt"})).await.unwrap();
        let key = f.ws.resolve(Path::new("a.txt"));
        assert_eq!(f.tracker.get(&key), None);
        assert_eq!(stage.len(), 1);
        stage.commit();
        assert_eq!(f.tracker.get(&key), Some(content_hash(b"one\n")));
    }
}
