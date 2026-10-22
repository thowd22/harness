//! The session-start memory index: `MEMORY.md` plus the current project's
//! note index, capped and frozen.

use std::path::Path;

use sha2::{Digest, Sha256};

use crate::drive::{
    DriveError, MEMORY_FILE, Note, NotePath, PROJECTS_DIR, list_notes, read_note_at,
};

/// Appended when the index was cut to fit the token cap.
pub const TRUNCATION_NOTICE: &str =
    "… (memory index truncated; use the memory tool's search action for more)";

/// Rough token estimate: one token per four bytes, rounded up.
pub fn estimate_tokens(text: &str) -> usize {
    text.len().div_ceil(4)
}

/// The project index section: one line per note under
/// `projects/<project_key>/`, as `- [[link]] Title (N notes)`. Empty when
/// the project has no notes.
pub fn project_section(checkout: &Path, project_key: &str) -> Result<String, DriveError> {
    let prefix = format!("{PROJECTS_DIR}/{project_key}/");
    let mut lines = Vec::new();
    for np in list_notes(checkout)? {
        if !np.as_str().starts_with(&prefix) {
            continue;
        }
        let Some(text) = read_note_at(checkout, &np)? else {
            continue;
        };
        let note = Note::parse(&text);
        let title = note
            .title()
            .map_or_else(|| np.default_title(), str::to_owned);
        let n = note.bullets().count();
        lines.push(format!(
            "- [[{}]] {title} ({n} {})",
            np.link(),
            if n == 1 { "memory" } else { "memories" }
        ));
    }
    if lines.is_empty() {
        return Ok(String::new());
    }
    Ok(format!(
        "## Project notes ({project_key})\n{}\n",
        lines.join("\n")
    ))
}

/// The memory loaded into the stable prompt prefix at session start.
///
/// It is computed once by [`SessionIndex::load`] and never changes for the
/// session: memories written during the session apply to future sessions,
/// which keeps the prompt prefix byte-identical and cacheable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionIndex {
    text: String,
    truncated: bool,
    sha256: String,
}

impl SessionIndex {
    /// Builds the index from `MEMORY.md` and the project section for
    /// `project_key` under `checkout`, cut at line boundaries to at most
    /// `max_tokens` (estimated; see [`estimate_tokens`]).
    pub fn load(
        checkout: &Path,
        project_key: Option<&str>,
        max_tokens: u32,
    ) -> Result<Self, DriveError> {
        let memory = NotePath::parse(MEMORY_FILE)?;
        let mut full = read_note_at(checkout, &memory)?.unwrap_or_default();
        if let Some(key) = project_key {
            let section = project_section(checkout, key)?;
            if !section.is_empty() {
                if !full.is_empty() && !full.ends_with("\n\n") {
                    full.push_str(if full.ends_with('\n') { "\n" } else { "\n\n" });
                }
                full.push_str(&section);
            }
        }
        let (text, truncated) = cap(&full, max_tokens as usize);
        Ok(Self::from_text(text, truncated))
    }

    /// An index with exactly `text` (for tests and disabled memory).
    pub fn from_text(text: String, truncated: bool) -> Self {
        let sha256 = hex::encode(Sha256::digest(text.as_bytes()));
        Self {
            text,
            truncated,
            sha256,
        }
    }

    /// An empty index (memory off).
    pub fn empty() -> Self {
        Self::from_text(String::new(), false)
    }

    /// The frozen text.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// True when the text was cut to fit the cap.
    pub fn truncated(&self) -> bool {
        self.truncated
    }

    /// sha256 of the text, for cache-stability assertions.
    pub fn sha256(&self) -> &str {
        &self.sha256
    }

    /// Estimated tokens of the text.
    pub fn tokens(&self) -> usize {
        estimate_tokens(&self.text)
    }
}

fn cap(full: &str, max_tokens: usize) -> (String, bool) {
    if estimate_tokens(full) <= max_tokens {
        return (full.to_owned(), false);
    }
    let budget = max_tokens
        .saturating_sub(estimate_tokens(TRUNCATION_NOTICE) + 1)
        .saturating_mul(4);
    let mut out = String::new();
    for line in full.lines() {
        if out.len() + line.len() + 1 > budget {
            break;
        }
        out.push_str(line);
        out.push('\n');
    }
    if max_tokens > estimate_tokens(TRUNCATION_NOTICE) {
        out.push_str(TRUNCATION_NOTICE);
        out.push('\n');
    }
    (out, true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn setup() -> tempfile::TempDir {
        let tmp = tempfile::tempdir().unwrap();
        let r = tmp.path();
        fs::write(r.join("MEMORY.md"), "# Memory\n- Prefer terse answers.\n").unwrap();
        fs::create_dir_all(r.join("projects/app-1")).unwrap();
        fs::create_dir_all(r.join("projects/other-2")).unwrap();
        fs::write(
            r.join("projects/app-1/testing.md"),
            "# Testing (app)\n- a\n- b\n",
        )
        .unwrap();
        fs::write(r.join("projects/app-1/style.md"), "- c\n").unwrap();
        fs::write(r.join("projects/other-2/x.md"), "# Other\n- z\n").unwrap();
        tmp
    }

    #[test]
    fn loads_memory_and_current_project_only() {
        let tmp = setup();
        let idx = SessionIndex::load(tmp.path(), Some("app-1"), 1500).unwrap();
        assert_eq!(
            idx.text(),
            "# Memory\n- Prefer terse answers.\n\n## Project notes (app-1)\n\
             - [[projects/app-1/style]] Style (app-1) (1 memory)\n\
             - [[projects/app-1/testing]] Testing (app) (2 memories)\n"
        );
        assert!(!idx.truncated());
        assert!(!idx.text().contains("Other"));
    }

    #[test]
    fn respects_token_cap() {
        let tmp = setup();
        let mut big = String::from("# Memory\n");
        for i in 0..200 {
            big.push_str(&format!("- preference number {i} is quite long text\n"));
        }
        fs::write(tmp.path().join("MEMORY.md"), big).unwrap();
        for cap in [50u32, 200, 1500] {
            let idx = SessionIndex::load(tmp.path(), Some("app-1"), cap).unwrap();
            assert!(idx.tokens() <= cap as usize, "{} > {cap}", idx.tokens());
            assert!(idx.truncated());
            assert!(idx.text().ends_with(&format!("{TRUNCATION_NOTICE}\n")));
        }
    }
}
