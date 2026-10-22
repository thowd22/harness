//! Streaming Markdown rendering for assistant text.
//!
//! Assistant text arrives in deltas, so re-rendering the whole message for
//! every token would cost O(length) per frame. [`MarkdownCache`] renders
//! completed blocks once (a block is complete when a blank line outside a
//! code fence, or a closing fence, follows it) and re-renders only the open
//! tail block. Code blocks are highlighted by [`super::highlight`], which
//! loads its tables on first use.
//!
//! Supported: headings, paragraphs with `code`, **bold**, *italic* and
//! `[links](url)`, bullet and numbered lists, block quotes, rules and fenced
//! code blocks. Everything else is shown as text.
//!
//! Every fenced code block starts with a header line (its language, if
//! any, and a `⧉ copy` button); [`MarkdownCache::code_headers`] says where
//! they are and [`code_blocks`] returns the blocks' text for copying.

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use super::highlight::{self, State};
use super::text::{truncate_line, width as text_width, wrap_line};
use crate::clipboard::COPY_LABEL;
use crate::theme::{Theme, Token};

/// Incrementally rendered Markdown for one growing text.
#[derive(Debug, Clone, Default)]
pub struct MarkdownCache {
    width: usize,
    markdown: bool,
    stable_bytes: usize,
    stable_lines: Vec<Line<'static>>,
    stable_headers: Vec<CodeHeader>,
    headers: Vec<CodeHeader>,
    /// Blocks rendered so far (for tests: proves completed blocks are cached).
    renders: usize,
}

impl MarkdownCache {
    /// An empty cache.
    pub fn new() -> MarkdownCache {
        MarkdownCache::default()
    }

    /// Number of block renders performed (completed blocks are rendered once).
    pub fn block_renders(&self) -> usize {
        self.renders
    }

    /// Lines for `text` at `width`. `text` must only ever grow (streaming
    /// appends); if it shrinks or `width`/`markdown` change, the cache resets.
    pub fn lines(
        &mut self,
        text: &str,
        width: usize,
        theme: &Theme,
        markdown: bool,
    ) -> Vec<Line<'static>> {
        if width != self.width || markdown != self.markdown || text.len() < self.stable_bytes {
            self.width = width;
            self.markdown = markdown;
            self.stable_bytes = 0;
            self.stable_lines.clear();
            self.stable_headers.clear();
        }
        let boundary = self.stable_bytes + stable_boundary(&text[self.stable_bytes..]);
        if boundary > self.stable_bytes {
            let chunk = &text[self.stable_bytes..boundary];
            self.renders += 1;
            let (lines, headers) = render_indexed(chunk, width, theme, markdown);
            let base = self.stable_lines.len();
            self.stable_headers
                .extend(headers.into_iter().map(|h| h.offset(base)));
            self.stable_lines.extend(lines);
            self.stable_bytes = boundary;
        }
        let mut out = self.stable_lines.clone();
        self.headers = self.stable_headers.clone();
        let tail = &text[self.stable_bytes..];
        if !tail.is_empty() {
            self.renders += 1;
            let (lines, headers) = render_indexed(tail, width, theme, markdown);
            let base = out.len();
            self.headers
                .extend(headers.into_iter().map(|h| h.offset(base)));
            out.extend(lines);
        }
        while out
            .last()
            .is_some_and(|l| l.spans.iter().all(|s| s.content.trim().is_empty()))
        {
            out.pop();
        }
        out
    }

    /// Code block headers of the last [`MarkdownCache::lines`] call, in
    /// block order.
    pub fn code_headers(&self) -> &[CodeHeader] {
        &self.headers
    }
}

/// Where a code block's header line (and its copy button) is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CodeHeader {
    /// Line index in the rendered lines.
    pub line: usize,
    /// Column of the `⧉ copy` button.
    pub col: usize,
}

impl CodeHeader {
    fn offset(self, base: usize) -> CodeHeader {
        CodeHeader {
            line: self.line + base,
            ..self
        }
    }
}

/// The text of every fenced code block in `text` (fences and the language
/// tag removed), in order. An unterminated block runs to the end.
pub fn code_blocks(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur: Option<Vec<&str>> = None;
    for raw in text.lines() {
        let t = raw.trim_start();
        if t.starts_with("```") || t.starts_with("~~~") {
            match cur.take() {
                Some(lines) => out.push(lines.join("\n")),
                None => cur = Some(Vec::new()),
            }
            continue;
        }
        if let Some(lines) = cur.as_mut() {
            lines.push(raw);
        }
    }
    if let Some(lines) = cur {
        out.push(lines.join("\n"));
    }
    out
}

/// Byte length of the prefix of `text` made of complete blocks.
fn stable_boundary(text: &str) -> usize {
    let mut in_fence = false;
    let mut pos = 0;
    let mut boundary = 0;
    for line in text.split_inclusive('\n') {
        pos += line.len();
        if !line.ends_with('\n') {
            break;
        }
        let t = line.trim();
        if t.starts_with("```") || t.starts_with("~~~") {
            in_fence = !in_fence;
            if !in_fence {
                boundary = pos;
            }
        } else if !in_fence && t.is_empty() {
            boundary = pos;
        }
    }
    boundary
}

/// Renders Markdown (or plain text when `markdown` is false) to lines no
/// wider than `width`.
pub fn render_blocks(
    text: &str,
    width: usize,
    theme: &Theme,
    markdown: bool,
) -> Vec<Line<'static>> {
    render_indexed(text, width, theme, markdown).0
}

/// [`render_blocks`] plus the code block headers.
fn render_indexed(
    text: &str,
    width: usize,
    theme: &Theme,
    markdown: bool,
) -> (Vec<Line<'static>>, Vec<CodeHeader>) {
    let mut out = Vec::new();
    let mut headers = Vec::new();
    let fg = theme.fg(Token::Fg);
    if !markdown {
        for l in text.lines() {
            out.extend(wrap_line(
                Line::from(Span::styled(l.to_owned(), fg)),
                width,
                &[],
            ));
        }
        return (out, headers);
    }
    let mut fence: Option<(Option<&'static highlight::Lang>, State)> = None;
    for raw in text.lines() {
        let t = raw.trim_start();
        if t.starts_with("```") || t.starts_with("~~~") {
            if fence.is_some() {
                fence = None;
            } else {
                let lang = highlight::lang_for(t.trim_start_matches(['`', '~']));
                fence = Some((lang, State::default()));
                let label = t.trim_start_matches(['`', '~']).trim();
                let head = if label.is_empty() {
                    "  ".to_owned()
                } else {
                    format!("  {label}  ")
                };
                headers.push(CodeHeader {
                    line: out.len(),
                    col: text_width(&head),
                });
                out.push(truncate_line(
                    Line::from(vec![
                        Span::styled(head, theme.fg(Token::Muted)),
                        Span::styled(COPY_LABEL, theme.fg(Token::Muted)),
                    ]),
                    width,
                ));
            }
            continue;
        }
        if let Some((lang, state)) = fence.as_mut() {
            let mut spans = vec![Span::styled("│ ", theme.fg(Token::Border))];
            spans.extend(highlight::highlight_line(
                *lang,
                &raw.replace('\t', "    "),
                state,
                theme,
                Style::default(),
            ));
            out.push(truncate_line(Line::from(spans), width));
            continue;
        }
        if t.is_empty() {
            out.push(Line::default());
            continue;
        }
        if let Some(level) = heading_level(t) {
            let body = t[level..].trim();
            let style = theme.fg(Token::Accent).add_modifier(Modifier::BOLD);
            let mut spans = inline(body, theme, style);
            if level == 1 {
                for s in &mut spans {
                    s.style = s.style.add_modifier(Modifier::UNDERLINED);
                }
            }
            out.extend(wrap_line(Line::from(spans), width, &[]));
            continue;
        }
        if t.chars().all(|c| matches!(c, '-' | '*' | '_')) && t.len() >= 3 {
            out.push(Line::from(Span::styled(
                "─".repeat(width),
                theme.fg(Token::Border),
            )));
            continue;
        }
        let indent = raw.len() - t.len();
        let pad = " ".repeat(indent.min(8));
        if let Some(rest) = t
            .strip_prefix("- ")
            .or_else(|| t.strip_prefix("* "))
            .or_else(|| t.strip_prefix("+ "))
        {
            let mut spans = vec![
                Span::raw(pad.clone()),
                Span::styled("• ", theme.fg(Token::Accent)),
            ];
            spans.extend(inline(rest, theme, fg));
            let cont = [Span::raw(format!("{pad}  "))];
            out.extend(wrap_line(Line::from(spans), width, &cont));
            continue;
        }
        if let Some((num, rest)) = numbered(t) {
            let marker = format!("{num}. ");
            let cont = [Span::raw(format!("{pad}{}", " ".repeat(marker.len())))];
            let mut spans = vec![
                Span::raw(pad.clone()),
                Span::styled(marker, theme.fg(Token::Accent)),
            ];
            spans.extend(inline(rest, theme, fg));
            out.extend(wrap_line(Line::from(spans), width, &cont));
            continue;
        }
        if let Some(rest) = t.strip_prefix('>') {
            let style = theme.fg(Token::Muted).add_modifier(Modifier::ITALIC);
            let bar = Span::styled("▎ ", theme.fg(Token::Border));
            let mut spans = vec![bar.clone()];
            spans.extend(inline(rest.trim_start(), theme, style));
            out.extend(wrap_line(Line::from(spans), width, &[bar]));
            continue;
        }
        out.extend(wrap_line(Line::from(inline(t, theme, fg)), width, &[]));
    }
    (out, headers)
}

fn heading_level(t: &str) -> Option<usize> {
    let n = t.chars().take_while(|c| *c == '#').count();
    (1..=6)
        .contains(&n)
        .then_some(n)
        .filter(|&n| t[n..].starts_with(' '))
}

fn numbered(t: &str) -> Option<(&str, &str)> {
    let digits = t.chars().take_while(char::is_ascii_digit).count();
    if digits == 0 || digits > 3 {
        return None;
    }
    let rest = t[digits..].strip_prefix(". ")?;
    Some((&t[..digits], rest))
}

/// Inline Markdown: `code`, **bold**, *italic* / _italic_, `[text](url)`.
pub fn inline(text: &str, theme: &Theme, base: Style) -> Vec<Span<'static>> {
    let mut out: Vec<Span<'static>> = Vec::new();
    let mut plain = String::new();
    let mut bold = false;
    let mut italic = false;
    let style_now = |bold: bool, italic: bool| {
        let mut s = base;
        if bold {
            s = s.add_modifier(Modifier::BOLD);
        }
        if italic {
            s = s.add_modifier(Modifier::ITALIC);
        }
        s
    };
    let flush = |plain: &mut String, out: &mut Vec<Span<'static>>, style: Style| {
        if !plain.is_empty() {
            out.push(Span::styled(std::mem::take(plain), style));
        }
    };
    let mut rest = text;
    while let Some(c) = rest.chars().next() {
        if c == '`'
            && let Some(end) = rest[1..].find('`')
        {
            flush(&mut plain, &mut out, style_now(bold, italic));
            out.push(Span::styled(
                rest[1..1 + end].to_owned(),
                theme.fg(Token::Ok),
            ));
            rest = &rest[end + 2..];
            continue;
        }
        if (rest.starts_with("**") || rest.starts_with("__"))
            && (bold || rest[2..].contains(&rest[..2]))
        {
            flush(&mut plain, &mut out, style_now(bold, italic));
            bold = !bold;
            rest = &rest[2..];
            continue;
        }
        if (c == '*' || c == '_')
            && (italic || rest[1..].contains(c))
            && !(c == '_' && plain.chars().last().is_some_and(char::is_alphanumeric))
        {
            flush(&mut plain, &mut out, style_now(bold, italic));
            italic = !italic;
            rest = &rest[1..];
            continue;
        }
        if c == '['
            && let Some(close) = rest.find("](")
            && let Some(end) = rest[close..].find(')')
        {
            flush(&mut plain, &mut out, style_now(bold, italic));
            let label = &rest[1..close];
            out.push(Span::styled(
                label.to_owned(),
                theme.fg(Token::Link).add_modifier(Modifier::UNDERLINED),
            ));
            rest = &rest[close + end + 1..];
            continue;
        }
        plain.push(c);
        rest = &rest[c.len_utf8()..];
    }
    flush(&mut plain, &mut out, style_now(bold, italic));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(lines: &[Line<'_>]) -> Vec<String> {
        lines
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect()
    }

    #[test]
    fn renders_common_blocks() {
        let theme = Theme::default();
        let md = "# Title\n\nSome **bold** and `code`.\n\n- one\n- two\n\n1. first\n\n> quote\n\n```rust\nlet x = 1;\n```\n";
        let lines = render_blocks(md, 40, &theme, true);
        let t = texts(&lines);
        assert_eq!(t[0], "Title");
        assert!(t.contains(&"Some bold and code.".to_owned()), "{t:?}");
        assert!(t.contains(&"• one".to_owned()));
        assert!(t.contains(&"1. first".to_owned()));
        assert!(t.contains(&"▎ quote".to_owned()));
        assert!(t.contains(&"│ let x = 1;".to_owned()));
        // bold is styled bold, code in the ok token
        let para = lines
            .iter()
            .find(|l| l.spans.iter().any(|s| s.content == "bold"))
            .unwrap();
        let bold = para.spans.iter().find(|s| s.content == "bold").unwrap();
        assert!(bold.style.add_modifier.contains(Modifier::BOLD));
        let code = para.spans.iter().find(|s| s.content == "code").unwrap();
        assert_eq!(code.style.fg, theme.fg(Token::Ok).fg);
        // the code line is highlighted: `let` is a keyword
        let code_line = lines
            .iter()
            .find(|l| l.spans.iter().any(|s| s.content == "let"))
            .unwrap();
        let kw = code_line.spans.iter().find(|s| s.content == "let").unwrap();
        assert_eq!(kw.style.fg, theme.fg(Token::Keyword).fg);
    }

    #[test]
    fn plain_mode_keeps_markup() {
        let theme = Theme::default();
        let t = texts(&render_blocks("**x**", 20, &theme, false));
        assert_eq!(t, vec!["**x**"]);
    }

    #[test]
    fn streaming_renders_completed_blocks_once() {
        let theme = Theme::default();
        let mut cache = MarkdownCache::new();
        let mut text = String::new();
        for i in 0..50 {
            text.push_str(&format!("paragraph {i} text\n\n"));
            cache.lines(&text, 40, &theme, true);
        }
        // Each call renders at most the newly completed block plus the tail.
        assert!(cache.block_renders() <= 100, "{}", cache.block_renders());
        let before = cache.block_renders();
        text.push_str("open tail");
        let lines = cache.lines(&text, 40, &theme, true);
        assert_eq!(cache.block_renders(), before + 1);
        assert_eq!(texts(&lines).last().unwrap(), "open tail");
        // An unterminated fence is the open tail until it closes.
        text.push_str("\n\n```rust\nfn a() {}\n");
        let lines = cache.lines(&text, 40, &theme, true);
        assert!(texts(&lines).contains(&"│ fn a() {}".to_owned()));
        text.push_str("```\n");
        let full = render_blocks(&text, 40, &theme, true);
        let lines = cache.lines(&text, 40, &theme, true);
        let mut full = texts(&full);
        while full.last().is_some_and(|l| l.trim().is_empty()) {
            full.pop();
        }
        assert_eq!(texts(&lines), full);
    }

    #[test]
    fn wraps_to_width() {
        let theme = Theme::default();
        let lines = render_blocks(&"word ".repeat(40), 30, &theme, true);
        assert!(lines.len() > 1);
        assert!(
            lines
                .iter()
                .all(|l| super::super::text::line_width(l) <= 30)
        );
    }

    #[test]
    fn code_blocks_have_copy_headers_and_extract_exactly() {
        let theme = Theme::default();
        let md = "Intro\n\n```rust\nfn a() {}\n\tlet x = 1;\n```\n\nthen\n\n```\nplain\n```\n";
        assert_eq!(
            code_blocks(md),
            vec!["fn a() {}\n\tlet x = 1;".to_owned(), "plain".to_owned()]
        );
        assert_eq!(code_blocks("```sh\nopen"), vec!["open".to_owned()]);
        let mut cache = MarkdownCache::new();
        let lines = cache.lines(md, 40, &theme, true);
        let t = texts(&lines);
        let headers = cache.code_headers().to_vec();
        assert_eq!(headers.len(), 2, "{t:?}");
        assert_eq!(t[headers[0].line], "  rust  ⧉ copy");
        assert_eq!(headers[0].col, 8);
        assert_eq!(t[headers[1].line], "  ⧉ copy");
        assert_eq!(headers[1].col, 2);
        // Streaming: headers of the open tail are reported too.
        let mut cache = MarkdownCache::new();
        cache.lines("a\n\n```py\nx", 40, &theme, true);
        assert_eq!(cache.code_headers().len(), 1);
    }
}
