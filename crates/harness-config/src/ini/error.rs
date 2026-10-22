//! Error types for `harness.ini` loading. Every error that comes from file
//! content carries a [`Location`] (`file:line`).

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// A position in a config file.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Location {
    /// The file, or `None` for in-memory text.
    pub file: Option<Arc<Path>>,
    /// 1-based line number.
    pub line: usize,
}

impl Location {
    /// Creates a location.
    pub fn new(file: Option<Arc<Path>>, line: usize) -> Self {
        Self { file, line }
    }
}

impl fmt::Display for Location {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.file {
            Some(p) => write!(f, "{}:{}", p.display(), self.line),
            None => write!(f, "<harness.ini>:{}", self.line),
        }
    }
}

/// One problem found while loading `harness.ini`.
///
/// Messages never include the value of a secret key.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum IniError {
    /// The line is not valid INI.
    #[error("{location}: {message}")]
    Syntax {
        /// Where.
        location: Location,
        /// What is wrong.
        message: String,
    },
    /// A `[section]` appears twice.
    #[error("{location}: duplicate section [{name}] (first defined on line {first_line})")]
    DuplicateSection {
        /// Where the second header is.
        location: Location,
        /// Section name.
        name: String,
        /// Line of the first header.
        first_line: usize,
    },
    /// A key appears twice in one section.
    #[error("{location}: duplicate key `{key}` in [{section}] (first set on line {first_line})")]
    DuplicateKey {
        /// Where the second entry is.
        location: Location,
        /// Section name.
        section: String,
        /// Key name.
        key: String,
        /// Line of the first entry.
        first_line: usize,
    },
    /// The section name is not part of the schema.
    #[error("{location}: unknown section [{name}]{}", hint_suffix(.hint))]
    UnknownSection {
        /// Header location.
        location: Location,
        /// Section name as written.
        name: String,
        /// Closest known name, if any.
        hint: Option<String>,
    },
    /// The key is not valid in its section.
    #[error("{location}: unknown key `{key}` in [{section}]{}", hint_suffix(.hint))]
    UnknownKey {
        /// Entry location.
        location: Location,
        /// Section name.
        section: String,
        /// Key as written.
        key: String,
        /// Closest known key, if any.
        hint: Option<String>,
    },
    /// The value does not have the expected type or range.
    #[error("{location}: invalid value for `{key}` in [{section}]: {message}")]
    InvalidValue {
        /// Entry location.
        location: Location,
        /// Section name.
        section: String,
        /// Key name.
        key: String,
        /// What was expected (never echoes secret values).
        message: String,
    },
    /// A required key is missing from a section.
    #[error("{location}: [{section}] is missing required key `{key}`")]
    MissingKey {
        /// Section header location.
        location: Location,
        /// Section name.
        section: String,
        /// Missing key.
        key: String,
    },
    /// The file could not be read.
    #[error("cannot read {}: {source}", .path.display())]
    Io {
        /// File path.
        path: PathBuf,
        /// Underlying error.
        #[source]
        source: std::io::Error,
    },
    /// The file holds inline secrets but is readable by group or others.
    #[error(
        "{} contains api_key secrets but is readable by group or others (mode {mode:04o}); \
         refusing to load it. Fix with: chmod 600 {}",
        .path.display(),
        .path.display()
    )]
    InsecurePermissions {
        /// File path.
        path: PathBuf,
        /// Permission bits (`mode & 0o777`).
        mode: u32,
    },
}

impl IniError {
    /// Location in the file, for errors that come from file content.
    pub fn location(&self) -> Option<&Location> {
        match self {
            IniError::Syntax { location, .. }
            | IniError::DuplicateSection { location, .. }
            | IniError::DuplicateKey { location, .. }
            | IniError::UnknownSection { location, .. }
            | IniError::UnknownKey { location, .. }
            | IniError::InvalidValue { location, .. }
            | IniError::MissingKey { location, .. } => Some(location),
            IniError::Io { .. } | IniError::InsecurePermissions { .. } => None,
        }
    }
}

fn hint_suffix(hint: &Option<String>) -> String {
    match hint {
        Some(h) => format!(" (did you mean `{h}`?)"),
        None => String::new(),
    }
}

/// All problems found in one load, in file order. Never empty.
#[derive(Debug, thiserror::Error)]
pub struct IniErrors(pub Vec<IniError>);

impl IniErrors {
    /// The individual errors.
    pub fn errors(&self) -> &[IniError] {
        &self.0
    }
}

impl fmt::Display for IniErrors {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (i, e) in self.0.iter().enumerate() {
            if i > 0 {
                writeln!(f)?;
            }
            write!(f, "{e}")?;
        }
        Ok(())
    }
}

impl From<IniError> for IniErrors {
    fn from(e: IniError) -> Self {
        IniErrors(vec![e])
    }
}

/// Returns the candidate closest to `input` by edit distance, if it is a
/// plausible typo (distance at most 2, or 1 for short names).
pub(crate) fn closest<'a>(
    input: &str,
    candidates: impl IntoIterator<Item = &'a str>,
) -> Option<String> {
    let max = if input.len() <= 4 { 1 } else { 2 };
    candidates
        .into_iter()
        .map(|c| (levenshtein(input, c), c))
        .filter(|(d, _)| *d <= max)
        .min_by_key(|(d, _)| *d)
        .map(|(_, c)| c.to_owned())
}

fn levenshtein(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut cur = vec![i + 1; b.len() + 1];
        for (j, cb) in b.iter().enumerate() {
            let cost = usize::from(ca != *cb);
            cur[j + 1] = (prev[j] + cost).min(prev[j + 1] + 1).min(cur[j] + 1);
        }
        prev = cur;
    }
    prev[b.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn suggests_close_names_only() {
        assert_eq!(
            closest("defualt_model", ["default_model", "mode"]),
            Some("default_model".into())
        );
        assert_eq!(closest("zzz", ["default_model", "mode"]), None);
    }
}
