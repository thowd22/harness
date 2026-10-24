//! The layout test matrix (TASK-160): every view rendered with long-content
//! fixtures at a matrix of terminal sizes on `TestBackend`.
//!
//! Each case asserts the layout rules from `docs/tui.md`:
//!
//! - no panic at any size, down to tiny terminals;
//! - nothing is drawn on or past a border: every box found on screen has
//!   both side borders intact on every row and a corner at each end of its
//!   top and bottom rows ([`check_borders`]);
//! - every fixture token is visible, possibly wrapped over several rows,
//!   or reachable through the view's documented action (scroll, expand,
//!   open a detail), which the case performs ([`assert_reachable`]);
//! - no raw tab or control character reaches the screen.
//!
//! The cases live in the submodules, one per area.

use std::panic::{AssertUnwindSafe, catch_unwind};

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

mod chat;
mod overlays;
mod pickers;
mod screens;

/// Terminal sizes every case runs at (columns, rows).
pub const SIZES: [(u16, u16); 5] = [(40, 12), (60, 20), (80, 24), (120, 40), (250, 60)];

/// Extra tiny sizes every case must survive without a panic (contents are
/// not checked: nothing useful fits).
pub const TINY: [(u16, u16); 4] = [(1, 1), (8, 3), (20, 5), (30, 8)];

/// A word longer than any tested width.
pub const LONG_WORD: &str =
    "Pneumonoultramicroscopicsilicovolcanoconiosis_and_antidisestablishmentarianism";
/// The directory part of a deep path.
pub const DEEP_DIR: &str =
    "/private/var/folders/n6/hn37q6pj7sj_9dm0000gn/T/.tmpXyZ123/project/deeply/nested/src/module";
/// A long URL (without its unique tail).
pub const URL: &str =
    "https://example.com/a/very/long/url/path/that/keeps/going/on?query=string&and=more#frag";
/// Wide characters: CJK and emoji (two cells each).
pub const WIDE: &str = "日本語のテキストと絵文字🎉🚀を含む長い行です中文字符測試結尾";

/// Long-content fixtures. Every value carries a unique tag, and every
/// token handed out is recorded, so a case can assert that each field it
/// filled is visible or reachable (not just one copy of a shared string).
#[derive(Debug, Default, Clone)]
pub struct Fx {
    /// Every token handed out so far.
    pub tokens: Vec<String>,
}

impl Fx {
    /// No tokens yet.
    pub fn new() -> Fx {
        Fx::default()
    }

    fn keep(&mut self, s: String) -> String {
        self.tokens.push(s.clone());
        s
    }

    /// A word longer than any tested width.
    pub fn word(&mut self, tag: &str) -> String {
        self.keep(format!("{LONG_WORD}_{tag}"))
    }

    /// A deep path.
    pub fn path(&mut self, tag: &str) -> String {
        self.keep(format!("{DEEP_DIR}/{tag}_file.rs"))
    }

    /// A long URL.
    pub fn url(&mut self, tag: &str) -> String {
        self.keep(format!("{URL}_{tag}"))
    }

    /// CJK and emoji.
    pub fn wide(&mut self, tag: &str) -> String {
        self.keep(format!("{WIDE}{tag}"))
    }

    /// Columns separated by tabs.
    pub fn tabs(&mut self, tag: &str) -> String {
        self.keep(format!("tab_one\ttab_two\ttab_{tag}"))
    }

    /// A sentence with one of each kind, tagged `tag.1` to `tag.5`.
    pub fn prose(&mut self, tag: &str) -> String {
        let (p, u, w, c, t) = (
            self.path(&format!("{tag}1")),
            self.url(&format!("{tag}2")),
            self.word(&format!("{tag}3")),
            self.wide(&format!("{tag}4")),
            self.tabs(&format!("{tag}5")),
        );
        format!("See {p} and {u} for {w}; {c} {t} done.")
    }

    /// The tokens as `&str`s.
    pub fn all(&self) -> Vec<&str> {
        self.tokens.iter().map(String::as_str).collect()
    }
}

/// A scripted run full of long content: a user prompt, reasoning and a
/// markdown answer (code block, table, URL), tool cards with long paths,
/// arguments and output, a file change with a diff, a failing bash call,
/// todos, a subagent on a remote host with long names and a verify run.
/// Returns the events and the fixtures they hold.
pub fn run_events() -> (Vec<harness_events::schema::RunEvent>, Fx) {
    use crate::demo::events::{EventBuilder, NodeSpec, usage};
    use harness_events::schema::{DeltaChannel, FileChangeKind};
    use serde_json::json;
    let mut fx = Fx::new();
    let mut b = EventBuilder::new();
    b.run_started("layout", None);
    let model = fx.word("model");
    b.start(NodeSpec::step("main").agent("coder").model(&model));
    let prompt = fx.prose("user");
    b.user_message("main", &prompt);
    b.model_call("main", "c1", &model);
    let thinking = fx.prose("think");
    b.delta("main", "c1", DeltaChannel::Reasoning, &thinking);
    let (w1, u1, code, c1, path1, wide1, t1) = (
        fx.word("md1"),
        fx.url("md2"),
        fx.word("code"),
        fx.wide("code"),
        fx.path("table"),
        fx.wide("table"),
        fx.tabs("item"),
    );
    let md = format!(
        "Answer with {w1} and {u1}.\n\n```rust\nlet s = \"{code}\"; // {c1}\n```\n\n| file | note |\n|---|---|\n| {path1} | {wide1} |\n\n- item {t1}\n"
    );
    b.delta("main", "c1", DeltaChannel::Text, &md);
    b.advance(1_000);
    b.finish_call("main", "c1", usage(500, 11_900, 812, 120), 4_200);
    b.cost("main", 0.021);
    let read_path = fx.path("read");
    b.tool_call("main", "t1", "read", json!({"path": read_path}));
    let read_out = fx.prose("readout");
    b.tool_result("main", "t1", true, &read_out, 3);
    let (edit_path, old, new, ctx) = (
        fx.path("edit"),
        fx.word("old"),
        fx.url("new"),
        fx.wide("ctx"),
    );
    b.tool_call(
        "main",
        "t2",
        "edit",
        json!({"path": edit_path, "old_string": old, "new_string": new}),
    );
    b.file_changed(
        "main",
        &edit_path,
        FileChangeKind::Modified,
        Some(&format!(
            "--- a{edit_path}\n+++ b{edit_path}\n@@ -1,2 +1,2 @@\n-{old}\n+{new}\n \t{ctx}\n"
        )),
    );
    b.tool_result("main", "t2", true, "ok", 3);
    let (cmd_word, cmd_path) = (fx.word("cmd"), fx.path("cmd"));
    b.tool_call(
        "main",
        "t3",
        "bash",
        json!({"command": format!("cargo test {cmd_word} --manifest-path {cmd_path}")}),
    );
    let bash_out = fx.prose("bashout");
    b.tool_result(
        "main",
        "t3",
        false,
        &format!("{bash_out}\nexit code 101"),
        8_100,
    );
    let (goal, todo1, todo2, todo3) = (
        fx.word("goal"),
        fx.path("todo1"),
        fx.wide("todo2"),
        fx.url("todo3"),
    );
    b.tool_call(
        "main",
        "todo1",
        "todo",
        json!({"goal": goal, "todos": [
            {"content": todo1, "status": "completed"},
            {"content": todo2, "status": "in_progress"},
            {"content": todo3, "status": "pending"},
        ]}),
    );
    b.tool_result("main", "todo1", true, "ok", 1);
    let (sub, host) = (fx.word("sub"), fx.word("host"));
    b.start(
        NodeSpec::subagent("r1", "main")
            .name(&sub)
            .model("haiku")
            .host(&host),
    );
    let sub_path = fx.path("subread");
    b.tool_call("r1", "t4", "read", json!({"path": sub_path}));
    b.tool_result("r1", "t4", true, "ok", 2);
    let (verify_cmd, verify_out) = (fx.word("verify"), fx.prose("verifyout"));
    b.verify(
        "main",
        &format!("cargo test {verify_cmd}"),
        false,
        &verify_out,
        1_000,
    );
    (b.into_events(), fx)
}

fn is_tl(c: char) -> bool {
    matches!(c, '╭' | '┌' | '╔' | '┏')
}
fn is_tr(c: char) -> bool {
    matches!(c, '╮' | '┐' | '╗' | '┓')
}
fn is_bl(c: char) -> bool {
    matches!(c, '╰' | '└' | '╚' | '┗')
}
fn is_br(c: char) -> bool {
    matches!(c, '╯' | '┘' | '╝' | '┛')
}
fn is_side(c: char) -> bool {
    matches!(
        c,
        '│' | '┃'
            | '║'
            | '├'
            | '┤'
            | '┣'
            | '┫'
            | '╟'
            | '╢'
            | '▐'
            | '▌'
            | '█'
            | '░'
            | '▒'
            | '▓'
    )
}

/// A glyph class test.
type Glyph = fn(char) -> bool;

fn cell(buf: &Buffer, x: u16, y: u16) -> char {
    buf[(x, y)].symbol().chars().next().unwrap_or(' ')
}

/// Checks every box in `area` of `buf`: from each top-left corner, the
/// top row reaches a top-right corner on the same row; below it both
/// sides stay borders until the bottom corners (or until both end
/// together, where the box is cut by a scrolled region). Bottom-left
/// corners are checked upwards the same way. Returns the problems found.
pub fn check_borders(buf: &Buffer, area: Rect) -> Vec<String> {
    let mut out = Vec::new();
    let right = area.right();
    for y in area.top()..area.bottom() {
        for x in area.left()..right {
            let c = cell(buf, x, y);
            let (top, corner_ok, end_l, end_r): (bool, Glyph, Glyph, Glyph) = if is_tl(c) {
                (true, is_tr, is_bl, is_br)
            } else if is_bl(c) {
                (false, is_br, is_tl, is_tr)
            } else {
                continue;
            };
            let Some(x2) = (x + 1..right).find(|&xx| {
                let d = cell(buf, xx, y);
                corner_ok(d) || is_tl(d) || is_bl(d)
            }) else {
                out.push(format!(
                    "({x},{y}) `{c}`: no matching corner on the row (box runs past the edge)"
                ));
                continue;
            };
            if !corner_ok(cell(buf, x2, y)) {
                out.push(format!(
                    "({x},{y}) `{c}`: the row has another box corner before its own"
                ));
                continue;
            }
            let rows: Box<dyn Iterator<Item = u16>> = if top {
                Box::new(y + 1..area.bottom())
            } else {
                Box::new((area.top()..y).rev())
            };
            for r in rows {
                let (l, rr) = (cell(buf, x, r), cell(buf, x2, r));
                if end_l(l) && end_r(rr) {
                    break;
                }
                let (ls, rs) = (is_side(l) || end_l(l), is_side(rr) || end_r(rr));
                if ls && rs {
                    continue;
                }
                if !ls && !rs {
                    break;
                }
                out.push(format!(
                    "box ({x},{y})-({x2},{y}): row {r} has `{l}` on the left and `{rr}` on the right border"
                ));
                break;
            }
        }
    }
    out
}

/// The text of `area` in `buf`, one line per row. The cell hidden behind
/// a wide character is skipped, as a terminal shows it.
pub fn text_in(buf: &Buffer, area: Rect) -> String {
    let area = area.intersection(buf.area);
    (area.top()..area.bottom())
        .map(|y| {
            let mut row = String::new();
            let mut x = area.left();
            while x < area.right() {
                let sym = buf[(x, y)].symbol();
                row.push_str(sym);
                x += (crate::chat::text::width(sym) as u16).max(1);
            }
            row
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The whole screen of `buf` ([`text_in`]).
pub fn screen(buf: &Buffer) -> String {
    text_in(buf, buf.area)
}

/// Draws one frame with `render` and returns exactly what was drawn (the
/// completed frame, not the backend's diffed cells).
pub fn frame<B: ratatui::backend::Backend>(
    term: &mut ratatui::Terminal<B>,
    render: impl FnOnce(&mut ratatui::Frame<'_>),
) -> Buffer {
    term.draw(render).expect("draw").buffer.clone()
}

/// Asserts the frame is clean ([`assert_clean`]) and returns its text.
pub fn checked(what: &str, buf: &Buffer) -> String {
    assert_clean(what, buf, buf.area);
    screen(buf)
}

/// `s` without whitespace, box drawing and scrollbar glyphs: wrapped text
/// reads as one run, so a token wrapped over rows still matches.
pub fn squash(s: &str) -> String {
    s.chars()
        .filter(|c| {
            !c.is_whitespace() && !('\u{2500}'..='\u{259f}').contains(c) && *c != '\u{200b}'
        })
        .collect()
}

/// Whether `token` shows in `frame`, possibly wrapped: it starts in some
/// row and each following part starts the next row, after chrome such as
/// borders, gutters and indents ([`squash`]). Anything may follow a part
/// on its row (a right-aligned timestamp, a pane beside it); a cut part
/// (`…`) fails because the next row does not continue the token.
pub fn shows(frame: &str, token: &str) -> bool {
    let rows: Vec<Vec<char>> = frame.lines().map(|l| squash(l).chars().collect()).collect();
    let want: Vec<char> = squash(&crate::chat::text::sanitize(token))
        .chars()
        .collect();
    let common = |a: &[char], b: &[char]| a.iter().zip(b).take_while(|(x, y)| x == y).count();
    // The rest of the token continues at the start of row `r`.
    fn rest(
        rows: &[Vec<char>],
        r: usize,
        want: &[char],
        common: &dyn Fn(&[char], &[char]) -> usize,
    ) -> bool {
        let Some(row) = rows.get(r) else { return false };
        let n = common(row, want);
        n == want.len() || (n > 0 && rest(rows, r + 1, &want[n..], common))
    }
    rows.iter().enumerate().any(|(r, row)| {
        (0..row.len()).any(|i| {
            let n = common(&row[i..], &want);
            n == want.len() || (n > 0 && rest(&rows, r + 1, &want[n..], &common))
        })
    })
}

/// Asserts every token shows ([`shows`]) in one of `frames`.
pub fn assert_reachable(what: &str, frames: &[String], tokens: &[&str]) {
    let missing: Vec<&str> = tokens
        .iter()
        .copied()
        .filter(|t| !frames.iter().any(|f| shows(f, t)))
        .collect();
    assert!(
        missing.is_empty(),
        "{what}: neither visible nor reachable:\n  {}\nframes:\n{}",
        missing.join("\n  "),
        frames.join("\n=====\n")
    );
}

/// Asserts no border problem and no raw control character in `buf`.
pub fn assert_clean(what: &str, buf: &Buffer, area: Rect) {
    let problems = check_borders(buf, area);
    let text = text_in(buf, buf.area);
    assert!(
        problems.is_empty(),
        "{what}: content on or past a border:\n{}\n{text}",
        problems.join("\n")
    );
    assert!(
        !text.chars().any(|c| c.is_control() && c != '\n'),
        "{what}: a control character reached the screen:\n{text:?}"
    );
}

/// Runs `case` at every size in [`SIZES`] (and [`TINY`], where only a
/// panic fails), reporting the size of a failure.
pub fn matrix(name: &str, case: impl Fn(u16, u16, bool)) {
    for (w, h) in TINY {
        if let Err(e) = catch_unwind(AssertUnwindSafe(|| case(w, h, false))) {
            panic!("{name} panicked at {w}x{h}: {}", panic_text(&e));
        }
    }
    for (w, h) in SIZES {
        if let Err(e) = catch_unwind(AssertUnwindSafe(|| case(w, h, true))) {
            panic!("{name} at {w}x{h}: {}", panic_text(&e));
        }
    }
}

fn panic_text(e: &Box<dyn std::any::Any + Send>) -> String {
    e.downcast_ref::<String>()
        .cloned()
        .or_else(|| e.downcast_ref::<&str>().map(|s| (*s).to_owned()))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::widgets::{Block, BorderType, Borders, Paragraph};

    #[test]
    fn border_check_finds_overdrawn_sides_and_cut_boxes() {
        let mut t = Terminal::new(TestBackend::new(20, 6)).unwrap();
        t.draw(|f| {
            f.render_widget(
                Block::new()
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded),
                Rect::new(0, 0, 10, 4),
            );
        })
        .unwrap();
        assert!(check_borders(t.backend().buffer(), Rect::new(0, 0, 20, 6)).is_empty());
        // Text drawn over the right border.
        t.draw(|f| {
            f.render_widget(
                Block::new()
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded),
                Rect::new(0, 0, 10, 4),
            );
            f.render_widget(Paragraph::new("overflowing"), Rect::new(1, 1, 11, 1));
        })
        .unwrap();
        assert!(!check_borders(t.backend().buffer(), Rect::new(0, 0, 20, 6)).is_empty());
        // A box running past the screen edge.
        t.draw(|f| {
            f.render_widget(Paragraph::new("╭──────────"), Rect::new(10, 0, 10, 1));
        })
        .unwrap();
        assert!(!check_borders(t.backend().buffer(), Rect::new(0, 0, 20, 6)).is_empty());
    }

    #[test]
    fn tokens_match_across_wrapped_rows() {
        let frame = "│ x abc  12:00 │\n│ def  │\n│ gh… │";
        assert!(shows(frame, "abcdef"));
        assert!(shows(frame, "abcdefgh"));
        assert!(!shows(frame, "abcdefghi"));
        assert_eq!(squash("a\tb"), "ab");
    }
}
