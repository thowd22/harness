//! The `write` tool, plus the mutation helpers it shares with `edit`.
//!
//! Every mutation:
//! 1. refuses to overwrite an existing file unless the model's last read of
//!    it is still current (see [`ReadTracker`]);
//! 2. writes atomically through [`Workspace::replace_atomic`], which keeps
//!    permission bits;
//! 3. keeps the file's line-ending style (CRLF files stay CRLF);
//! 4. emits a [`ToolEvent::FileChanged`] with a unified diff for the change
//!    ledger, and records the new hash so follow-up edits need no re-read.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::Deserialize;
use serde_json::{Value, json};

use crate::tool::{
    ChangeKind, FileChange, ToolContext, ToolError, ToolErrorKind, ToolEvent, ToolOutput,
    parse_args,
};
use crate::tracker::{ReadTracker, content_hash};
use crate::workspace::{FileKind, Workspace, WsError};

/// Tool name.
pub const NAME: &str = "write";

/// One-line description shown to the model.
pub const DESCRIPTION: &str =
    "Create a file or replace its whole content. Read an existing file before overwriting it.";

/// Arguments of the write tool.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WriteArgs {
    /// File path, relative to the project root.
    pub path: String,
    /// Full new content.
    pub content: String,
}

/// JSON schema of the write tool's arguments.
pub fn schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "path": {"type": "string"},
            "content": {"type": "string", "description": "Full file content"}
        },
        "required": ["path", "content"],
        "additionalProperties": false
    })
}

/// True if `text` mostly uses CRLF line endings.
pub fn uses_crlf(text: &str) -> bool {
    let crlf = text.matches("\r\n").count();
    crlf > 0 && crlf * 2 >= text.matches('\n').count()
}

/// Convert all line endings in `text` to CRLF.
pub fn to_crlf(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\n', "\r\n")
}

/// Unified diff between `before` and `after` for `display` path.
pub fn unified_diff(display: &str, before: &str, after: &str) -> String {
    similar::TextDiff::from_lines(before, after)
        .unified_diff()
        .context_radius(3)
        .header(&format!("a/{display}"), &format!("b/{display}"))
        .to_string()
}

/// State shared by the mutating tools of one session.
#[derive(Clone)]
pub(crate) struct Mutator {
    pub(crate) ws: Arc<dyn Workspace>,
    pub(crate) tracker: Arc<ReadTracker>,
}

impl Mutator {
    /// Current content of `path` if it exists, after checking that the model
    /// read this exact version. `Ok(None)` means the file does not exist.
    pub(crate) async fn fresh_content(
        &self,
        path: &Path,
        display: &str,
        action: &str,
    ) -> Result<Option<Vec<u8>>, ToolError> {
        match self.ws.stat(path).await {
            Ok(st) if st.kind == FileKind::Dir => {
                return Err(ToolError::new(
                    ToolErrorKind::Unsupported,
                    format!("{display} is a directory; give the path of a file inside it."),
                ));
            }
            Ok(_) => {}
            Err(WsError::NotFound(_)) => return Ok(None),
            Err(e) => return Err(e.into()),
        }
        let current = self.ws.read(path).await?;
        match self.tracker.get(path) {
            None => Err(ToolError::new(
                ToolErrorKind::NotRead,
                format!(
                    "{display} already exists and you have not read it in this session. Call read on {display} first, then retry the {action} based on its current content."
                ),
            )),
            Some(seen) if seen != content_hash(&current) => Err(ToolError::new(
                ToolErrorKind::StaleRead,
                format!(
                    "{display} changed since you last read it (edited by a command, the user or another agent). Call read on {display} again and redo the {action} against the new content."
                ),
            )),
            Some(_) => Ok(Some(current)),
        }
    }

    /// Atomically write `after`, record its hash and emit the change event.
    pub(crate) async fn commit(
        &self,
        ctx: &ToolContext,
        tool: &'static str,
        path: PathBuf,
        display: &str,
        before: Option<&[u8]>,
        after: &[u8],
    ) -> Result<FileChange, ToolError> {
        self.ws.replace_atomic(&path, after).await?;
        let after_hash = content_hash(after);
        self.tracker.record(&path, after_hash.clone());
        let before_text = before.map(String::from_utf8_lossy).unwrap_or_default();
        let change = FileChange {
            tool,
            path,
            kind: if before.is_some() {
                ChangeKind::Modified
            } else {
                ChangeKind::Created
            },
            before_hash: before.map(content_hash),
            after_hash,
            diff: unified_diff(display, &before_text, &String::from_utf8_lossy(after)),
        };
        ctx.emit(ToolEvent::FileChanged(change.clone()));
        Ok(change)
    }
}

/// The write tool.
#[derive(Clone)]
pub struct WriteTool {
    m: Mutator,
}

impl std::fmt::Debug for WriteTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WriteTool")
            .field("root", &self.m.ws.root())
            .finish_non_exhaustive()
    }
}

impl WriteTool {
    /// A write tool over `ws` sharing `tracker` with read and edit.
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
    pub async fn run(&self, ctx: &ToolContext, args: WriteArgs) -> Result<ToolOutput, ToolError> {
        let path = self.m.ws.resolve(Path::new(&args.path));
        let before = self.m.fresh_content(&path, &args.path, "write").await?;
        let keep_crlf = before
            .as_deref()
            .is_some_and(|b| uses_crlf(&String::from_utf8_lossy(b)))
            && !args.content.contains("\r\n");
        let content = if keep_crlf {
            to_crlf(&args.content)
        } else {
            args.content
        };
        let change = self
            .m
            .commit(
                ctx,
                NAME,
                path,
                &args.path,
                before.as_deref(),
                content.as_bytes(),
            )
            .await?;
        let verb = match change.kind {
            ChangeKind::Created => "Created",
            ChangeKind::Modified => "Overwrote",
        };
        let crlf = if keep_crlf {
            ", kept CRLF line endings"
        } else {
            ""
        };
        Ok(ToolOutput::Text(format!(
            "{verb} {} ({} lines{crlf}).",
            args.path,
            content.lines().count()
        )))
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt as _;

    use super::*;
    use crate::local::LocalWorkspace;
    use crate::read::ReadTool;
    use crate::tool::RecordingEvents;

    pub(crate) struct Fx {
        pub dir: tempfile::TempDir,
        pub ws: Arc<LocalWorkspace>,
        pub read: ReadTool,
        pub write: WriteTool,
        pub events: Arc<RecordingEvents>,
        pub ctx: ToolContext,
    }

    pub(crate) fn fx() -> Fx {
        let dir = tempfile::tempdir().unwrap();
        let ws = Arc::new(LocalWorkspace::new(dir.path()));
        let tracker = Arc::new(ReadTracker::new());
        let events = Arc::new(RecordingEvents::new());
        Fx {
            read: ReadTool::new(ws.clone(), tracker.clone(), Default::default()),
            write: WriteTool::new(ws.clone(), tracker),
            ctx: ToolContext::new("w1").with_events(events.clone()),
            dir,
            ws,
            events,
        }
    }

    async fn write(f: &Fx, path: &str, content: &str) -> Result<ToolOutput, ToolError> {
        f.write
            .call(&f.ctx, json!({"path": path, "content": content}))
            .await
    }

    #[tokio::test]
    async fn creates_new_file_without_prior_read() {
        let f = fx();
        let out = write(&f, "src/new.txt", "a\nb\n").await.unwrap();
        assert_eq!(out.text(), "Created src/new.txt (2 lines).");
        assert_eq!(
            std::fs::read_to_string(f.dir.path().join("src/new.txt")).unwrap(),
            "a\nb\n"
        );
    }

    #[tokio::test]
    async fn overwrite_requires_current_read() {
        let f = fx();
        f.ws.write(Path::new("a.txt"), b"old\n").await.unwrap();
        let err = write(&f, "a.txt", "new\n").await.unwrap_err();
        assert_eq!(err.kind, ToolErrorKind::NotRead);
        assert!(err.message.contains("Call read on a.txt first"), "{err}");

        f.read.call(&f.ctx, json!({"path": "a.txt"})).await.unwrap();
        f.ws.write(Path::new("a.txt"), b"changed behind our back\n")
            .await
            .unwrap();
        let err = write(&f, "a.txt", "new\n").await.unwrap_err();
        assert_eq!(err.kind, ToolErrorKind::StaleRead);
        assert!(err.message.contains("read on a.txt again"), "{err}");

        f.read.call(&f.ctx, json!({"path": "a.txt"})).await.unwrap();
        write(&f, "a.txt", "new\n").await.unwrap();
        // Own writes refresh the hash: a second write needs no re-read.
        write(&f, "a.txt", "newer\n").await.unwrap();
        assert_eq!(f.ws.read(Path::new("a.txt")).await.unwrap(), b"newer\n");
    }

    #[tokio::test]
    async fn preserves_permissions_and_crlf() {
        let f = fx();
        let p = f.dir.path().join("run.sh");
        std::fs::write(&p, "echo a\r\necho b\r\n").unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        f.read
            .call(&f.ctx, json!({"path": "run.sh"}))
            .await
            .unwrap();
        let out = write(&f, "run.sh", "echo c\necho d\n").await.unwrap();
        assert!(out.text().contains("kept CRLF"), "{}", out.text());
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "echo c\r\necho d\r\n");
        let mode = std::fs::metadata(&p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o755);
    }

    #[tokio::test]
    async fn emits_change_events_with_diff() {
        let f = fx();
        write(&f, "n.txt", "one\n").await.unwrap();
        write(&f, "n.txt", "one\ntwo\n").await.unwrap();
        let evs = f.events.events();
        let changes: Vec<_> = evs
            .iter()
            .filter_map(|(id, e)| match e {
                ToolEvent::FileChanged(c) => Some((id.as_str(), c)),
                _ => None,
            })
            .collect();
        assert_eq!(changes.len(), 2);
        let (id, created) = changes[0];
        assert_eq!(id, "w1");
        assert_eq!(created.kind, ChangeKind::Created);
        assert_eq!(created.before_hash, None);
        assert!(created.diff.contains("+one"), "{}", created.diff);
        let (_, modified) = changes[1];
        assert_eq!(modified.kind, ChangeKind::Modified);
        assert_eq!(
            modified.before_hash.as_deref(),
            Some(created.after_hash.as_str())
        );
        assert!(
            modified.diff.contains("--- a/n.txt\n+++ b/n.txt\n"),
            "{}",
            modified.diff
        );
        assert!(modified.diff.contains("+two"), "{}", modified.diff);
        assert_eq!(modified.path, f.ws.resolve(Path::new("n.txt")));
    }

    #[tokio::test]
    async fn directory_and_bad_args_errors() {
        let f = fx();
        std::fs::create_dir(f.dir.path().join("d")).unwrap();
        let err = write(&f, "d", "x").await.unwrap_err();
        assert_eq!(err.kind, ToolErrorKind::Unsupported);
        let err = f
            .write
            .call(&f.ctx, json!({"path": "x"}))
            .await
            .unwrap_err();
        assert_eq!(err.kind, ToolErrorKind::InvalidArgs);
        assert!(err.message.contains("content"), "{err}");
    }

    #[test]
    fn line_ending_helpers() {
        assert!(uses_crlf("a\r\nb\r\n"));
        assert!(!uses_crlf("a\nb\n"));
        assert!(!uses_crlf("a\r\nb\nc\nd\n"));
        assert_eq!(to_crlf("a\nb\r\n"), "a\r\nb\r\n");
    }
}
