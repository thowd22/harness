//! The update dialog: a newer harness release is out, with the command
//! that installs it.
//!
//! ```text
//!        ╭ Update available ─────────────────────────────────╮
//!        │ A new harness release is available                │
//!        │                                                   │
//!        │ Current version  0.4.0                            │
//!        │ New version      0.5.0                            │
//!        │                                                   │
//!        │ Release notes:                                    │
//!        │ https://github.com/thowd22/harness/releases/tag/v │
//!        │ 0.5.0                                             │
//!        │                                                   │
//!        │ Install it with:                                  │
//!        │ curl -fsSL https://raw.githubusercontent.com/thow │
//!        │ d22/harness/main/install.sh | bash                │
//!        │                                                   │
//!        │ [ copy install command ]  [ copy link ]  [ close ] │
//!        │ Tab move · Enter select · Esc close               │
//!        ╰───────────────────────────────────────────────────╯
//! ```
//!
//! The link and the command are cut into rows at the dialog width, never
//! shortened; the link's rows are OSC 8 hyperlinks ([`Hyperlink`]). Every
//! button is clickable and reachable with Tab and the arrows; Enter
//! activates the focused one (the copy-command button has the focus when
//! the dialog opens). Copies go through the app's clipboard (OSC 52, so
//! they work over SSH). The dialog closes only with Esc or `[ close ]`
//! ([`UpdateOutcome::Dismiss`], which the core records so the version is
//! not offered again); other keys and clicks outside it do nothing.

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};

use crate::chat::text::{sanitize, width, wrap_line, wrap_text};
use crate::interaction::{Dispatch, ElementId, HitMap, Interaction};
use crate::signin::{Hyperlink, chunks};
use crate::theme::{Theme, Token};

/// Element id of the copy-install-command button.
pub const COPY_COMMAND_ID: &str = "update.copy_command";
/// Element id of the copy-link button.
pub const COPY_LINK_ID: &str = "update.copy_link";
/// Element id of the close button.
pub const CLOSE_ID: &str = "update.close";
/// Widest dialog.
pub const MAX_WIDTH: u16 = 76;
/// Dialog title.
pub const TITLE: &str = " Update available ";
/// The key help line.
pub const HINT: &str = "Tab move · Enter select · Esc close";

/// A newer release, as the core found it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateNotice {
    /// The running version (`0.4.0`).
    pub current: String,
    /// The newer version (`0.5.0`).
    pub latest: String,
    /// Its release-notes page.
    pub url: String,
    /// The command that installs it.
    pub command: String,
}

/// What a clicked or activated button does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpdateAction {
    /// Copy the install command.
    CopyCommand,
    /// Copy the release-notes link.
    CopyLink,
    /// Close the dialog, as Esc does.
    Close,
}

/// What the app should do after an input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateOutcome {
    /// Nothing (the dialog stays open).
    Ignored,
    /// Handled; redraw.
    Changed,
    /// Copy `text` (with the app's clipboard) and toast it as `what`.
    Copy {
        /// For the toast.
        what: &'static str,
        /// What to copy.
        text: String,
    },
    /// Esc or `[ close ]`: close the dialog and do not offer this version
    /// again.
    Dismiss,
}

/// The update dialog.
#[derive(Debug)]
pub struct UpdateModal {
    notice: UpdateNotice,
    /// First body row after a manual scroll (`None`: the top).
    top: Option<usize>,
    /// First body row and largest first row of the last frame.
    last: std::cell::Cell<(usize, usize)>,
}

impl UpdateModal {
    /// A dialog for `notice`.
    pub fn new(notice: UpdateNotice) -> Self {
        Self {
            notice,
            top: None,
            last: std::cell::Cell::new((0, 0)),
        }
    }

    /// What it shows.
    pub fn notice(&self) -> &UpdateNotice {
        &self.notice
    }

    /// Scrolls the dialog by `lines` rows (the wheel, PageUp/PageDown).
    pub fn scroll(&mut self, lines: i32) -> UpdateOutcome {
        let (first, max) = self.last.get();
        let base = self.top.unwrap_or(first) as i64;
        self.top = Some((base + i64::from(lines)).clamp(0, max as i64) as usize);
        UpdateOutcome::Changed
    }

    /// Handles a key: Tab and the arrows move between the buttons, Enter
    /// activates the focused one, Esc dismisses, everything else is
    /// swallowed.
    pub fn handle_key(
        &mut self,
        key: &KeyEvent,
        interaction: &mut Interaction<UpdateAction>,
    ) -> UpdateOutcome {
        if key.kind == KeyEventKind::Release {
            return UpdateOutcome::Ignored;
        }
        match key.code {
            KeyCode::Esc => return UpdateOutcome::Dismiss,
            KeyCode::PageUp => return self.scroll(-5),
            KeyCode::PageDown => return self.scroll(5),
            _ => {}
        }
        match interaction.handle_key(key) {
            Some(Dispatch::Activate(a)) => self.activate(a),
            Some(Dispatch::Scroll { lines, .. }) => self.scroll(lines),
            Some(Dispatch::Focus(_)) => UpdateOutcome::Changed,
            Some(Dispatch::Back) | None => UpdateOutcome::Ignored,
        }
    }

    /// Handles a mouse event: a click on a button activates it, the wheel
    /// scrolls, and a click outside the dialog does nothing.
    pub fn handle_mouse(
        &mut self,
        mouse: &crossterm::event::MouseEvent,
        interaction: &mut Interaction<UpdateAction>,
    ) -> UpdateOutcome {
        match interaction.handle_mouse(mouse) {
            Some(Dispatch::Activate(a)) => self.activate(a),
            Some(Dispatch::Scroll { lines, .. }) => self.scroll(lines),
            Some(Dispatch::Focus(_)) => UpdateOutcome::Changed,
            Some(Dispatch::Back) | None => UpdateOutcome::Ignored,
        }
    }

    /// Runs a button's action.
    pub fn activate(&mut self, action: UpdateAction) -> UpdateOutcome {
        match action {
            UpdateAction::CopyCommand => UpdateOutcome::Copy {
                what: "install command",
                text: self.notice.command.clone(),
            },
            UpdateAction::CopyLink => UpdateOutcome::Copy {
                what: "release notes link",
                text: self.notice.url.clone(),
            },
            UpdateAction::Close => UpdateOutcome::Dismiss,
        }
    }

    /// The dialog body for a content width `w`, and the rows that hold the
    /// link.
    fn body(&self, theme: &Theme, w: usize) -> (Vec<Line<'static>>, Vec<usize>) {
        let mut lines = Vec::new();
        let mut links = Vec::new();
        let text = |lines: &mut Vec<Line<'static>>, s: &str, style: Style| {
            lines.extend(wrap_line(
                Line::from(Span::styled(sanitize(s), style)),
                w,
                &[],
            ));
        };
        text(
            &mut lines,
            "A new harness release is available",
            theme.fg(Token::Fg).add_modifier(Modifier::BOLD),
        );
        lines.push(Line::default());
        for (label, version, token) in [
            ("Current version  ", &self.notice.current, Token::Muted),
            ("New version      ", &self.notice.latest, Token::Ok),
        ] {
            lines.extend(wrap_line(
                Line::from(vec![
                    Span::styled(label, theme.fg(Token::Muted)),
                    Span::styled(
                        sanitize(version),
                        theme.fg(token).add_modifier(Modifier::BOLD),
                    ),
                ]),
                w,
                &[],
            ));
        }
        lines.push(Line::default());
        text(&mut lines, "Release notes:", theme.fg(Token::Fg));
        let link = theme.fg(Token::Accent).add_modifier(Modifier::UNDERLINED);
        for chunk in chunks(&sanitize(&self.notice.url), w) {
            links.push(lines.len());
            lines.push(Line::from(Span::styled(chunk, link)));
        }
        lines.push(Line::default());
        text(&mut lines, "Install it with:", theme.fg(Token::Fg));
        for chunk in chunks(&sanitize(&self.notice.command), w) {
            lines.push(Line::from(Span::styled(chunk, theme.fg(Token::Fg))));
        }
        lines.push(Line::default());
        (lines, links)
    }

    /// Draws the dialog over `area`, registers its buttons, and adds the
    /// link's rows to `links`.
    pub fn render<A: Clone>(
        &self,
        frame: &mut Frame<'_>,
        area: Rect,
        theme: &Theme,
        hits: &mut HitMap<A>,
        wrap: &dyn Fn(UpdateAction) -> A,
        links: &mut Vec<Hyperlink>,
    ) {
        let w = MAX_WIDTH
            .min(area.width.saturating_sub(4))
            .max(20)
            .min(area.width);
        let cw = w.saturating_sub(4) as usize;
        if cw == 0 {
            return;
        }
        let (body, link_rows) = self.body(theme, cw);
        let buttons = [
            (
                COPY_COMMAND_ID,
                "[ copy install command ]",
                UpdateAction::CopyCommand,
            ),
            (COPY_LINK_ID, "[ copy link ]", UpdateAction::CopyLink),
            (CLOSE_ID, "[ close ]", UpdateAction::Close),
        ];
        let mut placed = Vec::new();
        let (mut bx, mut brow) = (0usize, 0usize);
        for (id, label, action) in buttons {
            let tw = width(label);
            if bx > 0 && bx + tw > cw {
                brow += 1;
                bx = 0;
            }
            placed.push((id, label, action, brow, bx));
            bx += tw + 2;
        }
        let button_rows = brow + 1;
        let hint = wrap_text(HINT, cw);
        let foot = (button_rows + hint.len()) as u16;
        let h = (body.len() as u16 + foot + 2).min(area.height);
        if h < 3 {
            return;
        }
        let rect = Rect::new(
            area.x + (area.width - w) / 2,
            area.y + (area.height - h) / 2,
            w,
            h,
        );
        hits.push_modal(rect);
        frame.render_widget(Clear, rect);
        let block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(theme.border(true))
            .title(Span::styled(TITLE, theme.fg(Token::Accent)))
            .style(theme.base());
        let inner = block.inner(rect);
        frame.render_widget(block, rect);
        let inner = Rect {
            x: inner.x + 1,
            width: inner.width.saturating_sub(2),
            ..inner
        };
        if inner.height < 2 {
            return;
        }
        let foot = foot.min(inner.height.saturating_sub(1));
        let rows = (inner.height - foot) as usize;
        hits.register_pane(
            "update",
            Rect {
                height: rows as u16,
                ..inner
            },
        );
        let total = body.len();
        let max = total.saturating_sub(rows);
        let first = self.top.unwrap_or(0).min(max);
        self.last.set((first, max));
        for (row, line) in body.into_iter().enumerate().skip(first).take(rows) {
            let y = inner.y + (row - first) as u16;
            if link_rows.contains(&row) {
                let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
                let style = line.spans.first().map_or(Style::default(), |s| s.style);
                links.push(Hyperlink {
                    x: inner.x,
                    y,
                    text,
                    url: self.notice.url.clone(),
                    style: theme.base().patch(style),
                });
            }
            frame.render_widget(Paragraph::new(line), Rect::new(inner.x, y, inner.width, 1));
        }
        if max > 0 {
            let pos = format!(" {}-{}/{} ", first + 1, first + rows, total);
            let pw = width(&pos) as u16;
            if rect.width > pw + 2 {
                frame.render_widget(
                    Paragraph::new(Span::styled(pos, theme.fg(Token::Muted))),
                    Rect::new(rect.right() - pw - 1, rect.bottom() - 1, pw, 1),
                );
            }
        }
        let by = inner.y + rows as u16;
        for (id, label, action, row, x) in placed {
            let y = by + row as u16;
            let x = inner.x + x as u16;
            if y >= inner.bottom() || x >= inner.right() {
                continue;
            }
            let bw = (width(label) as u16).min(inner.right() - x);
            let r = Rect::new(x, y, bw, 1);
            let mut style = theme.fg(Token::Accent);
            if hits.is_focused(&ElementId::from(id)) {
                style = style.add_modifier(Modifier::REVERSED | Modifier::BOLD);
            }
            frame.render_widget(Paragraph::new(Span::styled(label, style)), r);
            hits.register_button(id, r, wrap(action));
        }
        for (i, t) in hint.into_iter().enumerate() {
            let y = by + (button_rows + i) as u16;
            if y >= inner.bottom() {
                break;
            }
            frame.render_widget(
                Paragraph::new(Span::styled(t, theme.fg(Token::Muted))),
                Rect::new(inner.x, y, inner.width, 1),
            );
        }
    }
}

#[cfg(test)]
mod tests;
