//! Tool call cards.
//!
//! One [`ToolCard`] per tool call, laid out as a bordered card:
//!
//! ```text
//! ╭ ✔ edit  src/parse.rs  +3 −1 ─────────────────────── 3ms · 14:32:09 ╮
//! │  41 │-      .unwrap();                                              │
//! │  41 │+      .ok_or(ParseError::MissingColon)?;                      │
//! ╰──────────────────────────────────────────────────────────────────────╯
//! ```
//!
//! The header has the status icon, tool name, argument summary and badges,
//! and on the right the duration and timestamp. The card is tinted by its
//! [`CardStatus`] (`card.pending`, `card.ok`, `card.error`,
//! `card.approval`). Bodies are tool specific: `read` shows numbered lines,
//! `write` the new content, `edit` a syntax-coloured unified diff with line
//! numbers, `bash` the command output (streamed live while running) with
//! exit / timeout / killed badges and a pointer to spilled output. Bodies
//! longer than `[ui] tool_preview_lines` are collapsed with a count of hidden
//! lines; [`ToolCard::toggle`] (Enter or a click in the chat) expands them.
//! Errors always show the full message the model received. `todo`,
//! `memory` and `spawn_agent` render as compact one-line summaries.
//! MCP calls ([`ToolCard::mcp`], from `mcp_call` events) show
//! `mcp <server> › <tool>` instead of the name offered to the model, with
//! the arguments sent to the server and its output.
//! Parallel calls from one model turn are grouped by [`CardGroup`] under one
//! header with a count.
//!
//! With [`CardCtx::copy_buttons`] the bottom border carries `⧉ copy output`
//! and `⧉ copy diff` buttons ([`CardButton`]); [`ToolCard::output_text`] and
//! [`ToolCard::diff_text`] are what they copy: plain text without borders,
//! highlighting or (unless asked for) the line numbers of `read`.

use harness_events::schema::{FileChangeKind, Payload};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use serde_json::Value;
use time::OffsetDateTime;

use crate::chat::highlight::{self, State};
use crate::chat::text::{
    fmt_bytes, fmt_ms, line_width, right_align, sanitize, take_wrap_marker, width as width_of,
    wrap_line, wrap_right,
};
use crate::chat::timefmt::TimeFormat;
use crate::clipboard::{COPY_DIFF_LABEL, COPY_OUTPUT_LABEL};
use crate::indicators::{PAUSE_ICON, Spinner};
use crate::theme::{Theme, Token};

/// Tools rendered as compact one-line summaries.
pub const COMPACT_TOOLS: [&str; 3] = ["todo", "memory", "spawn_agent"];

/// State of a tool call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CardStatus {
    /// Announced, not started.
    Pending,
    /// Running.
    Running,
    /// Finished successfully.
    Ok,
    /// Failed; the output is the error message given to the model.
    Error,
    /// Waiting for the user's approval.
    Approval,
}

impl CardStatus {
    /// Header icon.
    pub fn icon(self, spinner: &Spinner) -> char {
        match self {
            CardStatus::Pending => '○',
            CardStatus::Running => spinner.glyph(),
            CardStatus::Ok => '✔',
            CardStatus::Error => '✗',
            CardStatus::Approval => PAUSE_ICON,
        }
    }

    /// Colour of the icon.
    pub fn token(self) -> Token {
        match self {
            CardStatus::Pending => Token::Muted,
            CardStatus::Running => Token::Running,
            CardStatus::Ok => Token::Ok,
            CardStatus::Error => Token::Error,
            CardStatus::Approval => Token::Approval,
        }
    }

    /// Card background tint.
    pub fn tint(self) -> Token {
        match self {
            CardStatus::Pending | CardStatus::Running => Token::CardPending,
            CardStatus::Ok => Token::CardOk,
            CardStatus::Error => Token::CardError,
            CardStatus::Approval => Token::CardApproval,
        }
    }

    /// Whether the call has finished.
    pub fn is_finished(self) -> bool {
        matches!(self, CardStatus::Ok | CardStatus::Error)
    }
}

/// Why a card is waiting for approval.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApprovalNote {
    /// The matching rule, e.g. `outward.ask = git push*`.
    pub rule: String,
    /// Short reason, e.g. `outward action`.
    pub reason: String,
}

/// A file change recorded for the call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CardChange {
    /// Created, modified or deleted.
    pub kind: FileChangeKind,
    /// Unified diff, when known.
    pub diff: Option<String>,
}

/// Everything a card needs to lay itself out.
#[derive(Debug, Clone, Copy)]
pub struct CardCtx<'a> {
    /// Colours.
    pub theme: &'a Theme,
    /// Width in cells.
    pub width: usize,
    /// Timestamp style.
    pub time: TimeFormat,
    /// Now, for relative timestamps.
    pub now: OffsetDateTime,
    /// `[ui] tool_preview_lines`.
    pub preview_lines: usize,
    /// Spinner for running cards.
    pub spinner: Spinner,
    /// Whether the card has keyboard focus.
    pub focused: bool,
    /// Draw copy buttons on the bottom border.
    pub copy_buttons: bool,
    /// The focused element id (highlights a focused copy button).
    pub focus: Option<&'a str>,
}

/// What a card's copy button copies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CardCopy {
    /// The tool output ([`ToolCard::output_text`]).
    Output,
    /// The file change as a unified diff ([`ToolCard::diff_text`]).
    Diff,
}

impl CardCopy {
    /// Button label.
    pub fn label(self) -> &'static str {
        match self {
            CardCopy::Output => COPY_OUTPUT_LABEL,
            CardCopy::Diff => COPY_DIFF_LABEL,
        }
    }

    /// What it copies, for toasts (`tool output`, `diff`).
    pub fn what(self) -> &'static str {
        match self {
            CardCopy::Output => "tool output",
            CardCopy::Diff => "diff",
        }
    }
}

/// Element id of a card's copy button.
pub fn copy_button_id(call_id: &str, kind: CardCopy) -> String {
    match kind {
        CardCopy::Output => format!("copy.out.{call_id}"),
        CardCopy::Diff => format!("copy.diff.{call_id}"),
    }
}

/// A copy button inside a rendered card.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CardButton {
    /// What it copies.
    pub kind: CardCopy,
    /// Line in the card's (or group's) lines.
    pub line: usize,
    /// First column.
    pub col: usize,
    /// Width in cells.
    pub width: usize,
}

/// The MCP server tool behind a call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpLabel {
    /// `[mcp.<server>]` name.
    pub server: String,
    /// The tool's name on the server.
    pub tool: String,
}

/// One tool call.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolCard {
    /// Tool call id.
    pub call_id: String,
    /// Tool name.
    pub tool: String,
    /// Arguments.
    pub args: Value,
    /// State.
    pub status: CardStatus,
    /// When the call was made (from the event log).
    pub ts: OffsetDateTime,
    /// Duration once finished.
    pub duration_ms: Option<u64>,
    /// Output so far (streamed) or the final output.
    pub output: String,
    /// The file change, for write and edit.
    pub change: Option<CardChange>,
    /// Why it waits for approval.
    pub approval: Option<ApprovalNote>,
    /// Expanded past the preview.
    pub expanded: bool,
    /// The MCP server tool, for MCP calls.
    pub mcp: Option<McpLabel>,
    version: u64,
}

impl ToolCard {
    /// A running card for a new call.
    pub fn new(
        call_id: impl Into<String>,
        tool: impl Into<String>,
        args: Value,
        ts: OffsetDateTime,
    ) -> ToolCard {
        ToolCard {
            call_id: call_id.into(),
            tool: tool.into(),
            args,
            status: CardStatus::Running,
            ts,
            duration_ms: None,
            output: String::new(),
            change: None,
            approval: None,
            expanded: false,
            mcp: None,
            version: 0,
        }
    }

    /// Marks the card as a call of `tool` on MCP server `server`.
    pub fn set_mcp(&mut self, server: &str, tool: &str) {
        self.mcp = Some(McpLabel {
            server: server.to_owned(),
            tool: tool.to_owned(),
        });
        self.bump();
    }

    /// The name shown in the header: `mcp <server> › <tool>` for MCP calls,
    /// else the tool name.
    pub fn display_name(&self) -> String {
        match &self.mcp {
            Some(m) => format!("mcp {} › {}", m.server, m.tool),
            None => self.tool.clone(),
        }
    }

    /// The header name as styled spans.
    fn name_spans(&self, theme: &Theme) -> Vec<Span<'static>> {
        let accent = theme.fg(Token::Accent).add_modifier(Modifier::BOLD);
        match &self.mcp {
            Some(m) => vec![
                Span::styled("mcp ", theme.fg(Token::Muted)),
                Span::styled(m.server.clone(), theme.fg(Token::Agent)),
                Span::styled(" › ", theme.fg(Token::Muted)),
                Span::styled(m.tool.clone(), accent),
            ],
            None => vec![Span::styled(self.tool.clone(), accent)],
        }
    }

    /// Changes whenever the card's content changes (for render caches).
    pub fn version(&self) -> u64 {
        self.version
    }

    fn bump(&mut self) {
        self.version += 1;
    }

    /// Appends live output (bash streaming).
    pub fn push_output(&mut self, chunk: &str) {
        self.output.push_str(chunk);
        self.bump();
    }

    /// Records the result.
    pub fn finish(&mut self, ok: bool, output: String, duration_ms: u64) {
        self.status = if ok {
            CardStatus::Ok
        } else {
            CardStatus::Error
        };
        self.output = output;
        self.duration_ms = Some(duration_ms);
        self.approval = None;
        self.bump();
    }

    /// Records the file change of a write or edit.
    pub fn set_change(&mut self, kind: FileChangeKind, diff: Option<String>) {
        self.change = Some(CardChange { kind, diff });
        self.bump();
    }

    /// Marks the card as waiting for approval (`Some`) or running again.
    pub fn set_approval(&mut self, note: Option<ApprovalNote>) {
        match note {
            Some(n) => {
                self.status = CardStatus::Approval;
                self.approval = Some(n);
            }
            None => {
                if self.status == CardStatus::Approval {
                    self.status = CardStatus::Running;
                }
                self.approval = None;
            }
        }
        self.bump();
    }

    /// Expands or collapses the card.
    pub fn toggle(&mut self) {
        self.expanded = !self.expanded;
        self.bump();
    }

    /// Whether this tool renders as a compact one-line summary.
    pub fn is_compact(&self) -> bool {
        COMPACT_TOOLS.contains(&self.tool.as_str()) && self.status != CardStatus::Error
    }

    fn path(&self) -> String {
        str_arg(&self.args, &["path", "file_path", "file"]).unwrap_or_default()
    }

    /// Header summary after the tool name, e.g. `src/parse.rs` or `$ cargo test`.
    /// For an MCP call through `mcp_tools`, the arguments sent to the server.
    pub fn summary(&self) -> String {
        match (&self.mcp, self.args.get("call"), self.args.get("arguments")) {
            (Some(_), Some(_), Some(a)) => arg_summary("", a),
            (Some(_), Some(_), None) => String::new(),
            _ => arg_summary(&self.tool, &self.args),
        }
    }

    /// Header badges (left side, after the summary).
    fn badges(&self, theme: &Theme) -> Vec<Span<'static>> {
        let mut out = Vec::new();
        let muted = theme.fg(Token::Muted);
        match self.tool.as_str() {
            "read" => {
                if let Some((from, to, total)) = read_range(&self.args, &self.output) {
                    out.push(Span::styled(
                        format!("  L{from}-{to}"),
                        theme.fg(Token::Number),
                    ));
                    out.push(Span::styled(format!(" · {} lines", to + 1 - from), muted));
                    if to < total {
                        out.push(Span::styled(format!(" of {total}"), muted));
                        out.push(Span::styled("  truncated", theme.fg(Token::Warn)));
                    }
                }
            }
            "write" => {
                let bytes = str_arg(&self.args, &["content"]).map_or(0, |c| c.len());
                out.push(Span::styled(
                    format!("  {}", fmt_bytes(bytes as u64)),
                    theme.fg(Token::Number),
                ));
                if self.is_new_file() {
                    out.push(Span::styled("  new file", theme.fg(Token::Ok)));
                }
            }
            "edit" => {
                let rows = self.diff_rows();
                let add = rows.iter().filter(|r| r.kind == DiffKind::Add).count();
                let del = rows.iter().filter(|r| r.kind == DiffKind::Del).count();
                if add + del > 0 {
                    out.push(Span::styled(
                        format!("  +{add}"),
                        theme.fg(Token::DiffAdded),
                    ));
                    out.push(Span::styled(
                        format!(" −{del}"),
                        theme.fg(Token::DiffRemoved),
                    ));
                }
            }
            _ => {}
        }
        out
    }

    /// Whether a write created the file.
    pub fn is_new_file(&self) -> bool {
        match &self.change {
            Some(c) => c.kind == FileChangeKind::Created,
            None => self.output.starts_with("Created"),
        }
    }

    /// Right-side badges (bash exit status, approval reason).
    fn right_badges(&self, theme: &Theme) -> Vec<Span<'static>> {
        let mut out = Vec::new();
        if let Some(n) = &self.approval {
            out.push(Span::styled(n.reason.clone(), theme.fg(Token::Approval)));
            out.push(Span::styled(" · ", theme.fg(Token::Muted)));
        }
        if self.tool == "bash" && self.status.is_finished() {
            let exit = bash_exit(&self.output);
            let badge = match exit {
                BashExit::Code(c) => Some((format!("exit {c}"), Token::Error)),
                BashExit::Timeout => Some(("timeout".to_owned(), Token::Error)),
                BashExit::Killed => Some(("killed".to_owned(), Token::Warn)),
                BashExit::Signal(s) => Some((format!("signal {s}"), Token::Error)),
                BashExit::Ok => None,
            };
            if let Some((text, token)) = badge {
                out.push(Span::styled(text, theme.fg(token)));
                out.push(Span::styled(" · ", theme.fg(Token::Muted)));
            }
        }
        out
    }

    /// The copy buttons this card offers: output once there is some, diff
    /// for edits and writes with a recorded change.
    pub fn copy_kinds(&self) -> Vec<CardCopy> {
        let mut v = Vec::new();
        if !self.output_text(false).trim().is_empty() {
            v.push(CardCopy::Output);
        }
        if self.diff_text().is_some() {
            v.push(CardCopy::Diff);
        }
        v
    }

    /// The output as plain text: the `<tool_result>` wrapper and `read`'s
    /// range note removed, and `read`'s line numbers removed unless
    /// `numbered`.
    pub fn output_text(&self, numbered: bool) -> String {
        let mut lines: Vec<&str> = self.output.lines().collect();
        if lines
            .first()
            .is_some_and(|l| l.starts_with("<tool_result") && l.ends_with('>'))
        {
            lines.remove(0);
            if lines.last().is_some_and(|l| l.trim() == "</tool_result>") {
                lines.pop();
            }
        }
        if self.tool != "read" || self.status == CardStatus::Error {
            return lines.join("\n");
        }
        lines
            .into_iter()
            .filter(|l| !l.starts_with("[Showing lines"))
            .map(|l| match l.split_once('\t') {
                Some((num, text)) if !numbered && num.trim().parse::<u64>().is_ok() => {
                    text.to_owned()
                }
                _ => l.to_owned(),
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// The change as a unified diff: the recorded diff, else (for edits)
    /// one computed from the arguments.
    pub fn diff_text(&self) -> Option<String> {
        if let Some(d) = self.change.as_ref().and_then(|c| c.diff.clone()) {
            return (!d.trim().is_empty()).then_some(d);
        }
        if self.tool != "edit" {
            return None;
        }
        let old = str_arg(&self.args, &["old_string"]).unwrap_or_default();
        let new = str_arg(&self.args, &["new_string"]).unwrap_or_default();
        if old.is_empty() && new.is_empty() {
            return None;
        }
        let path = self.path();
        Some(
            similar::TextDiff::from_lines(old.as_str(), new.as_str())
                .unified_diff()
                .context_radius(3)
                .header(&format!("a/{path}"), &format!("b/{path}"))
                .to_string(),
        )
    }

    /// Diff rows of an edit (from the recorded file change, else computed
    /// from the edit arguments).
    pub fn diff_rows(&self) -> Vec<DiffRow> {
        if let Some(diff) = self.change.as_ref().and_then(|c| c.diff.as_deref()) {
            return parse_unified(diff);
        }
        let old = str_arg(&self.args, &["old_string"]).unwrap_or_default();
        let new = str_arg(&self.args, &["new_string"]).unwrap_or_default();
        if old.is_empty() && new.is_empty() {
            return Vec::new();
        }
        let diff = similar::TextDiff::from_lines(old.as_str(), new.as_str())
            .unified_diff()
            .context_radius(3)
            .to_string();
        parse_unified(&diff)
    }

    /// The full body (before collapsing).
    pub fn body(&self, theme: &Theme) -> Vec<Line<'static>> {
        let fg = theme.fg(Token::Fg);
        if self.status == CardStatus::Error {
            return self
                .output_text(false)
                .lines()
                .map(|l| Line::from(Span::styled(sanitize(l), theme.fg(Token::Error))))
                .collect();
        }
        if let Some(n) = &self.approval {
            return vec![Line::from(vec![
                Span::styled("rule: ", theme.fg(Token::Muted)),
                Span::styled(n.rule.clone(), fg),
            ])];
        }
        let lang = highlight::lang_for(&self.path());
        match self.tool.as_str() {
            "read" => {
                let mut st = State::default();
                self.output
                    .lines()
                    .filter(|l| !l.starts_with("[Showing lines") && !l.is_empty())
                    .map(|l| match l.split_once('\t') {
                        Some((num, text)) => {
                            let mut spans = vec![Span::styled(
                                format!("{:>4} │ ", num.trim()),
                                theme.fg(Token::Muted),
                            )];
                            spans.extend(highlight::highlight_line(
                                lang,
                                &sanitize(text),
                                &mut st,
                                theme,
                                Style::default(),
                            ));
                            Line::from(spans)
                        }
                        None => Line::from(Span::styled(sanitize(l), theme.fg(Token::Muted))),
                    })
                    .collect()
            }
            "write" => {
                let content = str_arg(&self.args, &["content"]).unwrap_or_default();
                let mut st = State::default();
                content
                    .lines()
                    .map(|l| {
                        Line::from(highlight::highlight_line(
                            lang,
                            &sanitize(l),
                            &mut st,
                            theme,
                            Style::default(),
                        ))
                    })
                    .collect()
            }
            "edit" => diff_lines(&self.diff_rows(), lang, theme),
            "bash" => bash_body(&self.output, theme),
            _ if self.mcp.is_some() => self
                .output_text(false)
                .lines()
                .map(|l| Line::from(Span::styled(sanitize(l), fg)))
                .collect(),
            _ => self
                .output
                .lines()
                .map(|l| Line::from(Span::styled(sanitize(l), fg)))
                .collect(),
        }
    }

    /// Lays the card out at `ctx.width`.
    pub fn render(&self, ctx: &CardCtx<'_>) -> Vec<Line<'static>> {
        self.render_with_buttons(ctx).0
    }

    /// Lays the card out and returns where its copy buttons are (none for
    /// compact cards or without [`CardCtx::copy_buttons`]).
    ///
    /// Nothing is cut: a header too long for the top border keeps the
    /// status icon and tool name there and wraps the rest (summary,
    /// badges) onto rows inside the box; body lines wrap inside the box
    /// (continuation rows keep the line-number gutter blank); copy buttons
    /// that do not fit in the bottom border go on a row above it.
    pub fn render_with_buttons(&self, ctx: &CardCtx<'_>) -> (Vec<Line<'static>>, Vec<CardButton>) {
        if self.is_compact() {
            return (self.render_compact(ctx), Vec::new());
        }
        let theme = ctx.theme;
        // Narrower than this a box cannot hold any text.
        let width = ctx.width.max(5);
        let tint = theme.bg(self.status.tint());
        let border = if ctx.focused {
            theme.fg(Token::BorderFocus)
        } else {
            theme.fg(Token::Border)
        };
        let inner = width.saturating_sub(4).max(1);
        let mut head = vec![Span::styled(
            format!("{} ", self.status.icon(&ctx.spinner)),
            theme.fg(self.status.token()),
        )];
        head.extend(self.name_spans(theme));
        let mut tail = Vec::new();
        let summary = self.summary();
        if !summary.is_empty() {
            let token = if self.tool == "bash" {
                Token::Fg
            } else {
                Token::Path
            };
            tail.push(Span::styled(
                format!("  {}", sanitize(&summary)),
                theme.fg(token),
            ));
        }
        tail.extend(self.badges(theme));
        let mut right = self.right_badges(theme);
        if let Some(ms) = self.duration_ms {
            right.push(Span::styled(fmt_ms(ms), theme.fg(Token::Number)));
        }
        if let Some(ts) = ctx.time.format(self.ts, ctx.now) {
            // The badges end with their own separator.
            if self.duration_ms.is_some() {
                right.push(Span::styled(" · ", theme.fg(Token::Muted)));
            }
            right.push(Span::styled(ts, theme.fg(Token::Muted)));
        }
        let mut lines = card_top(head, tail, right, width, border);
        let boxed_wrapped =
            |line: Line<'static>, indent: &[Span<'static>], lines: &mut Vec<Line<'static>>| {
                lines.extend(card_row(line, indent, width, border));
            };

        let body = self.body(theme);
        let total = body.len();
        let collapsible = self.status != CardStatus::Error && total > ctx.preview_lines;
        let shown = if collapsible && !self.expanded {
            ctx.preview_lines
        } else {
            total
        };
        let gutter = matches!(self.tool.as_str(), "read" | "edit")
            .then(|| Span::styled("     │ ", theme.fg(Token::Muted)));
        for l in body.into_iter().take(shown) {
            let has_gutter = gutter.is_some()
                && l.spans
                    .first()
                    .is_some_and(|s| s.content.ends_with('│') || s.content.ends_with("│ "));
            let indent: Vec<Span<'static>> = if has_gutter {
                gutter.iter().cloned().collect()
            } else {
                Vec::new()
            };
            boxed_wrapped(l, &indent, &mut lines);
        }
        if collapsible {
            let hint = if self.expanded {
                "Enter to collapse".to_owned()
            } else {
                format!("… {} more lines · Enter to expand", total - shown)
            };
            boxed_wrapped(
                Line::from(Span::styled(hint, theme.fg(Token::Muted))),
                &[],
                &mut lines,
            );
        }
        if let Some(p) = (self.tool == "bash" || self.mcp.is_some())
            .then(|| spill_pointer(&self.output))
            .flatten()
        {
            boxed_wrapped(
                Line::from(Span::styled(p, theme.fg(Token::Warn))),
                &[],
                &mut lines,
            );
        }
        let kinds = if ctx.copy_buttons {
            self.copy_kinds()
        } else {
            Vec::new()
        };
        let (bottom, mut buttons, left_over) = self.bottom_border(width, border, &kinds, ctx);
        // Buttons that did not fit in the bottom border: one per row
        // inside the box, above it.
        let mut extra = Vec::new();
        for kind in left_over {
            let label = kind.label();
            let id = copy_button_id(&self.call_id, kind);
            let mut st = ctx.theme.fg(Token::Accent);
            if ctx.focus == Some(id.as_str()) {
                st = st.patch(ctx.theme.focused());
            }
            extra.push(CardButton {
                kind,
                line: lines.len(),
                col: 2,
                width: width_of(label).min(inner),
            });
            for l in wrap_line(Line::from(Span::styled(label, st)), inner, &[]) {
                lines.push(boxed(l, inner, border));
            }
        }
        for b in &mut buttons {
            b.line = lines.len();
        }
        extra.extend(buttons);
        lines.push(bottom);
        (
            lines.into_iter().map(|l| l.patch_style(tint)).collect(),
            extra,
        )
    }

    /// `╰─ ⧉ copy output ─ ⧉ copy diff ───╯`: buttons that fit, then fill.
    /// Returns the kinds that did not fit too.
    fn bottom_border(
        &self,
        width: usize,
        border: Style,
        kinds: &[CardCopy],
        ctx: &CardCtx<'_>,
    ) -> (Line<'static>, Vec<CardButton>, Vec<CardCopy>) {
        let mut spans = vec![Span::styled("╰", border)];
        let mut col = 1;
        let mut buttons = Vec::new();
        let mut left_over = Vec::new();
        for &kind in kinds {
            let label = kind.label();
            let w = crate::chat::text::width(label);
            // "─ " + label + " " and room for "─╯".
            if !left_over.is_empty() || col + 2 + w + 1 + 2 > width {
                left_over.push(kind);
                continue;
            }
            spans.push(Span::styled("─ ", border));
            let id = copy_button_id(&self.call_id, kind);
            let mut st = ctx.theme.fg(Token::Accent);
            if ctx.focus == Some(id.as_str()) {
                st = st.patch(ctx.theme.focused());
            }
            spans.push(Span::styled(label, st));
            spans.push(Span::styled(" ", border));
            buttons.push(CardButton {
                kind,
                line: 0,
                col: col + 2,
                width: w,
            });
            col += 2 + w + 1;
        }
        spans.push(Span::styled(
            "─".repeat(width.saturating_sub(col + 1)),
            border,
        ));
        spans.push(Span::styled("╯", border));
        (Line::from(spans), buttons, left_over)
    }

    fn render_compact(&self, ctx: &CardCtx<'_>) -> Vec<Line<'static>> {
        let theme = ctx.theme;
        let mut left = vec![
            Span::styled(
                format!("{} ", self.status.icon(&ctx.spinner)),
                theme.fg(self.status.token()),
            ),
            Span::styled(self.tool.clone(), theme.fg(Token::Accent)),
            Span::styled(
                format!(
                    "  {}",
                    sanitize(&compact_summary(&self.tool, &self.args, &self.output))
                ),
                theme.fg(Token::Fg),
            ),
        ];
        if ctx.focused {
            for s in &mut left {
                s.style = s.style.patch(theme.focused());
            }
        }
        let mut right = Vec::new();
        if let Some(ms) = self.duration_ms {
            right.push(Span::styled(
                format!(" {}", fmt_ms(ms)),
                theme.fg(Token::Number),
            ));
        }
        if let Some(ts) = ctx.time.format(self.ts, ctx.now) {
            right.push(Span::styled(format!(" · {ts}"), theme.fg(Token::Muted)));
        }
        let mut lines = wrap_right(
            Line::from(left),
            right,
            ctx.width,
            Span::raw(" "),
            &[Span::raw("  ")],
        );
        if self.expanded {
            for item in compact_detail(&self.tool, &self.args, &self.output) {
                lines.extend(wrap_line(
                    Line::from(Span::styled(
                        format!("    {}", sanitize(&item)),
                        theme.fg(Token::Muted),
                    )),
                    ctx.width,
                    &[Span::raw("      ")],
                ));
            }
        }
        lines
    }
}

/// The top border of a box card, `╭ <head><tail> ─── <right> ╮`, at
/// `width` (at least 5). Nothing is cut: when `head` and `tail` do not fit
/// beside `right`, the top row keeps `head` (or only the border when even
/// that does not fit) and the rest wraps onto rows inside the box; `right`
/// moves inside too when it does not fit beside the corners.
pub(crate) fn card_top(
    head: Vec<Span<'static>>,
    tail: Vec<Span<'static>>,
    mut right: Vec<Span<'static>>,
    width: usize,
    border: Style,
) -> Vec<Line<'static>> {
    let width = width.max(5);
    let span_w = |v: &[Span<'static>]| v.iter().map(|s| width_of(&s.content)).sum::<usize>();
    // "╭ " + content + " " + at least one fill cell + " " + right + " ╮"
    // (right_align keeps a cell between the two parts).
    let mut right_w = span_w(&right);
    let mut moved_right = Vec::new();
    if !right.is_empty() && right_w + 6 > width {
        moved_right = std::mem::take(&mut right);
        right_w = 0;
    }
    let room = width.saturating_sub(4 + if right_w > 0 { right_w + 3 } else { 2 });
    let (top, rest): (Vec<Span<'static>>, Vec<Span<'static>>) =
        if span_w(&head) + span_w(&tail) <= room {
            (head.into_iter().chain(tail).collect(), Vec::new())
        } else if span_w(&head) <= room {
            (head, tail)
        } else {
            (Vec::new(), head.into_iter().chain(tail).collect())
        };
    let mut left = vec![Span::styled("╭ ", border)];
    left.extend(top);
    left.push(Span::styled(" ", border));
    if !right.is_empty() {
        right.insert(0, Span::styled(" ", border));
    }
    right.push(Span::styled(" ╮", border));
    let mut lines = vec![right_align(
        Line::from(left),
        right,
        width,
        Span::styled("─", border),
    )];
    if !rest.is_empty() {
        let mut rest = rest;
        if let Some(first) = rest.first_mut() {
            first.content = first.content.trim_start().to_owned().into();
        }
        lines.extend(card_row(Line::from(rest), &[], width, border));
    }
    if !moved_right.is_empty() {
        lines.extend(card_row(Line::from(moved_right), &[], width, border));
    }
    lines
}

/// A body line of a box card at `width`: wrapped inside `│ … │`,
/// continuation rows starting with `indent`.
pub(crate) fn card_row(
    line: Line<'static>,
    indent: &[Span<'static>],
    width: usize,
    border: Style,
) -> Vec<Line<'static>> {
    let inner = width.max(5) - 4;
    wrap_line(line, inner, indent)
        .into_iter()
        .map(|l| boxed(l, inner, border))
        .collect()
}

/// The bottom border of a box card without buttons.
pub(crate) fn card_bottom(width: usize, border: Style) -> Line<'static> {
    let width = width.max(5);
    Line::from(vec![
        Span::styled("╰", border),
        Span::styled("─".repeat(width - 2), border),
        Span::styled("╯", border),
    ])
}

/// `│ line │`, padded to `inner`. A soft-wrap marker stays the first span.
fn boxed(mut line: Line<'static>, inner: usize, border: Style) -> Line<'static> {
    let marker = take_wrap_marker(&mut line);
    let w = line_width(&line);
    let mut spans: Vec<Span<'static>> = marker.into_iter().collect();
    spans.push(Span::styled("│ ", border));
    spans.extend(line.spans);
    spans.push(Span::raw(" ".repeat(inner.saturating_sub(w))));
    spans.push(Span::styled(" │", border));
    Line::from(spans)
}

/// Parallel calls from one model turn.
#[derive(Debug, Clone, PartialEq)]
pub struct CardGroup {
    /// The calls, in call order.
    pub cards: Vec<ToolCard>,
}

/// Where a card sits inside a rendered group.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CardSpan {
    /// Tool call id.
    pub call_id: String,
    /// First line of the card in the group's lines.
    pub start: usize,
    /// Number of lines.
    pub len: usize,
    /// Copy buttons, with lines relative to the group's lines.
    pub buttons: Vec<CardButton>,
}

impl CardGroup {
    /// A group with one card.
    pub fn new(card: ToolCard) -> CardGroup {
        CardGroup { cards: vec![card] }
    }

    /// The card with `call_id`.
    pub fn card_mut(&mut self, call_id: &str) -> Option<&mut ToolCard> {
        self.cards.iter_mut().find(|c| c.call_id == call_id)
    }

    /// The card with `call_id`.
    pub fn card(&self, call_id: &str) -> Option<&ToolCard> {
        self.cards.iter().find(|c| c.call_id == call_id)
    }

    /// Sum of card versions (for render caches).
    pub fn version(&self) -> u64 {
        self.cards.iter().map(|c| c.version + 1).sum::<u64>() + self.cards.len() as u64
    }

    /// Lays out the group: a `⧉ N parallel calls` header when it has more
    /// than one card, then every card. `focused` is the focused call id.
    pub fn render(
        &self,
        ctx: &CardCtx<'_>,
        focused: Option<&str>,
    ) -> (Vec<Line<'static>>, Vec<CardSpan>) {
        let theme = ctx.theme;
        let mut lines = Vec::new();
        let mut spans = Vec::new();
        if self.cards.len() > 1 {
            let done = self
                .cards
                .iter()
                .filter(|c| c.status == CardStatus::Ok)
                .count();
            let failed = self
                .cards
                .iter()
                .filter(|c| c.status == CardStatus::Error)
                .count();
            let running = self.cards.len() - done - failed;
            let mut left = vec![
                Span::styled("⧉ ", theme.fg(Token::Accent)),
                Span::styled(
                    format!("{} parallel calls", self.cards.len()),
                    theme.fg(Token::Fg).add_modifier(Modifier::BOLD),
                ),
            ];
            for (n, label, token) in [
                (done, "ok", Token::Ok),
                (failed, "failed", Token::Error),
                (running, "running", Token::Running),
            ] {
                if n > 0 {
                    left.push(Span::styled(" · ", theme.fg(Token::Muted)));
                    left.push(Span::styled(format!("{n} {label}"), theme.fg(token)));
                }
            }
            let right = ctx
                .time
                .format(self.cards[0].ts, ctx.now)
                .map(|t| vec![Span::styled(t, theme.fg(Token::Muted))])
                .unwrap_or_default();
            lines.extend(wrap_right(
                Line::from(left),
                right,
                ctx.width,
                Span::raw(" "),
                &[Span::raw("  ")],
            ));
        }
        for card in &self.cards {
            let c = CardCtx {
                focused: focused == Some(card.call_id.as_str()),
                ..*ctx
            };
            let (l, mut buttons) = card.render_with_buttons(&c);
            for b in &mut buttons {
                b.line += lines.len();
            }
            spans.push(CardSpan {
                call_id: card.call_id.clone(),
                start: lines.len(),
                len: l.len(),
                buttons,
            });
            lines.extend(l);
        }
        (lines, spans)
    }
}

/// Kind of a diff row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffKind {
    /// Unchanged context.
    Context,
    /// Added line.
    Add,
    /// Removed line.
    Del,
    /// Hunk separator.
    Hunk,
}

/// One row of a unified diff with its line number.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffRow {
    /// Row kind.
    pub kind: DiffKind,
    /// Line number in the new file (old file for removals).
    pub line: Option<u32>,
    /// Text without the sign.
    pub text: String,
}

/// Parses a unified diff (`---`/`+++` headers are skipped).
pub fn parse_unified(diff: &str) -> Vec<DiffRow> {
    let mut rows = Vec::new();
    let (mut old, mut new) = (1u32, 1u32);
    for l in diff.lines() {
        if l.starts_with("--- ")
            || l.starts_with("+++ ")
            || l.starts_with("diff ")
            || l.starts_with("index ")
        {
            continue;
        }
        if let Some(h) = l.strip_prefix("@@") {
            // "-a,b +c,d", "-a +c,d", "-a,b +c" or "-a +c".
            let (o, n) = h.split_once('+').unwrap_or((h, ""));
            let first = |s: &str| {
                s.split(|c: char| !c.is_ascii_digit())
                    .find(|x| !x.is_empty())
                    .and_then(|x| x.parse().ok())
            };
            if let (Some(a), Some(c)) = (first(o), first(n)) {
                old = a;
                new = c;
            }
            if !rows.is_empty() {
                rows.push(DiffRow {
                    kind: DiffKind::Hunk,
                    line: None,
                    text: "⋯".into(),
                });
            }
            continue;
        }
        let (kind, text) = match l.chars().next() {
            Some('+') => (DiffKind::Add, &l[1..]),
            Some('-') => (DiffKind::Del, &l[1..]),
            Some(' ') => (DiffKind::Context, &l[1..]),
            Some('\\') => continue,
            _ => (DiffKind::Context, l),
        };
        let line = match kind {
            DiffKind::Del => {
                old += 1;
                Some(old - 1)
            }
            DiffKind::Add => {
                new += 1;
                Some(new - 1)
            }
            _ => {
                old += 1;
                new += 1;
                Some(new - 1)
            }
        };
        rows.push(DiffRow {
            kind,
            line,
            text: text.to_owned(),
        });
    }
    rows
}

/// Renders diff rows with line numbers, signs, backgrounds and syntax colours.
pub fn diff_lines(
    rows: &[DiffRow],
    lang: Option<&highlight::Lang>,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let mut st = State::default();
    rows.iter()
        .map(|r| {
            let (sign, sign_tok, bg) = match r.kind {
                DiffKind::Add => ("+", Token::DiffAdded, Some(Token::DiffAddedBg)),
                DiffKind::Del => ("-", Token::DiffRemoved, Some(Token::DiffRemovedBg)),
                DiffKind::Context => (" ", Token::Muted, None),
                DiffKind::Hunk => {
                    return Line::from(Span::styled("     ⋯", theme.fg(Token::Muted)));
                }
            };
            let base = bg.map_or_else(Style::default, |b| theme.bg(b));
            let num = r
                .line
                .map_or_else(|| "    ".to_owned(), |n| format!("{n:>4}"));
            let mut spans = vec![
                Span::styled(format!("{num} │"), theme.fg(Token::Muted)),
                Span::styled(sign, base.patch(theme.fg(sign_tok))),
            ];
            spans.extend(highlight::highlight_line(
                lang,
                &sanitize(&r.text),
                &mut st,
                theme,
                base,
            ));
            Line::from(spans)
        })
        .collect()
}

/// How a bash call ended, parsed from its output's status lines.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BashExit {
    /// Exit code 0.
    Ok,
    /// Non-zero exit code.
    Code(i32),
    /// Killed by the timeout.
    Timeout,
    /// Cancelled.
    Killed,
    /// Killed by a signal.
    Signal(i32),
}

fn is_status_line(l: &str) -> bool {
    l.starts_with("[exit code ")
        || l.starts_with("[timed out after ")
        || l.starts_with("[cancelled: ")
        || l.starts_with("[killed by signal ")
        || l.starts_with("[timeout capped at")
        || l == "[no output]"
}

/// Parses the bash tool's trailing status lines.
pub fn bash_exit(output: &str) -> BashExit {
    for l in output.lines().rev().take(4) {
        if let Some(c) = l
            .strip_prefix("[exit code ")
            .and_then(|r| r.strip_suffix(']'))
        {
            return c.parse().map_or(BashExit::Code(1), BashExit::Code);
        }
        if l.starts_with("[timed out after ") {
            return BashExit::Timeout;
        }
        if l.starts_with("[cancelled: ") {
            return BashExit::Killed;
        }
        if let Some(s) = l
            .strip_prefix("[killed by signal ")
            .and_then(|r| r.strip_suffix(']'))
        {
            return BashExit::Signal(s.parse().unwrap_or(0));
        }
    }
    BashExit::Ok
}

/// The pointer shown when bash output was truncated and spilled.
pub fn spill_pointer(output: &str) -> Option<String> {
    let l = output
        .lines()
        .find(|l| l.starts_with("[... output truncated"))?;
    let hidden = l
        .strip_prefix("[... output truncated: ")
        .and_then(|r| r.split_whitespace().next())
        .unwrap_or("some");
    let id = l
        .split_whitespace()
        .find(|w| w.starts_with("spill:"))
        .map(|w| w.trim_end_matches([';', '.', ',']));
    Some(match id {
        Some(id) => format!("⋯ {hidden} lines truncated · full output saved as {id}"),
        None => format!("⋯ {hidden} lines truncated · full output not saved"),
    })
}

fn bash_body(output: &str, theme: &Theme) -> Vec<Line<'static>> {
    output
        .lines()
        .filter(|l| !is_status_line(l) && !l.starts_with("[... output truncated"))
        .map(|l| Line::from(Span::styled(sanitize(l), theme.fg(Token::Fg))))
        .collect()
}

/// `(first, last, total)` line numbers of a read result.
pub fn read_range(args: &Value, output: &str) -> Option<(usize, usize, usize)> {
    if let Some(l) = output
        .lines()
        .rev()
        .find(|l| l.starts_with("[Showing lines "))
    {
        let nums: Vec<usize> = l
            .split(|c: char| !c.is_ascii_digit())
            .filter_map(|s| s.parse().ok())
            .collect();
        if nums.len() >= 3 {
            return Some((nums[0], nums[1], nums[2]));
        }
    }
    let numbered: Vec<usize> = output
        .lines()
        .filter_map(|l| l.split_once('\t').and_then(|(n, _)| n.trim().parse().ok()))
        .collect();
    let first = *numbered.first()?;
    let last = *numbered.last()?;
    let _ = args;
    Some((first, last, last))
}

/// A string argument under the first of `keys` that is present.
pub fn str_arg(args: &Value, keys: &[&str]) -> Option<String> {
    keys.iter()
        .find_map(|k| args.get(*k).and_then(Value::as_str))
        .map(str::to_owned)
}

/// The JSON value of a payload: `Json` as is, `Text` parsed when it is JSON.
pub fn payload_json(p: &Payload) -> Option<Value> {
    match p {
        Payload::Json(v) => Some(v.clone()),
        Payload::Text(t) => serde_json::from_str(t).ok(),
        Payload::Blob(_) => None,
    }
}

/// The text of a payload (JSON strings unquoted, other JSON compact).
pub fn payload_text(p: &Payload) -> String {
    match p {
        Payload::Text(t) => t.clone(),
        Payload::Json(Value::String(s)) => s.clone(),
        Payload::Json(v) => v.to_string(),
        Payload::Blob(b) => format!(
            "[{} bytes stored as blob {}]",
            b.size,
            &b.sha256[..b.sha256.len().min(12)]
        ),
    }
}

/// One-line summary of a call's arguments, e.g. `src/main.rs` or
/// `$ cargo test parse`.
pub fn arg_summary(tool: &str, args: &Value) -> String {
    let path = str_arg(args, &["path", "file_path", "file"]);
    match tool {
        "bash" => str_arg(args, &["command"])
            .map(|c| format!("$ {}", c.lines().next().unwrap_or("")))
            .unwrap_or_default(),
        "read" | "write" | "edit" => path.unwrap_or_default(),
        "grep" | "glob" => {
            let pattern = str_arg(args, &["pattern"]).unwrap_or_default();
            match path {
                Some(p) => format!("{pattern} in {p}"),
                None => pattern,
            }
        }
        _ => match path {
            Some(p) => p,
            None => match args {
                Value::Object(m) => m
                    .iter()
                    .take(3)
                    .map(|(k, v)| {
                        let v = match v {
                            Value::String(s) => s.lines().next().unwrap_or("").to_owned(),
                            other => other.to_string(),
                        };
                        format!("{k}={v}")
                    })
                    .collect::<Vec<_>>()
                    .join(" "),
                Value::Null => String::new(),
                other => other.to_string(),
            },
        },
    }
}

fn todo_items(args: &Value) -> Vec<(String, String)> {
    args.get("todos")
        .or_else(|| args.get("items"))
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .map(|t| {
                    let content = str_arg(t, &["content", "title", "text"]).unwrap_or_default();
                    let status =
                        str_arg(t, &["status", "state"]).unwrap_or_else(|| "pending".into());
                    (content, status)
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The compact summary of a todo, memory or spawn_agent call.
pub fn compact_summary(tool: &str, args: &Value, output: &str) -> String {
    match tool {
        "todo" if str_arg(args, &["action"]).is_some() => {
            // The task-board tool: one operation per call.
            let action = str_arg(args, &["action"]).unwrap_or_default();
            let id = str_arg(args, &["id"]).unwrap_or_default();
            let title = str_arg(args, &["title"]);
            let status = str_arg(args, &["status"]);
            let mut out = format!("{action} {id}").trim().to_owned();
            if let Some(t) = title {
                out.push_str(&format!(": {t}"));
            }
            if let Some(st) = status {
                out.push_str(&format!(" → {st}"));
            }
            out
        }
        "todo" => {
            let items = todo_items(args);
            let count = |s: &[&str]| {
                items
                    .iter()
                    .filter(|(_, st)| s.contains(&st.as_str()))
                    .count()
            };
            let done = count(&["done", "completed", "complete"]);
            let active = count(&["in_progress", "active", "doing"]);
            let pending = items.len() - done - active;
            format!(
                "{} tasks · {done} done · {active} in progress · {pending} pending",
                items.len()
            )
        }
        "memory" => {
            let action = str_arg(args, &["action", "op"]).unwrap_or_else(|| "note".into());
            let what = str_arg(args, &["summary", "title", "key", "query", "content"])
                .map(|s| s.lines().next().unwrap_or("").to_owned())
                .unwrap_or_else(|| output.lines().next().unwrap_or("").to_owned());
            format!("{action}: {what}")
        }
        "spawn_agent" => {
            let agent = str_arg(args, &["agent", "name"]).unwrap_or_else(|| "agent".into());
            let task = str_arg(args, &["brief", "task", "prompt", "goal"]).unwrap_or_default();
            format!("{agent}: {}", task.lines().next().unwrap_or(""))
        }
        _ => arg_summary(tool, args),
    }
}

fn compact_detail(tool: &str, args: &Value, output: &str) -> Vec<String> {
    match tool {
        "todo" => todo_items(args)
            .into_iter()
            .map(|(c, s)| {
                let icon = match s.as_str() {
                    "done" | "completed" | "complete" => '☑',
                    "in_progress" | "active" | "doing" => '◐',
                    _ => '☐',
                };
                format!("{icon} {c}")
            })
            .collect(),
        _ => output.lines().map(str::to_owned).collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use time::macros::datetime;

    fn text(l: &Line<'_>) -> String {
        l.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    fn ctx(theme: &Theme) -> CardCtx<'_> {
        CardCtx {
            theme,
            width: 70,
            time: TimeFormat::default(),
            now: datetime!(2026-10-22 14:40:00 UTC),
            preview_lines: 3,
            spinner: Spinner::new(true),
            focused: false,
            copy_buttons: false,
            focus: None,
        }
    }

    const TS: OffsetDateTime = datetime!(2026-10-22 14:32:09 UTC);

    fn lines(card: &ToolCard, theme: &Theme) -> Vec<String> {
        card.render(&ctx(theme)).iter().map(text).collect()
    }

    #[test]
    fn header_has_icon_name_summary_duration_timestamp_and_tint() {
        let theme = Theme::default();
        let mut card = ToolCard::new("c1", "read", json!({"path": "src/lib.rs"}), TS);
        let running = card.render(&ctx(&theme));
        assert!(text(&running[0]).starts_with("╭ ⠋ read  src/lib.rs"));
        assert_eq!(running[0].style.bg, theme.bg(Token::CardPending).bg);
        card.finish(true, "     1\tfn a() {}\n     2\tfn b() {}\n".into(), 3);
        let l = card.render(&ctx(&theme));
        let h = text(&l[0]);
        assert!(h.starts_with("╭ ✔ read  src/lib.rs  L1-2 · 2 lines"), "{h}");
        assert!(h.ends_with("3ms · 14:32:09 ╮"), "{h}");
        assert_eq!(line_width(&l[0]), 70);
        assert!(l.iter().all(|x| line_width(x) == 70));
        assert_eq!(l[0].style.bg, theme.bg(Token::CardOk).bg);
        card.status = CardStatus::Error;
        assert_eq!(
            card.render(&ctx(&theme))[0].style.bg,
            theme.bg(Token::CardError).bg
        );
        card.set_approval(Some(ApprovalNote {
            rule: "outward.ask = git push*".into(),
            reason: "outward action".into(),
        }));
        let l = card.render(&ctx(&theme));
        assert_eq!(l[0].style.bg, theme.bg(Token::CardApproval).bg);
        assert!(text(&l[0]).contains("⏸ read"));
        assert!(text(&l[0]).contains("outward action"));
        // The summary no longer fits beside the reason: it wraps into the
        // box instead of being cut.
        assert!(
            text(&l[1]).contains("src/lib.rs  L1-2 · 2 lines"),
            "{:?}",
            l
        );
        assert!(text(&l[2]).contains("rule: outward.ask = git push*"));
    }

    #[test]
    fn error_bodies_drop_the_provenance_wrapper() {
        let theme = Theme::default();
        let mut card = ToolCard::new("c1", "mcp__t__write_note", json!({"text": "x"}), TS);
        card.set_mcp("t", "write_note");
        card.finish(
            false,
            "<tool_result tool=\"mcp__t__write_note\" call_id=\"c1\" status=\"error\" trust=\"untrusted\" origin=\"mcp server\">\nnot run: the approval was denied\n</tool_result>".into(),
            3,
        );
        let body = lines(&card, &theme).join("\n");
        assert!(body.contains("not run: the approval was denied"), "{body}");
        assert!(!body.contains("tool_result"), "{body}");
    }

    #[test]
    fn mcp_calls_name_server_and_tool_with_semantic_tokens() {
        let theme = Theme::default();
        let mut card = ToolCard::new(
            "c1",
            "mcp__github__create_issue",
            json!({"title": "Crash", "body": "x"}),
            TS,
        );
        assert_eq!(card.display_name(), "mcp__github__create_issue");
        let v = card.version();
        card.set_mcp("github", "create_issue");
        assert!(card.version() > v, "render caches see the change");
        assert_eq!(card.display_name(), "mcp github › create_issue");
        card.finish(
            false,
            "`create_issue` reported an error: rate limited".into(),
            42,
        );
        let l = card.render(&ctx(&theme));
        let h = text(&l[0]);
        assert!(
            h.starts_with("╭ ✗ mcp github › create_issue  body=x title=Crash"),
            "{h}"
        );
        assert!(h.contains("42ms"), "{h}");
        let span = |t: &str| l[0].spans.iter().find(|s| s.content == t).unwrap().style;
        assert_eq!(span("github").fg, theme.fg(Token::Agent).fg);
        assert_eq!(span("create_issue").fg, theme.fg(Token::Accent).fg);
        assert_eq!(span("mcp ").fg, theme.fg(Token::Muted).fg);
        assert_eq!(l[0].style.bg, theme.bg(Token::CardError).bg);
        // Errors show the full message.
        assert!(
            lines(&card, &theme)
                .iter()
                .any(|x| x.contains("rate limited"))
        );
        // Through `mcp_tools`, the summary is the arguments sent to the server.
        let mut gw = ToolCard::new(
            "c2",
            "mcp_tools",
            json!({"call": "db/query", "arguments": {"sql": "select 1"}}),
            TS,
        );
        gw.set_mcp("db", "query");
        assert_eq!(gw.summary(), "sql=select 1");
    }

    #[test]
    fn read_shows_range_count_and_truncated_badge() {
        let theme = Theme::default();
        let mut card = ToolCard::new("c", "read", json!({"path": "a.rs", "offset": 10}), TS);
        let out = "    10\tlet a = 1;\n    11\tlet b = 2;\n\n[Showing lines 10-11 of 400. To read more, call read with offset=12.]";
        card.finish(true, out.into(), 1);
        let l = lines(&card, &theme);
        assert!(
            l[0].contains("a.rs  L10-11 · 2 lines of 400  truncated"),
            "{}",
            l[0]
        );
        assert!(l[1].contains("  10 │ let a = 1;"), "{}", l[1]);
    }

    #[test]
    fn write_shows_path_bytes_and_new_file_badge() {
        let theme = Theme::default();
        let mut card = ToolCard::new(
            "c",
            "write",
            json!({"path": "new.txt", "content": "hello\nworld\n"}),
            TS,
        );
        card.set_change(FileChangeKind::Created, None);
        card.finish(true, "Created new.txt (2 lines).".into(), 2);
        let l = lines(&card, &theme);
        assert!(l[0].contains("new.txt  12 B  new file"), "{}", l[0]);
        assert!(l[1].contains("hello"));
        let mut card = ToolCard::new("c", "write", json!({"path": "old.txt", "content": "x"}), TS);
        card.finish(true, "Wrote old.txt (1 lines).".into(), 2);
        assert!(!lines(&card, &theme)[0].contains("new file"));
    }

    #[test]
    fn edit_shows_colored_diff_with_line_numbers_and_counts() {
        let theme = Theme::default();
        let mut card = ToolCard::new(
            "c",
            "edit",
            json!({"path": "src/parse.rs", "old_string": "a", "new_string": "b"}),
            TS,
        );
        card.set_change(
            FileChangeKind::Modified,
            Some("--- a/src/parse.rs\n+++ b/src/parse.rs\n@@ -40,2 +40,2 @@\n     let hdr = buf;\n-    .unwrap();\n+    .ok_or(ParseError::MissingColon)?;\n".into()),
        );
        card.finish(true, "Edited src/parse.rs".into(), 3);
        let rendered = card.render(&ctx(&theme));
        let l: Vec<String> = rendered.iter().map(text).collect();
        assert!(l[0].contains("src/parse.rs  +1 −1"), "{}", l[0]);
        assert!(l[1].contains("  40 │     let hdr = buf;"), "{}", l[1]);
        assert!(l[2].contains("  41 │-    .unwrap();"), "{}", l[2]);
        assert!(
            l[3].contains("  41 │+    .ok_or(ParseError::MissingColon)?;"),
            "{}",
            l[3]
        );
        let add = &rendered[3];
        let sign = add.spans.iter().find(|s| s.content == "+").unwrap();
        assert_eq!(sign.style.fg, theme.fg(Token::DiffAdded).fg);
        assert_eq!(sign.style.bg, theme.bg(Token::DiffAddedBg).bg);
        let ty = add
            .spans
            .iter()
            .find(|s| s.content == "ParseError")
            .unwrap();
        assert_eq!(ty.style.fg, theme.fg(Token::Running).fg, "syntax colored");
        // Without a recorded diff, the diff is computed from the arguments.
        let card = ToolCard::new(
            "c",
            "edit",
            json!({"path": "x.rs", "old_string": "a\n", "new_string": "b\nc\n"}),
            TS,
        );
        assert!(lines(&card, &theme)[0].contains("+2 −1"));
    }

    #[test]
    fn bash_streams_and_shows_exit_badges_and_spill_pointer() {
        let theme = Theme::default();
        let mut card = ToolCard::new("c", "bash", json!({"command": "cargo test parse"}), TS);
        card.push_output("test a ... ok\n");
        card.push_output("test b ... ");
        let l = lines(&card, &theme);
        assert!(l[0].starts_with("╭ ⠋ bash  $ cargo test parse"));
        assert!(l[1].contains("test a ... ok"));
        assert!(l[2].contains("test b ..."));
        card.finish(
            false,
            "test parse::header_colon ... FAILED\n[... output truncated: 212 of 260 lines (9000 of 12000 bytes) not shown. Full output saved as spill:ab12cd34; page through it with the read tool, e.g. path=\"spill:ab12cd34\" offset=41. ...]\ntail\n[exit code 101]".into(),
            8100,
        );
        card.status = CardStatus::Ok; // a failing command is still a finished call
        let l = lines(&card, &theme);
        assert!(l[0].ends_with("exit 101 · 8.1s · 14:32:09 ╮"), "{}", l[0]);
        assert!(
            l.iter()
                .any(|x| x.contains("⋯ 212 lines truncated · full output saved as spill:ab12cd34")),
            "{l:?}"
        );
        assert!(!l.iter().any(|x| x.contains("[exit code")));
        assert_eq!(bash_exit("x\n[timed out after 1s: ...]"), BashExit::Timeout);
        assert_eq!(bash_exit("[cancelled: the command ...]"), BashExit::Killed);
        assert_eq!(bash_exit("[killed by signal 9]"), BashExit::Signal(9));
        assert_eq!(bash_exit("fine"), BashExit::Ok);
        let mut t = ToolCard::new("c", "bash", json!({"command": "sleep 9"}), TS);
        t.finish(true, "[timed out after 1s: killed]".into(), 1000);
        assert!(lines(&t, &theme)[0].contains("timeout · 1.0s"));
    }

    #[test]
    fn output_collapses_past_preview_and_toggles() {
        let theme = Theme::default();
        let mut card = ToolCard::new("c", "bash", json!({"command": "seq 10"}), TS);
        let out: String = (1..=10).map(|i| format!("{i}\n")).collect();
        card.finish(true, out, 5);
        let l = lines(&card, &theme);
        // header + 3 preview + hint + footer
        assert_eq!(l.len(), 6);
        assert!(l[4].contains("… 7 more lines · Enter to expand"));
        card.toggle();
        let l = lines(&card, &theme);
        assert_eq!(l.len(), 1 + 10 + 1 + 1);
        assert!(l[11].contains("Enter to collapse"));
    }

    #[test]
    fn errors_show_the_full_message() {
        let theme = Theme::default();
        let mut card = ToolCard::new("c", "edit", json!({"path": "a.rs"}), TS);
        let msg = "old_string was not found in a.rs.\nRe-read the file with read and copy the exact text.\nline3\nline4\nline5";
        card.finish(false, msg.into(), 1);
        let rendered = card.render(&ctx(&theme));
        let l: Vec<String> = rendered.iter().map(text).collect();
        assert!(l[0].starts_with("╭ ✗ edit"));
        for line in msg.lines() {
            assert!(l.iter().any(|x| x.contains(line)), "missing {line}");
        }
        assert!(!l.iter().any(|x| x.contains("more lines")));
        let span = rendered[1]
            .spans
            .iter()
            .find(|s| s.content.contains("old_string"))
            .unwrap();
        assert_eq!(span.style.fg, theme.fg(Token::Error).fg);
    }

    #[test]
    fn parallel_calls_are_grouped_with_a_count() {
        let theme = Theme::default();
        let mut g = CardGroup::new(ToolCard::new("a", "read", json!({"path": "a"}), TS));
        g.cards
            .push(ToolCard::new("b", "read", json!({"path": "b"}), TS));
        g.cards
            .push(ToolCard::new("c", "bash", json!({"command": "ls"}), TS));
        g.card_mut("a").unwrap().finish(true, "     1\tx".into(), 1);
        let (lines, spans) = g.render(&ctx(&theme), Some("b"));
        let h = text(&lines[0]);
        assert!(
            h.starts_with("⧉ 3 parallel calls · 1 ok · 2 running"),
            "{h}"
        );
        assert!(h.ends_with("14:32:09"));
        assert_eq!(spans.len(), 3);
        assert_eq!(spans[0].start, 1);
        assert_eq!(spans[1].start, spans[0].start + spans[0].len);
        // The focused card gets the focus border.
        let focused = &lines[spans[1].start];
        assert_eq!(focused.spans[0].style.fg, theme.fg(Token::BorderFocus).fg);
        let single = CardGroup::new(ToolCard::new("a", "read", json!({"path": "a"}), TS));
        let (l, _) = single.render(&ctx(&theme), None);
        assert!(text(&l[0]).starts_with("╭"));
    }

    #[test]
    fn task_board_calls_and_spawn_briefs_are_summarised() {
        let theme = Theme::default();
        let mut add = ToolCard::new(
            "t",
            "todo",
            json!({"action": "add", "id": "t1", "title": "Find the parser", "status": "in_progress"}),
            TS,
        );
        add.finish(true, "ok".into(), 1);
        let l = lines(&add, &theme);
        assert!(
            l[0].starts_with("✔ todo  add t1: Find the parser → in_progress"),
            "{}",
            l[0]
        );
        let mut spawn = ToolCard::new(
            "s",
            "spawn_agent",
            json!({"agent": "researcher", "brief": "Find the header parser"}),
            TS,
        );
        spawn.finish(true, "ok".into(), 1);
        assert!(lines(&spawn, &theme)[0].contains("researcher: Find the header parser"));
    }

    #[test]
    fn todo_memory_and_spawn_agent_are_compact() {
        let theme = Theme::default();
        let mut todo = ToolCard::new(
            "t",
            "todo",
            json!({"todos": [
                {"content": "parse", "status": "done"},
                {"content": "test", "status": "in_progress"},
                {"content": "docs", "status": "pending"}
            ]}),
            TS,
        );
        todo.finish(true, "ok".into(), 1);
        let l = lines(&todo, &theme);
        assert_eq!(l.len(), 1);
        assert!(
            l[0].starts_with("✔ todo  3 tasks · 1 done · 1 in progress · 1 pending"),
            "{}",
            l[0]
        );
        assert!(l[0].ends_with("1ms · 14:32:09"));
        todo.toggle();
        let l = lines(&todo, &theme);
        assert_eq!(&l[1..], &["    ☑ parse", "    ◐ test", "    ☐ docs"]);

        let mut mem = ToolCard::new(
            "m",
            "memory",
            json!({"action": "update", "summary": "prefers tabs"}),
            TS,
        );
        mem.finish(true, "Updated memory".into(), 1);
        assert!(lines(&mem, &theme)[0].starts_with("✔ memory  update: prefers tabs"));

        let spawn = ToolCard::new(
            "s",
            "spawn_agent",
            json!({"agent": "researcher", "task": "read the net module"}),
            TS,
        );
        let l = lines(&spawn, &theme);
        assert_eq!(l.len(), 1);
        assert!(l[0].starts_with("⠋ spawn_agent  researcher: read the net module"));
    }

    #[test]
    fn summaries() {
        assert_eq!(
            arg_summary("bash", &json!({"command": "ls -la\npwd"})),
            "$ ls -la"
        );
        assert_eq!(
            arg_summary("lookup", &json!({"pattern": "fn", "n": 3})),
            "n=3 pattern=fn"
        );
        assert_eq!(
            arg_summary(
                "grep",
                &json!({"pattern": "fn main", "path": "src", "-n": true})
            ),
            "fn main in src"
        );
        assert_eq!(
            arg_summary("glob", &json!({"pattern": "**/*.rs"})),
            "**/*.rs"
        );
        assert_eq!(payload_text(&Payload::Json(json!("hi"))), "hi");
        assert_eq!(
            payload_json(&Payload::Text("{\"a\":1}".into())),
            Some(json!({"a": 1}))
        );
    }

    /// Every row of a card is exactly `width` cells with intact side
    /// borders, and nothing is cut.
    fn assert_boxed(rows: &[String], width: usize) {
        for r in rows {
            assert_eq!(
                crate::chat::text::width(r),
                width,
                "{r}\n{}",
                rows.join("\n")
            );
            assert!(!r.contains('…'), "cut: {r}");
        }
        assert!(
            rows[0].starts_with('╭') && rows[0].ends_with('╮'),
            "{}",
            rows[0]
        );
        assert!(rows.last().unwrap().starts_with('╰'), "{}", rows.join("\n"));
        for r in &rows[1..rows.len() - 1] {
            assert!(r.starts_with('│') && r.ends_with('│'), "{r}");
        }
    }

    #[test]
    fn long_headers_and_bodies_wrap_inside_the_box() {
        let theme = Theme::default();
        let path =
            "/private/var/folders/n6/hn37q6pj7sj_9dm0000gn/T/.tmpXyZ/project/src/deep/file_end.rs";
        let mut card = ToolCard::new("t1", "read", json!({"path": path}), TS);
        let long_line = format!("1\tlet s = \"{}\";", "x".repeat(90));
        card.finish(
            true,
            format!("{long_line}\n2\t日本語のテキストと絵文字🎉🚀を含む長い行です中文字符測試結尾"),
            3,
        );
        card.toggle();
        let c = CardCtx {
            width: 40,
            copy_buttons: true,
            ..ctx(&theme)
        };
        let (l, buttons) = card.render_with_buttons(&c);
        let rows: Vec<String> = l.iter().map(text).collect();
        assert_boxed(&rows, 40);
        // The tool name stays in the top border; the path wraps below it.
        assert!(rows[0].contains("✔ read"), "{}", rows[0]);
        let squashed: String = rows
            .concat()
            .chars()
            .filter(|c| !c.is_whitespace() && *c != '│')
            .collect();
        assert!(
            squashed.contains(&path.replace(' ', "")),
            "{}",
            rows.join("\n")
        );
        assert!(squashed.contains(&"x".repeat(90)), "{}", rows.join("\n"));
        // Continuation rows of a numbered line keep the gutter blank.
        assert!(
            rows.iter().any(|r| r.starts_with("│      │ x")),
            "{}",
            rows.join("\n")
        );
        // Soft-wrapped body rows start with the zero-width marker, before
        // the border.
        assert!(
            l.iter()
                .any(|l| crate::chat::text::soft_join(l) != crate::selection::Join::Hard)
        );
        // The copy button still shows (on its own row when narrow).
        assert_eq!(buttons.len(), 1);
        let b = &buttons[0];
        let label: String = rows[b.line].chars().skip(b.col).take(b.width).collect();
        assert_eq!(label, CardCopy::Output.label(), "{}", rows.join("\n"));
    }

    #[test]
    fn narrow_cards_never_exceed_their_width() {
        let theme = Theme::default();
        let mut card = ToolCard::new(
            "t1",
            "bash",
            json!({"command": "cargo test --workspace"}),
            TS,
        );
        card.finish(false, "error: boom\n[exit code 101]".into(), 8_100);
        for w in [5, 8, 12, 20, 33] {
            let c = CardCtx {
                width: w,
                copy_buttons: true,
                ..ctx(&theme)
            };
            let rows: Vec<String> = card.render(&c).iter().map(text).collect();
            assert_boxed(&rows, w);
            let all: String = rows
                .concat()
                .chars()
                .filter(|c| !c.is_whitespace() && *c != '│' && *c != '─')
                .collect();
            for want in ["cargotest--workspace", "exit101", "error:boom"] {
                assert!(all.contains(want), "{want} at {w}:\n{}", rows.join("\n"));
            }
        }
    }

    #[test]
    fn compact_cards_wrap_their_summary_and_details() {
        let theme = Theme::default();
        let long = "a todo title that is much longer than the forty columns we have here_end";
        let mut card = ToolCard::new(
            "t1",
            "todo",
            json!({"todos": [{"content": long, "status": "pending"}]}),
            TS,
        );
        card.finish(true, "ok".into(), 1);
        card.toggle();
        let c = CardCtx {
            width: 30,
            ..ctx(&theme)
        };
        let rows: Vec<String> = card.render(&c).iter().map(text).collect();
        for r in &rows {
            assert!(crate::chat::text::width(r) <= 30, "{r}");
            assert!(!r.contains('…'), "{r}");
        }
        let all: String = rows.concat().split_whitespace().collect();
        assert!(all.contains(&long.replace(' ', "")), "{}", rows.join("\n"));
    }

    #[test]
    fn a_header_that_just_fits_keeps_its_gap_before_the_rule() {
        let theme = Theme::default();
        for n in 20..60 {
            let mut card = ToolCard::new("t1", "bash", json!({"command": "x".repeat(n)}), TS);
            card.finish(true, "ok".into(), 8);
            let rows: Vec<String> = card.render(&ctx(&theme)).iter().map(text).collect();
            assert!(!rows[0].contains('…'), "{}", rows[0]);
            assert!(
                !rows[0].contains("x─"),
                "no gap before the rule: {}",
                rows[0]
            );
            assert_eq!(crate::chat::text::width(&rows[0]), 70);
        }
    }
}
