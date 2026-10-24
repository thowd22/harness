//! Inline approval prompts.
//!
//! When the policy engine asks, the prompt appears inline above the input
//! (never as a modal), one card per request:
//!
//! ```text
//! ╭ ⏸ approve bash  $ git push origin main ─────────────── outward action ╮
//! │ rule: ini · bash.ask: git push* (asks before pushing)                  │
//! │ [y] once  [s] session: git push *  [a] always  [n] deny                │
//! ╰────────────────────────────────────────────────────────────────────────╯
//! ```
//!
//! Writes and edits show a diff preview, bash shows the parsed command and
//! the matching rule. Keys act on the focused request: `y` approves once,
//! `s` approves for the session with the suggested pattern, `a` approves
//! always (proposes an ini rule), `n` denies, Up/Down move between
//! requests. For a batch, `Y` approves all remaining once and `N` denies
//! them; Esc denies everything still open. Every button is clickable.
//!
//! Nothing is cut: the subject, rule and buttons wrap inside the card. A
//! long preview shows [`PREVIEW_LINES`] lines and a `… N more lines`
//! button (`e` or a click shows all, again collapses). When the cards are
//! taller than their area, a hint row says so and PageUp/PageDown,
//! Home/End or the wheel scroll them.
//!
//! [`ApprovalQueue`] takes `harness_policy::approvals::PromptEnvelope`s from
//! `ChannelUi`'s receiver and replies on the envelope once every request in
//! the batch has an answer. [`IniConfirm`] is the separate confirmation
//! shown before an "always" rule is written to `harness.ini`.

use std::collections::VecDeque;
use std::path::PathBuf;

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use harness_policy::approvals::{
    ApprovalAnswer, ApprovalPrompt, ApprovalRequest, ApprovalScope, IniEditError, IniRuleProposal,
    PromptEnvelope,
};
use harness_policy::rules::Action;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use tokio::sync::oneshot;

use crate::cards::{diff_lines, parse_unified};
use crate::chat::highlight;
use crate::chat::text::{
    line_width, right_align, sanitize, take_wrap_marker, width, wrap_line, wrap_text,
};
use crate::interaction::{ElementId, HitMap};
use crate::theme::{Theme, Token};

/// Diff preview lines shown before `… N more lines`.
pub const PREVIEW_LINES: usize = 12;
/// Scroll pane id of the approval cards (the wheel scrolls them).
pub const APPROVAL_PANE: &str = "approvals";

/// Buttons of the approval cards.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalAction {
    /// Approve request `n` once.
    Once(usize),
    /// Approve request `n` for the session.
    Session(usize),
    /// Approve request `n` always.
    Always(usize),
    /// Deny request `n`.
    Deny(usize),
    /// Approve every open request once.
    AllOnce,
    /// Deny every open request.
    AllDeny,
    /// Answer the ini rule confirmation.
    ConfirmIni(bool),
    /// Show all (or collapse) request `n`'s preview.
    Preview(usize),
}

/// Result of an approval event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApprovalOutcome {
    /// Not handled.
    Ignored,
    /// Handled; redraw.
    Changed,
    /// Every request of batch `batch` is answered.
    Answered {
        /// Batch number.
        batch: u64,
        /// One answer per request id.
        answers: Vec<(String, ApprovalAnswer)>,
    },
}

/// One prompt being answered.
#[derive(Debug, Clone)]
pub struct ApprovalPanel {
    prompt: ApprovalPrompt,
    answers: Vec<Option<ApprovalAnswer>>,
    focus: usize,
    /// Previews shown in full.
    expanded: Vec<bool>,
    /// First row shown after a manual scroll; `None` follows the focus.
    top: Option<usize>,
    /// First row and largest first row of the last frame.
    last: std::cell::Cell<(usize, usize)>,
}

fn deny(reason: &str) -> ApprovalAnswer {
    ApprovalAnswer::Deny {
        reason: Some(reason.to_owned()),
    }
}

impl ApprovalPanel {
    /// A panel for `prompt`, focused on its first request.
    pub fn new(prompt: ApprovalPrompt) -> ApprovalPanel {
        let n = prompt.requests.len();
        ApprovalPanel {
            prompt,
            answers: vec![None; n],
            focus: 0,
            expanded: vec![false; n],
            top: None,
            last: std::cell::Cell::new((0, 0)),
        }
    }

    /// Scrolls the cards by `lines` rows (the wheel, PageUp/PageDown).
    pub fn scroll(&mut self, lines: i32) {
        let (skip, max) = self.last.get();
        let base = self.top.unwrap_or(skip) as i64;
        self.top = Some((base + i64::from(lines)).clamp(0, max as i64) as usize);
    }

    /// The prompt.
    pub fn prompt(&self) -> &ApprovalPrompt {
        &self.prompt
    }

    /// Index of the focused request.
    pub fn focus(&self) -> usize {
        self.focus
    }

    /// The answer given to request `i`, if any.
    pub fn answer_of(&self, i: usize) -> Option<&ApprovalAnswer> {
        self.answers.get(i).and_then(Option::as_ref)
    }

    /// Requests still open.
    pub fn open_count(&self) -> usize {
        self.answers.iter().filter(|a| a.is_none()).count()
    }

    fn done(&mut self) -> ApprovalOutcome {
        if self.open_count() > 0 {
            if let Some(next) = (0..self.answers.len())
                .map(|k| (self.focus + k) % self.answers.len())
                .find(|&k| self.answers[k].is_none())
            {
                self.focus = next;
            }
            return ApprovalOutcome::Changed;
        }
        ApprovalOutcome::Answered {
            batch: self.prompt.batch,
            answers: self
                .prompt
                .requests
                .iter()
                .zip(&self.answers)
                .map(|(r, a)| {
                    (
                        r.approval_id.clone(),
                        a.clone().unwrap_or_else(|| deny("not answered")),
                    )
                })
                .collect(),
        }
    }

    fn scoped(&self, i: usize, always: bool) -> Option<ApprovalAnswer> {
        let pattern = self.prompt.requests.get(i)?.suggested_pattern.clone()?;
        Some(ApprovalAnswer::Approve(if always {
            ApprovalScope::Always { pattern }
        } else {
            ApprovalScope::Session { pattern }
        }))
    }

    /// Answers request `i`.
    pub fn answer(&mut self, i: usize, answer: ApprovalAnswer) -> ApprovalOutcome {
        let Some(slot) = self.answers.get_mut(i) else {
            return ApprovalOutcome::Ignored;
        };
        *slot = Some(answer);
        self.focus = i;
        self.top = None;
        self.done()
    }

    /// Answers every open request.
    pub fn answer_all(&mut self, answer: ApprovalAnswer) -> ApprovalOutcome {
        for slot in self.answers.iter_mut().filter(|a| a.is_none()) {
            *slot = Some(answer.clone());
        }
        self.done()
    }

    /// Runs a clicked button.
    pub fn activate(&mut self, action: ApprovalAction) -> ApprovalOutcome {
        match action {
            ApprovalAction::Once(i) => self.answer(i, ApprovalAnswer::Approve(ApprovalScope::Once)),
            ApprovalAction::Session(i) => match self.scoped(i, false) {
                Some(a) => self.answer(i, a),
                None => ApprovalOutcome::Ignored,
            },
            ApprovalAction::Always(i) => match self.scoped(i, true) {
                Some(a) => self.answer(i, a),
                None => ApprovalOutcome::Ignored,
            },
            ApprovalAction::Deny(i) => self.answer(i, deny("denied by user")),
            ApprovalAction::AllOnce => {
                self.answer_all(ApprovalAnswer::Approve(ApprovalScope::Once))
            }
            ApprovalAction::AllDeny => self.answer_all(deny("denied by user")),
            ApprovalAction::ConfirmIni(_) => ApprovalOutcome::Ignored,
            ApprovalAction::Preview(i) => match self.expanded.get_mut(i) {
                Some(e) => {
                    *e = !*e;
                    ApprovalOutcome::Changed
                }
                None => ApprovalOutcome::Ignored,
            },
        }
    }

    /// Handles a key.
    pub fn handle_key(&mut self, key: &KeyEvent) -> ApprovalOutcome {
        if key.kind == KeyEventKind::Release {
            return ApprovalOutcome::Ignored;
        }
        if !key.modifiers.difference(KeyModifiers::SHIFT).is_empty() {
            return ApprovalOutcome::Ignored;
        }
        let i = self.focus;
        let n = self.answers.len();
        match key.code {
            KeyCode::Char('y') => self.activate(ApprovalAction::Once(i)),
            KeyCode::Char('s') => self.activate(ApprovalAction::Session(i)),
            KeyCode::Char('a') => self.activate(ApprovalAction::Always(i)),
            KeyCode::Char('n') => self.activate(ApprovalAction::Deny(i)),
            KeyCode::Char('Y') if n > 1 => self.activate(ApprovalAction::AllOnce),
            KeyCode::Char('N') if n > 1 => self.activate(ApprovalAction::AllDeny),
            KeyCode::Char('e') => self.activate(ApprovalAction::Preview(i)),
            KeyCode::Esc => self.answer_all(deny("dismissed")),
            KeyCode::Up if n > 1 => {
                self.focus = (self.focus + n - 1) % n;
                self.top = None;
                ApprovalOutcome::Changed
            }
            KeyCode::Down if n > 1 => {
                self.focus = (self.focus + 1) % n;
                self.top = None;
                ApprovalOutcome::Changed
            }
            KeyCode::PageUp | KeyCode::PageDown => {
                let page = 5;
                self.scroll(if key.code == KeyCode::PageUp {
                    -page
                } else {
                    page
                });
                ApprovalOutcome::Changed
            }
            KeyCode::Home => {
                self.top = Some(0);
                ApprovalOutcome::Changed
            }
            KeyCode::End => {
                self.top = Some(self.last.get().1);
                ApprovalOutcome::Changed
            }
            _ => ApprovalOutcome::Ignored,
        }
    }

    /// Lines and buttons at `width`.
    fn layout(
        &self,
        width: usize,
        theme: &Theme,
        focused_el: Option<&ElementId>,
    ) -> (Vec<Line<'static>>, Vec<Button>) {
        let mut lines = Vec::new();
        let mut buttons = Vec::new();
        let n = self.prompt.requests.len();
        if n > 1 {
            let items = vec![
                Item::Text(Span::styled(
                    format!("⏸ {n} approvals · {} open", self.open_count()),
                    theme.fg(Token::Approval).add_modifier(Modifier::BOLD),
                )),
                button(
                    "[Y] approve all once",
                    ApprovalAction::AllOnce,
                    Token::Ok,
                    theme,
                    focused_el,
                ),
                button(
                    "[N] deny all",
                    ApprovalAction::AllDeny,
                    Token::Error,
                    theme,
                    focused_el,
                ),
            ];
            let (l, b) = flow(items, width.max(1));
            buttons.extend(b.into_iter().map(|mut b| {
                b.row += lines.len();
                b
            }));
            lines.extend(l);
        }
        for (i, r) in self.prompt.requests.iter().enumerate() {
            let focused = i == self.focus;
            match &self.answers[i] {
                Some(a) => lines.extend(answered_line(r, a, theme, width)),
                None => self.request_lines(
                    i,
                    r,
                    focused,
                    width,
                    theme,
                    focused_el,
                    &mut lines,
                    &mut buttons,
                ),
            }
        }
        (lines, buttons)
    }

    #[allow(clippy::too_many_arguments)]
    fn request_lines(
        &self,
        i: usize,
        r: &ApprovalRequest,
        focused: bool,
        width: usize,
        theme: &Theme,
        focused_el: Option<&ElementId>,
        lines: &mut Vec<Line<'static>>,
        buttons: &mut Vec<Button>,
    ) {
        let tint = theme.bg(Token::CardApproval);
        let border = if focused {
            theme.fg(Token::BorderFocus)
        } else {
            theme.fg(Token::Approval)
        };
        let inner = width.saturating_sub(4);
        let (subject, path) = match &r.action {
            Action::Bash { command } => (
                format!("$ {}", r.verdict.command.as_deref().unwrap_or(command)),
                None,
            ),
            Action::Read { path, .. } | Action::Write { path, .. } => {
                (path.display().to_string(), Some(path.clone()))
            }
            Action::Tool { name } => (name.clone(), None),
        };
        let subject_token = if path.is_some() {
            Token::Path
        } else {
            Token::Fg
        };
        let why = if r.verdict.outward {
            "outward action".to_owned()
        } else {
            r.verdict.rule.reason.clone()
        };
        let head = vec![
            Span::styled("╭ ", border),
            Span::styled("⏸ approve ", theme.fg(Token::Approval)),
            Span::styled(
                r.action.tool().to_owned(),
                theme.fg(Token::Accent).add_modifier(Modifier::BOLD),
            ),
        ];
        let subject = Span::styled(format!("  {}", sanitize(&subject)), theme.fg(subject_token));
        let why_span = Span::styled(sanitize(&why), theme.fg(Token::Approval));
        let mut left = head.clone();
        left.push(subject.clone());
        left.push(Span::styled(" ", border));
        let right = vec![
            Span::styled(" ", border),
            why_span.clone(),
            Span::styled(" ╮", border),
        ];
        let fill = Span::styled("─", border);
        let one_row = line_width(&Line::from(left.clone()))
            + right
                .iter()
                .map(|s| crate::chat::text::width(&s.content))
                .sum::<usize>()
            <= width;
        // Rows inside the box: wrapped, the soft-wrap marker kept first.
        let boxed = |l: Line<'static>| -> Vec<Line<'static>> {
            wrap_line(l, inner.max(1), &[])
                .into_iter()
                .map(|mut l| {
                    let marker = take_wrap_marker(&mut l);
                    let w = line_width(&l);
                    let mut spans: Vec<Span<'static>> = marker.into_iter().collect();
                    spans.push(Span::styled("│ ", border));
                    spans.extend(l.spans);
                    spans.push(Span::raw(" ".repeat(inner.saturating_sub(w))));
                    spans.push(Span::styled(" │", border));
                    Line::from(spans)
                })
                .collect()
        };
        let mut block = Vec::new();
        if one_row {
            block.push(right_align(Line::from(left), right, width, fill));
        } else {
            // The subject and reason move into the box, wrapped.
            let mut h = head.clone();
            h.push(Span::styled(" ", border));
            let fits = line_width(&Line::from(h.clone())) + 2 <= width;
            let h = if fits {
                h
            } else {
                vec![Span::styled("╭", border)]
            };
            block.push(right_align(
                Line::from(h),
                vec![Span::styled(" ╮", border)],
                width,
                fill,
            ));
            if !fits {
                let mut t = head[1..].to_vec();
                t.push(subject.clone());
                block.extend(boxed(Line::from(t)));
            } else {
                block.extend(boxed(Line::from(Span::styled(
                    sanitize(&subject.content).trim_start().to_owned(),
                    subject.style,
                ))));
            }
            block.extend(boxed(Line::from(why_span)));
        }
        // Matching rule.
        let rule = &r.verdict.rule;
        let layer = format!("{:?}", rule.layer).to_lowercase();
        let mut rule_spans = vec![
            Span::styled("rule: ", theme.fg(Token::Muted)),
            Span::styled(
                format!("{layer} · {}", sanitize(&rule.origin)),
                theme.fg(Token::Muted),
            ),
        ];
        if !rule.rule.is_empty() {
            rule_spans.push(Span::styled(
                format!(": {}", sanitize(&rule.rule)),
                theme.fg(Token::Fg),
            ));
        }
        if !rule.reason.is_empty() && rule.reason != why {
            rule_spans.push(Span::styled(
                format!(" ({})", sanitize(&rule.reason)),
                theme.fg(Token::Muted),
            ));
        }
        block.extend(boxed(Line::from(rule_spans)));
        // Preview: a diff for writes and edits, text otherwise.
        let mut extra: Vec<Button> = Vec::new();
        if let Some(p) = &r.preview {
            let rows = parse_unified(p);
            let is_diff = rows
                .iter()
                .any(|r| r.kind != crate::cards::DiffKind::Context)
                || p.starts_with("--- ");
            let preview: Vec<Line<'static>> = if is_diff {
                let lang = path
                    .as_ref()
                    .and_then(|p| highlight::lang_for(&p.display().to_string()));
                diff_lines(&rows, lang, theme)
            } else {
                p.lines()
                    .map(|l| Line::from(Span::styled(sanitize(l), theme.fg(Token::Fg))))
                    .collect()
            };
            let total = preview.len();
            let all = self.expanded.get(i).copied().unwrap_or(false);
            let shown = if all { total } else { PREVIEW_LINES };
            for l in preview.into_iter().take(shown) {
                block.extend(boxed(l));
            }
            if total > PREVIEW_LINES {
                let label = if all {
                    "▴ show less (e)".to_owned()
                } else {
                    format!(
                        "… {} more lines · e or click to show all",
                        total - PREVIEW_LINES
                    )
                };
                let action = ApprovalAction::Preview(i);
                let mut st = theme.fg(Token::Link);
                if focused_el.is_some_and(|f| f.0 == button_id(action)) {
                    st = st.patch(theme.focused());
                }
                let rows = boxed(Line::from(Span::styled(label, st)));
                extra.push(Button {
                    row: lines.len() + block.len(),
                    x: 2,
                    w: inner,
                    h: rows.len(),
                    action,
                });
                block.extend(rows);
            }
        }
        // Buttons.
        let mut items = vec![button(
            "[y] once",
            ApprovalAction::Once(i),
            Token::Ok,
            theme,
            focused_el,
        )];
        if let Some(pat) = &r.suggested_pattern {
            items.push(button(
                &format!("[s] session: {}", sanitize(pat)),
                ApprovalAction::Session(i),
                Token::Ok,
                theme,
                focused_el,
            ));
            items.push(button(
                "[a] always",
                ApprovalAction::Always(i),
                Token::Warn,
                theme,
                focused_el,
            ));
        }
        items.push(button(
            "[n] deny",
            ApprovalAction::Deny(i),
            Token::Error,
            theme,
            focused_el,
        ));
        items.push(Item::Text(Span::styled(
            "esc denies all",
            theme.fg(Token::Muted),
        )));
        let (rows, btns) = flow(items, inner.max(1));
        let base = lines.len() + block.len();
        buttons.extend(extra);
        buttons.extend(btns.into_iter().map(|mut b| {
            b.row += base;
            b.x += 2; // inside "│ "
            b
        }));
        for l in rows {
            block.extend(boxed(l));
        }
        block.push(Line::from(vec![
            Span::styled("╰", border),
            Span::styled("─".repeat(width.saturating_sub(2)), border),
            Span::styled("╯", border),
        ]));
        lines.extend(block.into_iter().map(|l| l.patch_style(tint)));
    }

    /// Rows needed at `width`.
    pub fn height(&self, width: u16, theme: &Theme) -> u16 {
        self.layout(width as usize, theme, None).0.len() as u16
    }

    /// Draws the panel into `area` and registers its buttons. When `area`
    /// is shorter than the panel, the focused request is kept in view.
    pub fn render<A: Clone>(
        &self,
        frame: &mut Frame<'_>,
        area: Rect,
        theme: &Theme,
        hits: &mut HitMap<A>,
        wrap: &dyn Fn(ApprovalAction) -> A,
    ) {
        let focused_el = self.focused_element(hits);
        let (lines, buttons) = self.layout(area.width as usize, theme, focused_el.as_ref());
        let total = lines.len();
        // Too tall: the last row says how to see the rest.
        let over = total > area.height as usize && area.height > 1;
        let h = if over {
            area.height as usize - 1
        } else {
            area.height as usize
        };
        let max = total.saturating_sub(h);
        // Keep the focused request's buttons visible unless scrolled.
        let focus_row = buttons
            .iter()
            .find(|b| b.action == ApprovalAction::Once(self.focus))
            .map_or(0, |b| b.row);
        let auto = (focus_row + 2).saturating_sub(h).min(max);
        let skip = self.top.map_or(auto, |t| t.min(max));
        self.last.set((skip, max));
        hits.register_pane(APPROVAL_PANE, area);
        for b in &buttons {
            let top = b.row.max(skip);
            let bottom = (b.row + b.h).min(skip + h);
            if bottom <= top {
                continue;
            }
            let x = area.x + b.x as u16;
            if x >= area.right() {
                continue;
            }
            hits.register_button(
                b.id(),
                Rect::new(
                    x,
                    area.y + (top - skip) as u16,
                    (b.w as u16).min(area.right() - x),
                    (bottom - top) as u16,
                ),
                wrap(b.action),
            );
        }
        let mut visible: Vec<Line<'static>> = lines.into_iter().skip(skip).take(h).collect();
        if over {
            let pos = format!("↕ {}-{}/{}", skip + 1, skip + h, total);
            let aw = area.width as usize;
            let hint = [
                format!("{pos} · pgup/pgdn or wheel to scroll"),
                format!("{pos} · pgup/pgdn"),
                pos,
            ]
            .into_iter()
            .find(|t| width(t) <= aw)
            .unwrap_or_default();
            visible.push(Line::from(Span::styled(hint, theme.fg(Token::Muted))));
        }
        frame.render_widget(Paragraph::new(visible).style(theme.base()), area);
    }

    fn focused_element<A: Clone>(&self, hits: &HitMap<A>) -> Option<ElementId> {
        let n = self.prompt.requests.len();
        let mut candidates = vec![ApprovalAction::AllOnce, ApprovalAction::AllDeny];
        for i in 0..n {
            candidates.extend([
                ApprovalAction::Once(i),
                ApprovalAction::Session(i),
                ApprovalAction::Always(i),
                ApprovalAction::Deny(i),
                ApprovalAction::Preview(i),
            ]);
        }
        candidates
            .into_iter()
            .map(|a| ElementId(button_id(a)))
            .find(|id| hits.is_focused(id))
    }
}

fn answered_line(
    r: &ApprovalRequest,
    a: &ApprovalAnswer,
    theme: &Theme,
    width: usize,
) -> Vec<Line<'static>> {
    let (icon, text, token) = match a {
        ApprovalAnswer::Approve(ApprovalScope::Once) => {
            ('✔', "approved once".to_owned(), Token::Ok)
        }
        ApprovalAnswer::Approve(ApprovalScope::Session { pattern }) => {
            ('✔', format!("approved for session: {pattern}"), Token::Ok)
        }
        ApprovalAnswer::Approve(ApprovalScope::Always { pattern }) => {
            ('✔', format!("approved always: {pattern}"), Token::Ok)
        }
        ApprovalAnswer::Deny { .. } => ('✗', "denied".to_owned(), Token::Error),
    };
    wrap_line(
        Line::from(vec![
            Span::styled(format!("{icon} "), theme.fg(token)),
            Span::styled(sanitize(&r.action.summary()), theme.fg(Token::Muted)),
            Span::styled(format!(" · {}", sanitize(&text)), theme.fg(token)),
        ]),
        width.max(1),
        &[Span::raw("  ")],
    )
}

/// A piece of a row of buttons.
enum Item {
    /// Plain text.
    Text(Span<'static>),
    /// A button.
    Button(Span<'static>, ApprovalAction),
}

fn button(
    label: &str,
    action: ApprovalAction,
    token: Token,
    theme: &Theme,
    focused: Option<&ElementId>,
) -> Item {
    let mut style: Style = theme.fg(token).add_modifier(Modifier::BOLD);
    if focused.is_some_and(|f| f.0 == button_id(action)) {
        style = style.patch(theme.focused());
    }
    Item::Button(Span::styled(label.to_owned(), style), action)
}

/// Lays `items` out left to right, two cells apart, onto as many rows of
/// `width` as they need. An item wider than a row wraps on rows of its
/// own. Returns the rows and the buttons (rows relative to the first).
fn flow(items: Vec<Item>, width: usize) -> (Vec<Line<'static>>, Vec<Button>) {
    let mut rows: Vec<Vec<Span<'static>>> = vec![Vec::new()];
    let mut used = 0usize;
    let mut buttons = Vec::new();
    for item in items {
        let (span, action) = match item {
            Item::Text(s) => (s, None),
            Item::Button(s, a) => (s, Some(a)),
        };
        let w = crate::chat::text::width(&span.content);
        if w > width {
            if used > 0 {
                rows.push(Vec::new());
            }
            let parts = wrap_text(&span.content, width);
            let row = rows.len() - 1;
            let n = parts.len();
            for (k, p) in parts.into_iter().enumerate() {
                if k > 0 {
                    rows.push(Vec::new());
                }
                rows.last_mut()
                    .expect("a row")
                    .push(Span::styled(p, span.style));
            }
            if let Some(action) = action {
                buttons.push(Button {
                    row,
                    x: 0,
                    w: width,
                    h: n,
                    action,
                });
            }
            rows.push(Vec::new());
            used = 0;
            continue;
        }
        if used > 0 && used + 2 + w > width {
            rows.push(Vec::new());
            used = 0;
        }
        if used > 0 {
            // Text takes its gap in its own style (buttons have plain gaps).
            let gap = if action.is_some() {
                Span::raw("  ")
            } else {
                Span::styled("  ", span.style)
            };
            rows.last_mut().expect("a row").push(gap);
            used += 2;
        }
        if let Some(action) = action {
            buttons.push(Button {
                row: rows.len() - 1,
                x: used,
                w,
                h: 1,
                action,
            });
        }
        rows.last_mut().expect("a row").push(span);
        used += w;
    }
    if rows.len() > 1 && rows.last().is_some_and(Vec::is_empty) {
        rows.pop();
    }
    (rows.into_iter().map(Line::from).collect(), buttons)
}

#[derive(Debug, Clone)]
struct Button {
    row: usize,
    x: usize,
    w: usize,
    /// Rows (a label wider than a row wraps).
    h: usize,
    action: ApprovalAction,
}

impl Button {
    fn id(&self) -> String {
        button_id(self.action)
    }
}

/// Element id of an approval button.
pub fn button_id(a: ApprovalAction) -> String {
    match a {
        ApprovalAction::Once(i) => format!("approval.{i}.once"),
        ApprovalAction::Session(i) => format!("approval.{i}.session"),
        ApprovalAction::Always(i) => format!("approval.{i}.always"),
        ApprovalAction::Deny(i) => format!("approval.{i}.deny"),
        ApprovalAction::AllOnce => "approval.all.once".into(),
        ApprovalAction::AllDeny => "approval.all.deny".into(),
        ApprovalAction::ConfirmIni(true) => "approval.ini.yes".into(),
        ApprovalAction::ConfirmIni(false) => "approval.ini.no".into(),
        ApprovalAction::Preview(i) => format!("approval.{i}.preview"),
    }
}

type Reply = oneshot::Sender<Vec<(String, ApprovalAnswer)>>;

/// Pending approval prompts, answered one batch at a time.
#[derive(Debug, Default)]
pub struct ApprovalQueue {
    queue: VecDeque<(ApprovalPanel, Option<Reply>)>,
}

impl ApprovalQueue {
    /// An empty queue.
    pub fn new() -> ApprovalQueue {
        ApprovalQueue::default()
    }

    /// Queues a prompt from `ChannelUi`; the answers go to its reply channel.
    pub fn push_envelope(&mut self, env: PromptEnvelope) {
        self.queue
            .push_back((ApprovalPanel::new(env.prompt), Some(env.reply)));
    }

    /// Queues a prompt without a reply channel (answers are only returned
    /// as [`ApprovalOutcome::Answered`]).
    pub fn push_prompt(&mut self, prompt: ApprovalPrompt) {
        self.queue.push_back((ApprovalPanel::new(prompt), None));
    }

    /// The prompt being answered.
    pub fn current(&self) -> Option<&ApprovalPanel> {
        self.queue.front().map(|(p, _)| p)
    }

    /// Whether nothing is pending.
    pub fn is_empty(&self) -> bool {
        self.queue.is_empty()
    }

    /// Prompts waiting, including the current one.
    pub fn len(&self) -> usize {
        self.queue.len()
    }

    fn settle(&mut self, out: ApprovalOutcome) -> ApprovalOutcome {
        if let ApprovalOutcome::Answered { answers, .. } = &out
            && let Some((_, reply)) = self.queue.pop_front()
            && let Some(reply) = reply
        {
            // The broker may have given up (cancelled turn); nothing to do then.
            let _ = reply.send(answers.clone());
        }
        out
    }

    /// Handles a key for the current prompt.
    pub fn handle_key(&mut self, key: &KeyEvent) -> ApprovalOutcome {
        let Some((panel, _)) = self.queue.front_mut() else {
            return ApprovalOutcome::Ignored;
        };
        let out = panel.handle_key(key);
        self.settle(out)
    }

    /// Scrolls the current prompt's cards by `lines` rows.
    pub fn scroll(&mut self, lines: i32) {
        if let Some((panel, _)) = self.queue.front_mut() {
            panel.scroll(lines);
        }
    }

    /// Runs a clicked button of the current prompt.
    pub fn activate(&mut self, action: ApprovalAction) -> ApprovalOutcome {
        let Some((panel, _)) = self.queue.front_mut() else {
            return ApprovalOutcome::Ignored;
        };
        let out = panel.activate(action);
        self.settle(out)
    }
}

/// Result of the ini rule confirmation.
#[derive(Debug, PartialEq, Eq)]
pub enum IniConfirmOutcome {
    /// Not handled.
    Ignored,
    /// The rule was appended to `harness.ini`.
    Written,
    /// The user declined.
    Declined,
}

/// "Add `pattern` to \[policy\] bash.allow in harness.ini?" shown after an
/// "always" approval. The file is only changed on yes.
#[derive(Debug, Clone)]
pub struct IniConfirm {
    proposal: IniRuleProposal,
    path: PathBuf,
}

impl IniConfirm {
    /// A confirmation for `proposal`, writing to the config file at `path`.
    pub fn new(proposal: IniRuleProposal, path: PathBuf) -> IniConfirm {
        IniConfirm { proposal, path }
    }

    /// The question.
    pub fn question(&self) -> String {
        self.proposal.describe()
    }

    /// Answers the confirmation; `true` writes the rule.
    pub fn answer(&self, yes: bool) -> Result<IniConfirmOutcome, IniEditError> {
        if yes {
            self.proposal.clone().confirm(&self.path)?;
            Ok(IniConfirmOutcome::Written)
        } else {
            Ok(IniConfirmOutcome::Declined)
        }
    }

    /// `y` / `n` / Esc.
    pub fn handle_key(&self, key: &KeyEvent) -> Result<IniConfirmOutcome, IniEditError> {
        match key.code {
            KeyCode::Char('y') => self.answer(true),
            KeyCode::Char('n') | KeyCode::Esc => self.answer(false),
            _ => Ok(IniConfirmOutcome::Ignored),
        }
    }

    fn rows(
        &self,
        width: usize,
        theme: &Theme,
        focused: Option<&ElementId>,
    ) -> (Vec<Line<'static>>, Vec<Button>) {
        let items = vec![
            Item::Text(Span::styled(
                format!("⏸ {}", sanitize(&self.question())),
                theme.fg(Token::Approval),
            )),
            button(
                "[y] yes",
                ApprovalAction::ConfirmIni(true),
                Token::Ok,
                theme,
                focused,
            ),
            button(
                "[n] no",
                ApprovalAction::ConfirmIni(false),
                Token::Error,
                theme,
                focused,
            ),
        ];
        flow(items, width.max(1))
    }

    /// Rows the confirmation needs at `width` (the question wraps).
    pub fn height(&self, width: u16) -> u16 {
        self.rows(width as usize, &Theme::default(), None).0.len() as u16
    }

    /// Draws the confirmation, wrapped to [`IniConfirm::height`] rows, with
    /// clickable yes / no buttons.
    pub fn render<A: Clone>(
        &self,
        frame: &mut Frame<'_>,
        area: Rect,
        theme: &Theme,
        hits: &mut HitMap<A>,
        wrap: &dyn Fn(ApprovalAction) -> A,
    ) {
        let focused = [true, false]
            .into_iter()
            .map(|b| ElementId(button_id(ApprovalAction::ConfirmIni(b))))
            .find(|id| hits.is_focused(id));
        let (lines, buttons) = self.rows(area.width as usize, theme, focused.as_ref());
        for b in buttons {
            let x = area.x + b.x as u16;
            let y = area.y + b.row as u16;
            if x < area.right() && y < area.bottom() {
                hits.register_button(
                    b.id(),
                    Rect::new(
                        x,
                        y,
                        (b.w as u16).min(area.right() - x),
                        (b.h as u16).min(area.bottom() - y),
                    ),
                    wrap(b.action),
                );
            }
        }
        frame.render_widget(
            Paragraph::new(lines).style(theme.bg(Token::CardApproval)),
            area,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interaction::testing::{buffer_text, find_text};
    use crate::interaction::{Dispatch, Interaction};
    use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
    use harness_policy::rules::{Decision, Layer, MatchedRule, Verdict};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn verdict(cmd: Option<&str>, outward: bool) -> Verdict {
        Verdict {
            decision: Decision::Ask,
            rule: MatchedRule {
                layer: Layer::Ini,
                origin: "harness.ini".into(),
                rule: "bash.ask = git push*".into(),
                reason: "asks before pushing".into(),
            },
            command: cmd.map(Into::into),
            outward,
        }
    }

    fn bash_req(n: usize, cmd: &str) -> ApprovalRequest {
        ApprovalRequest {
            approval_id: format!("approval-1-{n}"),
            call_id: format!("call-{n}"),
            node_id: None,
            action: Action::Bash {
                command: format!("cd x && {cmd}"),
            },
            verdict: verdict(Some(cmd), true),
            preview: None,
            suggested_pattern: Some("git push *".into()),
        }
    }

    fn edit_req(n: usize) -> ApprovalRequest {
        ApprovalRequest {
            approval_id: format!("approval-1-{n}"),
            call_id: format!("call-{n}"),
            node_id: None,
            action: Action::Write {
                tool: "edit".into(),
                path: "src/parse.rs".into(),
            },
            verdict: Verdict {
                outward: false,
                command: None,
                rule: MatchedRule {
                    layer: Layer::Mode,
                    origin: "mode ask".into(),
                    rule: String::new(),
                    reason: "ask mode".into(),
                },
                decision: Decision::Ask,
            },
            preview: Some("--- a/src/parse.rs\n+++ b/src/parse.rs\n@@ -41 +41 @@\n-    .unwrap();\n+    .ok_or(Error::Colon)?;\n".into()),
            suggested_pattern: None,
        }
    }

    fn prompt(reqs: Vec<ApprovalRequest>) -> ApprovalPrompt {
        ApprovalPrompt {
            batch: 1,
            requests: reqs,
        }
    }

    fn key(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
    }

    fn draw(panel: &ApprovalPanel, it: &mut Interaction<ApprovalAction>) -> Terminal<TestBackend> {
        let theme = Theme::default();
        let mut term = Terminal::new(TestBackend::new(90, 20)).unwrap();
        let mut hits = it.begin_frame();
        term.draw(|f| {
            let h = panel.height(90, &theme);
            panel.render(f, Rect::new(0, 0, 90, h.min(20)), &theme, &mut hits, &|a| a)
        })
        .unwrap();
        it.end_frame(hits);
        term
    }

    #[test]
    fn single_request_keys_once_session_always_deny() {
        for (c, expect) in [
            ('y', ApprovalAnswer::Approve(ApprovalScope::Once)),
            (
                's',
                ApprovalAnswer::Approve(ApprovalScope::Session {
                    pattern: "git push *".into(),
                }),
            ),
            (
                'a',
                ApprovalAnswer::Approve(ApprovalScope::Always {
                    pattern: "git push *".into(),
                }),
            ),
            (
                'n',
                ApprovalAnswer::Deny {
                    reason: Some("denied by user".into()),
                },
            ),
        ] {
            let mut p = ApprovalPanel::new(prompt(vec![bash_req(0, "git push origin main")]));
            assert_eq!(
                p.handle_key(&key(c)),
                ApprovalOutcome::Answered {
                    batch: 1,
                    answers: vec![("approval-1-0".into(), expect)]
                }
            );
        }
        let mut p = ApprovalPanel::new(prompt(vec![bash_req(0, "git push")]));
        let out = p.handle_key(&KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(
            matches!(out, ApprovalOutcome::Answered { ref answers, .. } if matches!(answers[0].1, ApprovalAnswer::Deny { .. }))
        );
        // Without a suggested pattern, `s` does nothing.
        let mut p = ApprovalPanel::new(prompt(vec![edit_req(0)]));
        assert_eq!(p.handle_key(&key('s')), ApprovalOutcome::Ignored);
    }

    #[test]
    fn bash_shows_parsed_command_and_rule_edits_show_diff() {
        let p = ApprovalPanel::new(prompt(vec![
            bash_req(0, "git push origin main"),
            edit_req(1),
        ]));
        let mut it = Interaction::new();
        let term = draw(&p, &mut it);
        let text = buffer_text(term.backend().buffer());
        assert!(
            text.contains("⏸ approve bash  $ git push origin main"),
            "{text}"
        );
        assert!(!text.contains("cd x &&"), "shows the parsed simple command");
        assert!(text.contains("outward action"));
        assert!(
            text.contains("rule: ini · harness.ini: bash.ask = git push* (asks before pushing)"),
            "{text}"
        );
        assert!(text.contains("[y] once  [s] session: git push *  [a] always  [n] deny"));
        assert!(text.contains("⏸ approve edit  src/parse.rs"));
        assert!(text.contains("  41 │-    .unwrap();"));
        assert!(text.contains("  41 │+    .ok_or(Error::Colon)?;"));
        let buf = term.backend().buffer();
        let (x, y) = find_text(buf, "+    .ok_or").unwrap();
        assert_eq!(buf[(x, y)].bg, Theme::default().color(Token::DiffAddedBg));
    }

    #[test]
    fn batch_answers_individually_or_together() {
        let reqs = vec![
            bash_req(0, "git push"),
            bash_req(1, "git push --tags"),
            bash_req(2, "gh pr create"),
        ];
        let mut p = ApprovalPanel::new(prompt(reqs.clone()));
        assert_eq!(p.handle_key(&key('y')), ApprovalOutcome::Changed);
        assert_eq!(p.focus(), 1);
        assert_eq!(p.handle_key(&key('n')), ApprovalOutcome::Changed);
        let ApprovalOutcome::Answered { answers, .. } = p.handle_key(&key('y')) else {
            panic!()
        };
        assert_eq!(answers.len(), 3);
        assert!(matches!(answers[1].1, ApprovalAnswer::Deny { .. }));
        // Together.
        let mut p = ApprovalPanel::new(prompt(reqs.clone()));
        p.handle_key(&KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        p.handle_key(&key('n'));
        let ApprovalOutcome::Answered { answers, .. } =
            p.handle_key(&KeyEvent::new(KeyCode::Char('Y'), KeyModifiers::SHIFT))
        else {
            panic!()
        };
        assert_eq!(answers[0].1, ApprovalAnswer::Approve(ApprovalScope::Once));
        assert!(matches!(answers[1].1, ApprovalAnswer::Deny { .. }));
        assert_eq!(answers[2].1, ApprovalAnswer::Approve(ApprovalScope::Once));
        let mut p = ApprovalPanel::new(prompt(reqs));
        let ApprovalOutcome::Answered { answers, .. } =
            p.handle_key(&KeyEvent::new(KeyCode::Char('N'), KeyModifiers::SHIFT))
        else {
            panic!()
        };
        assert!(
            answers
                .iter()
                .all(|a| matches!(a.1, ApprovalAnswer::Deny { .. }))
        );
    }

    #[test]
    fn buttons_are_clickable_and_keyboard_focusable() {
        let mut p = ApprovalPanel::new(prompt(vec![
            bash_req(0, "git push"),
            bash_req(1, "gh pr create"),
        ]));
        let mut it = Interaction::new();
        let term = draw(&p, &mut it);
        assert!(buffer_text(term.backend().buffer()).contains("[Y] approve all once"));
        // Click [n] deny of the first request.
        let (x, y) = find_text(term.backend().buffer(), "[n] deny").unwrap();
        let click = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: x + 1,
            row: y,
            modifiers: KeyModifiers::NONE,
        };
        let Some(Dispatch::Activate(a)) = it.handle_mouse(&click) else {
            panic!("button not clickable")
        };
        assert_eq!(a, ApprovalAction::Deny(0));
        assert_eq!(p.activate(a), ApprovalOutcome::Changed);
        assert!(matches!(p.answer_of(0), Some(ApprovalAnswer::Deny { .. })));
        // Tab to the first button ([Y] approve all once) and press Enter.
        let term = draw(&p, &mut it);
        assert!(buffer_text(term.backend().buffer()).contains("· denied"));
        it.clear_focus();
        it.handle_key(&KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        let Some(Dispatch::Activate(a)) =
            it.handle_key(&KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
        else {
            panic!()
        };
        assert_eq!(a, ApprovalAction::AllOnce);
        assert!(matches!(p.activate(a), ApprovalOutcome::Answered { .. }));
    }

    #[tokio::test]
    async fn queue_replies_on_the_envelope() {
        let (ui, mut rx) = harness_policy::approvals::ChannelUi::new(4);
        let ask = tokio::spawn(async move {
            use harness_policy::approvals::ApprovalUi;
            ui.ask(prompt(vec![bash_req(0, "git push")])).await
        });
        let env = rx.recv().await.unwrap();
        let mut q = ApprovalQueue::new();
        q.push_envelope(env);
        assert_eq!(q.len(), 1);
        assert!(matches!(
            q.handle_key(&key('y')),
            ApprovalOutcome::Answered { .. }
        ));
        assert!(q.is_empty());
        let answers = ask.await.unwrap();
        assert_eq!(
            answers,
            vec![(
                "approval-1-0".into(),
                ApprovalAnswer::Approve(ApprovalScope::Once)
            )]
        );
    }

    #[test]
    fn ini_confirmation_writes_only_on_yes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("harness.ini");
        std::fs::write(&path, "[policy]\n").unwrap();
        let c = IniConfirm::new(
            IniRuleProposal {
                pattern: "git push *".into(),
            },
            path.clone(),
        );
        assert!(c.question().contains("git push *"));
        assert_eq!(
            c.handle_key(&key('n')).unwrap(),
            IniConfirmOutcome::Declined
        );
        assert!(!std::fs::read_to_string(&path).unwrap().contains("git push"));
        assert_eq!(c.handle_key(&key('y')).unwrap(), IniConfirmOutcome::Written);
        assert!(
            std::fs::read_to_string(&path)
                .unwrap()
                .contains("git push *")
        );
        let theme = Theme::default();
        let mut term = Terminal::new(TestBackend::new(90, 1)).unwrap();
        let mut it: Interaction<ApprovalAction> = Interaction::new();
        let mut hits = it.begin_frame();
        term.draw(|f| c.render(f, f.area(), &theme, &mut hits, &|a| a))
            .unwrap();
        it.end_frame(hits);
        let (x, y) = find_text(term.backend().buffer(), "[n] no").unwrap();
        let click = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: x,
            row: y,
            modifiers: KeyModifiers::NONE,
        };
        assert_eq!(
            it.handle_mouse(&click),
            Some(Dispatch::Activate(ApprovalAction::ConfirmIni(false)))
        );
    }
}
