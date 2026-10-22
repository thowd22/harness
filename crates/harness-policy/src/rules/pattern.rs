//! Command patterns: `*` matches any run of characters (spaces and `/`
//! included), `?` matches one character, everything else is literal.

/// A compiled command pattern.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandPattern {
    text: String,
    chars: Vec<char>,
}

impl CommandPattern {
    /// Compile `pattern` (surrounding whitespace is ignored).
    pub fn new(pattern: &str) -> Self {
        let text = pattern.trim().to_string();
        Self {
            chars: text.chars().collect(),
            text,
        }
    }

    /// The pattern as written.
    pub fn as_str(&self) -> &str {
        &self.text
    }

    /// The text before the first wildcard (`git push*` → `git push`).
    pub fn literal_prefix(&self) -> &str {
        let end = self.text.find(['*', '?']).unwrap_or(self.text.len());
        &self.text[..end]
    }

    /// True if the whole of `text` matches.
    pub fn matches(&self, text: &str) -> bool {
        let t: Vec<char> = text.chars().collect();
        let p = &self.chars;
        let (mut pi, mut ti) = (0, 0);
        let mut star: Option<(usize, usize)> = None;
        while ti < t.len() {
            if pi < p.len() && (p[pi] == '?' || (p[pi] != '*' && p[pi] == t[ti])) {
                pi += 1;
                ti += 1;
            } else if pi < p.len() && p[pi] == '*' {
                star = Some((pi, ti));
                pi += 1;
            } else if let Some((sp, st)) = star {
                pi = sp + 1;
                ti = st + 1;
                star = Some((sp, st + 1));
            } else {
                return false;
            }
        }
        p[pi..].iter().all(|c| *c == '*')
    }

    /// True if any of `forms` matches.
    pub fn matches_any<S: AsRef<str>>(&self, forms: &[S]) -> bool {
        forms.iter().any(|f| self.matches(f.as_ref()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wildcards() {
        let p = CommandPattern::new(" git push* ");
        assert_eq!(p.as_str(), "git push*");
        assert!(p.matches("git push"));
        assert!(p.matches("git push --force origin main"));
        assert!(!p.matches("git status"));
        assert!(!p.matches("xgit push"));
        assert!(CommandPattern::new("rm -rf /*").matches("rm -rf /home/x"));
        assert!(CommandPattern::new("ls?").matches("lsx"));
        assert!(!CommandPattern::new("ls?").matches("ls"));
        assert!(CommandPattern::new("*").matches(""));
        assert!(CommandPattern::new("a*b*c").matches("a1b2b3c"));
        assert!(!CommandPattern::new("a*b*c").matches("a1b2b3"));
        assert_eq!(
            CommandPattern::new("git push o*").literal_prefix(),
            "git push o"
        );
        assert_eq!(CommandPattern::new("*").literal_prefix(), "");
        assert_eq!(CommandPattern::new("ls").literal_prefix(), "ls");
    }
}
