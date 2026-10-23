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

use crate::chat::text::{truncate_line, width, wrap_line};
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
        Ok(Self { flow })
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
        let ev = self.flow.handle(LoginKey::Paste(text.to_owned()));
        self.outcome(ev)
    }

    /// Runs a clicked element's action.
    pub fn activate(&mut self, action: LoginAction) -> LoginOutcome {
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
        lines.push(Line::from(Span::styled(
            self.flow.title(),
            theme.fg(Token::Fg).add_modifier(Modifier::BOLD),
        )));
        lines.push(Line::default());
        for (i, o) in self.flow.options().into_iter().enumerate() {
            let selected = i == self.flow.selected();
            let label_style = if selected {
                theme.fg(Token::Accent).add_modifier(Modifier::BOLD)
            } else {
                theme.fg(Token::Fg)
            };
            options.push(lines.len());
            lines.push(Line::from(vec![
                Span::styled(if selected { "▸ " } else { "  " }, theme.fg(Token::Accent)),
                Span::styled(o.label, label_style),
                Span::styled(format!("  {}", o.detail), theme.fg(Token::Muted)),
            ]));
        }
        if let Some(text) = self.flow.field() {
            let prompt = self.flow.method().map_or("Value", |m| m.prompt());
            field = Some((lines.len(), width(prompt) + 3 + width(&text)));
            lines.push(Line::from(vec![
                Span::styled(prompt.to_owned(), theme.fg(Token::Muted)),
                Span::styled(" › ", theme.fg(Token::Accent)),
                Span::styled(text, theme.fg(Token::Fg)),
            ]));
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
        // Title row is part of the body; add buttons, hint and borders.
        let h = (body.lines.len() as u16 + 4).min(area.height);
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
        let rows = inner.height - 2; // buttons and hint at the bottom
        for (row, line) in body.lines.into_iter().enumerate().take(rows as usize) {
            let y = inner.y + row as u16;
            frame.render_widget(
                Paragraph::new(truncate_line(line, cw)),
                Rect::new(inner.x, y, inner.width, 1),
            );
            if let Some(i) = body.options.iter().position(|&r| r == row) {
                hits.register(
                    format!("{OPTION_PREFIX}{i}"),
                    Rect::new(inner.x, y, inner.width, 1),
                    wrap(LoginAction::Option(i)),
                );
            }
            if let Some((r, col)) = body.field
                && r == row
            {
                let cx = inner.x + (col.min(cw.saturating_sub(1))) as u16;
                frame.set_cursor_position(Position::new(cx, y));
            }
        }

        // Buttons.
        let by = inner.bottom() - 2;
        let (primary, back) = self.buttons();
        let mut x = inner.x;
        for (id, label, action, token) in [
            (PRIMARY_ID, primary, LoginAction::Primary, Token::Accent),
            (BACK_ID, back, LoginAction::Back, Token::Muted),
        ] {
            let Some(label) = label else { continue };
            let text = format!("[ {label} ]");
            let bw = (width(&text) as u16).min(inner.right().saturating_sub(x));
            if bw == 0 {
                break;
            }
            let r = Rect::new(x, by, bw, 1);
            frame.render_widget(Paragraph::new(Span::styled(text, theme.fg(token))), r);
            hits.register_button(id, r, wrap(action));
            x = x.saturating_add(bw + 2);
        }
        frame.render_widget(
            Paragraph::new(truncate_line(
                Line::from(Span::styled(self.flow.hint(), theme.fg(Token::Muted))),
                cw,
            )),
            Rect::new(inner.x, by + 1, inner.width, 1),
        );
    }
}

/// Rendered dialog text.
struct Body {
    lines: Vec<Line<'static>>,
    /// Line index of each option.
    options: Vec<usize>,
    /// Line index and cursor column of the text field.
    field: Option<(usize, usize)>,
}

#[cfg(test)]
mod tests;
