//! The `edit` tool: exact string replacement in one file.
//!
//! - `old_string` must match exactly once unless `replace_all` is set.
//! - The file must have been read, and not changed since (stale-read check).
//! - CRLF files are matched and written with CRLF, so the model can use the
//!   LF text it sees in read output.
//! - Writes are atomic, keep permissions, and emit a file-change event.
//! - Every failure says how to recover (e.g. where a whitespace-only
//!   mismatch is, or which lines an ambiguous match is on).

use std::path::Path;
use std::sync::Arc;

use serde::Deserialize;
use serde_json::{Value, json};

use crate::tool::{ToolContext, ToolError, ToolErrorKind, ToolOutput, parse_args};
use crate::tracker::ReadTracker;
use crate::workspace::Workspace;
use crate::write::{Mutator, to_crlf, uses_crlf};

/// Tool name.
pub const NAME: &str = "edit";

/// One-line description shown to the model.
pub const DESCRIPTION: &str = "Replace an exact string in a file you have read with the read tool (bash output does not count). old_string must be unique unless replace_all is true.";

/// Arguments of the edit tool.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EditArgs {
    /// File path, relative to the project root.
    pub path: String,
    /// Exact text to find (without read's line-number prefixes).
    pub old_string: String,
    /// Replacement text.
    pub new_string: String,
    /// Replace every occurrence instead of requiring a unique match.
    #[serde(default)]
    pub replace_all: bool,
}

/// JSON schema of the edit tool's arguments.
pub fn schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "path": {"type": "string"},
            "old_string": {"type": "string", "description": "Exact text to replace"},
            "new_string": {"type": "string"},
            "replace_all": {"type": "boolean", "default": false}
        },
        "required": ["path", "old_string", "new_string"],
        "additionalProperties": false
    })
}

/// The edit tool.
#[derive(Clone)]
pub struct EditTool {
    m: Mutator,
}

impl std::fmt::Debug for EditTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EditTool")
            .field("root", &self.m.ws.root())
            .finish_non_exhaustive()
    }
}

fn line_of(text: &str, byte: usize) -> usize {
    text[..byte].matches('\n').count() + 1
}

/// True if a line looks like read output (`   12\tcode`).
fn has_line_number_prefix(line: &str) -> bool {
    let t = line.trim_start_matches(' ');
    let digits = t.bytes().take_while(u8::is_ascii_digit).count();
    digits > 0 && t[digits..].starts_with('\t')
}

/// First line where `needle` matches `text` when leading/trailing whitespace
/// of every line is ignored.
fn whitespace_insensitive_match(text: &str, needle: &str) -> Option<usize> {
    let want: Vec<&str> = needle.lines().map(str::trim).collect();
    if want.iter().all(|l| l.is_empty()) {
        return None;
    }
    let have: Vec<&str> = text.lines().map(str::trim).collect();
    have.windows(want.len())
        .position(|w| w == want.as_slice())
        .map(|i| i + 1)
}

fn no_match_hint(text: &str, old: &str) -> String {
    if old.lines().any(has_line_number_prefix) {
        return "old_string seems to include the line-number prefixes from read output; remove the number and tab at the start of each line and retry.".into();
    }
    if let Some(line) = whitespace_insensitive_match(text, old) {
        return format!(
            "Text that differs only in indentation or surrounding whitespace starts at line {line}; copy it exactly as read shows it (everything after the tab) and retry."
        );
    }
    "Call read on the file again and copy old_string exactly, including whitespace and line breaks."
        .into()
}

impl EditTool {
    /// An edit tool over `ws` sharing `tracker` with read and write.
    pub fn new(ws: Arc<dyn Workspace>, tracker: Arc<ReadTracker>) -> Self {
        Self {
            m: Mutator { ws, tracker },
        }
    }

    /// Run with raw JSON arguments.
    pub async fn call(&self, ctx: &ToolContext, args: Value) -> Result<ToolOutput, ToolError> {
        self.run(ctx, parse_args(NAME, args)?).await
    }

    /// Run with typed arguments.
    pub async fn run(&self, ctx: &ToolContext, args: EditArgs) -> Result<ToolOutput, ToolError> {
        let display = args.path.as_str();
        if args.old_string.is_empty() {
            return Err(ToolError::invalid_args(
                "old_string is empty. To create a file use write; to insert text, put an existing neighbouring line in old_string and that line plus the new text in new_string.",
            ));
        }
        if args.old_string == args.new_string {
            return Err(ToolError::invalid_args(
                "old_string and new_string are identical, so there is nothing to change. Put the replacement text in new_string.",
            ));
        }
        let path = self.m.ws.resolve(Path::new(display));
        let Some(before) = self.m.fresh_content(&path, display, "edit").await? else {
            return Err(ToolError::new(
                ToolErrorKind::NotFound,
                format!(
                    "{display} does not exist. Check the path, or use write to create the file."
                ),
            ));
        };
        let text = std::str::from_utf8(&before).map_err(|_| {
            ToolError::new(
                ToolErrorKind::Unsupported,
                format!("{display} is not valid UTF-8 text, so edit cannot change it safely. Use write to replace it entirely, or bash for binary edits."),
            )
        })?;
        let (old, new) = if uses_crlf(text) {
            (to_crlf(&args.old_string), to_crlf(&args.new_string))
        } else {
            (args.old_string.clone(), args.new_string.clone())
        };
        let positions: Vec<usize> = text.match_indices(&old).map(|(i, _)| i).collect();
        match positions.len() {
            0 => {
                return Err(ToolError::new(
                    ToolErrorKind::NoMatch,
                    format!(
                        "old_string was not found in {display}. {}",
                        no_match_hint(text, &old)
                    ),
                ));
            }
            n if n > 1 && !args.replace_all => {
                let lines: Vec<String> = positions
                    .iter()
                    .take(5)
                    .map(|&p| line_of(text, p).to_string())
                    .collect();
                let more = if n > 5 { ", ..." } else { "" };
                return Err(ToolError::new(
                    ToolErrorKind::AmbiguousMatch,
                    format!(
                        "old_string occurs {n} times in {display} (lines {}{more}). Include more surrounding lines to make it unique, or set replace_all=true to replace every occurrence.",
                        lines.join(", ")
                    ),
                ));
            }
            _ => {}
        }
        let after = if args.replace_all {
            text.replace(&old, &new)
        } else {
            text.replacen(&old, &new, 1)
        };
        self.m
            .commit(ctx, NAME, path, display, Some(&before), after.as_bytes())
            .await?;
        let n = positions.len();
        let s = if n == 1 { "" } else { "s" };
        Ok(ToolOutput::Text(format!(
            "Edited {display}: replaced {n} occurrence{s} (first at line {}).",
            line_of(text, positions[0])
        )))
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt as _;

    use super::*;
    use crate::local::LocalWorkspace;
    use crate::read::ReadTool;
    use crate::tool::{ChangeKind, RecordingEvents, ToolEvent};

    struct Fx {
        dir: tempfile::TempDir,
        read: ReadTool,
        edit: EditTool,
        events: Arc<RecordingEvents>,
        ctx: ToolContext,
    }

    fn fx(name: &str, content: &str) -> Fx {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(name), content).unwrap();
        let ws = Arc::new(LocalWorkspace::new(dir.path()));
        let tracker = Arc::new(ReadTracker::new());
        let events = Arc::new(RecordingEvents::new());
        Fx {
            read: ReadTool::new(ws.clone(), tracker.clone(), Default::default()),
            edit: EditTool::new(ws, tracker),
            ctx: ToolContext::new("e1").with_events(events.clone()),
            dir,
            events,
        }
    }

    impl Fx {
        async fn read(&self, path: &str) {
            self.read
                .call(&self.ctx, json!({"path": path}))
                .await
                .unwrap();
        }
        async fn edit(&self, v: Value) -> Result<ToolOutput, ToolError> {
            self.edit.call(&self.ctx, v).await
        }
        fn content(&self, name: &str) -> String {
            std::fs::read_to_string(self.dir.path().join(name)).unwrap()
        }
    }

    #[tokio::test]
    async fn replaces_unique_match() {
        let f = fx("a.rs", "fn a() {}\nfn b() {}\n");
        f.read("a.rs").await;
        let out = f
            .edit(json!({"path": "a.rs", "old_string": "fn b() {}", "new_string": "fn c() {}"}))
            .await
            .unwrap();
        assert_eq!(
            out.text(),
            "Edited a.rs: replaced 1 occurrence (first at line 2)."
        );
        assert_eq!(f.content("a.rs"), "fn a() {}\nfn c() {}\n");
    }

    #[tokio::test]
    async fn ambiguous_match_fails_unless_replace_all() {
        let f = fx("a.txt", "x\ny\nx\n");
        f.read("a.txt").await;
        let err = f
            .edit(json!({"path": "a.txt", "old_string": "x", "new_string": "z"}))
            .await
            .unwrap_err();
        assert_eq!(err.kind, ToolErrorKind::AmbiguousMatch);
        assert!(
            err.message.contains("occurs 2 times in a.txt (lines 1, 3)"),
            "{err}"
        );
        assert!(err.message.contains("replace_all=true"), "{err}");
        assert_eq!(f.content("a.txt"), "x\ny\nx\n", "unchanged on error");
        let out = f
            .edit(
                json!({"path": "a.txt", "old_string": "x", "new_string": "z", "replace_all": true}),
            )
            .await
            .unwrap();
        assert!(out.text().contains("replaced 2 occurrences"));
        assert_eq!(f.content("a.txt"), "z\ny\nz\n");
    }

    #[tokio::test]
    async fn requires_fresh_read() {
        let f = fx("a.txt", "hello\n");
        let err = f
            .edit(json!({"path": "a.txt", "old_string": "hello", "new_string": "bye"}))
            .await
            .unwrap_err();
        assert_eq!(err.kind, ToolErrorKind::NotRead);
        f.read("a.txt").await;
        std::fs::write(f.dir.path().join("a.txt"), "hello world\n").unwrap();
        let err = f
            .edit(json!({"path": "a.txt", "old_string": "hello", "new_string": "bye"}))
            .await
            .unwrap_err();
        assert_eq!(err.kind, ToolErrorKind::StaleRead);
        assert!(err.message.contains("read on a.txt again"), "{err}");
        assert_eq!(f.content("a.txt"), "hello world\n");
        // Consecutive edits after one read work: the tool records its own write.
        f.read("a.txt").await;
        f.edit(json!({"path": "a.txt", "old_string": "hello", "new_string": "bye"}))
            .await
            .unwrap();
        f.edit(json!({"path": "a.txt", "old_string": "world", "new_string": "all"}))
            .await
            .unwrap();
        assert_eq!(f.content("a.txt"), "bye all\n");
    }

    #[tokio::test]
    async fn crlf_and_permissions_preserved() {
        let f = fx("w.bat", "@echo off\r\necho one\r\necho two\r\n");
        let p = f.dir.path().join("w.bat");
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o750)).unwrap();
        f.read("w.bat").await;
        f.edit(json!({"path": "w.bat", "old_string": "echo one\necho two", "new_string": "echo 1\necho 2\necho 3"}))
            .await
            .unwrap();
        assert_eq!(
            f.content("w.bat"),
            "@echo off\r\necho 1\r\necho 2\r\necho 3\r\n"
        );
        let mode = std::fs::metadata(&p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o750);
    }

    #[tokio::test]
    async fn emits_change_event_with_diff() {
        let f = fx("a.txt", "a\nb\nc\n");
        f.read("a.txt").await;
        f.edit(json!({"path": "a.txt", "old_string": "b", "new_string": "B"}))
            .await
            .unwrap();
        let evs = f.events.events();
        let [(id, ToolEvent::FileChanged(c))] = evs.as_slice() else {
            panic!("{evs:?}")
        };
        assert_eq!(id, "e1");
        assert_eq!(c.tool, "edit");
        assert_eq!(c.kind, ChangeKind::Modified);
        assert!(c.diff.contains("-b\n+B\n"), "{}", c.diff);
        assert_ne!(c.before_hash.as_deref(), Some(c.after_hash.as_str()));
    }

    #[tokio::test]
    async fn errors_explain_recovery() {
        let f = fx("a.py", "def f():\n    return 1\n");
        f.read("a.py").await;
        let err = f
            .edit(json!({"path": "a.py", "old_string": "def f():\n  return 1", "new_string": "x"}))
            .await
            .unwrap_err();
        assert_eq!(err.kind, ToolErrorKind::NoMatch);
        assert!(err.message.contains("starts at line 1"), "{err}");
        let err = f
            .edit(json!({"path": "a.py", "old_string": "     2\t    return 1", "new_string": "x"}))
            .await
            .unwrap_err();
        assert!(err.message.contains("line-number prefixes"), "{err}");
        let err = f
            .edit(json!({"path": "a.py", "old_string": "nothing like it", "new_string": "x"}))
            .await
            .unwrap_err();
        assert!(err.message.contains("copy old_string exactly"), "{err}");
        let err = f
            .edit(json!({"path": "a.py", "old_string": "", "new_string": "x"}))
            .await
            .unwrap_err();
        assert!(err.message.contains("use write"), "{err}");
        let err = f
            .edit(json!({"path": "a.py", "old_string": "f", "new_string": "f"}))
            .await
            .unwrap_err();
        assert_eq!(err.kind, ToolErrorKind::InvalidArgs);
        let err = f
            .edit(json!({"path": "nope.py", "old_string": "a", "new_string": "b"}))
            .await
            .unwrap_err();
        assert_eq!(err.kind, ToolErrorKind::NotFound);
        assert!(err.message.contains("use write"), "{err}");
    }
}
