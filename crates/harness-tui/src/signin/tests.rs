//! Sign-in modal tests: keyboard and mouse paths on the modal alone and
//! inside the app, the full link with its OSC 8 rows, and the run loop
//! writing those links.

use crossterm::event::Event;
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use tokio::sync::mpsc;

use super::*;
use crate::app::{App, AppConfig, AppEvent, CoreEvent, Effect, RecordingControl, UiRequest};
use crate::interaction::testing::{buffer_text, click, find_text, key, keys, type_text};

const CODE: &str = "ac_secret-one-time-code-0123456789";

fn url() -> String {
    format!(
        "https://auth.example.com/api/accounts/authorize?client_id=dynamic_agent_client&response_type=code&redirect_uri=http%3A%2F%2F127.0.0.1%3A1455%2Fauth%2Fcallback&scope=openid%20profile%20email%20offline_access&state={}&code_challenge={}&code_challenge_method=S256",
        "s".repeat(43),
        "c".repeat(43)
    )
}

fn address() -> String {
    format!(
        "http://127.0.0.1:1455/auth/callback?code={CODE}&state={}",
        "s".repeat(43)
    )
}

fn prompt(id: &str) -> SignInPrompt {
    SignInPrompt {
        id: id.into(),
        title: "Sign in with ChatGPT".into(),
        url: url(),
        forward: Some("ssh -N -L 1455:127.0.0.1:1455 dev@10.0.0.2".into()),
        no_browser: Some("this is an SSH session".into()),
    }
}

/// The modal alone, with its hit map, like the app drives it.
struct Modal {
    modal: SignInModal,
    interaction: Interaction<SignInAction>,
    term: Terminal<TestBackend>,
    links: Vec<Hyperlink>,
}

impl Modal {
    fn new(prompt: SignInPrompt, w: u16, h: u16) -> Modal {
        let mut m = Modal {
            modal: SignInModal::new(prompt),
            interaction: Interaction::new(),
            term: Terminal::new(TestBackend::new(w, h)).unwrap(),
            links: Vec::new(),
        };
        m.draw();
        m
    }

    fn draw(&mut self) {
        let theme = Theme::default();
        let (modal, interaction, links) = (&self.modal, &mut self.interaction, &mut self.links);
        links.clear();
        self.term
            .draw(|f| {
                let mut hits = interaction.begin_frame();
                modal.render(f, f.area(), &theme, &mut hits, &|a| a, links);
                interaction.end_frame(hits);
            })
            .unwrap();
    }

    fn send(&mut self, ev: Event) -> SignInOutcome {
        let out = match &ev {
            Event::Key(k) => self.modal.handle_key(k, &mut self.interaction),
            Event::Paste(t) => self.modal.paste(t),
            Event::Mouse(m) => match self.interaction.handle_mouse(m) {
                Some(Dispatch::Activate(a)) => self.modal.activate(a),
                Some(Dispatch::Back) if self.modal.is_finished() => SignInOutcome::Close,
                _ => SignInOutcome::Ignored,
            },
            _ => SignInOutcome::Ignored,
        };
        self.draw();
        out
    }

    fn key(&mut self, chord: &str) -> SignInOutcome {
        self.send(key(chord))
    }

    fn click_text(&mut self, text: &str) -> SignInOutcome {
        let (x, y) = find_text(self.term.backend().buffer(), text)
            .unwrap_or_else(|| panic!("`{text}` not on screen:\n{}", self.text()));
        self.send(click(x, y))
    }

    fn text(&self) -> String {
        buffer_text(self.term.backend().buffer())
    }

    fn focused(&self) -> Option<&str> {
        self.interaction.focused().map(|f| f.0.as_str())
    }
}

#[test]
fn the_link_is_shown_in_full_as_hyperlinked_rows() {
    let m = Modal::new(prompt("chatgpt"), 80, 40);
    let text = m.text();
    for s in [
        "Sign in with ChatGPT",
        "No browser opened here (this is an SSH session).",
        "Open this link in a browser on any machine:",
        "ssh -N -L 1455:127.0.0.1:1455 dev@10.0.0.2",
        "Address › paste it here",
        "[ copy link ]",
        "[ copy ssh command ]",
        "[ submit ]",
        "[ cancel ]",
    ] {
        assert!(text.contains(s), "{s}:\n{text}");
    }
    // Every row of the link is on screen and links to the whole URL;
    // together the rows are the URL, not shortened.
    assert!(m.links.len() > 1, "a long link wraps");
    let joined: String = m.links.iter().map(|l| l.text.as_str()).collect();
    assert_eq!(joined, url());
    for l in &m.links {
        assert_eq!(l.url, url());
        assert_eq!(
            find_text(m.term.backend().buffer(), &l.text),
            Some((l.x, l.y))
        );
    }
    // The address field has the focus when the dialog opens.
    assert_eq!(m.focused(), Some(FIELD_ID));
}

#[test]
fn keyboard_path_pastes_copies_and_cancels() {
    let mut m = Modal::new(prompt("chatgpt"), 80, 40);
    // Enter on an empty field asks for the address.
    assert!(matches!(m.key("enter"), SignInOutcome::Changed));
    assert!(m.text().contains("paste the address"), "{}", m.text());
    // A bracketed paste: the code stays off the screen.
    m.send(Event::Paste(format!("{}\n", address())));
    let text = m.text();
    assert!(!text.contains(CODE), "the code is on screen:\n{text}");
    assert!(
        text.contains("Address › http://127.0.0.1:1455/auth/callback?… ("),
        "{text}"
    );
    let SignInOutcome::Paste(p) = m.key("enter") else {
        panic!("Enter submits the field");
    };
    assert_eq!(p.text(), address());
    assert!(
        !format!("{p:?} {:?}", m.modal).contains(CODE),
        "Debug hides it"
    );
    assert!(m.text().contains("checking…"));
    // The flow refuses it: the reason shows, and typing starts over.
    m.modal
        .answer(Err("that address belongs to another sign-in attempt".into()));
    m.draw();
    assert!(m.text().contains("another sign-in attempt"), "{}", m.text());
    for e in type_text("?code=x&state=y") {
        m.send(e);
    }
    m.key("backspace");
    assert!(matches!(m.key("enter"), SignInOutcome::Paste(p) if p.text() == "?code=x&state="));
    m.modal.answer(Ok(()));
    m.draw();
    assert!(m.text().contains("finishing the sign-in"), "{}", m.text());

    // Tab and the arrows walk the buttons; Enter activates the focused one.
    m.key("tab");
    assert_eq!(m.focused(), Some(COPY_LINK_ID));
    assert!(matches!(
        m.key("enter"),
        SignInOutcome::Copy { what: "sign-in link", text } if text == url()
    ));
    m.key("down");
    assert_eq!(m.focused(), Some(COPY_COMMAND_ID));
    assert!(matches!(
        m.key("enter"),
        SignInOutcome::Copy { what: "ssh command", text } if text.starts_with("ssh -N -L 1455")
    ));
    m.key("shift+tab");
    m.key("up");
    assert_eq!(m.focused(), Some(FIELD_ID));
    // ctrl+y copies the link from anywhere; Esc cancels the sign-in.
    assert!(matches!(
        m.key("ctrl+y"),
        SignInOutcome::Copy {
            what: "sign-in link",
            ..
        }
    ));
    assert!(matches!(m.key("esc"), SignInOutcome::Cancel));
}

#[test]
fn mouse_path_clicks_every_button() {
    let mut m = Modal::new(prompt("mcp.docs"), 80, 40);
    assert!(matches!(
        m.click_text("[ copy link ]"),
        SignInOutcome::Copy {
            what: "sign-in link",
            ..
        }
    ));
    assert!(matches!(
        m.click_text("[ copy ssh command ]"),
        SignInOutcome::Copy {
            what: "ssh command",
            ..
        }
    ));
    // A click outside does not throw the sign-in away.
    assert!(matches!(m.send(click(0, 0)), SignInOutcome::Ignored));
    m.send(Event::Paste(address()));
    assert!(matches!(
        m.click_text("[ submit ]"),
        SignInOutcome::Paste(_)
    ));
    assert!(matches!(m.click_text("[ cancel ]"), SignInOutcome::Cancel));
    // Clicking the field focuses it (and submits like Enter).
    m.modal.answer(Err("no".into()));
    m.draw();
    m.send(Event::Paste("?code=abc&state=s".into()));
    assert!(matches!(m.click_text("Address ›"), SignInOutcome::Paste(_)));
}

#[test]
fn a_finished_sign_in_shows_the_outcome_and_closes() {
    let mut m = Modal::new(prompt("mcp.docs"), 80, 40);
    m.modal
        .finish(Err("authorization was declined in the browser".into()));
    m.draw();
    let text = m.text();
    assert!(text.contains("authorization was declined"), "{text}");
    assert!(
        text.contains("[ close ]") && !text.contains("[ submit ]"),
        "{text}"
    );
    assert!(!text.contains("Address ›"), "{text}");
    // Typing does nothing now; Enter, Esc, the button or a click outside
    // close it.
    assert!(matches!(
        m.send(Event::Paste("x".into())),
        SignInOutcome::Ignored
    ));
    assert!(matches!(m.key("enter"), SignInOutcome::Close));
    assert!(matches!(m.key("esc"), SignInOutcome::Close));
    assert!(matches!(m.click_text("[ close ]"), SignInOutcome::Close));
    assert!(matches!(m.send(click(0, 0)), SignInOutcome::Close));
}

#[test]
fn a_short_terminal_scrolls_and_keeps_the_field_in_view() {
    let mut m = Modal::new(prompt("chatgpt"), 60, 16);
    assert!(m.text().contains("Address ›"), "{}", m.text());
    assert!(m.text().contains("[ copy link ]"), "{}", m.text());
    // The rest is reachable by scrolling; the position shows on the border.
    assert!(m.text().contains("/"), "{}", m.text());
    m.key("pageup");
    m.key("pageup");
    m.key("pageup");
    assert!(m.text().contains("Sign in with ChatGPT"), "{}", m.text());
    // Only visible link rows become hyperlinks.
    for l in &m.links {
        assert!(l.y < 16);
    }
}

#[test]
fn hyperlink_sequences_cannot_be_broken_out_of() {
    let l = Hyperlink {
        x: 3,
        y: 4,
        text: "abc\x1b]0;title\x07".into(),
        url: "https://x.example/\x1b\\".into(),
        style: Style::default().add_modifier(Modifier::UNDERLINED),
    };
    let s = l.sequence();
    assert!(s.starts_with("\x1b7\x1b[5;4H"), "{s:?}");
    assert!(s.ends_with("\x1b[0m\x1b8"), "{s:?}");
    assert!(
        s.contains("\x1b]8;;https://x.example/\\\x1b\\abc]0;title\x1b]8;;\x1b\\"),
        "{s:?}"
    );
    assert!(!s.contains('\x07'));
}

/// The modal inside the app.
struct Ui {
    app: App,
    term: Terminal<TestBackend>,
    requests: mpsc::UnboundedReceiver<UiRequest>,
}

impl Ui {
    fn new() -> Ui {
        let (tx, requests) = mpsc::unbounded_channel();
        let mut ui = Ui {
            app: App::new(AppConfig::default(), tx),
            term: Terminal::new(TestBackend::new(100, 40)).unwrap(),
            requests,
        };
        ui.draw();
        ui
    }

    fn draw(&mut self) {
        let app = &mut self.app;
        self.term.draw(|f| app.render(f)).unwrap();
    }

    fn send(&mut self, events: Vec<Event>) {
        for e in events {
            self.app.handle(AppEvent::Terminal(e));
            self.draw();
        }
    }

    fn core(&mut self, c: CoreEvent) {
        self.app.handle(AppEvent::Core(c));
        self.draw();
    }

    fn click_text(&mut self, text: &str) {
        let (x, y) = find_text(self.term.backend().buffer(), text)
            .unwrap_or_else(|| panic!("`{text}` not on screen:\n{}", self.text()));
        self.send(vec![click(x, y)]);
    }

    fn text(&self) -> String {
        buffer_text(self.term.backend().buffer())
    }

    fn requests(&mut self) -> Vec<UiRequest> {
        std::iter::from_fn(|| self.requests.try_recv().ok()).collect()
    }
}

#[test]
fn app_chatgpt_sign_in_over_the_login_modal() {
    let mut ui = Ui::new();
    ui.send(type_text("draft"));
    ui.core(CoreEvent::Login {
        provider: Some("openai".into()),
    });
    // Sign in with ChatGPT is the last method.
    ui.send(keys("up enter"));
    assert!(matches!(ui.requests().as_slice(), [UiRequest::Login(_)]));
    ui.core(CoreEvent::SignIn(prompt(CHATGPT_ID)));
    assert!(ui.app.sign_in().is_some() && ui.app.login().is_some());
    assert!(
        ui.text().contains("No browser opened here"),
        "{}",
        ui.text()
    );
    assert!(!ui.app.hyperlinks().is_empty());

    // A bracketed paste goes to the sign-in, not the chat input.
    ui.app.handle(AppEvent::Terminal(Event::Paste(address())));
    ui.send(keys("enter"));
    assert_eq!(ui.app.draft(), "draft");
    let reqs = ui.requests();
    let [UiRequest::SignInPaste { id, text }] = reqs.as_slice() else {
        panic!("{reqs:?}")
    };
    assert_eq!((id.as_str(), text.text()), (CHATGPT_ID, address().as_str()));
    ui.core(CoreEvent::SignInAnswer {
        id: CHATGPT_ID.into(),
        result: Ok(()),
    });
    assert!(ui.text().contains("finishing the sign-in"), "{}", ui.text());

    // ctrl+y copies the link through the app's clipboard.
    ui.send(vec![key("ctrl+y")]);
    assert!(ui.app.take_effects().contains(&Effect::Copy {
        what: "sign-in link".into(),
        text: url(),
    }));

    // Done: the sign-in modal closes and the `/login` modal shows the
    // outcome.
    ui.core(CoreEvent::SignInDone {
        id: CHATGPT_ID.into(),
        result: Ok("signed in with ChatGPT as user@example.com".into()),
    });
    ui.core(CoreEvent::LoginResult(Ok(
        "signed in with ChatGPT as user@example.com".into(),
    )));
    assert!(ui.app.sign_in().is_none());
    assert!(ui.app.hyperlinks().is_empty());
    assert!(ui.text().contains("OpenAI: logged in"), "{}", ui.text());
    assert!(!ui.text().contains(CODE));
}

#[test]
fn app_cancel_stops_the_sign_in_with_keyboard_or_mouse() {
    for by_mouse in [false, true] {
        let mut ui = Ui::new();
        ui.core(CoreEvent::Login {
            provider: Some("openai".into()),
        });
        ui.send(keys("up enter"));
        ui.requests();
        ui.core(CoreEvent::SignIn(prompt(CHATGPT_ID)));
        if by_mouse {
            ui.click_text("[ cancel ]");
        } else {
            ui.send(keys("esc"));
        }
        assert_eq!(
            ui.requests(),
            vec![UiRequest::SignInCancel(CHATGPT_ID.into())]
        );
        assert!(ui.app.sign_in().is_none());
        assert!(
            ui.app.login().is_none(),
            "the waiting /login modal closes too"
        );
    }
}

#[test]
fn app_mcp_sign_in_shows_the_outcome_in_the_modal() {
    let mut ui = Ui::new();
    let mut p = prompt("mcp.docs");
    p.title = "Authorize MCP server docs".into();
    p.forward = None;
    p.no_browser = None;
    ui.core(CoreEvent::SignIn(p));
    let text = ui.text();
    assert!(text.contains("Your browser should open"), "{text}");
    assert!(!text.contains("[ copy ssh command ]"), "{text}");
    ui.click_text("[ copy link ]");
    assert!(matches!(
        ui.app.take_effects().as_slice(),
        [Effect::Copy { what, .. }] if what == "sign-in link"
    ));
    // Another sign-in's events do not touch this one.
    ui.core(CoreEvent::SignInDone {
        id: "mcp.other".into(),
        result: Err("x".into()),
    });
    assert!(!ui.app.sign_in().unwrap().is_finished());
    ui.core(CoreEvent::SignInDone {
        id: "mcp.docs".into(),
        result: Ok("authorized MCP server docs".into()),
    });
    assert!(
        ui.text().contains("authorized MCP server docs"),
        "{}",
        ui.text()
    );
    ui.send(keys("enter"));
    assert!(ui.app.sign_in().is_none());
    assert!(
        ui.requests().is_empty(),
        "closing a finished sign-in sends nothing"
    );
}

#[tokio::test]
async fn the_run_loop_draws_the_link_as_osc8_after_each_frame() {
    let (req_tx, _req_rx) = mpsc::unbounded_channel();
    let (tx, mut rx) = mpsc::channel(16);
    let mut app = App::new(
        AppConfig {
            animations: false,
            ..AppConfig::default()
        },
        req_tx,
    );
    let mut term = Terminal::new(TestBackend::new(100, 40)).unwrap();
    let control = RecordingControl::default();
    let mut c = control.clone();
    tx.send(AppEvent::Core(CoreEvent::SignIn(prompt("mcp.docs"))))
        .await
        .unwrap();
    let handle = tokio::spawn(async move {
        crate::app::run_loop(&mut term, &mut c, &mut app, &mut rx)
            .await
            .unwrap();
    });
    tokio::task::yield_now().await;
    // The loop ends when its event channel closes.
    drop(tx);
    handle.await.unwrap();
    let osc: Vec<String> = control
        .calls()
        .into_iter()
        .filter(|c| c.starts_with("osc ") && c.contains("]8;;"))
        .collect();
    assert!(!osc.is_empty(), "{:?}", control.calls());
    let first = url().chars().take(20).collect::<String>();
    assert!(
        osc.iter().all(|c| c.contains(&format!("]8;;{}", url()))),
        "{osc:?}"
    );
    assert!(osc.iter().any(|c| c.contains(&first)));
}
