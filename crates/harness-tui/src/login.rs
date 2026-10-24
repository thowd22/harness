//! The `/login` modal: renders a [`LoginFlow`] and maps keys and clicks to
//! it.
//!
//! ```text
//!        ╭ Log in ───────────────────────────────────────────╮
//!        │ OpenRouter: API key                               │
//!        │                                                   │
//!        │ API key › ••••••••••••                            │
//!        │                                                   │
//!        │ API key only.                                     │
//!        │ [ submit ]  [ back ]                              │
//!        │ Enter submit · Esc back                           │
//!        ╰───────────────────────────────────────────────────╯
//! ```
//!
//! Menu steps (provider, storage method) list their options; Up / Down /
//! Tab move, Enter chooses, a click on an option chooses it. On the value
//! step typed and pasted text goes into the field; an API key is shown only
//! as `•` characters. Every button is clickable and has a key (Enter /
//! Esc), and a click outside the dialog backs out.
//!
//! Every text wraps inside the dialog. When the dialog is taller than the
//! screen, it keeps the selected option or the field in view, PageUp /
//! PageDown and the wheel scroll it, and the bottom border shows the
//! position.
//!
//! The modal never touches the network or the file system: a submit
//! returns [`LoginOutcome::Submit`] with the request, the core verifies and
//! saves it, and reports back with [`LoginModal::finish`].

use std::sync::Arc;

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use harness_config::login::{LoginEvent, LoginFlow, LoginKey, LoginRequest, LoginStep};
use ratatui::Frame;
use ratatui::layout::{Position, Rect};
use ratatui::style::Modifier;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};

use crate::chat::text::{sanitize, width, wrap_line, wrap_text};
use crate::interaction::HitMap;
use crate::theme::{Theme, Token};

/// Element id prefix of a menu option (`login.option.<index>`).
pub const OPTION_PREFIX: &str = "login.option.";
/// Element id of the primary button (submit, try again, close).
pub const PRIMARY_ID: &str = "login.primary";
/// Element id of the back / close button.
pub const BACK_ID: &str = "login.back";
/// Widest dialog.
pub const MAX_WIDTH: u16 = 72;
/// Dialog title.
pub const TITLE: &str = " Log in ";

/// What a clicked element does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoginAction {
    /// Choose menu option `index`.
    Option(usize),
    /// The primary button: like Enter.
    Primary,
    /// The back button: like Esc.
    Back,
}

/// A submitted login, sent to the core. The request is shared, never
/// copied, and its `Debug` shows the provider and storage key only.
#[derive(Clone)]
pub struct LoginSubmission(Arc<LoginRequest>);

impl LoginSubmission {
    /// Wraps a request.
    pub fn new(request: LoginRequest) -> Self {
        Self(Arc::new(request))
    }

    /// The request to verify and save.
    pub fn request(&self) -> &LoginRequest {
        &self.0
    }
}

impl std::fmt::Debug for LoginSubmission {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "LoginSubmission({}, {})",
            self.0.provider,
            self.0.credential.ini_key()
        )
    }
}

impl PartialEq for LoginSubmission {
    /// Two submissions are equal when they are the same submit.
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl Eq for LoginSubmission {}

/// What the app should do after an input.
#[derive(Debug)]
pub enum LoginOutcome {
    /// Not handled.
    Ignored,
    /// Handled; redraw.
    Changed,
    /// Close the modal.
    Close,
    /// Verify and save this, then call [`LoginModal::finish`].
    Submit(LoginSubmission),
}

/// The `/login` modal.
#[derive(Debug)]
pub struct LoginModal {
    flow: LoginFlow,
    /// First body row after a manual scroll; `None` follows the selection.
    top: Option<usize>,
    /// First body row and largest first row of the last frame.
    last: std::cell::Cell<(usize, usize)>,
}

impl LoginModal {
    /// A modal starting at the provider choice, or at the method choice of
    /// `provider`. An error names the known providers.
    pub fn new(provider: Option<&str>) -> Result<Self, String> {
        let flow = match provider {
            None => LoginFlow::new(),
            Some(p) => LoginFlow::for_provider(p).ok_or_else(|| {
                let known: Vec<&str> = harness_config::login::AUTH_SUPPORT
                    .iter()
                    .map(|a| a.provider)
                    .collect();
                format!("unknown provider `{p}`; use one of {}", known.join(", "))
            })?,
        };
        Ok(Self {
            flow,
            top: None,
            last: std::cell::Cell::new((0, 0)),
        })
    }

    /// Scrolls the dialog by `lines` rows (the wheel, PageUp/PageDown).
    pub fn scroll(&mut self, lines: i32) -> LoginOutcome {
        let (first, max) = self.last.get();
        let base = self.top.unwrap_or(first) as i64;
        self.top = Some((base + i64::from(lines)).clamp(0, max as i64) as usize);
        LoginOutcome::Changed
    }

    /// The flow behind the modal.
    pub fn flow(&self) -> &LoginFlow {
        &self.flow
    }

    fn outcome(&mut self, ev: LoginEvent) -> LoginOutcome {
        match ev {
            LoginEvent::None => LoginOutcome::Changed,
            LoginEvent::Submit(req) => LoginOutcome::Submit(LoginSubmission::new(req)),
            LoginEvent::Cancelled | LoginEvent::Closed => LoginOutcome::Close,
        }
    }

    /// Handles a key.
    pub fn handle_key(&mut self, key: &KeyEvent) -> LoginOutcome {
        if key.kind == KeyEventKind::Release {
            return LoginOutcome::Ignored;
        }
        match key.code {
            KeyCode::PageUp => return self.scroll(-5),
            KeyCode::PageDown => return self.scroll(5),
            _ => {}
        }
        self.top = None;
        let plain = key.modifiers.difference(KeyModifiers::SHIFT).is_empty();
        let menu = !self.flow.options().is_empty();
        let k = match key.code {
            KeyCode::Esc => LoginKey::Esc,
            KeyCode::Enter => LoginKey::Enter,
            KeyCode::Up | KeyCode::BackTab if menu => LoginKey::Up,
            KeyCode::Tab if menu && key.modifiers.contains(KeyModifiers::SHIFT) => LoginKey::Up,
            KeyCode::Down | KeyCode::Tab if menu => LoginKey::Down,
            KeyCode::Backspace => LoginKey::Backspace,
            KeyCode::Char(c) if plain => LoginKey::Char(c),
            _ => return LoginOutcome::Ignored,
        };
        let ev = self.flow.handle(k);
        self.outcome(ev)
    }

    /// Pasted text goes into the field (line breaks dropped).
    pub fn paste(&mut self, text: &str) -> LoginOutcome {
        self.top = None;
        let ev = self.flow.handle(LoginKey::Paste(text.to_owned()));
        self.outcome(ev)
    }

    /// Runs a clicked element's action.
    pub fn activate(&mut self, action: LoginAction) -> LoginOutcome {
        self.top = None;
        let ev = match action {
            LoginAction::Option(i) => self.flow.click(i),
            LoginAction::Primary => self.flow.handle(LoginKey::Enter),
            LoginAction::Back => self.flow.handle(LoginKey::Esc),
        };
        self.outcome(ev)
    }

    /// Reports the core's verify-and-save result.
    pub fn finish(&mut self, result: Result<String, String>) {
        self.flow.finish(result);
    }

    /// Labels of the buttons on the current step: `(primary, back)`.
    fn buttons(&self) -> (Option<&'static str>, Option<&'static str>) {
        match self.flow.step() {
            LoginStep::Provider => (None, Some("cancel")),
            LoginStep::Method => (None, Some("back")),
            LoginStep::Value => (Some("submit"), Some("back")),
            LoginStep::Verifying => (None, Some("close")),
            LoginStep::Done => (Some("close"), None),
            LoginStep::Failed => (Some("try again"), Some("close")),
        }
    }

    /// The dialog lines for a content width `w`, with the row of each
    /// option and of the field.
    fn lines(&self, theme: &Theme, w: usize) -> Body {
        let mut lines: Vec<Line<'static>> = Vec::new();
        let mut options = Vec::new();
        let mut field = None;
        let wrap = |lines: &mut Vec<Line<'static>>, text: &str, token: Token| {
            lines.extend(wrap_line(
                Line::from(Span::styled(text.to_owned(), theme.fg(token))),
                w,
                &[],
            ));
        };
        lines.extend(wrap_line(
            Line::from(Span::styled(
                sanitize(&self.flow.title()),
                theme.fg(Token::Fg).add_modifier(Modifier::BOLD),
            )),
            w,
            &[],
        ));
        lines.push(Line::default());
        for (i, o) in self.flow.options().into_iter().enumerate() {
            let selected = i == self.flow.selected();
            let label_style = if selected {
                theme.fg(Token::Accent).add_modifier(Modifier::BOLD)
            } else {
                theme.fg(Token::Fg)
            };
            let rows = wrap_line(
                Line::from(vec![
                    Span::styled(if selected { "▸ " } else { "  " }, theme.fg(Token::Accent)),
                    Span::styled(o.label, label_style),
                    Span::styled(format!("  {}", o.detail), theme.fg(Token::Muted)),
                ]),
                w,
                &[Span::raw("    ")],
            );
            options.push((lines.len(), rows.len()));
            lines.extend(rows);
        }
        if let Some(text) = self.flow.field() {
            let prompt = self.flow.method().map_or("Value", |m| m.prompt());
            let rows = wrap_line(
                Line::from(vec![
                    Span::styled(prompt.to_owned(), theme.fg(Token::Muted)),
                    Span::styled(" › ", theme.fg(Token::Accent)),
                    Span::styled(sanitize(&text), theme.fg(Token::Fg)),
                ]),
                w,
                &[],
            );
            // The cursor goes after the text, on its last row.
            let last = rows.last().map_or(0, |l| {
                l.spans.iter().map(|s| width(&s.content)).sum::<usize>()
            });
            field = Some((lines.len() + rows.len() - 1, last));
            lines.extend(rows);
        }
        if let Some(m) = self.flow.message() {
            lines.push(Line::default());
            let token = match self.flow.step() {
                LoginStep::Done => Token::Ok,
                LoginStep::Failed => Token::Error,
                _ => Token::Warn,
            };
            wrap(&mut lines, m, token);
        }
        if self.flow.step() == LoginStep::Verifying {
            wrap(
                &mut lines,
                "one authenticated request to the provider…",
                Token::Running,
            );
        }
        if let Some(note) = self.flow.note() {
            lines.push(Line::default());
            wrap(&mut lines, note, Token::Muted);
        }
        Body {
            lines,
            options,
            field,
        }
    }

    /// Draws the modal over `area` and registers its options and buttons.
    pub fn render<A: Clone>(
        &self,
        frame: &mut Frame<'_>,
        area: Rect,
        theme: &Theme,
        hits: &mut HitMap<A>,
        wrap: &dyn Fn(LoginAction) -> A,
    ) {
        let w = MAX_WIDTH.min(area.width.saturating_sub(4)).max(20);
        let w = w.min(area.width);
        // Content width: borders and one cell of padding on each side.
        let cw = w.saturating_sub(4) as usize;
        if cw == 0 {
            return;
        }
        let body = self.lines(theme, cw);
        // Below the body: the buttons (wrapping when narrow) and the hint.
        let (primary, back) = self.buttons();
        let mut buttons = Vec::new();
        let (mut bx, mut brow) = (0usize, 0usize);
        for (id, label, action, token) in [
            (PRIMARY_ID, primary, LoginAction::Primary, Token::Accent),
            (BACK_ID, back, LoginAction::Back, Token::Muted),
        ] {
            let Some(label) = label else { continue };
            let text = format!("[ {label} ]");
            let tw = width(&text);
            if bx > 0 && bx + tw > cw {
                brow += 1;
                bx = 0;
            }
            buttons.push((id, text, action, token, brow, bx));
            bx += tw + 2;
        }
        let button_rows = if buttons.is_empty() { 0 } else { brow + 1 };
        let hint = wrap_text(self.flow.hint(), cw);
        let foot = (button_rows + hint.len()) as u16;
        // Title row is part of the body; add buttons, hint and borders.
        let h = (body.lines.len() as u16 + foot + 2).min(area.height);
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
            "login",
            Rect {
                height: rows as u16,
                ..inner
            },
        );
        // Keep the selected option, else the field, in view.
        let total = body.lines.len();
        let max = total.saturating_sub(rows);
        let focus = body
            .options
            .get(self.flow.selected())
            .map(|&(r, n)| r + n)
            .or(body.field.map(|(r, _)| r + 1))
            .unwrap_or(0);
        let auto = focus.saturating_sub(rows).min(max);
        let first = self.top.map_or(auto, |t| t.min(max));
        self.last.set((first, max));
        for (row, line) in body.lines.into_iter().enumerate().skip(first).take(rows) {
            let y = inner.y + (row - first) as u16;
            frame.render_widget(Paragraph::new(line), Rect::new(inner.x, y, inner.width, 1));
            if let Some((r, col)) = body.field
                && r == row
            {
                let cx = inner.x + (col.min(cw.saturating_sub(1))) as u16;
                frame.set_cursor_position(Position::new(cx, y));
            }
        }
        for (i, &(r, n)) in body.options.iter().enumerate() {
            let top = r.max(first);
            let bottom = (r + n).min(first + rows);
            if bottom > top {
                hits.register(
                    format!("{OPTION_PREFIX}{i}"),
                    Rect::new(
                        inner.x,
                        inner.y + (top - first) as u16,
                        inner.width,
                        (bottom - top) as u16,
                    ),
                    wrap(LoginAction::Option(i)),
                );
            }
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

        // Buttons, then the hint.
        let by = inner.y + rows as u16;
        for (id, text, action, token, row, x) in buttons {
            let y = by + row as u16;
            let x = inner.x + x as u16;
            if y >= inner.bottom() || x >= inner.right() {
                continue;
            }
            let bw = (width(&text) as u16).min(inner.right() - x);
            let r = Rect::new(x, y, bw, 1);
            frame.render_widget(Paragraph::new(Span::styled(text, theme.fg(token))), r);
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

/// Rendered dialog text.
struct Body {
    lines: Vec<Line<'static>>,
    /// First line and rows of each option.
    options: Vec<(usize, usize)>,
    /// Line index and cursor column of the text field.
    field: Option<(usize, usize)>,
}

#[cfg(test)]
mod tests;
