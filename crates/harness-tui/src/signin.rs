//! The browser sign-in modal (Sign in with ChatGPT, MCP OAuth): shows the
//! sign-in link in full and lets the user finish from a browser on another
//! machine.
//!
//! ```text
//!        ╭ Sign in ──────────────────────────────────────────╮
//!        │ Sign in with ChatGPT                              │
//!        │                                                   │
//!        │ No browser opened here (this is an SSH session).  │
//!        │ Open this link in a browser on any machine:       │
//!        │ https://auth.openai.com/api/accounts/authorize?cl │
//!        │ ient_id=…                                         │
//!        │                                                   │
//!        │ After you approve, the browser lands on a         │
//!        │ 127.0.0.1 address … paste it below.               │
//!        │                                                   │
//!        │ Or forward the port first, …                      │
//!        │ ssh -N -L 1455:127.0.0.1:1455 dev@10.0.0.2        │
//!        │                                                   │
//!        │ Address › http://127.0.0.1:1455/auth/callback?…   │
//!        │ [ copy link ]  [ copy ssh command ]  [ submit ]   │
//!        │ [ cancel ]                                        │
//!        │ Tab move · Enter submit · ctrl+y copy link · Esc… │
//!        ╰───────────────────────────────────────────────────╯
//! ```
//!
//! The link is cut into rows at the dialog width, never shortened, and
//! each row is an OSC 8 hyperlink to the whole link ([`Hyperlink`], written
//! over the drawn cells after each frame). Typed and pasted text goes into
//! the address field; only the part before `?` is shown, since the rest
//! holds the one-time code. Tab and the arrows move between the field and
//! the buttons, Enter activates, Esc cancels the sign-in, and every button
//! is clickable. A click outside the dialog does nothing while the sign-in
//! runs, so a stray click cannot throw it away.
//!
//! For a device authorization (RFC 8628, MCP servers that offer it when
//! the browser is elsewhere) the dialog shows the verification link and
//! the user code instead, with `[ copy link ]`, `[ copy code ]` (also `c`)
//! and `[ cancel ]`; there is no address field, since nothing comes back
//! to paste. Esc cancels, which stops the polling.
//!
//! The modal never touches the network: a submit returns
//! [`SignInOutcome::Paste`], the core hands it to the running flow and
//! answers with [`SignInModal::answer`], and reports the end with
//! [`SignInModal::finish`].

use std::sync::Arc;

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
pub use harness_providers::signin::DeviceCode;
use ratatui::Frame;
use ratatui::layout::{Position, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};
use zeroize::Zeroizing;

use crate::chat::text::{sanitize, width, wrap_line, wrap_text};
use crate::interaction::{Dispatch, ElementId, HitMap, Interaction};
use crate::theme::{Theme, Token};

/// Element id of the address field.
pub const FIELD_ID: &str = "signin.field";
/// Element id of the copy-link button.
pub const COPY_LINK_ID: &str = "signin.copy_link";
/// Element id of the copy-ssh-command button.
pub const COPY_COMMAND_ID: &str = "signin.copy_command";
/// Element id of the copy-code button (device authorization).
pub const COPY_CODE_ID: &str = "signin.copy_code";
/// Element id of the submit button.
pub const SUBMIT_ID: &str = "signin.submit";
/// Element id of the cancel / close button.
pub const CANCEL_ID: &str = "signin.cancel";
/// Sign-in id of Sign in with ChatGPT (MCP sign-ins use `mcp.<server>`).
pub const CHATGPT_ID: &str = "chatgpt";
/// Widest dialog.
pub const MAX_WIDTH: u16 = 76;
/// Dialog title.
pub const TITLE: &str = " Sign in ";

/// What the core shows for a running browser sign-in. `Debug` leaves out
/// the link's query, which holds the state or the user code.
#[derive(Clone, PartialEq, Eq)]
pub struct SignInPrompt {
    /// Which sign-in ([`CHATGPT_ID`], `mcp.<server>`).
    pub id: String,
    /// Heading (`Sign in with ChatGPT`).
    pub title: String,
    /// The authorization URL, in full (for a device authorization, the
    /// link to open: [`DeviceCode::link`]).
    pub url: String,
    /// The `ssh -L` command, over SSH.
    pub forward: Option<String>,
    /// Why no browser was opened here; `None` when one was.
    pub no_browser: Option<String>,
    /// A device authorization (RFC 8628): show its code; nothing is pasted.
    pub device: Option<DeviceCode>,
}

impl std::fmt::Debug for SignInPrompt {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let url = self
            .url
            .split_once('?')
            .map_or(self.url.as_str(), |(u, _)| u);
        f.debug_struct("SignInPrompt")
            .field("id", &self.id)
            .field("title", &self.title)
            .field("url", &format_args!("{url}?…"))
            .field("forward", &self.forward)
            .field("no_browser", &self.no_browser)
            .field("device", &self.device)
            .finish()
    }
}

/// Text pasted into the modal. Shared, never copied; `Debug` hides it,
/// since it holds a one-time code.
#[derive(Clone)]
pub struct PastedText(Arc<Zeroizing<String>>);

impl PastedText {
    /// Wraps `text`.
    pub fn new(text: String) -> Self {
        Self(Arc::new(Zeroizing::new(text)))
    }

    /// The text.
    pub fn text(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for PastedText {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "PastedText({} chars)", self.0.chars().count())
    }
}

impl PartialEq for PastedText {
    /// Two pastes are equal when they are the same submit.
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl Eq for PastedText {}

/// A hyperlink drawn over cells of the last frame: `text` at `(x, y)`,
/// linking to `url`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hyperlink {
    /// Column of the first cell.
    pub x: u16,
    /// Row.
    pub y: u16,
    /// The text in those cells.
    pub text: String,
    /// The link target.
    pub url: String,
    /// The cells' style.
    pub style: Style,
}

impl Hyperlink {
    /// The escape sequence that redraws the cells as an OSC 8 hyperlink:
    /// save the cursor, move, set the style, write the link, reset and
    /// restore the cursor. Terminals without OSC 8 show the same text.
    pub fn sequence(&self) -> String {
        use ratatui::backend::IntoCrossterm;
        let clean = |s: &str| s.chars().filter(|c| !c.is_control()).collect::<String>();
        let mut sgr = String::new();
        // The cells' theme colors, as drawn.
        if let Some(fg) = self.style.fg {
            sgr.push_str(&crossterm::style::SetForegroundColor(fg.into_crossterm()).to_string());
        }
        if let Some(bg) = self.style.bg {
            sgr.push_str(&crossterm::style::SetBackgroundColor(bg.into_crossterm()).to_string());
        }
        if self.style.add_modifier.contains(Modifier::UNDERLINED) {
            sgr.push_str("\x1b[4m");
        }
        if self.style.add_modifier.contains(Modifier::BOLD) {
            sgr.push_str("\x1b[1m");
        }
        format!(
            "\x1b7\x1b[{};{}H{sgr}\x1b]8;;{}\x1b\\{}\x1b]8;;\x1b\\\x1b[0m\x1b8",
            self.y + 1,
            self.x + 1,
            clean(&self.url),
            clean(&self.text)
        )
    }
}

/// What a clicked or activated element does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignInAction {
    /// The address field: Enter submits it.
    Field,
    /// Copy the link.
    CopyLink,
    /// Copy the `ssh -L` command.
    CopyCommand,
    /// Copy the device authorization's user code.
    CopyCode,
    /// Submit the pasted address.
    Submit,
    /// Cancel the sign-in, or close the finished dialog.
    Cancel,
}

/// What the app should do after an input.
#[derive(Debug)]
pub enum SignInOutcome {
    /// Not handled.
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
    /// Hand this to the running sign-in, then call
    /// [`SignInModal::answer`].
    Paste(PastedText),
    /// Stop the running sign-in and close the dialog.
    Cancel,
    /// Close the finished dialog.
    Close,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum State {
    Waiting,
    Checking,
    Done(String),
    Failed(String),
}

/// The sign-in modal.
#[derive(Debug)]
pub struct SignInModal {
    prompt: SignInPrompt,
    input: Zeroizing<String>,
    /// An answer to the last paste (`Token::Error` for a refusal).
    status: Option<(Token, String)>,
    state: State,
    /// First body row after a manual scroll; `None` keeps the field in
    /// view.
    top: Option<usize>,
    /// First body row and largest first row of the last frame.
    last: std::cell::Cell<(usize, usize)>,
}

impl SignInModal {
    /// A modal for `prompt`.
    pub fn new(prompt: SignInPrompt) -> Self {
        Self {
            prompt,
            input: Zeroizing::new(String::new()),
            status: None,
            state: State::Waiting,
            top: None,
            last: std::cell::Cell::new((0, 0)),
        }
    }

    /// Which sign-in this is.
    pub fn id(&self) -> &str {
        &self.prompt.id
    }

    /// What it shows.
    pub fn prompt(&self) -> &SignInPrompt {
        &self.prompt
    }

    /// Whether this is a device authorization (a code to confirm, nothing
    /// to paste).
    pub fn is_device(&self) -> bool {
        self.prompt.device.is_some()
    }

    /// Whether the sign-in has ended (the dialog only shows the outcome).
    pub fn is_finished(&self) -> bool {
        matches!(self.state, State::Done(_) | State::Failed(_))
    }

    /// The status or outcome line, if any.
    pub fn message(&self) -> Option<&str> {
        match &self.state {
            State::Done(m) | State::Failed(m) => Some(m),
            _ => self.status.as_ref().map(|(_, m)| m.as_str()),
        }
    }

    /// The running sign-in's answer to the last paste.
    pub fn answer(&mut self, result: Result<(), String>) {
        if self.is_finished() {
            return;
        }
        self.state = State::Waiting;
        self.status = Some(match result {
            Ok(()) => {
                self.state = State::Checking;
                (Token::Running, "got it; finishing the sign-in…".into())
            }
            Err(e) => (Token::Error, e),
        });
    }

    /// The sign-in ended: a one-line summary or an error.
    pub fn finish(&mut self, result: Result<String, String>) {
        self.status = None;
        self.state = match result {
            Ok(m) => State::Done(m),
            Err(e) => State::Failed(e),
        };
    }

    /// Scrolls the dialog by `lines` rows (the wheel, PageUp/PageDown).
    pub fn scroll(&mut self, lines: i32) -> SignInOutcome {
        let (first, max) = self.last.get();
        let base = self.top.unwrap_or(first) as i64;
        self.top = Some((base + i64::from(lines)).clamp(0, max as i64) as usize);
        SignInOutcome::Changed
    }

    /// Handles a key; `interaction` moves focus and activates the focused
    /// element.
    pub fn handle_key(
        &mut self,
        key: &KeyEvent,
        interaction: &mut Interaction<SignInAction>,
    ) -> SignInOutcome {
        if key.kind == KeyEventKind::Release {
            return SignInOutcome::Ignored;
        }
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let plain = key.modifiers.difference(KeyModifiers::SHIFT).is_empty();
        match key.code {
            KeyCode::Char('y') if ctrl => return self.activate(SignInAction::CopyLink),
            KeyCode::PageUp => return self.scroll(-5),
            KeyCode::PageDown => return self.scroll(5),
            // A device authorization has no field: `c` copies the code.
            KeyCode::Char('c' | 'C') if plain && self.is_device() && !self.is_finished() => {
                return self.activate(SignInAction::CopyCode);
            }
            KeyCode::Char(_) | KeyCode::Backspace if self.is_device() => {
                return SignInOutcome::Ignored;
            }
            KeyCode::Char(c) if plain && !self.is_finished() => {
                self.top = None;
                self.input.push(c);
                interaction.set_focus(FIELD_ID);
                return SignInOutcome::Changed;
            }
            KeyCode::Backspace if !self.is_finished() => {
                self.input.pop();
                interaction.set_focus(FIELD_ID);
                return SignInOutcome::Changed;
            }
            _ => {}
        }
        match interaction.handle_key(key) {
            Some(Dispatch::Activate(a)) => self.activate(a),
            Some(Dispatch::Back) => self.activate(SignInAction::Cancel),
            Some(Dispatch::Scroll { lines, .. }) => self.scroll(lines),
            Some(Dispatch::Focus(_)) => SignInOutcome::Changed,
            None if key.code == KeyCode::Enter && !self.is_device() => {
                self.activate(SignInAction::Submit)
            }
            None => SignInOutcome::Ignored,
        }
    }

    /// Pasted text goes into the field (line breaks dropped).
    pub fn paste(&mut self, text: &str) -> SignInOutcome {
        if self.is_finished() || self.is_device() {
            return SignInOutcome::Ignored;
        }
        self.top = None;
        self.input
            .extend(text.chars().filter(|c| *c != '\n' && *c != '\r'));
        SignInOutcome::Changed
    }

    /// Runs an element's action.
    pub fn activate(&mut self, action: SignInAction) -> SignInOutcome {
        match action {
            SignInAction::CopyLink => SignInOutcome::Copy {
                what: "sign-in link",
                text: self.prompt.url.clone(),
            },
            SignInAction::CopyCommand => match &self.prompt.forward {
                Some(c) => SignInOutcome::Copy {
                    what: "ssh command",
                    text: c.clone(),
                },
                None => SignInOutcome::Ignored,
            },
            SignInAction::CopyCode => match &self.prompt.device {
                Some(d) => SignInOutcome::Copy {
                    what: "sign-in code",
                    text: d.user_code.to_string(),
                },
                None => SignInOutcome::Ignored,
            },
            SignInAction::Cancel if self.is_finished() => SignInOutcome::Close,
            SignInAction::Cancel => SignInOutcome::Cancel,
            _ if self.is_finished() => SignInOutcome::Close,
            SignInAction::Field | SignInAction::Submit if self.is_device() => {
                SignInOutcome::Ignored
            }
            SignInAction::Field | SignInAction::Submit => {
                self.top = None;
                if self.state == State::Checking {
                    return SignInOutcome::Changed;
                }
                if self.input.trim().is_empty() {
                    self.status = Some((
                        Token::Warn,
                        "paste the address the browser went to after you approved".into(),
                    ));
                    return SignInOutcome::Changed;
                }
                let text = std::mem::take(&mut *self.input);
                self.state = State::Checking;
                self.status = Some((Token::Running, "checking…".into()));
                SignInOutcome::Paste(PastedText::new(text))
            }
        }
    }

    /// The field text as shown: only the part before `?`, since the rest
    /// holds the code.
    fn shown_input(&self) -> String {
        let t = self.input.trim();
        let n = t.chars().count();
        match t.split_once('?') {
            _ if n == 0 => String::new(),
            Some((before, _)) => format!("{}?… ({n} characters)", sanitize(before)),
            None if t.contains("://") => sanitize(t),
            None => format!("({n} characters)"),
        }
    }

    fn buttons(&self) -> Vec<(&'static str, String, SignInAction, Token)> {
        if self.is_finished() {
            return vec![(
                CANCEL_ID,
                "[ close ]".into(),
                SignInAction::Cancel,
                Token::Accent,
            )];
        }
        let mut b = vec![(
            COPY_LINK_ID,
            "[ copy link ]".to_owned(),
            SignInAction::CopyLink,
            Token::Accent,
        )];
        if self.is_device() {
            b.push((
                COPY_CODE_ID,
                "[ copy code ]".into(),
                SignInAction::CopyCode,
                Token::Accent,
            ));
            b.push((
                CANCEL_ID,
                "[ cancel ]".into(),
                SignInAction::Cancel,
                Token::Muted,
            ));
            return b;
        }
        if self.prompt.forward.is_some() {
            b.push((
                COPY_COMMAND_ID,
                "[ copy ssh command ]".into(),
                SignInAction::CopyCommand,
                Token::Accent,
            ));
        }
        b.push((
            SUBMIT_ID,
            "[ submit ]".into(),
            SignInAction::Submit,
            Token::Accent,
        ));
        b.push((
            CANCEL_ID,
            "[ cancel ]".into(),
            SignInAction::Cancel,
            Token::Muted,
        ));
        b
    }

    fn hint(&self) -> &'static str {
        if self.is_finished() {
            "Enter close"
        } else if self.is_device() {
            "Tab move · Enter activate · ctrl+y copy link · c copy code · Esc cancel"
        } else {
            "Tab move · Enter submit · ctrl+y copy link · Esc cancel"
        }
    }

    /// The dialog body for a content width `w`.
    fn body(&self, theme: &Theme, w: usize) -> Body {
        let mut b = Body::default();
        let text = |b: &mut Body, s: &str, style: Style| {
            b.lines.extend(wrap_line(
                Line::from(Span::styled(sanitize(s), style)),
                w,
                &[],
            ));
        };
        text(
            &mut b,
            &self.prompt.title,
            theme.fg(Token::Fg).add_modifier(Modifier::BOLD),
        );
        b.lines.push(Line::default());
        if let Some(device) = &self.prompt.device {
            self.device_body(&mut b, device, theme, w);
            return b;
        }
        match &self.prompt.no_browser {
            Some(why) => {
                text(
                    &mut b,
                    &format!("No browser opened here ({why})."),
                    theme.fg(Token::Warn),
                );
                text(
                    &mut b,
                    "Open this link in a browser on any machine:",
                    theme.fg(Token::Fg),
                );
            }
            None => text(
                &mut b,
                "Your browser should open. If it does not, open this link:",
                theme.fg(Token::Fg),
            ),
        }
        // The link, cut at the width (never shortened).
        b.link(&self.prompt.url, theme, w);
        if !self.is_finished() {
            b.lines.push(Line::default());
            text(
                &mut b,
                "After you approve, the browser lands on a 127.0.0.1 address. On another machine that page does not load: copy the whole address from its address bar and paste it below.",
                theme.fg(Token::Muted),
            );
            if let Some(cmd) = &self.prompt.forward {
                b.lines.push(Line::default());
                text(
                    &mut b,
                    "Or forward the port first, on the machine with the browser (OpenSSH, also on Windows), then open the link:",
                    theme.fg(Token::Muted),
                );
                for chunk in chunks(&sanitize(cmd), w) {
                    b.lines
                        .push(Line::from(Span::styled(chunk, theme.fg(Token::Fg))));
                }
            }
            b.lines.push(Line::default());
            let shown = self.shown_input();
            let rows = wrap_line(
                Line::from(vec![
                    Span::styled("Address", theme.fg(Token::Muted)),
                    Span::styled(" › ", theme.fg(Token::Accent)),
                    if shown.is_empty() {
                        Span::styled("paste it here", theme.fg(Token::Muted))
                    } else {
                        Span::styled(shown, theme.fg(Token::Fg))
                    },
                ]),
                w,
                &[],
            );
            let last = if self.input.is_empty() {
                width("Address › ")
            } else {
                rows.last().map_or(0, |l| {
                    l.spans.iter().map(|s| width(&s.content)).sum::<usize>()
                })
            };
            b.field = Some((b.lines.len(), rows.len(), last));
            b.lines.extend(rows);
        }
        self.outcome(&mut b, theme, w);
        b
    }

    /// The outcome or status line, if any.
    fn outcome(&self, b: &mut Body, theme: &Theme, w: usize) {
        let outcome = match &self.state {
            State::Done(m) => Some((Token::Ok, m.as_str())),
            State::Failed(m) => Some((Token::Error, m.as_str())),
            _ => self.status.as_ref().map(|(t, m)| (*t, m.as_str())),
        };
        if let Some((token, m)) = outcome {
            b.lines.push(Line::default());
            b.text(m, theme.fg(token), w);
        }
    }

    /// The body of a device authorization: the link, the code and the
    /// plain verification page, then the outcome.
    fn device_body(&self, b: &mut Body, device: &DeviceCode, theme: &Theme, w: usize) {
        let complete = device.verification_uri_complete.is_some();
        if let Some(why) = &self.prompt.no_browser {
            b.text(
                &format!("No browser opened here ({why})."),
                theme.fg(Token::Warn),
                w,
            );
        }
        b.text(
            if complete {
                "Open this link on any device and check that it shows the code below:"
            } else {
                "Open this link on any device and enter the code below:"
            },
            theme.fg(Token::Fg),
            w,
        );
        b.link(&self.prompt.url, theme, w);
        b.lines.push(Line::default());
        let code = sanitize(&device.user_code);
        let rows = wrap_line(
            Line::from(vec![
                Span::styled("Code ", theme.fg(Token::Muted)),
                Span::styled(code, theme.fg(Token::Accent).add_modifier(Modifier::BOLD)),
            ]),
            w,
            &[],
        );
        b.lines.extend(rows);
        if !self.is_finished() {
            if complete {
                b.lines.push(Line::default());
                b.text(
                    "Or open this page and type the code:",
                    theme.fg(Token::Muted),
                    w,
                );
                b.link(&device.verification_uri, theme, w);
            }
            b.lines.push(Line::default());
            b.text(
                &format!(
                    "Waiting for approval… The code expires in {}. Nothing needs to be pasted back.",
                    device.lifetime()
                ),
                theme.fg(Token::Muted),
                w,
            );
        }
        self.outcome(b, theme, w);
    }

    /// Draws the modal over `area`, registers its field and buttons, and
    /// adds the link's rows to `links`.
    pub fn render<A: Clone>(
        &self,
        frame: &mut Frame<'_>,
        area: Rect,
        theme: &Theme,
        hits: &mut HitMap<A>,
        wrap: &dyn Fn(SignInAction) -> A,
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
        let body = self.body(theme, cw);
        let buttons = self.buttons();
        let mut placed = Vec::new();
        let (mut bx, mut brow) = (0usize, 0usize);
        for (id, label, action, token) in buttons {
            let tw = width(&label);
            if bx > 0 && bx + tw > cw {
                brow += 1;
                bx = 0;
            }
            placed.push((id, label, action, token, brow, bx));
            bx += tw + 2;
        }
        let button_rows = brow + 1;
        let hint = wrap_text(self.hint(), cw);
        let foot = (button_rows + hint.len()) as u16;
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
            "signin",
            Rect {
                height: rows as u16,
                ..inner
            },
        );
        let total = body.lines.len();
        let max = total.saturating_sub(rows);
        let focus = body.field.map_or(0, |(r, n, _)| r + n);
        let auto = focus.saturating_sub(rows).min(max);
        let first = self.top.map_or(auto, |t| t.min(max));
        self.last.set((first, max));
        let field_id = ElementId::from(FIELD_ID);
        let visible = |row: usize| row >= first && row < first + rows;
        let y_of = |row: usize| inner.y + (row - first) as u16;
        // The field first, so it gets the focus when the dialog opens.
        if let Some((r, n, col)) = body.field {
            let top = r.max(first);
            let bottom = (r + n).min(first + rows);
            if bottom > top {
                hits.register(
                    FIELD_ID,
                    Rect::new(inner.x, y_of(top), inner.width, (bottom - top) as u16),
                    wrap(SignInAction::Field),
                );
                let last = r + n - 1;
                if hits.is_focused(&field_id) && visible(last) {
                    let cx = inner.x + (col.min(cw.saturating_sub(1))) as u16;
                    frame.set_cursor_position(Position::new(cx, y_of(last)));
                }
            }
        }
        for (row, line) in body.lines.into_iter().enumerate().skip(first).take(rows) {
            let y = y_of(row);
            if let Some((_, url)) = body.links.iter().find(|(r, _)| *r == row) {
                let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
                let style = line.spans.first().map_or(Style::default(), |s| s.style);
                links.push(Hyperlink {
                    x: inner.x,
                    y,
                    text,
                    url: url.clone(),
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
        for (id, label, action, token, row, x) in placed {
            let y = by + row as u16;
            let x = inner.x + x as u16;
            if y >= inner.bottom() || x >= inner.right() {
                continue;
            }
            let bw = (width(&label) as u16).min(inner.right() - x);
            let r = Rect::new(x, y, bw, 1);
            let mut style = theme.fg(token);
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

/// `s` cut into pieces of at most `w` columns (for links and commands,
/// which must not be reflowed at spaces or shortened).
pub(crate) fn chunks(s: &str, w: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut cw = 0;
    for c in s.chars() {
        let cwid = width(c.encode_utf8(&mut [0; 4]));
        if cw + cwid > w && !cur.is_empty() {
            out.push(std::mem::take(&mut cur));
            cw = 0;
        }
        cur.push(c);
        cw += cwid;
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// Rendered dialog text.
#[derive(Default)]
struct Body {
    lines: Vec<Line<'static>>,
    /// Rows that hold a link, and its target.
    links: Vec<(usize, String)>,
    /// First row, row count and cursor column of the address field.
    field: Option<(usize, usize, usize)>,
}

impl Body {
    /// Wrapped text.
    fn text(&mut self, s: &str, style: Style, w: usize) {
        self.lines.extend(wrap_line(
            Line::from(Span::styled(sanitize(s), style)),
            w,
            &[],
        ));
    }

    /// `url` cut into rows at the width (never shortened), each a link.
    fn link(&mut self, url: &str, theme: &Theme, w: usize) {
        let style = theme.fg(Token::Accent).add_modifier(Modifier::UNDERLINED);
        for chunk in chunks(&sanitize(url), w) {
            self.links.push((self.lines.len(), url.to_owned()));
            self.lines.push(Line::from(Span::styled(chunk, style)));
        }
    }
}

#[cfg(test)]
mod tests;
