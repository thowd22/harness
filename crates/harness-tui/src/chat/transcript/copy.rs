//! Copy targets and the keyboard selection mode of the transcript.
//!
//! A [`CopyTarget`] names something with a copy button: a message, a code
//! block of an assistant turn, or a tool card's output or diff.
//! [`Transcript::copy_text`] returns its plain text, and
//! [`Transcript::target_for`] maps a focused element to its target (so the
//! `copy` key copies whatever has focus).
//!
//! Selection mode keeps a line cursor over the rendered transcript
//! ([`Selection`]); positions are `(entry index, line)` so they survive
//! scrolling and new output. A mark fixes one end of the range, and
//! [`Transcript::selected_text`] returns the range through
//! [`crate::clipboard::plain_line`].

use ratatui::style::{Modifier, Style};

use super::{EntryKind, Frame, Transcript, code_blocks};
use crate::cards::CardCopy;
use crate::clipboard::plain_line;

/// Something the transcript can copy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CopyTarget {
    /// A user or assistant message (entry id); the raw text, Markdown as
    /// written.
    Message(u64),
    /// Code block `block` of the assistant turn `entry`.
    Code {
        /// Entry id.
        entry: u64,
        /// Block index in the turn.
        block: usize,
    },
    /// A tool card's output or diff.
    Card {
        /// Tool call id.
        call_id: String,
        /// Output or diff.
        kind: CardCopy,
    },
    /// A tool card's output with `read` line numbers kept.
    NumberedOutput(String),
}

/// The keyboard selection: a cursor line and an optional mark.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Selection {
    /// Cursor position `(entry index, line)`.
    pub cursor: (usize, usize),
    /// The other end of the range, once marked.
    pub mark: Option<(usize, usize)>,
}

impl Selection {
    /// First and last selected positions (the cursor alone without a mark).
    pub fn range(&self) -> ((usize, usize), (usize, usize)) {
        let m = self.mark.unwrap_or(self.cursor);
        (m.min(self.cursor), m.max(self.cursor))
    }
}

fn line_text(l: &ratatui::text::Line<'_>) -> String {
    l.spans.iter().map(|s| s.content.as_ref()).collect()
}

impl Transcript {
    fn entry_index(&self, id: u64) -> Option<usize> {
        self.entries.iter().position(|e| e.id == id)
    }

    /// The text of `target` and what it is (`message`, `code block`,
    /// `tool output`, `diff`). `None` when it is gone or empty.
    pub fn copy_text(&self, target: &CopyTarget) -> Option<(String, String)> {
        let (what, text) = match target {
            CopyTarget::Message(id) => match &self.entries[self.entry_index(*id)?].kind {
                EntryKind::User { text } => ("message", text.clone()),
                EntryKind::Assistant(t) => ("message", t.text.clone()),
                EntryKind::Subagent(s) => ("subagent summary", s.summary.clone()),
                EntryKind::Notice(n) => {
                    let mut t = n.text.clone();
                    for d in &n.detail {
                        t.push('\n');
                        t.push_str(d);
                    }
                    ("notice", t)
                }
                _ => return None,
            },
            CopyTarget::Code { entry, block } => {
                match &self.entries[self.entry_index(*entry)?].kind {
                    EntryKind::Assistant(t) => {
                        ("code block", code_blocks(&t.text).get(*block)?.clone())
                    }
                    _ => return None,
                }
            }
            CopyTarget::Card { call_id, kind } => {
                let card = self.card(call_id)?;
                let text = match kind {
                    CardCopy::Output => card.output_text(false),
                    CardCopy::Diff => card.diff_text()?,
                };
                (kind.what(), text)
            }
            CopyTarget::NumberedOutput(call_id) => {
                ("tool output", self.card(call_id)?.output_text(true))
            }
        };
        (!text.trim().is_empty()).then(|| (what.to_owned(), text))
    }

    /// The copy target of a focused element: a copy button, a card (its
    /// diff, else its output), a reasoning toggle (the message) or a
    /// subagent card (its summary).
    pub fn target_for(&self, element: &str) -> Option<CopyTarget> {
        if let Some(n) = element.strip_prefix("copy.msg.") {
            return n.parse().ok().map(CopyTarget::Message);
        }
        if let Some(rest) = element.strip_prefix("copy.code.") {
            let (e, b) = rest.split_once('.')?;
            return Some(CopyTarget::Code {
                entry: e.parse().ok()?,
                block: b.parse().ok()?,
            });
        }
        let card = |call_id: &str, kind| CopyTarget::Card {
            call_id: call_id.to_owned(),
            kind,
        };
        if let Some(c) = element.strip_prefix("copy.out.") {
            return Some(card(c, CardCopy::Output));
        }
        if let Some(c) = element.strip_prefix("copy.diff.") {
            return Some(card(c, CardCopy::Diff));
        }
        if let Some(c) = element.strip_prefix("card.") {
            let kind = if self.card(c)?.diff_text().is_some() {
                CardCopy::Diff
            } else {
                CardCopy::Output
            };
            return Some(card(c, kind));
        }
        if let Some(n) = element.strip_prefix("thinking.") {
            return n.parse().ok().map(CopyTarget::Message);
        }
        if let Some(node) = element.strip_prefix("subagent.") {
            let i = self
                .subagents
                .iter()
                .find(|(k, _)| k.as_str() == node)
                .map(|(_, i)| *i)?;
            return Some(CopyTarget::Message(self.entries[i].id));
        }
        None
    }

    /// The latest assistant message with text, else the latest user
    /// message.
    pub fn last_message(&self) -> Option<CopyTarget> {
        let find = |assistant: bool| {
            self.entries.iter().rev().find_map(|e| match &e.kind {
                EntryKind::Assistant(t) if assistant && !t.text.trim().is_empty() => {
                    Some(CopyTarget::Message(e.id))
                }
                EntryKind::User { .. } if !assistant => Some(CopyTarget::Message(e.id)),
                _ => None,
            })
        };
        find(true).or_else(|| find(false))
    }

    /// The last code block of the latest assistant turn that has one.
    pub fn last_code(&self) -> Option<CopyTarget> {
        self.entries.iter().rev().find_map(|e| match &e.kind {
            EntryKind::Assistant(t) => {
                let n = code_blocks(&t.text).len();
                (n > 0).then(|| CopyTarget::Code {
                    entry: e.id,
                    block: n - 1,
                })
            }
            _ => None,
        })
    }

    /// The latest tool card with an output (`Output`) or a diff (`Diff`).
    pub fn last_card(&self, kind: CardCopy) -> Option<CopyTarget> {
        self.entries.iter().rev().find_map(|e| match &e.kind {
            EntryKind::Tools { group, .. } => group.cards.iter().rev().find_map(|c| {
                c.copy_kinds().contains(&kind).then(|| CopyTarget::Card {
                    call_id: c.call_id.clone(),
                    kind,
                })
            }),
            _ => None,
        })
    }

    // ---- selection mode ----

    /// The active selection.
    pub fn selection(&self) -> Option<&Selection> {
        self.selection.as_ref()
    }

    /// Moves `pos` by `delta` rendered lines, clamped to the transcript.
    fn step(&mut self, pos: (usize, usize), delta: i64, f: &Frame<'_>) -> (usize, usize) {
        let width = self.last_width.max(1);
        let (mut i, mut off) = pos;
        if delta < 0 {
            let mut up = delta.unsigned_abs() as usize;
            while up > 0 {
                if off >= up {
                    off -= up;
                    up = 0;
                } else if i == 0 {
                    off = 0;
                    up = 0;
                } else {
                    up -= off + 1;
                    i -= 1;
                    off = self.height(i, width, f).saturating_sub(1);
                }
            }
        } else {
            let mut down = delta as usize;
            while down > 0 {
                let h = self.height(i, width, f);
                if off + down < h {
                    off += down;
                    down = 0;
                } else if i + 1 >= self.entries.len() {
                    off = h.saturating_sub(1);
                    down = 0;
                } else {
                    down -= h - off;
                    i += 1;
                    off = 0;
                }
            }
        }
        (i, off)
    }

    fn viewport_top(&mut self, f: &Frame<'_>) -> (usize, usize) {
        match self.anchor {
            Some(a) => a,
            None => self.tail_top(self.last_width.max(1), self.last_height.max(1), f),
        }
    }

    /// Scrolls so that `pos` is visible.
    fn reveal(&mut self, pos: (usize, usize), f: &Frame<'_>) {
        let height = self.last_height.max(1) as i64;
        let top = self.viewport_top(f);
        let new_top = if pos < top {
            pos
        } else if self.step(top, height - 1, f) < pos {
            self.step(pos, -(height - 1), f)
        } else {
            return;
        };
        self.pin(new_top, self.last_width.max(1), self.last_height.max(1), f);
    }

    /// Starts selection mode with the cursor on the bottom visible line.
    /// Returns false when there is nothing to select.
    pub fn select_start(&mut self, f: &Frame<'_>) -> bool {
        if self.entries.is_empty() {
            return false;
        }
        let top = self.viewport_top(f);
        let cursor = self.step(top, self.last_height.max(1) as i64 - 1, f);
        self.selection = Some(Selection { cursor, mark: None });
        true
    }

    /// Leaves selection mode.
    pub fn select_stop(&mut self) {
        self.selection = None;
    }

    /// Moves the cursor by `delta` lines (negative is up).
    pub fn select_move(&mut self, delta: i64, f: &Frame<'_>) {
        let Some(sel) = self.selection else {
            return;
        };
        let cursor = self.step(sel.cursor, delta, f);
        self.selection = Some(Selection { cursor, ..sel });
        self.reveal(cursor, f);
    }

    /// Moves the cursor to the first (`end` false) or last line.
    pub fn select_edge(&mut self, end: bool, f: &Frame<'_>) {
        let Some(sel) = self.selection else {
            return;
        };
        let cursor = if end {
            let last = self.entries.len() - 1;
            let h = self.height(last, self.last_width.max(1), f);
            (last, h.saturating_sub(1))
        } else {
            (0, 0)
        };
        self.selection = Some(Selection { cursor, ..sel });
        self.reveal(cursor, f);
    }

    /// Moves the cursor by a page.
    pub fn select_page(&mut self, down: bool, f: &Frame<'_>) {
        let page = self.last_height.max(2) as i64 - 1;
        self.select_move(if down { page } else { -page }, f);
    }

    /// Sets the mark at the cursor, or clears it.
    pub fn select_mark(&mut self) {
        if let Some(sel) = &mut self.selection {
            sel.mark = match sel.mark {
                Some(_) => None,
                None => Some(sel.cursor),
            };
        }
    }

    /// Puts the cursor on viewport row `row` (a click). With a mark set,
    /// this extends the range. Rows in the padding above a short transcript
    /// select its first line.
    pub fn select_row(&mut self, row: usize, f: &Frame<'_>) {
        let Some(sel) = self.selection else {
            return;
        };
        let top = self.viewport_top(f);
        let cursor = self.step(top, row.saturating_sub(self.last_pad) as i64, f);
        self.selection = Some(Selection { cursor, ..sel });
    }

    /// The selected lines as plain text (decoration lines dropped) and the
    /// number of rendered lines in the range.
    pub fn selected_text(&mut self, f: &Frame<'_>) -> Option<(String, usize)> {
        let sel = self.selection?;
        let (lo, hi) = sel.range();
        let width = self.last_width.max(1);
        let mut out: Vec<String> = Vec::new();
        let mut pos = lo;
        let mut count = 0;
        loop {
            let (i, off) = pos;
            if i >= self.entries.len() {
                break;
            }
            self.layout_entry(i, width, f);
            let layout = self.entries[i].layout.as_ref().expect("laid out");
            if let Some(l) = layout.lines.get(off) {
                count += 1;
                if let Some(t) = plain_line(&line_text(l)) {
                    out.push(t);
                }
            }
            if pos >= hi {
                break;
            }
            pos = if off + 1 < layout.lines.len() {
                (i, off + 1)
            } else {
                (i + 1, 0)
            };
        }
        while out.last().is_some_and(|l| l.trim().is_empty()) {
            out.pop();
        }
        while out.first().is_some_and(|l| l.trim().is_empty()) {
            out.remove(0);
        }
        Some((out.join("\n"), count))
    }

    /// Highlight of a rendered line at `pos` in selection mode.
    pub(super) fn selection_style(&self, pos: (usize, usize), f: &Frame<'_>) -> Option<Style> {
        let sel = self.selection.as_ref()?;
        let (lo, hi) = sel.range();
        if pos < lo || pos > hi {
            return None;
        }
        let st = f.theme.focused();
        Some(if pos == sel.cursor {
            st.add_modifier(Modifier::BOLD | Modifier::UNDERLINED)
        } else {
            st
        })
    }
}
