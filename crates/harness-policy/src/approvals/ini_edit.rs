//! "Always" approvals: adding an allow rule to harness.ini.
//!
//! The harness (never an agent: harness.ini is a protected path) appends
//! the pattern to `[policy] bash.allow`, keeping the rest
//! of the file, its comments and its permission bits unchanged.

use std::io::Write;
use std::path::{Path, PathBuf};

/// The `[policy]` list always-rules are added to. Outward actions are
/// allowed by a `bash.allow` pattern that names them explicitly (see
/// [`crate::outward`]).
pub const ALWAYS_KEY: &str = "bash.allow";

/// An allow rule waiting for the user's explicit confirmation.
///
/// Nothing is written until [`IniRuleProposal::confirm`] is called; the UI
/// shows [`IniRuleProposal::describe`] and calls it only when the user
/// confirms. Dropping the proposal discards it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[must_use = "an always-rule is only written after explicit confirmation"]
pub struct IniRuleProposal {
    /// The pattern.
    pub pattern: String,
}

impl IniRuleProposal {
    /// What will be written, for the confirmation prompt.
    pub fn describe(&self) -> String {
        format!(
            "Add `{}` to [policy] {ALWAYS_KEY} in harness.ini?",
            self.pattern
        )
    }

    /// The user confirmed: append the rule to the ini file at `path`.
    pub fn confirm(self, path: &Path) -> Result<(), IniEditError> {
        append_rule(path, &self.pattern)
    }
}

/// Failure to add a rule.
#[derive(Debug, thiserror::Error)]
pub enum IniEditError {
    /// The pattern cannot be stored in an ini list.
    #[error("pattern `{0}` cannot be stored in harness.ini (no commas, newlines or ` ;`)")]
    BadPattern(String),
    /// Reading or writing failed.
    #[error("{path}: {source}")]
    Io {
        /// File.
        path: PathBuf,
        /// Cause.
        #[source]
        source: std::io::Error,
    },
}

/// Append `pattern` to `[policy] bash.allow` in the ini file at `path`
/// (creating the key, section or file as needed). A pattern already present
/// is not added twice. The file is replaced atomically and keeps its mode
/// (new files are created `0600`).
pub fn append_rule(path: &Path, pattern: &str) -> Result<(), IniEditError> {
    let pattern = pattern.trim();
    if pattern.is_empty() || pattern.contains([',', '\n', '\r']) || pattern.contains(" ;") {
        return Err(IniEditError::BadPattern(pattern.into()));
    }
    let io = |source| IniEditError::Io {
        path: path.to_path_buf(),
        source,
    };
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(io(e)),
    };
    let Some(new) = edit(&text, ALWAYS_KEY, pattern) else {
        return Ok(());
    };
    write_atomic(path, &new).map_err(io)
}

/// Returns the edited text, or `None` if the pattern is already present.
fn edit(text: &str, key: &str, pattern: &str) -> Option<String> {
    let mut lines: Vec<String> = text.lines().map(str::to_string).collect();
    let header = |l: &str| l.trim().starts_with('[');
    let start = lines.iter().position(|l| l.trim() == "[policy]");
    let Some(start) = start else {
        if lines.last().is_some_and(|l| !l.trim().is_empty()) {
            lines.push(String::new());
        }
        lines.push("[policy]".into());
        lines.push(format!("{key} = {pattern}"));
        return Some(join(lines));
    };
    let end = lines[start + 1..]
        .iter()
        .position(|l| header(l))
        .map_or(lines.len(), |i| start + 1 + i);
    for line in &mut lines[start + 1..end] {
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        if k.trim() != key {
            continue;
        }
        let (value, comment) = split_comment(v);
        if value.split(',').any(|p| p.trim() == pattern) {
            return None;
        }
        let value = value.trim_end();
        let sep = if value.trim().is_empty() { " " } else { ", " };
        *line = format!("{}={value}{sep}{pattern}{comment}", k);
        return Some(join(lines));
    }
    let mut insert = end;
    while insert > start + 1 && lines[insert - 1].trim().is_empty() {
        insert -= 1;
    }
    lines.insert(insert, format!("{key} = {pattern}"));
    Some(join(lines))
}

fn split_comment(v: &str) -> (&str, &str) {
    let b = v.as_bytes();
    for i in 1..b.len() {
        if b[i] == b';' && b[i - 1].is_ascii_whitespace() {
            let mut j = i;
            while j > 0 && b[j - 1].is_ascii_whitespace() {
                j -= 1;
            }
            return (&v[..j], &v[j..]);
        }
    }
    (v, "")
}

fn join(lines: Vec<String>) -> String {
    let mut s = lines.join("\n");
    s.push('\n');
    s
}

fn write_atomic(path: &Path, text: &str) -> std::io::Result<()> {
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    let mode = std::fs::metadata(path)
        .map(|m| m.permissions().mode() & 0o7777)
        .unwrap_or(0o600);
    let dir = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let tmp = dir.join(format!(".{name}.tmp-{}", std::process::id()));
    let result = (|| {
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&tmp)?;
        f.write_all(text.as_bytes())?;
        f.set_permissions(std::fs::Permissions::from_mode(mode))?;
        f.sync_all()?;
        std::fs::rename(&tmp, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn appends_to_existing_key_keeping_comments() {
        let t = "[general]\nmode = ask\n\n[policy]\nbash.allow = ls*, rg* ; safe ones\nbash.deny = sudo*\n\n[ui]\n";
        let out = edit(t, "bash.allow", "cargo test*").unwrap();
        assert!(
            out.contains("bash.allow = ls*, rg*, cargo test* ; safe ones\n"),
            "{out}"
        );
        assert!(out.contains("[ui]"));
        assert_eq!(edit(&out, "bash.allow", "rg*"), None);
    }

    #[test]
    fn adds_key_or_section() {
        let out = edit(
            "[policy]\nbash.deny = x\n\n[ui]\n",
            "outward.allow",
            "git push*",
        )
        .unwrap();
        assert_eq!(
            out,
            "[policy]\nbash.deny = x\noutward.allow = git push*\n\n[ui]\n"
        );
        let out = edit("[general]\nmode = ask", "bash.allow", "ls*").unwrap();
        assert_eq!(out, "[general]\nmode = ask\n\n[policy]\nbash.allow = ls*\n");
        assert_eq!(
            edit("", "bash.allow", "ls*").unwrap(),
            "[policy]\nbash.allow = ls*\n"
        );
    }

    #[test]
    fn confirm_writes_file_keeping_mode() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("harness.ini");
        std::fs::write(&p, "[policy]\n").unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600)).unwrap();
        let proposal = IniRuleProposal {
            pattern: "cargo test*".into(),
        };
        assert!(proposal.describe().contains("bash.allow"));
        assert_eq!(
            std::fs::read_to_string(&p).unwrap(),
            "[policy]\n",
            "nothing written before confirm"
        );
        proposal.confirm(&p).unwrap();
        assert_eq!(
            std::fs::read_to_string(&p).unwrap(),
            "[policy]\nbash.allow = cargo test*\n"
        );
        assert_eq!(
            std::fs::metadata(&p).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert!(matches!(
            append_rule(&p, "a, b"),
            Err(IniEditError::BadPattern(_))
        ));
        let fresh = dir.path().join("new.ini");
        append_rule(&fresh, "ls*").unwrap();
        assert_eq!(
            std::fs::metadata(&fresh).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}
