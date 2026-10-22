//! Note format: Markdown files of one-line bullets with inline metadata.
//!
//! ```markdown
//! # Testing (my-app)
//! - Run `just test-int` for integration tests. (src: harness://run/01J.. · 2026-10-21)
//! - Prefer table-driven tests. See [[projects/my-app/style]] (src: harness://run/01J.. · 2026-10-18)
//! ```
//!
//! A bullet's metadata suffix is `(src: <run link> · <YYYY-MM-DD>)`. Links
//! between notes are `[[path]]`, where `path` is drive-relative without the
//! `.md` extension.

use std::fmt;
use std::sync::LazyLock;

use regex::Regex;
use time::Date;
use time::macros::format_description;

/// Scheme prefix of a run link (`harness://run/<run id>`).
pub const RUN_LINK_PREFIX: &str = "harness://run/";
/// Separator between the source link and the date in a bullet suffix.
pub const META_SEPARATOR: &str = " · ";
/// Longest accepted bullet text, in characters.
pub const MAX_BULLET_CHARS: usize = 400;

/// The run link for `run_id`.
pub fn run_link(run_id: &str) -> String {
    format!("{RUN_LINK_PREFIX}{run_id}")
}

static META_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\s*\(src: (?P<src>\S+) · (?P<date>\d{4}-\d{2}-\d{2})\)\s*$").expect("valid regex")
});
static LINK_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\[\[([^\[\]]+)\]\]").expect("valid regex"));

fn date_format() -> &'static [time::format_description::BorrowedFormatItem<'static>] {
    format_description!("[year]-[month]-[day]")
}

/// Formats a date as `YYYY-MM-DD`.
pub fn format_date(d: Date) -> String {
    d.format(date_format()).unwrap_or_default()
}

/// All `[[path]]` link targets in `text`, in order.
pub fn links(text: &str) -> Vec<String> {
    LINK_RE
        .captures_iter(text)
        .map(|c| c[1].trim().to_owned())
        .collect()
}

/// One memory: a single-line bullet with its source and date.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bullet {
    /// The lesson itself, without the leading `- ` or metadata suffix.
    pub text: String,
    /// Source link, usually `harness://run/<id>`.
    pub source: Option<String>,
    /// Date the memory was written.
    pub date: Option<Date>,
}

impl Bullet {
    /// A bullet with source and date.
    pub fn new(text: impl Into<String>, source: impl Into<String>, date: Date) -> Self {
        Self {
            text: text.into(),
            source: Some(source.into()),
            date: Some(date),
        }
    }

    /// Parses a `- ...` line. Returns `None` for non-bullet lines.
    pub fn parse(line: &str) -> Option<Self> {
        let rest = line
            .strip_prefix("- ")
            .or_else(|| line.strip_prefix("* "))?;
        if let Some(c) = META_RE.captures(rest) {
            let date = Date::parse(&c["date"], date_format()).ok();
            let start = c.get(0).map_or(rest.len(), |m| m.start());
            Some(Self {
                text: rest[..start].trim().to_owned(),
                source: Some(c["src"].to_owned()),
                date,
            })
        } else {
            Some(Self {
                text: rest.trim().to_owned(),
                source: None,
                date: None,
            })
        }
    }

    /// Links in the bullet text.
    pub fn links(&self) -> Vec<String> {
        links(&self.text)
    }
}

impl fmt::Display for Bullet {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "- {}", self.text)?;
        match (&self.source, self.date) {
            (Some(s), Some(d)) => write!(f, " (src: {s}{META_SEPARATOR}{})", format_date(d)),
            _ => Ok(()),
        }
    }
}

/// A line of a note.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Line {
    /// A memory bullet.
    Bullet(Bullet),
    /// Anything else (heading, blank line, prose), kept verbatim.
    Other(String),
}

/// A parsed note. Rendering a parsed note reproduces bullets in canonical
/// form and every other line verbatim.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Note {
    /// Lines in order.
    pub lines: Vec<Line>,
}

impl Note {
    /// A new note with a `# title` heading.
    pub fn with_title(title: &str) -> Self {
        Self {
            lines: vec![Line::Other(format!("# {title}"))],
        }
    }

    /// Parses Markdown text.
    pub fn parse(text: &str) -> Self {
        Self {
            lines: text
                .lines()
                .map(|l| Bullet::parse(l).map_or_else(|| Line::Other(l.to_owned()), Line::Bullet))
                .collect(),
        }
    }

    /// The first `# ` heading, if any.
    pub fn title(&self) -> Option<&str> {
        self.lines.iter().find_map(|l| match l {
            Line::Other(s) => s.strip_prefix("# ").map(str::trim),
            Line::Bullet(_) => None,
        })
    }

    /// All bullets in order.
    pub fn bullets(&self) -> impl Iterator<Item = &Bullet> {
        self.lines.iter().filter_map(|l| match l {
            Line::Bullet(b) => Some(b),
            Line::Other(_) => None,
        })
    }

    /// True when a bullet with exactly this text exists.
    pub fn contains_text(&self, text: &str) -> bool {
        self.bullets().any(|b| b.text == text)
    }

    /// Appends a bullet after the last existing bullet (or at the end).
    pub fn push(&mut self, bullet: Bullet) {
        let at = self
            .lines
            .iter()
            .rposition(|l| matches!(l, Line::Bullet(_)))
            .map_or(self.lines.len(), |i| i + 1);
        self.lines.insert(at, Line::Bullet(bullet));
    }

    /// Removes every bullet whose text contains `needle` (case-insensitive)
    /// and returns them.
    pub fn remove_matching(&mut self, needle: &str) -> Vec<Bullet> {
        let needle = needle.to_lowercase();
        let mut removed = Vec::new();
        self.lines.retain(|l| match l {
            Line::Bullet(b) if b.text.to_lowercase().contains(&needle) => {
                removed.push(b.clone());
                false
            }
            _ => true,
        });
        removed
    }

    /// Bullets whose text contains `needle` (case-insensitive).
    pub fn matching(&self, needle: &str) -> Vec<&Bullet> {
        let needle = needle.to_lowercase();
        self.bullets()
            .filter(|b| b.text.to_lowercase().contains(&needle))
            .collect()
    }

    /// True when the note has no bullets.
    pub fn is_empty(&self) -> bool {
        self.bullets().next().is_none()
    }

    /// All `[[links]]` in the note.
    pub fn links(&self) -> Vec<String> {
        self.bullets().flat_map(Bullet::links).collect()
    }

    /// Renders the note as Markdown, ending with a newline.
    pub fn render(&self) -> String {
        let mut s = String::new();
        for l in &self.lines {
            match l {
                Line::Bullet(b) => s.push_str(&b.to_string()),
                Line::Other(o) => s.push_str(o),
            }
            s.push('\n');
        }
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::date;

    #[test]
    fn bullet_round_trips_with_metadata() {
        let b = Bullet::new(
            "Run `just test-int`; plain `cargo test` skips them.",
            run_link("01J"),
            date!(2026 - 10 - 21),
        );
        let s = b.to_string();
        assert_eq!(
            s,
            "- Run `just test-int`; plain `cargo test` skips them. (src: harness://run/01J · 2026-10-21)"
        );
        assert_eq!(Bullet::parse(&s), Some(b));
    }

    #[test]
    fn links_are_extracted() {
        let b = Bullet::parse(
            "- Prefer table tests. See [[projects/my-app/style]] and [[tools/cargo]] (src: harness://run/x · 2026-10-18)",
        )
        .unwrap();
        assert_eq!(b.links(), vec!["projects/my-app/style", "tools/cargo"]);
        assert_eq!(b.source.as_deref(), Some("harness://run/x"));
    }

    #[test]
    fn note_parse_push_remove_render() {
        let text = "# Testing (my-app)\n- one (src: harness://run/a · 2026-10-01)\n\nSome prose.\n";
        let mut n = Note::parse(text);
        assert_eq!(n.render(), text);
        assert_eq!(n.title(), Some("Testing (my-app)"));
        n.push(Bullet::new("two", run_link("b"), date!(2026 - 10 - 02)));
        assert_eq!(
            n.render(),
            "# Testing (my-app)\n- one (src: harness://run/a · 2026-10-01)\n- two (src: harness://run/b · 2026-10-02)\n\nSome prose.\n"
        );
        let removed = n.remove_matching("ONE");
        assert_eq!(removed.len(), 1);
        assert!(!n.contains_text("one"));
        assert!(n.contains_text("two"));
    }

    #[test]
    fn plain_bullet_has_no_metadata() {
        let b = Bullet::parse("- just text").unwrap();
        assert_eq!(b.source, None);
        assert_eq!(b.to_string(), "- just text");
        assert!(Bullet::parse("# heading").is_none());
    }
}
