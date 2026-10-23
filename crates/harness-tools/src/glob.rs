//! The `glob` tool: find files by path pattern, newest first.
//!
//! - `pattern` uses globset syntax relative to `path` (default the project
//!   root): `*` stays within a directory, `**` crosses directories, braces
//!   pick alternatives (`src/**/*.{ts,tsx}`).
//! - `.gitignore` is honoured, hidden files are included and `.git` is
//!   skipped ([`crate::search`]).
//! - Results are sorted by modification time, newest first, and capped at
//!   [`MAX_RESULTS`]; paths the context's read filter rejects (protected
//!   paths) are never shown.
//! - Hosts without file search (an old remote helper) fall back to
//!   [`Workspace::glob`], sorted by path.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::Deserialize;
use serde_json::{Value, json};

use crate::grep::{display_path, relative_glob, search_error};
use crate::search::FindQuery;
use crate::spill::Spiller;
use crate::tool::{ToolContext, ToolError, ToolOutput, parse_args};
use crate::workspace::{Workspace, is_unsupported};

/// Tool name.
pub const NAME: &str = "glob";

/// Most paths one call lists.
pub const MAX_RESULTS: usize = 200;

/// One-line description shown to the model.
pub const DESCRIPTION: &str = "Find files by glob pattern (e.g. **/*.rs, src/**/test_*.py), newest first; honours .gitignore. Prefer it over bash find/ls.";

/// Arguments of the glob tool.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GlobArgs {
    /// Glob pattern relative to `path`.
    pub pattern: String,
    /// Directory to search (default: the project root).
    #[serde(default)]
    pub path: Option<String>,
}

/// JSON schema of the glob tool's arguments.
pub fn schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "pattern": {"type": "string", "description": "Glob, e.g. **/*.rs"},
            "path": {"type": "string", "description": "Directory (default: project root)"}
        },
        "required": ["pattern"],
        "additionalProperties": false
    })
}

/// The glob tool.
#[derive(Clone)]
pub struct GlobTool {
    ws: Arc<dyn Workspace>,
    spill: Spiller,
}

impl std::fmt::Debug for GlobTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GlobTool")
            .field("root", &self.ws.root())
            .finish_non_exhaustive()
    }
}

impl GlobTool {
    /// A glob tool over `ws`, spilling large output through `spill`.
    pub fn new(ws: Arc<dyn Workspace>, spill: Spiller) -> Self {
        Self { ws, spill }
    }

    /// Run with raw JSON arguments.
    pub async fn call(&self, ctx: &ToolContext, args: Value) -> Result<ToolOutput, ToolError> {
        self.run(ctx, parse_args(NAME, args)?).await
    }

    /// Run with typed arguments.
    pub async fn run(&self, ctx: &ToolContext, args: GlobArgs) -> Result<ToolOutput, ToolError> {
        let pattern = relative_glob(&args.pattern)?;
        let base = self
            .ws
            .resolve(Path::new(args.path.as_deref().unwrap_or(".")));
        let root = self.ws.root().to_path_buf();
        let query = FindQuery {
            base: base.clone(),
            glob: Some(pattern.clone()),
            ..Default::default()
        };
        let (mut files, by_time) = match self.ws.find(&query).await {
            Ok(r) => (
                r.files
                    .into_iter()
                    .map(|f| (f.path, f.modified))
                    .collect::<Vec<_>>(),
                true,
            ),
            Err(e) if is_unsupported(&e) => (self.fallback(&base, &pattern).await?, false),
            Err(e) => return Err(search_error(NAME, e)),
        };
        files.retain(|(p, _)| ctx.may_read(p));
        // Newest first; equal or unknown times keep path order.
        files.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        let total = files.len();
        if total == 0 {
            return Ok(ToolOutput::Text(format!(
                "No files match `{}`.",
                args.pattern
            )));
        }
        let mut out = String::new();
        for (p, _) in files.iter().take(MAX_RESULTS) {
            out.push_str(&display_path(&root, p));
            out.push('\n');
        }
        let order = if by_time {
            "newest first"
        } else {
            "sorted by path; this host cannot report modification times"
        };
        if total > MAX_RESULTS {
            out.push_str(&format!(
                "[Showing {MAX_RESULTS} of {total} files, {order}. Narrow the pattern or path to see the rest.]"
            ));
        } else if !by_time {
            out.push_str(&format!("[{order}]"));
        }
        let out = out.trim_end_matches('\n').to_owned();
        Ok(ToolOutput::Text(self.spill.apply(ctx, NAME, out)))
    }

    /// [`Workspace::glob`] under `base` for hosts without file search.
    async fn fallback(
        &self,
        base: &Path,
        pattern: &str,
    ) -> Result<Vec<(PathBuf, Option<std::time::SystemTime>)>, ToolError> {
        let root = self.ws.root();
        let rel = base.strip_prefix(root).map_err(|_| {
            ToolError::invalid_args(
                "on this host glob can only search inside the project; use bash `find` for other directories.",
            )
        })?;
        let full = if rel.as_os_str().is_empty() {
            pattern.to_owned()
        } else {
            format!("{}/{pattern}", rel.display())
        };
        let paths = self
            .ws
            .glob(&full)
            .await
            .map_err(|e| search_error(NAME, e))?;
        Ok(paths.into_iter().map(|p| (root.join(p), None)).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::local::LocalWorkspace;
    use crate::spill::MemoryBlobSink;
    use crate::workspace::{DirEntry, FileStat, WsResult};
    use std::time::{Duration, SystemTime};

    fn spiller() -> Spiller {
        Spiller::new(Arc::new(MemoryBlobSink::new()), &Default::default())
    }

    fn put(root: &Path, rel: &str, age_secs: u64) {
        let p = root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, rel).unwrap();
        let t = SystemTime::now() - Duration::from_secs(age_secs);
        std::fs::File::options()
            .write(true)
            .open(&p)
            .unwrap()
            .set_modified(t)
            .unwrap();
    }

    async fn glob(tool: &GlobTool, ctx: &ToolContext, args: Value) -> String {
        tool.call(ctx, args).await.unwrap().text().to_owned()
    }

    #[tokio::test]
    async fn newest_first_and_gitignore() {
        let d = tempfile::tempdir().unwrap();
        let r = d.path();
        put(r, ".gitignore", 500);
        std::fs::write(r.join(".gitignore"), "target/\n").unwrap();
        put(r, "src/old.rs", 300);
        put(r, "src/new.rs", 10);
        put(r, "src/mid/m.rs", 100);
        put(r, "target/gen.rs", 1);
        put(r, "README.md", 1);
        let tool = GlobTool::new(Arc::new(LocalWorkspace::new(r)), spiller());
        let ctx = ToolContext::new("t");
        assert_eq!(
            glob(&tool, &ctx, json!({"pattern": "**/*.rs"})).await,
            "src/new.rs\nsrc/mid/m.rs\nsrc/old.rs"
        );
        assert_eq!(
            glob(&tool, &ctx, json!({"pattern": "*.rs", "path": "src"})).await,
            "src/new.rs\nsrc/old.rs"
        );
        assert_eq!(
            glob(&tool, &ctx, json!({"pattern": "*.py"})).await,
            "No files match `*.py`."
        );
        let hidden = r.join("src/new.rs");
        let ctx = ctx.with_read_filter(Some(Arc::new(move |p: &Path| p != hidden)));
        assert_eq!(
            glob(&tool, &ctx, json!({"pattern": "src/*.rs"})).await,
            "src/old.rs"
        );
        let err = tool
            .call(&ctx, json!({"pattern": "../**"}))
            .await
            .unwrap_err();
        assert!(err.message.contains("`..`"), "{err}");
    }

    #[tokio::test]
    async fn caps_results() {
        let d = tempfile::tempdir().unwrap();
        for i in 0..(MAX_RESULTS + 5) {
            std::fs::write(d.path().join(format!("f{i:03}.txt")), "x").unwrap();
        }
        let tool = GlobTool::new(Arc::new(LocalWorkspace::new(d.path())), spiller());
        let out = glob(&tool, &ToolContext::new("t"), json!({"pattern": "*.txt"})).await;
        assert_eq!(out.lines().count(), MAX_RESULTS + 1);
        assert!(
            out.ends_with(&format!("[Showing {MAX_RESULTS} of {} files, newest first. Narrow the pattern or path to see the rest.]", MAX_RESULTS + 5)),
            "{out}"
        );
    }

    /// A host whose `find` is unsupported (an old remote helper).
    struct NoFind(LocalWorkspace);

    #[async_trait::async_trait]
    impl Workspace for NoFind {
        fn root(&self) -> &Path {
            self.0.root()
        }
        async fn read(&self, p: &Path) -> WsResult<Vec<u8>> {
            self.0.read(p).await
        }
        async fn write(&self, p: &Path, d: &[u8]) -> WsResult<()> {
            self.0.write(p, d).await
        }
        async fn replace_atomic(&self, p: &Path, d: &[u8]) -> WsResult<()> {
            self.0.replace_atomic(p, d).await
        }
        async fn set_mode(&self, p: &Path, m: u32) -> WsResult<()> {
            self.0.set_mode(p, m).await
        }
        async fn stat(&self, p: &Path) -> WsResult<FileStat> {
            self.0.stat(p).await
        }
        async fn list(&self, p: &Path) -> WsResult<Vec<DirEntry>> {
            self.0.list(p).await
        }
        async fn glob(&self, pattern: &str) -> WsResult<Vec<PathBuf>> {
            self.0.glob(pattern).await
        }
    }

    #[tokio::test]
    async fn falls_back_to_workspace_glob() {
        let d = tempfile::tempdir().unwrap();
        put(d.path(), "src/b.rs", 1);
        put(d.path(), "src/a.rs", 100);
        let ws = Arc::new(NoFind(LocalWorkspace::new(d.path())));
        let tool = GlobTool::new(ws.clone(), spiller());
        let out = glob(
            &tool,
            &ToolContext::new("t"),
            json!({"pattern": "*.rs", "path": "src"}),
        )
        .await;
        assert_eq!(
            out,
            "src/a.rs\nsrc/b.rs\n[sorted by path; this host cannot report modification times]"
        );
        // grep on such a host says to use bash.
        let grep = crate::grep::GrepTool::new(ws, spiller());
        let err = grep
            .call(&ToolContext::new("t"), json!({"pattern": "x"}))
            .await
            .unwrap_err();
        assert!(err.message.contains("Use bash"), "{err}");
    }
}
