//! Shared fuzzy matching for the command palette and the slash-command
//! dropdown.
//!
//! Both use one `nucleo-matcher` configuration with smart case and smart
//! Unicode normalization, so a query ranks the same way wherever it is
//! typed. [`Fuzzy`] owns the matcher and its scratch buffers;
//! [`highlight`] turns match indices into styled spans.

use nucleo_matcher::pattern::{AtomKind, CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Matcher, Utf32Str};
use ratatui::style::Style;
use ratatui::text::Span;

/// A query in palette syntax: words match independently, and `^`, `$`,
/// `'` and `!` change how a word matches (prefix, suffix, exact, not).
pub fn query(q: &str) -> Pattern {
    Pattern::parse(q, CaseMatching::Smart, Normalization::Smart)
}

/// A gapped fuzzy query with no special characters (`/mdl` matches
/// `model`).
pub fn fuzzy(q: &str) -> Pattern {
    Pattern::new(
        q,
        CaseMatching::Smart,
        Normalization::Smart,
        AtomKind::Fuzzy,
    )
}

/// A contiguous substring query with no special characters.
pub fn substring(q: &str) -> Pattern {
    Pattern::new(
        q,
        CaseMatching::Smart,
        Normalization::Smart,
        AtomKind::Substring,
    )
}

/// The matcher plus reusable buffers.
#[derive(Debug, Clone, Default)]
pub struct Fuzzy {
    matcher: Matcher,
    buf: Vec<char>,
}

impl Fuzzy {
    /// A matcher with the default configuration.
    pub fn new() -> Fuzzy {
        Fuzzy::default()
    }

    /// Scores a pre-converted haystack; `None` when it does not match.
    pub fn score(&mut self, pattern: &Pattern, hay: Utf32Str<'_>) -> Option<u32> {
        pattern.score(hay, &mut self.matcher)
    }

    /// Matches `hay`, returning the score and filling `indices` with the
    /// sorted, deduplicated character positions that matched.
    pub fn indices(&mut self, pattern: &Pattern, hay: &str, indices: &mut Vec<u32>) -> Option<u32> {
        indices.clear();
        let score = pattern.indices(
            Utf32Str::new(hay, &mut self.buf),
            &mut self.matcher,
            indices,
        );
        if score.is_none() {
            indices.clear();
        }
        indices.sort_unstable();
        indices.dedup();
        score
    }

    /// Fills `indices` with the sorted, deduplicated positions of every
    /// word of `pattern` that matches `hay` on its own, even when other
    /// words do not. The palette matches a query against title,
    /// description, kind and scope together, so a listed row can match
    /// only some query words in its title; those words are still shown.
    /// A word that fails leaves no positions behind.
    pub fn word_indices(&mut self, pattern: &Pattern, hay: &str, indices: &mut Vec<u32>) {
        indices.clear();
        let hay = Utf32Str::new(hay, &mut self.buf);
        for atom in &pattern.atoms {
            let len = indices.len();
            if atom.indices(hay, &mut self.matcher, indices).is_none() {
                indices.truncate(len);
            }
        }
        indices.sort_unstable();
        indices.dedup();
    }
}

/// Splits `text` into spans: characters whose position is in `indices`
/// (sorted) get `hit`, the rest get `base`. Runs of one style share a span.
pub fn highlight(text: &str, indices: &[u32], base: Style, hit: Style) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    let mut run = String::new();
    let mut run_hit = false;
    let mut next = indices.iter().peekable();
    for (i, ch) in text.chars().enumerate() {
        while next.peek().is_some_and(|&&n| (n as usize) < i) {
            next.next();
        }
        let is_hit = next.peek().is_some_and(|&&n| n as usize == i);
        if is_hit != run_hit && !run.is_empty() {
            let style = if run_hit { hit } else { base };
            spans.push(Span::styled(std::mem::take(&mut run), style));
        }
        run_hit = is_hit;
        run.push(ch);
    }
    if !run.is_empty() {
        spans.push(Span::styled(run, if run_hit { hit } else { base }));
    }
    spans
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::style::Modifier;

    #[test]
    fn gapped_matches_and_sorted_indices() {
        let mut f = Fuzzy::new();
        let mut idx = Vec::new();
        assert!(f.indices(&fuzzy("mdl"), "model", &mut idx).is_some());
        assert_eq!(idx, vec![0, 2, 4]);
        assert!(f.indices(&fuzzy("xyz"), "model", &mut idx).is_none());
        assert!(idx.is_empty());
        assert!(f.indices(&substring("mdl"), "model", &mut idx).is_none());
        assert!(f.indices(&substring("ode"), "model", &mut idx).is_some());
        assert_eq!(idx, vec![1, 2, 3]);
    }

    #[test]
    fn highlight_groups_runs() {
        let hit = Style::new().add_modifier(Modifier::BOLD);
        let spans = highlight("model", &[0, 1, 4], Style::new(), hit);
        let parts: Vec<(&str, bool)> = spans
            .iter()
            .map(|s| (s.content.as_ref(), s.style == hit))
            .collect();
        assert_eq!(parts, vec![("mo", true), ("de", false), ("l", true)]);
        assert!(highlight("", &[], Style::new(), hit).is_empty());
    }

    #[test]
    fn word_indices_keep_words_that_match() {
        let mut f = Fuzzy::new();
        let mut idx = vec![9];
        // `sw` misses the title, `mod` still hits it, in either order.
        f.word_indices(&query("mod sw"), "Model", &mut idx);
        assert_eq!(idx, vec![0, 1, 2]);
        f.word_indices(&query("sw mod"), "Model", &mut idx);
        assert_eq!(idx, vec![0, 1, 2]);
        f.word_indices(&query("xyz"), "Model", &mut idx);
        assert!(idx.is_empty());
        // `indices` still reports a miss with no positions.
        assert!(f.indices(&query("mod sw"), "Model", &mut idx).is_none());
        assert!(idx.is_empty());
    }
}
