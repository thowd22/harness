//! YAML parsing with `file:line:col` errors, and JSON Schema export.
//!
//! Prompts, agents and workflows are YAML; skills are Markdown with optional
//! YAML frontmatter. Every parser in this crate goes through [`parse`] so
//! errors look the same everywhere:
//!
//! ```text
//! ~/.config/harness/agents/impl.yaml:4:3: budget: unknown field `usdd`, expected `usd` or `tokens`
//! ```
//!
//! # Parser choice (TASK-42 spike)
//!
//! - `serde_yaml` is deprecated and unmaintained (archived in 2024).
//! - `serde_yml` was a hasty fork with soundness and maintenance concerns.
//! - `serde_yaml_ng` depends on the unmaintained `unsafe-libyaml`
//!   (RUSTSEC-2024-0449).
//! - `saphyr` is a promising pure-Rust YAML 1.2 parser, but its serde layer
//!   is not stable yet.
//! - **`serde_norway`** (chosen) is a maintained `serde_yaml` 0.9 fork with
//!   the same API, built on the maintained `unsafe-libyaml-norway`. Its errors
//!   carry a 1-based [`serde_norway::Location`] (line and column) and a field
//!   path prefix such as `budget: ...`, even for `deny_unknown_fields`
//!   errors and for nested values.
//!
//! YAML 1.2 does not allow `_` digit separators, so `2_000_000` is a string to
//! the parser; fields that take large counts use [`de::lenient_u64`].

use std::fmt;
use std::path::{Path, PathBuf};

use serde::de::DeserializeOwned;

pub mod de;
pub mod schema;

pub use schema::{SchemaKind, run_schema, schema};

/// A YAML error with a source location.
///
/// `line` and `column` are 1-based. `file` is `None` for in-memory input and
/// for built-in files without a disk path (those use [`YamlError::origin`]).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub struct YamlError {
    /// The file the error is in, if it came from disk.
    pub file: Option<PathBuf>,
    /// Display name used when `file` is `None` (e.g. `builtin:agents/x.yaml`).
    pub origin: Option<String>,
    /// 1-based line (0 when unknown).
    pub line: usize,
    /// 1-based column (0 when unknown).
    pub column: usize,
    /// The message, without location.
    pub message: String,
}

impl YamlError {
    /// An error at an explicit location.
    pub fn at(file: Option<&Path>, line: usize, column: usize, message: impl Into<String>) -> Self {
        YamlError {
            file: file.map(Path::to_path_buf),
            origin: None,
            line,
            column,
            message: message.into(),
        }
    }

    /// An error located at `key_path` in `text` (falls back to line 1, column 1).
    pub fn at_key(
        file: Option<&Path>,
        text: &str,
        key_path: &[&str],
        message: impl Into<String>,
    ) -> Self {
        let (line, column) = locate_key(text, key_path).unwrap_or((1, 1));
        Self::at(file, line, column, message)
    }

    /// Sets the display origin used when there is no file path.
    pub fn with_origin(mut self, origin: impl Into<String>) -> Self {
        self.origin = Some(origin.into());
        self
    }

    /// The display name of the source: the file path, the origin or `<input>`.
    pub fn source_name(&self) -> String {
        match (&self.file, &self.origin) {
            (Some(f), _) => f.display().to_string(),
            (None, Some(o)) => o.clone(),
            (None, None) => "<input>".to_owned(),
        }
    }

    fn from_serde(e: &serde_norway::Error, file: Option<&Path>) -> Self {
        let (line, column) = e
            .location()
            .map(|l| (l.line(), l.column()))
            .unwrap_or((0, 0));
        let mut message = e.to_string();
        // serde_norway appends " at line L column C"; we print it as a prefix.
        if let Some(i) = message.rfind(" at line ")
            && message[i + 9..].contains(" column ")
        {
            message.truncate(i);
        }
        Self::at(file, line, column, message)
    }
}

impl fmt::Display for YamlError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.source_name())?;
        if self.line > 0 {
            write!(f, ":{}", self.line)?;
            if self.column > 0 {
                write!(f, ":{}", self.column)?;
            }
        }
        write!(f, ": {}", self.message)
    }
}

/// Deserializes `text` into `T`; errors carry `file:line:col`.
///
/// Use `#[serde(deny_unknown_fields)]` on `T` so typos are reported.
pub fn parse<T: DeserializeOwned>(text: &str, file: Option<&Path>) -> Result<T, YamlError> {
    serde_norway::from_str(text).map_err(|e| YamlError::from_serde(&e, file))
}

/// Reads and deserializes a YAML file.
pub fn read<T: DeserializeOwned>(path: &Path) -> Result<T, YamlError> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| YamlError::at(Some(path), 0, 0, format!("cannot read file: {e}")))?;
    parse(&text, Some(path))
}

/// Finds the 1-based `(line, column)` of a block-style mapping key.
///
/// `key_path` walks nested mappings, e.g. `["budget", "usd"]`. List items
/// written as `- key: value` are matched too. Returns the deepest key that
/// was found, so a missing child resolves to its parent's line. This is a
/// line-based heuristic used to locate semantic (post-deserialization)
/// errors; parse errors come straight from the parser.
pub fn locate_key(text: &str, key_path: &[&str]) -> Option<(usize, usize)> {
    let mut found = None;
    let mut parent_indent: Option<usize> = None;
    let mut start = 0;
    let lines: Vec<&str> = text.lines().collect();
    for key in key_path {
        let mut hit = None;
        let mut level_col = None;
        for (i, line) in lines.iter().enumerate().skip(start) {
            let trimmed = line.trim_start();
            if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with("---") {
                continue;
            }
            let indent = line.len() - trimmed.len();
            if parent_indent.is_some_and(|p| indent <= p) {
                break;
            }
            let (col, rest) = match trimmed.strip_prefix("- ") {
                Some(r) => (
                    indent + 2 + (r.len() - r.trim_start().len()),
                    r.trim_start(),
                ),
                None => (indent, trimmed),
            };
            if col != *level_col.get_or_insert(col) {
                continue;
            }
            let bare = rest
                .strip_prefix(key)
                .or_else(|| rest.strip_prefix(&format!("\"{key}\"")))
                .or_else(|| rest.strip_prefix(&format!("'{key}'")));
            if let Some(after) = bare
                && after.trim_start().starts_with(':')
            {
                hit = Some((i, col));
                break;
            }
        }
        match hit {
            Some((i, col)) => {
                found = Some((i + 1, col + 1));
                parent_indent = Some(col);
                start = i + 1;
            }
            None => break,
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    #[derive(Debug, Deserialize)]
    #[serde(deny_unknown_fields)]
    #[allow(dead_code)]
    struct Doc {
        name: String,
        #[serde(default, deserialize_with = "de::lenient_opt_u64")]
        tokens: Option<u64>,
        budget: Option<Budget>,
    }

    #[derive(Debug, Deserialize)]
    #[serde(deny_unknown_fields)]
    #[allow(dead_code)]
    struct Budget {
        usd: f64,
    }

    fn err(text: &str) -> YamlError {
        parse::<Doc>(text, Some(Path::new("/x/a.yaml"))).unwrap_err()
    }

    #[test]
    fn syntax_error_has_file_line_col() {
        let e = err("name: x\n  bad: : :\n");
        assert_eq!((e.line, e.column), (2, 6));
        assert!(e.to_string().starts_with("/x/a.yaml:2:6: "), "{e}");
        assert!(!e.to_string().contains(" at line "), "{e}");
    }

    #[test]
    fn unknown_nested_field_is_located() {
        let e = err("name: x\nbudget:\n  usd: 1\n  extra: 2\n");
        assert_eq!((e.line, e.column), (4, 3));
        assert_eq!(
            e.to_string(),
            "/x/a.yaml:4:3: budget: unknown field `extra`, expected `usd`"
        );
    }

    #[test]
    fn type_error_is_located() {
        let e = err("name: [1]\n");
        assert_eq!((e.line, e.column), (1, 7));
        assert!(e.message.contains("invalid type"));
    }

    #[test]
    fn in_memory_and_origin_names() {
        let e = parse::<Doc>("nope: 1\n", None).unwrap_err();
        assert!(e.to_string().starts_with("<input>:1:1: "), "{e}");
        let e = e.with_origin("builtin:agents/a.yaml");
        assert!(e.to_string().starts_with("builtin:agents/a.yaml:1:1: "));
    }

    #[test]
    fn underscore_numbers_are_accepted() {
        let d: Doc = parse("name: x\ntokens: 2_000_000\n", None).unwrap();
        assert_eq!(d.tokens, Some(2_000_000));
        let e = err("name: x\ntokens: lots\n");
        assert_eq!(e.line, 2);
    }

    #[test]
    fn read_reports_missing_file() {
        let e = read::<Doc>(Path::new("/definitely/not/here.yaml")).unwrap_err();
        assert!(
            e.to_string()
                .starts_with("/definitely/not/here.yaml: cannot read file"),
            "{e}"
        );
    }

    #[test]
    fn locate_key_walks_nesting_and_lists() {
        let text = "name: a\nbudget:\n  usd: 2\n# c\npolicy:\n  bash.deny: [x]\nsteps:\n  - id: one\n    agent: b\n";
        assert_eq!(locate_key(text, &["budget", "usd"]), Some((3, 3)));
        assert_eq!(locate_key(text, &["policy", "bash.deny"]), Some((6, 3)));
        assert_eq!(locate_key(text, &["steps", "id"]), Some((8, 5)));
        assert_eq!(locate_key(text, &["steps", "agent"]), Some((9, 5)));
        assert_eq!(locate_key(text, &["budget", "missing"]), Some((2, 1)));
        assert_eq!(
            locate_key(text, &["usd"]),
            None,
            "nested keys are not top-level"
        );
    }
}
