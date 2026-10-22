//! Strict, position-preserving INI lexer.
//!
//! `rust-ini` drops line numbers, which the harness needs for precise error
//! messages, so this module implements the small INI dialect used by
//! `harness.ini` directly:
//!
//! - `[section]` headers; section names may contain dots (`provider.anthropic`).
//! - `key = value` entries; keys may contain dots (`bash.allow`) and
//!   model-ref characters (`anthropic/claude-sonnet-5-5`).
//! - Full-line comments start with `;` or `#`.
//! - Inline comments start with `;` preceded by whitespace. `#` is never an
//!   inline comment, so colour values such as `#bd93f9` work unquoted.
//! - Entries before the first section header, duplicate sections and
//!   duplicate keys are errors.

use std::path::Path;
use std::sync::Arc;

use super::error::{IniError, Location};

/// One `key = value` line.
#[derive(Clone, PartialEq, Eq)]
pub struct Entry {
    /// Key as written (trimmed).
    pub key: String,
    /// Raw value with the inline comment and surrounding whitespace removed.
    pub value: String,
    /// Where the entry was written.
    pub location: Location,
}

impl std::fmt::Debug for Entry {
    // Values may hold credentials, so they never appear in Debug output.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Entry")
            .field("key", &self.key)
            .field("value", &"<omitted>")
            .field("location", &self.location)
            .finish()
    }
}

/// A `[section]` and its entries, in file order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Section {
    /// Section name as written inside the brackets (trimmed).
    pub name: String,
    /// Location of the header line.
    pub location: Location,
    /// Entries in file order. Keys are unique within a section.
    pub entries: Vec<Entry>,
}

impl Section {
    /// Returns the entry for `key`, if present.
    pub fn get(&self, key: &str) -> Option<&Entry> {
        self.entries.iter().find(|e| e.key == key)
    }
}

/// A parsed INI file: sections in file order, each name unique.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Document {
    /// File the document was read from, if any.
    pub file: Option<Arc<Path>>,
    /// Sections in file order.
    pub sections: Vec<Section>,
}

impl Document {
    /// Returns the section called `name`, if present.
    pub fn section(&self, name: &str) -> Option<&Section> {
        self.sections.iter().find(|s| s.name == name)
    }

    /// Returns the entry `key` in section `section`, if present.
    pub fn get(&self, section: &str, key: &str) -> Option<&Entry> {
        self.section(section).and_then(|s| s.get(key))
    }
}

/// Parses INI `text`. `file` is used only to label error locations.
///
/// All syntax errors are collected so a single `harness config check` run
/// reports every problem.
pub fn parse(text: &str, file: Option<&Path>) -> Result<Document, Vec<IniError>> {
    let file: Option<Arc<Path>> = file.map(Arc::from);
    let loc = |line: usize| Location::new(file.clone(), line);
    let mut sections: Vec<Section> = Vec::new();
    let mut errors = Vec::new();
    // Index of the section entries currently go into; `None` before the
    // first header or after a duplicate header (whose entries are dropped).
    let mut current: Option<usize> = None;
    let mut seen_header = false;

    for (idx, raw) in text.lines().enumerate() {
        let line_no = idx + 1;
        let line = raw.trim_start_matches('\u{feff}').trim();
        if line.is_empty() || line.starts_with(';') || line.starts_with('#') {
            continue;
        }
        if let Some(rest) = line.strip_prefix('[') {
            seen_header = true;
            let header = strip_inline_comment(rest);
            let Some(name) = header.strip_suffix(']') else {
                errors.push(IniError::Syntax {
                    location: loc(line_no),
                    message: "section header is missing the closing `]`".into(),
                });
                current = None;
                continue;
            };
            let name = name.trim();
            if !valid_name(name) {
                errors.push(IniError::Syntax {
                    location: loc(line_no),
                    message: format!(
                        "invalid section name `{name}` (use letters, digits, `.`, `_` and `-`)"
                    ),
                });
                current = None;
                continue;
            }
            if let Some(first) = sections.iter().find(|s| s.name == name) {
                errors.push(IniError::DuplicateSection {
                    location: loc(line_no),
                    name: name.to_owned(),
                    first_line: first.location.line,
                });
                current = None;
                continue;
            }
            sections.push(Section {
                name: name.to_owned(),
                location: loc(line_no),
                entries: Vec::new(),
            });
            current = Some(sections.len() - 1);
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            errors.push(IniError::Syntax {
                location: loc(line_no),
                message: "expected `key = value`, `[section]` or a `;` comment".into(),
            });
            continue;
        };
        let key = key.trim();
        if !valid_key(key) {
            errors.push(IniError::Syntax {
                location: loc(line_no),
                message: format!(
                    "invalid key `{key}` (use letters, digits and `.` `_` `-` `/` `:` `+` `@`)"
                ),
            });
            continue;
        }
        let Some(section_idx) = current else {
            if !seen_header {
                errors.push(IniError::Syntax {
                    location: loc(line_no),
                    message: format!("key `{key}` appears before any [section] header"),
                });
            }
            continue;
        };
        let section = &mut sections[section_idx];
        if let Some(first) = section.get(key) {
            errors.push(IniError::DuplicateKey {
                location: loc(line_no),
                section: section.name.clone(),
                key: key.to_owned(),
                first_line: first.location.line,
            });
            continue;
        }
        section.entries.push(Entry {
            key: key.to_owned(),
            value: strip_inline_comment(value).trim().to_owned(),
            location: loc(line_no),
        });
    }

    if errors.is_empty() {
        Ok(Document { file, sections })
    } else {
        Err(errors)
    }
}

/// Removes a trailing `;` comment that is preceded by whitespace.
fn strip_inline_comment(s: &str) -> &str {
    let bytes = s.as_bytes();
    for (i, b) in bytes.iter().enumerate() {
        if *b == b';' && i > 0 && bytes[i - 1].is_ascii_whitespace() {
            return s[..i].trim_end();
        }
    }
    s.trim_end()
}

/// Keys additionally allow model-ref characters (`anthropic/claude-x:free`).
fn valid_key(key: &str) -> bool {
    !key.is_empty()
        && key.chars().all(|c| {
            c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '/' | ':' | '+' | '@')
        })
}

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_sections_keys_and_comments() {
        let text = "\
; leading comment
# hash comment
[general]
default_model = sonnet           ; alias
mode=auto

[provider.anthropic]
base_url = https://api.anthropic.com
[theme]
accent = #bd93f9
";
        let doc = parse(text, None).unwrap();
        assert_eq!(doc.sections.len(), 3);
        let e = doc.get("general", "default_model").unwrap();
        assert_eq!(e.value, "sonnet");
        assert_eq!(e.location.line, 4);
        assert_eq!(doc.get("general", "mode").unwrap().value, "auto");
        assert_eq!(doc.get("theme", "accent").unwrap().value, "#bd93f9");
        assert_eq!(doc.section("provider.anthropic").unwrap().location.line, 7);
    }

    #[test]
    fn semicolon_without_leading_space_is_part_of_value() {
        let doc = parse("[a]\nk = x;y ; comment\n", None).unwrap();
        assert_eq!(doc.get("a", "k").unwrap().value, "x;y");
    }

    #[test]
    fn reports_duplicates_and_syntax_errors_with_lines() {
        let text = "orphan = 1\n[a]\nk = 1\nk = 2\n[a]\nnot an entry\n[bad\n";
        let errs = parse(text, Some(Path::new("/x/harness.ini"))).unwrap_err();
        let lines: Vec<usize> = errs.iter().map(|e| e.location().unwrap().line).collect();
        assert_eq!(lines, vec![1, 4, 5, 6, 7]);
        assert!(errs[1].to_string().starts_with("/x/harness.ini:4:"));
        assert!(errs[1].to_string().contains("first set on line 3"));
    }

    #[test]
    fn entry_debug_hides_value() {
        let doc = parse("[p]\napi_key = sk-secret\n", None).unwrap();
        let dbg = format!("{doc:?}");
        assert!(!dbg.contains("sk-secret"));
    }
}
