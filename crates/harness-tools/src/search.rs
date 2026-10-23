//! File search shared by the `grep` and `glob` tools: a gitignore-aware,
//! parallel directory walk with an optional regex content filter.
//!
//! [`find_blocking`] is the native engine. [`crate::local::LocalWorkspace`]
//! runs it on a blocking thread, and the remote helper runs the same code on
//! its host (`fs.find`), so both see identical results. Walk rules:
//!
//! - `.gitignore`, `.ignore` and git's global excludes are honoured (also
//!   outside a git repository); hidden files are searched; the `.git`
//!   directory is always skipped.
//! - Binary files (a NUL byte in the first 8 KiB) and files larger than
//!   [`MAX_SEARCH_FILE_BYTES`] are skipped; the latter are counted.
//! - A file named directly as the search base is searched even when an
//!   ignore rule would skip it.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

use crate::workspace::{WsError, WsResult};

/// Files larger than this are not searched for content.
pub const MAX_SEARCH_FILE_BYTES: u64 = 32 * 1024 * 1024;

/// Most files one search returns; [`FindResult::truncated`] is set beyond.
pub const MAX_FOUND_FILES: usize = 100_000;

/// How many leading bytes are checked for NUL when sniffing binary content.
const SNIFF_BYTES: usize = 8192;

/// What to look for. Paths are absolute (already resolved by the caller).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FindQuery {
    /// Directory to walk, or a single file to search.
    pub base: PathBuf,
    /// Regex the file content must match; `None` lists every file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pattern: Option<String>,
    /// Match the pattern case-insensitively.
    #[serde(default)]
    pub case_insensitive: bool,
    /// Let `.` match newlines, so a match may span lines.
    #[serde(default)]
    pub multiline: bool,
    /// Glob (globset syntax) the path relative to `base` must match; `/` is
    /// only matched by `/`, `**` crosses directories.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub glob: Option<String>,
    /// ripgrep file type (`rs`, `py`, `js`, ...).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_type: Option<String>,
    /// Count matching lines per file (otherwise `matches` is 1 per file).
    #[serde(default)]
    pub count: bool,
}

/// One file the search found.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FoundFile {
    /// Absolute path.
    pub path: PathBuf,
    /// Matching lines when [`FindQuery::count`] was set, else 1.
    pub matches: u64,
    /// Last modification time, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub modified: Option<SystemTime>,
}

/// Result of [`find_blocking`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FindResult {
    /// Found files, sorted by path.
    pub files: Vec<FoundFile>,
    /// True when the search stopped at [`MAX_FOUND_FILES`].
    #[serde(default)]
    pub truncated: bool,
    /// Files skipped because they exceed [`MAX_SEARCH_FILE_BYTES`].
    #[serde(default)]
    pub skipped_large: u64,
}

/// Compile a search regex: `^`/`$` match at line boundaries (CRLF aware),
/// and `multiline` lets `.` match newlines.
pub fn build_regex(
    pattern: &str,
    case_insensitive: bool,
    multiline: bool,
) -> WsResult<regex::bytes::Regex> {
    regex::bytes::RegexBuilder::new(pattern)
        .case_insensitive(case_insensitive)
        .multi_line(true)
        .crlf(true)
        .dot_matches_new_line(multiline)
        .build()
        .map_err(|e| WsError::InvalidPattern {
            pattern: pattern.to_owned(),
            reason: format!(
                "invalid regex: {} (Rust regex syntax: escape literal ( ) [ ] {{ }} . * + ? | with a backslash)",
                first_line(&e.to_string())
            ),
        })
}

fn first_line(s: &str) -> String {
    // regex errors are multi-line drawings; the last line names the problem.
    s.lines()
        .rev()
        .find(|l| l.trim_start().starts_with("error:"))
        .unwrap_or_else(|| s.lines().next().unwrap_or(s))
        .trim()
        .to_owned()
}

fn build_glob(glob: &str) -> WsResult<globset::GlobMatcher> {
    globset::GlobBuilder::new(glob)
        .literal_separator(true)
        .build()
        .map(|g| g.compile_matcher())
        .map_err(|e| WsError::InvalidPattern {
            pattern: glob.to_owned(),
            reason: format!("invalid glob: {}", e.kind()),
        })
}

fn build_types(name: &str) -> WsResult<ignore::types::Types> {
    // ripgrep calls Rust `rust`; models often say `rs`.
    let name = if name == "rs" { "rust" } else { name };
    let mut b = ignore::types::TypesBuilder::new();
    b.add_defaults();
    if !b.definitions().iter().any(|d| d.name() == name) {
        return Err(WsError::InvalidPattern {
            pattern: name.to_owned(),
            reason: "unknown file type; use a ripgrep type such as rust, py, js, ts, go, java, c, cpp, md, json, yaml or toml, or a glob instead".into(),
        });
    }
    b.select(name);
    b.build().map_err(|e| WsError::InvalidPattern {
        pattern: name.to_owned(),
        reason: e.to_string(),
    })
}

/// True if `data` looks binary (contains NUL in its first 8 KiB).
pub fn looks_binary(data: &[u8]) -> bool {
    data[..data.len().min(SNIFF_BYTES)].contains(&0)
}

/// Line ranges (0-based, inclusive) covered by matches of `re` in `data`,
/// in order. Matches on the same line are merged; a match spanning lines
/// (with `multiline`) yields one range over all of them.
pub fn match_lines(re: &regex::bytes::Regex, data: &[u8]) -> Vec<(usize, usize)> {
    let mut starts = vec![0usize];
    starts.extend(
        data.iter()
            .enumerate()
            .filter(|&(i, &b)| b == b'\n' && i + 1 < data.len())
            .map(|(i, _)| i + 1),
    );
    let line_of = |pos: usize| starts.partition_point(|&s| s <= pos).saturating_sub(1);
    let mut out: Vec<(usize, usize)> = Vec::new();
    for m in re.find_iter(data) {
        // An empty match after the final newline is not on any line.
        if m.start() >= data.len() && (data.is_empty() || data.ends_with(b"\n")) {
            break;
        }
        let first = line_of(m.start());
        let last = line_of(m.end().saturating_sub(1).max(m.start()));
        match out.last_mut() {
            Some(prev) if first <= prev.1 => prev.1 = prev.1.max(last),
            _ => out.push((first, last)),
        }
    }
    out
}

/// Number of lines with a match (a match spanning lines counts once).
pub fn count_matching_lines(re: &regex::bytes::Regex, data: &[u8]) -> u64 {
    match_lines(re, data).len() as u64
}

struct Search {
    base: PathBuf,
    regex: Option<regex::bytes::Regex>,
    glob: Option<globset::GlobMatcher>,
    count: bool,
    found: Mutex<Vec<FoundFile>>,
    truncated: AtomicBool,
    skipped_large: AtomicU64,
    cancel: Arc<AtomicBool>,
}

impl Search {
    fn stopped(&self) -> bool {
        self.cancel.load(Ordering::Relaxed) || self.truncated.load(Ordering::Relaxed)
    }

    /// Check one file; `rel` is its path relative to the base.
    fn visit(&self, path: &Path, rel: &Path, meta: Option<std::fs::Metadata>) {
        if let Some(g) = &self.glob
            && !g.is_match(rel)
        {
            return;
        }
        let meta = match meta.map_or_else(|| std::fs::metadata(path), Ok) {
            Ok(m) => m,
            Err(err) => {
                tracing::debug!(%err, path = %path.display(), "search: skipping unreadable file");
                return;
            }
        };
        let matches = match &self.regex {
            None => 1,
            Some(re) => {
                if meta.len() > MAX_SEARCH_FILE_BYTES {
                    self.skipped_large.fetch_add(1, Ordering::Relaxed);
                    return;
                }
                let Ok(data) = std::fs::read(path) else {
                    return;
                };
                if looks_binary(&data) {
                    return;
                }
                if self.count {
                    count_matching_lines(re, &data)
                } else {
                    u64::from(re.is_match(&data))
                }
            }
        };
        if matches == 0 {
            return;
        }
        let mut found = self.found.lock().unwrap_or_else(|e| e.into_inner());
        if found.len() >= MAX_FOUND_FILES {
            self.truncated.store(true, Ordering::Relaxed);
            return;
        }
        found.push(FoundFile {
            path: path.to_path_buf(),
            matches,
            modified: meta.modified().ok(),
        });
    }
}

/// Run `query` on this machine. Setting `cancel` stops the walk early (the
/// partial result is returned).
pub fn find_blocking(query: &FindQuery, cancel: Arc<AtomicBool>) -> WsResult<FindResult> {
    let regex = query
        .pattern
        .as_deref()
        .map(|p| build_regex(p, query.case_insensitive, query.multiline))
        .transpose()?;
    let glob = query.glob.as_deref().map(build_glob).transpose()?;
    let types = query.file_type.as_deref().map(build_types).transpose()?;
    let base_meta = std::fs::metadata(&query.base).map_err(|e| WsError::from_io(&query.base, e))?;
    let search = Search {
        base: query.base.clone(),
        regex,
        glob,
        count: query.count,
        found: Mutex::new(Vec::new()),
        truncated: AtomicBool::new(false),
        skipped_large: AtomicU64::new(0),
        cancel,
    };

    if base_meta.is_file() {
        let name = query
            .base
            .file_name()
            .map(PathBuf::from)
            .unwrap_or_default();
        search.visit(&query.base, &name, Some(base_meta));
    } else {
        let mut walk = ignore::WalkBuilder::new(&query.base);
        walk.hidden(false)
            .require_git(false)
            .filter_entry(|e| e.file_name() != ".git")
            .threads(
                std::thread::available_parallelism()
                    .map_or(4, |n| n.get())
                    .min(12),
            );
        if let Some(t) = types {
            walk.types(t);
        }
        walk.build_parallel().run(|| {
            Box::new(|entry| {
                use ignore::WalkState;
                if search.stopped() {
                    return WalkState::Quit;
                }
                let Ok(entry) = entry else {
                    return WalkState::Continue;
                };
                if !entry.file_type().is_some_and(|t| t.is_file()) {
                    return WalkState::Continue;
                }
                let path = entry.path();
                let rel = path.strip_prefix(&search.base).unwrap_or(path);
                search.visit(path, rel, entry.metadata().ok());
                WalkState::Continue
            })
        });
    }

    let mut files = search.found.into_inner().unwrap_or_else(|e| e.into_inner());
    files.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(FindResult {
        files,
        truncated: search.truncated.load(Ordering::Relaxed),
        skipped_large: search.skipped_large.load(Ordering::Relaxed),
    })
}

/// Sets the flag when dropped, so a dropped async search stops its walk.
#[derive(Debug)]
pub struct CancelOnDrop(pub Arc<AtomicBool>);

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(root: &Path, rel: &str, data: &[u8]) {
        let p = root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, data).unwrap();
    }

    fn rels(root: &Path, r: &FindResult) -> Vec<String> {
        r.files
            .iter()
            .map(|f| f.path.strip_prefix(root).unwrap().display().to_string())
            .collect()
    }

    fn run(q: FindQuery) -> FindResult {
        find_blocking(&q, Arc::new(AtomicBool::new(false))).unwrap()
    }

    #[test]
    fn honours_gitignore_hidden_and_git_dir() {
        let d = tempfile::tempdir().unwrap();
        let r = d.path();
        write(r, ".gitignore", b"target/\n*.log\n");
        write(r, "src/a.rs", b"fn needle() {}\n");
        write(r, "target/b.rs", b"needle\n");
        write(r, "x.log", b"needle\n");
        write(r, ".github/ci.yml", b"needle: 1\n");
        write(r, ".git/config", b"needle\n");
        write(r, "bin.dat", b"needle\0\x01");
        let res = run(FindQuery {
            base: r.to_path_buf(),
            pattern: Some("needle".into()),
            ..Default::default()
        });
        assert_eq!(rels(r, &res), [".github/ci.yml", "src/a.rs"]);
        // A file named directly is searched even if ignored.
        let res = run(FindQuery {
            base: r.join("x.log"),
            pattern: Some("needle".into()),
            ..Default::default()
        });
        assert_eq!(res.files.len(), 1);
    }

    #[test]
    fn glob_type_count_and_case() {
        let d = tempfile::tempdir().unwrap();
        let r = d.path();
        write(r, "a.rs", b"Foo\nfoo foo\nbar\n");
        write(r, "sub/b.py", b"foo\n");
        write(r, "sub/c.rs", b"nothing\n");
        let res = run(FindQuery {
            base: r.to_path_buf(),
            pattern: Some("foo".into()),
            file_type: Some("rs".into()),
            count: true,
            ..Default::default()
        });
        assert_eq!(rels(r, &res), ["a.rs"]);
        assert_eq!(res.files[0].matches, 1);
        let res = run(FindQuery {
            base: r.to_path_buf(),
            pattern: Some("foo".into()),
            case_insensitive: true,
            glob: Some("**/*.rs".into()),
            count: true,
            ..Default::default()
        });
        assert_eq!(res.files[0].matches, 2);
        let res = run(FindQuery {
            base: r.to_path_buf(),
            glob: Some("sub/*".into()),
            ..Default::default()
        });
        assert_eq!(rels(r, &res), ["sub/b.py", "sub/c.rs"]);
        assert!(res.files.iter().all(|f| f.modified.is_some()));
    }

    #[test]
    fn bad_inputs_are_reported() {
        let d = tempfile::tempdir().unwrap();
        let q = |f: fn(&mut FindQuery)| {
            let mut q = FindQuery {
                base: d.path().to_path_buf(),
                ..Default::default()
            };
            f(&mut q);
            find_blocking(&q, Arc::new(AtomicBool::new(false))).unwrap_err()
        };
        let e = q(|q| q.pattern = Some("a(".into()));
        assert!(matches!(e, WsError::InvalidPattern { .. }), "{e}");
        assert!(e.to_string().contains("escape literal"), "{e}");
        let e = q(|q| q.file_type = Some("nosuchtype".into()));
        assert!(e.to_string().contains("unknown file type"), "{e}");
        let e = q(|q| q.base = q.base.join("missing"));
        assert!(matches!(e, WsError::NotFound(_)));
    }

    #[test]
    fn counts_lines_not_matches() {
        let re = build_regex("o", false, false).unwrap();
        assert_eq!(count_matching_lines(&re, b"foo\nbar\noo"), 2);
        let re = build_regex("a.b", false, true).unwrap();
        assert_eq!(count_matching_lines(&re, b"a\nb\nzzz\na\nb"), 2);
        let re = build_regex("^$", false, false).unwrap();
        assert_eq!(count_matching_lines(&re, b"x\n\ny\n"), 1);
        assert_eq!(match_lines(&re, b"x\n\ny\n"), [(1, 1)]);
        let re = build_regex("b\nc", false, false).unwrap();
        assert_eq!(match_lines(&re, b"a\nb\nc\nd"), [(1, 2)]);
        let re = build_regex("x$", false, false).unwrap();
        assert_eq!(match_lines(&re, b"ax\r\nb\r\ncx"), [(0, 0), (2, 2)]);
    }

    #[test]
    fn cancelled_search_stops() {
        let d = tempfile::tempdir().unwrap();
        for i in 0..50 {
            write(d.path(), &format!("f{i}.txt"), b"x\n");
        }
        let res = find_blocking(
            &FindQuery {
                base: d.path().to_path_buf(),
                ..Default::default()
            },
            Arc::new(AtomicBool::new(true)),
        )
        .unwrap();
        assert!(res.files.is_empty());
    }
}
