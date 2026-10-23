//! The `grep` tool: ripgrep-style content search over the workspace.
//!
//! - `pattern` is a Rust regex; `-i` ignores case and `multiline` lets `.`
//!   match newlines. `^`/`$` match at line boundaries.
//! - `path` (file or directory, default the project root), `glob` (e.g.
//!   `*.rs`, `src/**/*.{ts,tsx}`; without a `/` it matches file names at any
//!   depth) and `type` (ripgrep file types such as `rust`, `py`) narrow the
//!   search. `.gitignore` is honoured and `.git` skipped ([`crate::search`]).
//! - `output_mode`: `files_with_matches` (default, paths sorted by path),
//!   `content` (`path:line:text`, context lines `path-line-text`, groups
//!   separated by `--`; `-n`, `-A`, `-B`, `-C`) or `count` (`path:count`).
//! - `head_limit` (default [`DEFAULT_HEAD_LIMIT`], 0 for no limit) and
//!   `offset` page through entries; the whole output is still subject to
//!   the spill budget.
//! - Paths the context's read filter rejects (protected paths) are never
//!   shown. The tool is read-only and does not count as reading a file for
//!   edit's stale-read check.

use std::path::Path;
use std::sync::Arc;

use serde::Deserialize;
use serde_json::{Value, json};

use crate::search::{FindQuery, MAX_SEARCH_FILE_BYTES, build_regex, looks_binary, match_lines};
use crate::spill::Spiller;
use crate::tool::{ToolContext, ToolError, ToolErrorKind, ToolOutput, parse_args};
use crate::workspace::{Workspace, WsError, is_unsupported};

/// Tool name.
pub const NAME: &str = "grep";

/// Entries shown when the call gives no `head_limit`.
pub const DEFAULT_HEAD_LIMIT: usize = 250;

/// Longer lines are cut in content output.
pub const MAX_LINE_CHARS: usize = 400;

/// Largest `-A`/`-B`/`-C` value.
pub const MAX_CONTEXT: usize = 50;

/// One-line description shown to the model.
pub const DESCRIPTION: &str = "Search file contents with a regex (ripgrep-style; honours .gitignore). Prefer it over bash grep/rg. Modes: files_with_matches (default), content (with -n/-A/-B/-C), count.";

/// What grep prints.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutputMode {
    /// Paths of matching files.
    #[default]
    #[serde(alias = "files")]
    FilesWithMatches,
    /// Matching lines with optional context.
    Content,
    /// Matching lines per file.
    Count,
}

/// Arguments of the grep tool.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GrepArgs {
    /// Regex to search for.
    pub pattern: String,
    /// File or directory to search (default: the project root).
    #[serde(default)]
    pub path: Option<String>,
    /// Glob filter for file paths.
    #[serde(default)]
    pub glob: Option<String>,
    /// ripgrep file type.
    #[serde(default, rename = "type")]
    pub file_type: Option<String>,
    /// Output mode.
    #[serde(default)]
    pub output_mode: Option<OutputMode>,
    /// Case-insensitive.
    #[serde(default, rename = "-i")]
    pub case_insensitive: Option<bool>,
    /// Show line numbers in content mode (default true).
    #[serde(default, rename = "-n")]
    pub line_numbers: Option<bool>,
    /// Lines of context after each match.
    #[serde(default, rename = "-A")]
    pub after: Option<usize>,
    /// Lines of context before each match.
    #[serde(default, rename = "-B")]
    pub before: Option<usize>,
    /// Lines of context before and after each match.
    #[serde(default, rename = "-C")]
    pub context: Option<usize>,
    /// Let `.` match newlines.
    #[serde(default)]
    pub multiline: Option<bool>,
    /// Most entries to show (0 = no limit).
    #[serde(default)]
    pub head_limit: Option<usize>,
    /// Entries to skip first.
    #[serde(default)]
    pub offset: Option<usize>,
}

/// JSON schema of the grep tool's arguments.
pub fn schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "pattern": {"type": "string", "description": "Regex (Rust syntax)"},
            "path": {"type": "string", "description": "File or directory (default: project root)"},
            "glob": {"type": "string", "description": "File filter, e.g. *.rs or src/**/*.{ts,tsx}"},
            "type": {"type": "string", "description": "File type, e.g. rust, py, js"},
            "output_mode": {"type": "string", "enum": ["files_with_matches", "content", "count"]},
            "-i": {"type": "boolean"},
            "-n": {"type": "boolean"},
            "-A": {"type": "integer", "minimum": 0},
            "-B": {"type": "integer", "minimum": 0},
            "-C": {"type": "integer", "minimum": 0},
            "multiline": {"type": "boolean", "description": "Let . match newlines"},
            "head_limit": {"type": "integer", "minimum": 0, "description": "Max entries (default 250, 0 = all)"},
            "offset": {"type": "integer", "minimum": 0}
        },
        "required": ["pattern"],
        "additionalProperties": false
    })
}

/// Turn a model-supplied glob into one relative to the search base:
/// rejects absolute and `..` patterns, strips `./`.
pub fn relative_glob(glob: &str) -> Result<String, ToolError> {
    let g = glob.trim();
    let g = g.strip_prefix("./").unwrap_or(g);
    if g.is_empty() || g.starts_with('/') || g.starts_with('~') || g.split('/').any(|c| c == "..") {
        return Err(ToolError::invalid_args(format!(
            "glob {glob:?} must be relative to `path` and must not contain `..`. Put the directory in `path` and keep only the file pattern in the glob."
        )));
    }
    Ok(g.to_owned())
}

/// Path shown to the model: relative to `root` when inside it.
pub fn display_path(root: &Path, path: &Path) -> String {
    match path.strip_prefix(root) {
        Ok(rel) if rel.as_os_str().is_empty() => ".".into(),
        Ok(rel) => rel.display().to_string(),
        Err(_) => path.display().to_string(),
    }
}

/// The model-facing error for a failed workspace search.
pub fn search_error(tool: &str, e: WsError) -> ToolError {
    if is_unsupported(&e) {
        return ToolError::new(
            ToolErrorKind::Unsupported,
            format!(
                "{tool} is not available on this host (its remote helper is too old or missing). Use bash with rg, grep or find instead."
            ),
        );
    }
    e.into()
}

/// The grep tool.
#[derive(Clone)]
pub struct GrepTool {
    ws: Arc<dyn Workspace>,
    spill: Spiller,
}

impl std::fmt::Debug for GrepTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GrepTool")
            .field("root", &self.ws.root())
            .finish_non_exhaustive()
    }
}

impl GrepTool {
    /// A grep tool over `ws`, spilling large output through `spill`.
    pub fn new(ws: Arc<dyn Workspace>, spill: Spiller) -> Self {
        Self { ws, spill }
    }

    /// Run with raw JSON arguments.
    pub async fn call(&self, ctx: &ToolContext, args: Value) -> Result<ToolOutput, ToolError> {
        self.run(ctx, parse_args(NAME, args)?).await
    }

    /// Run with typed arguments.
    pub async fn run(&self, ctx: &ToolContext, args: GrepArgs) -> Result<ToolOutput, ToolError> {
        let text = self.search(ctx, &args).await?;
        Ok(ToolOutput::Text(self.spill.apply(ctx, NAME, text)))
    }

    async fn search(&self, ctx: &ToolContext, args: &GrepArgs) -> Result<String, ToolError> {
        if args.pattern.is_empty() {
            return Err(ToolError::invalid_args(
                "pattern is empty. Give a regex to search for; use the glob tool to list files.",
            ));
        }
        let glob = args
            .glob
            .as_deref()
            .map(|g| {
                relative_glob(g).map(|g| {
                    if g.contains('/') {
                        g
                    } else {
                        format!("**/{g}")
                    }
                })
            })
            .transpose()?;
        let mode = args.output_mode.unwrap_or_default();
        let case_insensitive = args.case_insensitive.unwrap_or(false);
        let multiline = args.multiline.unwrap_or(false);
        let base = self
            .ws
            .resolve(Path::new(args.path.as_deref().unwrap_or(".")));
        let query = FindQuery {
            base,
            pattern: Some(args.pattern.clone()),
            case_insensitive,
            multiline,
            glob,
            file_type: args.file_type.clone(),
            count: mode == OutputMode::Count,
        };
        let found = self
            .ws
            .find(&query)
            .await
            .map_err(|e| search_error(NAME, e))?;
        let root = self.ws.root().to_path_buf();
        let files: Vec<_> = found
            .files
            .into_iter()
            .filter(|f| ctx.may_read(&f.path))
            .collect();
        let offset = args.offset.unwrap_or(0);
        let limit = match args.head_limit {
            Some(0) => usize::MAX,
            Some(n) => n,
            None => DEFAULT_HEAD_LIMIT,
        };

        let mut out = String::new();
        let mut notes = Vec::new();
        if files.is_empty() {
            out.push_str("No matches found.");
        } else {
            match mode {
                OutputMode::FilesWithMatches | OutputMode::Count => {
                    let total = files.len();
                    let sum: u64 = files.iter().map(|f| f.matches).sum();
                    for f in files.iter().skip(offset).take(limit) {
                        out.push_str(&display_path(&root, &f.path));
                        if mode == OutputMode::Count {
                            out.push_str(&format!(":{}", f.matches));
                        }
                        out.push('\n');
                    }
                    let shown = total.saturating_sub(offset).min(limit);
                    if mode == OutputMode::Count {
                        notes.push(format!(
                            "{sum} matching {} in {}.",
                            plural(sum as usize, "line", "lines"),
                            files_word(total)
                        ));
                    } else if total == 1 {
                        notes.push("1 file matches.".into());
                    } else {
                        notes.push(format!("{total} files match."));
                    }
                    if offset + shown < total {
                        notes.push(format!(
                            "Showing {}-{} of {total}; pass offset={} to see more, or narrow with path, glob or type.",
                            offset + 1,
                            offset + shown,
                            offset + shown
                        ));
                    }
                }
                OutputMode::Content => {
                    let cfg = ContentOptions {
                        before: args.before.or(args.context).unwrap_or(0).min(MAX_CONTEXT),
                        after: args.after.or(args.context).unwrap_or(0).min(MAX_CONTEXT),
                        numbers: args.line_numbers.unwrap_or(true),
                    };
                    let re = build_regex(&args.pattern, case_insensitive, multiline)
                        .map_err(ToolError::from)?;
                    let mut page = Page {
                        skip: offset,
                        limit,
                        emitted: 0,
                        more: false,
                    };
                    for f in &files {
                        if page.more {
                            break;
                        }
                        let data = match self.ws.read(&f.path).await {
                            Ok(d) if !looks_binary(&d) => d,
                            _ => continue,
                        };
                        let shown = display_path(&root, &f.path);
                        render_content(&mut out, &mut page, &shown, &data, &re, &cfg);
                    }
                    if page.more {
                        notes.push(format!(
                            "Output limited to {limit} lines; pass offset={} to continue, or narrow with path, glob or type.",
                            offset + page.emitted
                        ));
                    } else if page.emitted == 0 {
                        out.push_str("No lines to show at this offset.");
                    }
                }
            }
        }
        if found.skipped_large > 0 {
            notes.push(format!(
                "{} files larger than {} MiB were not searched; use bash rg for them.",
                found.skipped_large,
                MAX_SEARCH_FILE_BYTES / (1024 * 1024)
            ));
        }
        if found.truncated {
            notes.push(
                "The search stopped early at its file limit; narrow it with path, glob or type."
                    .into(),
            );
        }
        if !notes.is_empty() {
            if !out.is_empty() && !out.ends_with('\n') {
                out.push('\n');
            }
            out.push_str(&format!("[{}]", notes.join(" ")));
        }
        Ok(out.trim_end_matches('\n').to_owned())
    }
}

fn plural(n: usize, one: &'static str, many: &'static str) -> &'static str {
    if n == 1 { one } else { many }
}

fn files_word(n: usize) -> String {
    format!("{n} {}", plural(n, "file", "files"))
}

struct ContentOptions {
    before: usize,
    after: usize,
    numbers: bool,
}

/// Paging state across files: lines skipped, emitted and whether more were cut.
struct Page {
    skip: usize,
    limit: usize,
    emitted: usize,
    more: bool,
}

impl Page {
    /// Push `line` unless skipped; returns false once the limit is reached.
    fn push(&mut self, out: &mut String, line: &str) -> bool {
        if self.skip > 0 {
            self.skip -= 1;
            return true;
        }
        if self.emitted >= self.limit {
            self.more = true;
            return false;
        }
        out.push_str(line);
        out.push('\n');
        self.emitted += 1;
        true
    }
}

fn truncate(line: &str) -> std::borrow::Cow<'_, str> {
    match line.char_indices().nth(MAX_LINE_CHARS) {
        None => line.into(),
        Some((cut, _)) => format!("{}… [line truncated]", &line[..cut]).into(),
    }
}

fn render_content(
    out: &mut String,
    page: &mut Page,
    path: &str,
    data: &[u8],
    re: &regex::bytes::Regex,
    cfg: &ContentOptions,
) {
    let spans = match_lines(re, data);
    if spans.is_empty() {
        return;
    }
    let text = String::from_utf8_lossy(data);
    let lines: Vec<&str> = text
        .split('\n')
        .map(|l| l.strip_suffix('\r').unwrap_or(l))
        .collect();
    let n = if text.ends_with('\n') {
        lines.len() - 1
    } else {
        lines.len()
    };
    // Group matches with their context; groups that touch are merged.
    let mut groups: Vec<(usize, usize)> = Vec::new();
    for &(a, b) in &spans {
        let lo = a.saturating_sub(cfg.before);
        let hi = (b + cfg.after).min(n.saturating_sub(1));
        match groups.last_mut() {
            Some(g) if lo <= g.1 + 1 => g.1 = g.1.max(hi),
            _ => groups.push((lo, hi)),
        }
    }
    let is_match = |i: usize| spans.iter().any(|&(a, b)| a <= i && i <= b);
    let context = cfg.before > 0 || cfg.after > 0;
    for &(lo, hi) in &groups {
        if context && page.skip == 0 && page.emitted > 0 {
            if page.emitted >= page.limit {
                page.more = true;
                return;
            }
            out.push_str("--\n");
        }
        for (i, line) in lines.iter().enumerate().take(hi + 1).skip(lo) {
            let sep = if is_match(i) { ':' } else { '-' };
            let rendered = if cfg.numbers {
                format!("{path}{sep}{}{sep}{}", i + 1, truncate(line))
            } else {
                format!("{path}{sep}{}", truncate(line))
            };
            if !page.push(out, &rendered) {
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::local::LocalWorkspace;
    use crate::spill::MemoryBlobSink;

    struct Fixture {
        dir: tempfile::TempDir,
        tool: GrepTool,
    }

    fn fixture() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let ws = Arc::new(LocalWorkspace::new(dir.path()));
        let spill = Spiller::new(Arc::new(MemoryBlobSink::new()), &Default::default());
        let f = Fixture {
            tool: GrepTool::new(ws, spill),
            dir,
        };
        f.put(".gitignore", "vendor/\n");
        f.put(
            "src/lib.rs",
            "mod net;\npub fn connect() {}\n// TODO: retry\nfn helper() {}\n",
        );
        f.put(
            "src/net/mod.rs",
            "pub fn connect_tcp() {}\nfn a() {}\nfn b() {}\nfn c() {}\npub fn Connect() {}\n",
        );
        f.put("vendor/dep.rs", "pub fn connect() {}\n");
        f.put("README.md", "Call connect() first.\n");
        f
    }

    impl Fixture {
        fn put(&self, rel: &str, text: &str) {
            let p = self.dir.path().join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, text).unwrap();
        }

        async fn grep(&self, args: Value) -> String {
            self.grep_ctx(&ToolContext::new("t"), args).await
        }

        async fn grep_ctx(&self, ctx: &ToolContext, args: Value) -> String {
            match self.tool.call(ctx, args).await.unwrap() {
                ToolOutput::Text(t) => t,
                other => panic!("{other:?}"),
            }
        }
    }

    #[tokio::test]
    async fn files_mode_honours_gitignore_and_filters() {
        let f = fixture();
        assert_eq!(
            f.grep(json!({"pattern": "connect"})).await,
            "README.md\nsrc/lib.rs\nsrc/net/mod.rs\n[3 files match.]"
        );
        assert_eq!(
            f.grep(json!({"pattern": "connect", "glob": "*.rs"})).await,
            "src/lib.rs\nsrc/net/mod.rs\n[2 files match.]"
        );
        assert_eq!(
            f.grep(json!({"pattern": "connect", "type": "md"})).await,
            "README.md\n[1 file matches.]"
        );
        assert_eq!(
            f.grep(json!({"pattern": "connect", "path": "src/net"}))
                .await,
            "src/net/mod.rs\n[1 file matches.]"
        );
        assert_eq!(
            f.grep(json!({"pattern": "nothing-here"})).await,
            "No matches found."
        );
    }

    #[tokio::test]
    async fn content_mode_with_numbers_context_and_case() {
        let f = fixture();
        assert_eq!(
            f.grep(json!({"pattern": "TODO", "output_mode": "content"}))
                .await,
            "src/lib.rs:3:// TODO: retry"
        );
        assert_eq!(
            f.grep(json!({"pattern": "TODO", "output_mode": "content", "-n": false, "-B": 1}))
                .await,
            "src/lib.rs-pub fn connect() {}\nsrc/lib.rs:// TODO: retry"
        );
        assert_eq!(
            f.grep(json!({"pattern": "^pub fn connect", "path": "src/net/mod.rs", "output_mode": "content", "-i": true, "-C": 1}))
                .await,
            "src/net/mod.rs:1:pub fn connect_tcp() {}\nsrc/net/mod.rs-2-fn a() {}\n--\nsrc/net/mod.rs-4-fn c() {}\nsrc/net/mod.rs:5:pub fn Connect() {}"
        );
    }

    #[tokio::test]
    async fn count_mode_and_paging() {
        let f = fixture();
        assert_eq!(
            f.grep(json!({"pattern": "fn", "output_mode": "count", "glob": "src/**"}))
                .await,
            "src/lib.rs:2\nsrc/net/mod.rs:5\n[7 matching lines in 2 files.]"
        );
        assert_eq!(
            f.grep(json!({"pattern": "connect", "head_limit": 1, "offset": 1}))
                .await,
            "src/lib.rs\n[3 files match. Showing 2-2 of 3; pass offset=2 to see more, or narrow with path, glob or type.]"
        );
        let out = f
            .grep(json!({"pattern": "fn", "output_mode": "content", "head_limit": 2}))
            .await;
        assert_eq!(
            out,
            "src/lib.rs:2:pub fn connect() {}\nsrc/lib.rs:4:fn helper() {}\n[Output limited to 2 lines; pass offset=2 to continue, or narrow with path, glob or type.]"
        );
        let out = f
            .grep(json!({"pattern": "fn", "output_mode": "content", "head_limit": 2, "offset": 2}))
            .await;
        assert!(out.starts_with("src/net/mod.rs:1:pub fn connect_tcp() {}\nsrc/net/mod.rs:2:fn a() {}\n[Output limited to 2 lines; pass offset=4"), "{out}");
    }

    #[tokio::test]
    async fn multiline_matches_span_lines() {
        let f = fixture();
        let out = f
            .grep(json!({"pattern": "connect\\(\\) \\{\\}.*TODO", "multiline": true, "output_mode": "content"}))
            .await;
        assert_eq!(
            out,
            "src/lib.rs:2:pub fn connect() {}\nsrc/lib.rs:3:// TODO: retry"
        );
    }

    #[tokio::test]
    async fn read_filter_hides_protected_paths() {
        let f = fixture();
        let secret = f.dir.path().join("src/net/mod.rs");
        let ctx =
            ToolContext::new("t").with_read_filter(Some(Arc::new(move |p: &Path| p != secret)));
        assert_eq!(
            f.grep_ctx(&ctx, json!({"pattern": "connect", "glob": "*.rs"}))
                .await,
            "src/lib.rs\n[1 file matches.]"
        );
    }

    #[tokio::test]
    async fn bad_arguments_explain_the_fix() {
        let f = fixture();
        let ctx = ToolContext::new("t");
        for (args, want) in [
            (json!({"pattern": "a("}), "escape literal"),
            (
                json!({"pattern": "a", "glob": "../*.rs"}),
                "must not contain `..`",
            ),
            (
                json!({"pattern": "a", "glob": "/etc/*"}),
                "relative to `path`",
            ),
            (
                json!({"pattern": "a", "type": "klingon"}),
                "unknown file type",
            ),
            (json!({"pattern": "a", "path": "missing"}), "no such file"),
            (json!({"pattern": ""}), "pattern is empty"),
            (json!({"pattern": "a", "bogus": 1}), "invalid arguments"),
        ] {
            let err = f.tool.call(&ctx, args.clone()).await.unwrap_err();
            assert!(err.message.contains(want), "{args}: {err}");
        }
    }

    #[tokio::test]
    async fn large_output_is_spilled() {
        let dir = tempfile::tempdir().unwrap();
        let body: String = (0..2000).map(|i| format!("match line {i}\n")).collect();
        std::fs::write(dir.path().join("big.txt"), body).unwrap();
        let sink = Arc::new(MemoryBlobSink::new());
        let tool = GrepTool::new(
            Arc::new(LocalWorkspace::new(dir.path())),
            Spiller::new(sink, &Default::default()),
        );
        let out = tool
            .call(
                &ToolContext::new("t"),
                json!({"pattern": "match", "output_mode": "content", "head_limit": 0}),
            )
            .await
            .unwrap();
        let text = out.text();
        assert!(text.contains("output truncated"), "{text}");
        assert!(text.contains("spill:"), "{text}");
        assert!(text.len() < 20_000, "{}", text.len());
    }

    #[tokio::test]
    async fn unsupported_hosts_point_to_bash() {
        let e = search_error(
            NAME,
            WsError::Io {
                path: "x".into(),
                source: std::io::Error::new(std::io::ErrorKind::Unsupported, "no"),
            },
        );
        assert_eq!(e.kind, ToolErrorKind::Unsupported);
        assert!(e.message.contains("bash"), "{e}");
    }
}
