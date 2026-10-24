//! Text layout and number formatting shared by the chat views.
//!
//! Views lay out their own lines (so the transcript can cache heights), so
//! they need display-width aware truncation, wrapping and right alignment.
//! The `fmt_*` helpers keep tokens, costs and durations formatted the same
//! way everywhere (activity line, cards, metadata line, status line).

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use crate::selection::Join;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// Display width of `s` in terminal cells.
pub fn width(s: &str) -> usize {
    UnicodeWidthStr::width(s)
}

/// Display width of a line.
pub fn line_width(line: &Line<'_>) -> usize {
    line.spans.iter().map(|s| width(&s.content)).sum()
}

/// Truncates `s` to at most `max` cells, ending in `…` when cut.
pub fn truncate(s: &str, max: usize) -> String {
    if width(s) <= max {
        return s.to_owned();
    }
    if max == 0 {
        return String::new();
    }
    let mut out = String::new();
    let mut w = 0;
    for c in s.chars() {
        let cw = c.width().unwrap_or(0);
        if w + cw + 1 > max {
            break;
        }
        out.push(c);
        w += cw;
    }
    out.push('…');
    out
}

/// Truncates the spans of `line` to `max` cells.
pub fn truncate_line(line: Line<'static>, max: usize) -> Line<'static> {
    let mut left = max;
    let mut spans = Vec::new();
    let total = line_width(&line);
    for span in line.spans {
        if left == 0 {
            break;
        }
        let w = width(&span.content);
        if w <= left && (total <= max || w < left) {
            left -= w;
            spans.push(span);
        } else {
            let cut = truncate(&span.content, left);
            spans.push(Span::styled(cut, span.style));
            break;
        }
    }
    Line::from(spans).style(line.style)
}

/// Pads `left` with `fill` so that `right` ends at column `width`. When the
/// two do not fit, `left` is truncated first; `right` is kept whole while it
/// fits on its own.
pub fn right_align(
    left: Line<'static>,
    right: Vec<Span<'static>>,
    width: usize,
    fill: Span<'static>,
) -> Line<'static> {
    let rw: usize = right.iter().map(|s| self::width(&s.content)).sum();
    if rw >= width {
        return truncate_line(Line::from(right), width);
    }
    let room = width - rw;
    let style = left.style;
    let mut left = truncate_line(left, room.saturating_sub(1).max(1).min(room));
    let lw = line_width(&left);
    let gap = room.saturating_sub(lw);
    if gap > 0 {
        let unit = if fill.content.is_empty() {
            " "
        } else {
            fill.content.as_ref()
        };
        let pad: String = unit.chars().cycle().take(gap).collect();
        left.spans.push(Span::styled(pad, fill.style));
    }
    left.spans.extend(right);
    left.style(style)
}

/// The zero-width span that starts a soft-wrapped continuation line (see
/// [`wrap_line`] and [`soft_join`]). It renders nothing, so the marker
/// survives caching, cloning and style patches without changing a cell.
fn wrap_marker(join: Join) -> Span<'static> {
    let m = match join {
        Join::Direct => Modifier::HIDDEN | Modifier::RAPID_BLINK,
        _ => Modifier::HIDDEN,
    };
    Span::styled("", Style::new().add_modifier(m))
}

/// How a line laid out by [`wrap_line`] continues the line above it:
/// [`Join::Hard`] for the first line of a wrapped line (and every line not
/// from `wrap_line`), [`Join::Space`] when the wrap replaced a space and
/// [`Join::Direct`] when it split a long word. Text selection uses it to
/// rejoin soft-wrapped lines.
pub fn soft_join(line: &Line<'_>) -> Join {
    match line.spans.first() {
        Some(s) if s.content.is_empty() && s.style.add_modifier.contains(Modifier::HIDDEN) => {
            if s.style.add_modifier.contains(Modifier::RAPID_BLINK) {
                Join::Direct
            } else {
                Join::Space
            }
        }
        _ => Join::Hard,
    }
}

/// Word-wraps `line` to `width` cells. Long words are split. Every output
/// line keeps the line style; `indent` spans are prepended to continuation
/// lines, which start with a zero-width marker telling [`soft_join`] how
/// they continue the line above.
pub fn wrap_line(
    line: Line<'static>,
    width: usize,
    indent: &[Span<'static>],
) -> Vec<Line<'static>> {
    wrap_line_with(line, width, width, indent)
}

/// [`wrap_line`] with a different width for the first output line, e.g.
/// to leave room for a right-aligned part next to it ([`wrap_right`]).
pub fn wrap_line_with(
    line: Line<'static>,
    first: usize,
    width: usize,
    indent: &[Span<'static>],
) -> Vec<Line<'static>> {
    let width = width.max(1);
    let first = first.max(1);
    if line_width(&line) <= first {
        return vec![line];
    }
    let indent_w: usize = indent.iter().map(|s| self::width(&s.content)).sum();
    let indent: &[Span<'static>] = if indent_w >= width { &[] } else { indent };
    let indent_w = if indent.is_empty() { 0 } else { indent_w };
    let style = line.style;
    // Split spans into runs of spaces and runs of non-spaces.
    let mut pieces: Vec<(String, Style)> = Vec::new();
    for span in &line.spans {
        let mut cur = String::new();
        let mut cur_space = None;
        for c in span.content.chars() {
            let is_space = c == ' ';
            if cur_space.is_some_and(|s| s != is_space) {
                pieces.push((std::mem::take(&mut cur), span.style));
            }
            cur_space = Some(is_space);
            cur.push(c);
        }
        if !cur.is_empty() {
            pieces.push((cur, span.style));
        }
    }
    let mut out: Vec<Line<'static>> = Vec::new();
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut w = 0;
    // Width at which the current line has no content yet.
    let mut base = 0;
    // `space`: the break replaces a space piece the caller skipped.
    let new_line = |spans: &mut Vec<Span<'static>>, out: &mut Vec<Line<'static>>, space: bool| {
        let mut popped = false;
        while spans.len() > 1 && spans.last().is_some_and(|s| s.content.trim().is_empty()) {
            popped |= spans.pop().is_some_and(|s| !s.content.is_empty());
        }
        out.push(Line::from(std::mem::take(spans)).style(style));
        spans.push(wrap_marker(if space || popped {
            Join::Space
        } else {
            Join::Direct
        }));
        spans.extend(indent.iter().cloned());
    };
    let at = |out: &Vec<Line<'static>>| if out.is_empty() { first } else { width };
    for (text, st) in pieces {
        let pw = self::width(&text);
        if w + pw <= at(&out) {
            if !(text.starts_with(' ') && w == base && !out.is_empty()) {
                spans.push(Span::styled(text, st));
                w += pw;
            }
            continue;
        }
        if text.starts_with(' ') {
            new_line(&mut spans, &mut out, true);
            w = indent_w;
            base = indent_w;
            continue;
        }
        if w > base && pw <= width.saturating_sub(indent_w) {
            new_line(&mut spans, &mut out, false);
            w = indent_w;
            base = indent_w;
            spans.push(Span::styled(text, st));
            w += pw;
            continue;
        }
        let mut chunk = String::new();
        for c in text.chars() {
            let cw = c.width().unwrap_or(0);
            if w + cw > at(&out) && w > base {
                if !chunk.is_empty() {
                    spans.push(Span::styled(std::mem::take(&mut chunk), st));
                }
                new_line(&mut spans, &mut out, false);
                w = indent_w;
                base = indent_w;
            }
            chunk.push(c);
            w += cw;
        }
        if !chunk.is_empty() {
            spans.push(Span::styled(chunk, st));
        }
    }
    out.push(Line::from(spans).style(style));
    out
}

/// Lays out `left` with `right` right-aligned on its first row, like
/// [`right_align`], but wraps `left` onto more rows instead of cutting it.
/// Continuation rows start with `indent`. When `right` alone is wider
/// than `width`, it wraps on its own rows after `left`.
pub fn wrap_right(
    left: Line<'static>,
    right: Vec<Span<'static>>,
    width: usize,
    fill: Span<'static>,
    indent: &[Span<'static>],
) -> Vec<Line<'static>> {
    let width = width.max(1);
    let rw: usize = right.iter().map(|s| self::width(&s.content)).sum();
    if line_width(&left) + rw <= width {
        return vec![right_align(left, right, width, fill)];
    }
    if rw + 1 >= width {
        let mut out = wrap_line(left, width, indent);
        out.extend(wrap_line(Line::from(right), width, &[]));
        return out;
    }
    let mut out = wrap_line_with(left, width - rw - 1, width, indent);
    let head = out.remove(0);
    out.insert(0, right_align(head, right, width, fill));
    out
}

/// Word-wraps every line of `lines` to `width` cells (see [`wrap_line`]).
pub fn wrap_lines(
    lines: impl IntoIterator<Item = Line<'static>>,
    width: usize,
    indent: &[Span<'static>],
) -> Vec<Line<'static>> {
    lines
        .into_iter()
        .flat_map(|l| wrap_line(l, width, indent))
        .collect()
}

/// Plain-text wrap: splits `s` at newlines and word-wraps each line to
/// `width` cells, splitting words longer than a line. Control characters
/// are replaced first ([`sanitize`]).
pub fn wrap_text(s: &str, width: usize) -> Vec<String> {
    sanitize(s)
        .split('\n')
        .flat_map(|l| wrap_line(Line::from(l.to_owned()), width, &[]))
        .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
        .collect()
}

/// Removes the zero-width soft-wrap marker that starts a continuation line
/// laid out by [`wrap_line`], if there is one. Callers that prepend chrome
/// (a border, a gutter) to wrapped lines put the marker back in front of it,
/// so it stays the first span ([`soft_join`] and text selection rely on it).
pub fn take_wrap_marker(line: &mut Line<'static>) -> Option<Span<'static>> {
    match soft_join(line) {
        Join::Hard => None,
        _ => Some(line.spans.remove(0)),
    }
}

/// Token count: `812`, `12.4k`, `1.2M`.
pub fn fmt_tokens(n: u64) -> String {
    if n < 1000 {
        n.to_string()
    } else if n < 100_000 {
        format!("{:.1}", n as f64 / 1000.0)
            .trim_end_matches(".0")
            .to_owned()
            + "k"
    } else if n < 1_000_000 {
        format!("{}k", n / 1000)
    } else {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    }
}

/// Cost in USD: `$0.021`, `$0.84`, `$12.30`; `None` (unknown) is `$?`.
pub fn fmt_cost(usd: Option<f64>) -> String {
    match usd {
        None => "$?".to_owned(),
        Some(v) if v < 0.1 => format!("${v:.3}"),
        Some(v) => format!("${v:.2}"),
    }
}

/// Duration in milliseconds: `3ms`, `8.1s`, `2m05s`, `1h02m`.
pub fn fmt_ms(ms: u64) -> String {
    if ms < 1000 {
        format!("{ms}ms")
    } else if ms < 60_000 {
        format!("{:.1}s", ms as f64 / 1000.0)
    } else if ms < 3_600_000 {
        format!("{}m{:02}s", ms / 60_000, (ms / 1000) % 60)
    } else {
        format!("{}h{:02}m", ms / 3_600_000, (ms / 60_000) % 60)
    }
}

/// Byte count: `512 B`, `1.2 KB`, `3.4 MB`.
pub fn fmt_bytes(n: u64) -> String {
    if n < 1024 {
        format!("{n} B")
    } else if n < 1024 * 1024 {
        format!("{:.1} KB", n as f64 / 1024.0)
    } else {
        format!("{:.1} MB", n as f64 / (1024.0 * 1024.0))
    }
}

/// Cache hit rate in percent of all input tokens, if there was any input.
pub fn cache_pct(input: u64, cached: u64) -> Option<u64> {
    let total = input + cached;
    (total > 0).then(|| (cached * 100 + total / 2) / total)
}

/// Replaces control characters (except newlines) with spaces so text cannot
/// move the cursor or change terminal state. A tab becomes spaces up to the
/// next 4-column stop, so it is measured and drawn as the cells it takes.
pub fn sanitize(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut col = 0usize;
    for c in s.chars() {
        match c {
            '\n' => {
                out.push('\n');
                col = 0;
            }
            '\t' => {
                let n = TAB_STOP - col % TAB_STOP;
                out.extend(std::iter::repeat_n(' ', n));
                col += n;
            }
            c if c.is_control() => {
                out.push(' ');
                col += 1;
            }
            c => {
                out.push(c);
                col += c.width().unwrap_or(0);
            }
        }
    }
    out
}

/// Columns between tab stops when [`sanitize`] expands tabs.
pub const TAB_STOP: usize = 4;

#[cfg(test)]
mod tests {
    use super::*;

    fn text(l: &Line<'_>) -> String {
        l.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn truncates_by_display_width() {
        assert_eq!(truncate("hello", 10), "hello");
        assert_eq!(truncate("hello world", 6), "hello…");
        assert_eq!(truncate("日本語テキスト", 5), "日本…");
        assert_eq!(truncate("x", 0), "");
    }

    #[test]
    fn right_align_pads_and_truncates() {
        let l = right_align(
            Line::from("left side"),
            vec![Span::raw("12:00:00")],
            20,
            Span::raw(" "),
        );
        assert_eq!(text(&l), "left side   12:00:00");
        let l = right_align(
            Line::from("a very long left side"),
            vec![Span::raw("12:00:00")],
            16,
            Span::raw("─"),
        );
        assert_eq!(line_width(&l), 16);
        assert!(text(&l).ends_with("12:00:00"));
    }

    #[test]
    fn wraps_words_and_long_words() {
        let lines = wrap_line(Line::from("the quick brown fox jumps"), 10, &[]);
        let t: Vec<String> = lines.iter().map(text).collect();
        assert_eq!(t, vec!["the quick", "brown fox", "jumps"]);
        let lines = wrap_line(Line::from("abcdefghijkl"), 5, &[]);
        let t: Vec<String> = lines.iter().map(text).collect();
        assert_eq!(t, vec!["abcde", "fghij", "kl"]);
        let lines = wrap_line(Line::from("- one two three"), 8, &[Span::raw("  ")]);
        let t: Vec<String> = lines.iter().map(text).collect();
        assert_eq!(t, vec!["- one", "  two", "  three"]);
        for l in &lines {
            assert!(line_width(l) <= 8);
        }
    }

    #[test]
    fn number_formats() {
        assert_eq!(fmt_tokens(812), "812");
        assert_eq!(fmt_tokens(12_400), "12.4k");
        assert_eq!(fmt_tokens(182_000), "182k");
        assert_eq!(fmt_tokens(1_200_000), "1.2M");
        assert_eq!(fmt_cost(Some(0.021)), "$0.021");
        assert_eq!(fmt_cost(Some(0.84)), "$0.84");
        assert_eq!(fmt_cost(None), "$?");
        assert_eq!(fmt_ms(3), "3ms");
        assert_eq!(fmt_ms(8100), "8.1s");
        assert_eq!(fmt_ms(125_000), "2m05s");
        assert_eq!(fmt_bytes(1234), "1.2 KB");
        assert_eq!(cache_pct(4, 96), Some(96));
        assert_eq!(cache_pct(0, 0), None);
        assert_eq!(sanitize("a\x1b[2Jb"), "a [2Jb");
    }
}
