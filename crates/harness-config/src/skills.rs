//! Skills: Markdown files callable as slash commands (`skills/<name>.md` or
//! `skills/<name>/SKILL.md`), optionally listed for the model.
//!
//! ```markdown
//! ---
//! name: release-notes
//! description: Draft release notes from commits since the last tag
//! args: "[since-tag]"
//! model_invocable: true
//! ---
//! Collect commits since $1 (default: latest tag) and ...
//! $ARGUMENTS
//! ```
//!
//! Frontmatter is optional. Without it the name is the file stem (or the
//! directory name for `SKILL.md`) and the description is the first
//! non-heading, non-empty line of the body.
//!
//! `/release-notes v1.2` expands the body with [`Skill::expand`]:
//! `$ARGUMENTS` is the whole argument string, `$1`..`$9` are positional
//! arguments (shell-style quoting), and `$$` is a literal `$`. If the body
//! has no placeholder at all and arguments were given, they are appended
//! after a blank line so they are never dropped silently.
//!
//! Model-invocable skills are advertised by [`listing`]: one line each with
//! name, description and path. The model reads the file with the existing
//! `read` tool, so no extra tool or schema is needed.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::discovery::{Artifact, Kind, Scope, SourceFile};
use crate::yaml::YamlError;

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct Frontmatter {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    args: Option<String>,
    #[serde(default)]
    model_invocable: bool,
}

/// A loaded skill.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Skill {
    /// Name (slash command `/name`).
    pub name: String,
    /// One-line description.
    pub description: String,
    /// Argument hint, e.g. `[since-tag]`.
    pub args: Option<String>,
    /// Listed for the model.
    pub model_invocable: bool,
    /// Body without frontmatter.
    pub body: String,
    /// File on disk, if any (what the model passes to `read`).
    pub path: Option<PathBuf>,
    /// Display location (`path` or `builtin:skills/x.md`).
    pub location: String,
}

/// Splits optional `---` YAML frontmatter. Returns `(yaml, body, body_line)`
/// where `body_line` is the 1-based line the body starts on.
fn split_frontmatter(text: &str) -> Option<(&str, &str, usize)> {
    let rest = text
        .strip_prefix("---\n")
        .or_else(|| text.strip_prefix("---\r\n"))?;
    let mut offset = 0;
    for (i, line) in rest.split_inclusive('\n').enumerate() {
        if line.trim_end() == "---" {
            let yaml = &rest[..offset];
            let body = &rest[offset + line.len()..];
            return Some((yaml, body, i + 3));
        }
        offset += line.len();
    }
    None
}

fn first_description_line(body: &str) -> String {
    body.lines()
        .map(str::trim)
        .find(|l| !l.is_empty() && !l.starts_with('#'))
        .unwrap_or("")
        .to_owned()
}

fn file_stem_name(rel: &str) -> String {
    let p = Path::new(rel);
    let stem = if p.file_name().is_some_and(|f| f == "SKILL.md") {
        p.parent().and_then(Path::file_name)
    } else {
        p.file_stem()
    };
    stem.map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default()
}

impl Artifact for Skill {
    const KIND: Kind = Kind::Skill;

    fn load(src: &SourceFile) -> Result<Self, YamlError> {
        let (fm, body) = match split_frontmatter(&src.text) {
            Some((yaml, body, _)) => {
                // Parse against the full text's line numbers by padding.
                let padded = format!("\n{yaml}");
                let fm: Frontmatter = if yaml.trim().is_empty() {
                    Frontmatter::default()
                } else {
                    src.parse_yaml(&padded)?
                };
                (fm, body)
            }
            None if src.text.starts_with("---\n") => {
                return Err(src.error_at_line(
                    1,
                    1,
                    "frontmatter starts with `---` but has no closing `---` line",
                ));
            }
            None => (Frontmatter::default(), src.text.as_str()),
        };
        let name = fm.name.unwrap_or_else(|| file_stem_name(&src.rel));
        if name.trim().is_empty() || name.contains(char::is_whitespace) || name.contains(':') {
            return Err(src.error_at(
                &["name"],
                format!("invalid skill name `{name}`: use letters, digits, `-` or `_`"),
            ));
        }
        let description = fm
            .description
            .unwrap_or_else(|| first_description_line(body));
        Ok(Skill {
            name,
            description,
            args: fm.args,
            model_invocable: fm.model_invocable,
            body: body.to_owned(),
            path: src.path.clone(),
            location: src.display(),
        })
    }

    fn name(&self) -> &str {
        &self.name
    }
}

/// Splits an argument string shell-style: whitespace separates, `'...'` and
/// `"..."` group, `\` escapes the next character outside single quotes.
pub fn split_args(args: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut have = false;
    let mut chars = args.chars();
    let mut quote: Option<char> = None;
    while let Some(c) = chars.next() {
        match (quote, c) {
            (Some('\''), '\'') | (Some('"'), '"') => quote = None,
            (Some('\''), c) => cur.push(c),
            (_, '\\') => {
                if let Some(n) = chars.next() {
                    cur.push(n);
                }
                have = true;
            }
            (Some(_), c) => cur.push(c),
            (None, '\'' | '"') => {
                quote = Some(c);
                have = true;
            }
            (None, c) if c.is_whitespace() => {
                if have || !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                    have = false;
                }
            }
            (None, c) => cur.push(c),
        }
    }
    if have || !cur.is_empty() {
        out.push(cur);
    }
    out
}

impl Skill {
    /// Expands the body for `/name <args>` (see the module docs).
    pub fn expand(&self, args: &str) -> String {
        let args = args.trim();
        let pos = split_args(args);
        let mut out = String::with_capacity(self.body.len() + args.len());
        let mut used = false;
        let mut rest = self.body.as_str();
        while let Some(i) = rest.find('$') {
            out.push_str(&rest[..i]);
            let after = &rest[i + 1..];
            if let Some(r) = after.strip_prefix("ARGUMENTS") {
                out.push_str(args);
                used = true;
                rest = r;
            } else if let Some(r) = after.strip_prefix('$') {
                out.push('$');
                rest = r;
            } else if let Some(d) = after.chars().next().filter(|c| ('1'..='9').contains(c)) {
                let n = d as usize - '0' as usize;
                out.push_str(pos.get(n - 1).map(String::as_str).unwrap_or(""));
                used = true;
                rest = &after[1..];
            } else {
                out.push('$');
                rest = after;
            }
        }
        out.push_str(rest);
        if !used && !args.is_empty() {
            let trimmed = out.trim_end().len();
            out.truncate(trimmed);
            out.push_str("\n\n");
            out.push_str(args);
        }
        out
    }

    /// The listing line for the model: `- name: description (path)`.
    pub fn listing_line(&self) -> String {
        let loc = self
            .path
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| self.location.clone());
        let args = self
            .args
            .as_deref()
            .map(|a| format!(" {a}"))
            .unwrap_or_default();
        if self.description.is_empty() {
            format!("- {}{args} ({loc})", self.name)
        } else {
            format!("- {}{args}: {} ({loc})", self.name, self.description)
        }
    }
}

/// Heading line placed before the skill listing in the system prompt.
pub const LISTING_HEADER: &str =
    "Skills you can use (read the file with the read tool when one fits the task):";

/// The system-prompt listing of model-invocable skills, one line each,
/// sorted by name. Empty when there are none. Built-in skills without a disk
/// path are omitted because the model could not read them.
pub fn listing<'a>(skills: impl IntoIterator<Item = &'a Skill>) -> String {
    let mut lines: Vec<(&str, String)> = skills
        .into_iter()
        .filter(|s| s.model_invocable && s.path.is_some())
        .map(|s| (s.name.as_str(), s.listing_line()))
        .collect();
    if lines.is_empty() {
        return String::new();
    }
    lines.sort();
    let mut out = String::from(LISTING_HEADER);
    for (_, l) in lines {
        out.push('\n');
        out.push_str(&l);
    }
    out
}

/// Resolves a slash command to a skill: `/name`, or `/global:name` /
/// `/project:name` to pick a shadowed definition. `command` excludes the
/// leading `/`.
pub fn resolve_command<'a>(
    registry: &'a crate::discovery::Registry<Skill>,
    command: &str,
) -> Option<&'a Skill> {
    match command.split_once(':') {
        Some((scope, name)) => {
            let scope = Scope::ALL.into_iter().find(|s| s.label() == scope)?;
            registry.get_in(scope, name).map(|e| &e.item)
        }
        None => registry.get(command),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn load(rel: &str, text: &str) -> Result<Skill, YamlError> {
        Skill::load(&SourceFile::inline(Scope::Global, Kind::Skill, rel, text))
    }

    #[test]
    fn frontmatter_fields() {
        let s = load(
            "skills/x.md",
            "---\nname: release-notes\ndescription: Draft notes\nargs: \"[since-tag]\"\nmodel_invocable: true\n---\nBody $1\n",
        )
        .unwrap();
        assert_eq!(s.name, "release-notes");
        assert_eq!(s.description, "Draft notes");
        assert_eq!(s.args.as_deref(), Some("[since-tag]"));
        assert!(s.model_invocable);
        assert_eq!(s.body, "Body $1\n");
    }

    #[test]
    fn defaults_without_frontmatter() {
        let s = load(
            "skills/deploy.md",
            "# Deploy\n\nShip the build to staging.\nMore.\n",
        )
        .unwrap();
        assert_eq!(s.name, "deploy");
        assert_eq!(s.description, "Ship the build to staging.");
        assert!(!s.model_invocable);
        let s = load("skills/review/SKILL.md", "Review the diff.\n").unwrap();
        assert_eq!(s.name, "review");
        assert_eq!(s.description, "Review the diff.");
        // Partial frontmatter falls back per field.
        let s = load(
            "skills/a.md",
            "---\nmodel_invocable: true\n---\n## H\nDesc here\n",
        )
        .unwrap();
        assert_eq!(
            (s.name.as_str(), s.description.as_str()),
            ("a", "Desc here")
        );
    }

    #[test]
    fn frontmatter_errors_are_located() {
        let e = load(
            "skills/a.md",
            "---\nname: a\nmodel_invokable: true\n---\nx\n",
        )
        .unwrap_err();
        assert_eq!(e.line, 3, "{e}");
        assert!(e.message.contains("unknown field `model_invokable`"), "{e}");
        let e = load("skills/a.md", "---\nname: a\nbody\n").unwrap_err();
        assert!(e.message.contains("no closing"), "{e}");
    }

    #[test]
    fn expand_arguments_and_positionals() {
        let s = load(
            "skills/a.md",
            "Since $1 until $2 ($3).\nAll: $ARGUMENTS\nCost $$5\n",
        )
        .unwrap();
        assert_eq!(
            s.expand(r#"v1.2 "next release""#),
            "Since v1.2 until next release ().\nAll: v1.2 \"next release\"\nCost $5\n"
        );
        let plain = load("skills/b.md", "Do the thing.\n").unwrap();
        assert_eq!(plain.expand(""), "Do the thing.\n");
        assert_eq!(plain.expand("fast"), "Do the thing.\n\nfast");
    }

    #[test]
    fn split_args_quoting() {
        assert_eq!(
            split_args(r#"a 'b c' "d \"e\"" f\ g ''"#),
            ["a", "b c", "d \"e\"", "f g", ""]
        );
        assert!(split_args("   ").is_empty());
    }

    #[test]
    fn listing_one_line_each_with_path() {
        let mut a = load(
            "skills/a.md",
            "---\nmodel_invocable: true\ndescription: Alpha\n---\nx",
        )
        .unwrap();
        a.path = Some("/home/u/.config/harness/skills/a.md".into());
        let mut b = load("skills/b.md", "Beta skill\n").unwrap();
        b.path = Some("/b.md".into());
        let mut c = load(
            "skills/c/SKILL.md",
            "---\nmodel_invocable: true\nargs: \"<x>\"\n---\nGamma\n",
        )
        .unwrap();
        c.path = Some("/c/SKILL.md".into());
        let builtin = load("skills/d.md", "---\nmodel_invocable: true\n---\nno path").unwrap();
        let out = listing([&c, &b, &a, &builtin]);
        assert_eq!(
            out,
            format!(
                "{LISTING_HEADER}\n- a: Alpha (/home/u/.config/harness/skills/a.md)\n- c <x>: Gamma (/c/SKILL.md)"
            )
        );
        assert_eq!(listing([&b]), "");
    }
}
