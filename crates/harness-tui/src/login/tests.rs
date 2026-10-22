//! `/login` modal tests: keyboard and mouse paths on the modal alone and
//! inside the app, rendered to a `TestBackend`.

use crossterm::event::Event;
use harness_config::ini::edit::Credential;
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use tokio::sync::mpsc;

use super::*;
use crate::app::{App, AppConfig, AppEvent, CoreEvent, UiRequest};
use crate::interaction::testing::{buffer_text, click, find_text, keys, type_text};
use crate::interaction::{Dispatch, Interaction};

const KEY: &str = "sk-or-v1-never-on-screen-1234";

/// The modal alone, with its hit map, like the app drives it.
struct Modal {
    modal: LoginModal,
    interaction: Interaction<LoginAction>,
    term: Terminal<TestBackend>,
}

impl Modal {
    fn new(provider: Option<&str>) -> Modal {
        let mut m = Modal {
            modal: LoginModal::new(provider).unwrap(),
            interaction: Interaction::new(),
            term: Terminal::new(TestBackend::new(90, 24)).unwrap(),
        };
        m.draw();
        m
    }

    fn draw(&mut self) {
        let theme = Theme::default();
        let (modal, interaction) = (&self.modal, &mut self.interaction);
        self.term
            .draw(|f| {
                let mut hits = interaction.begin_frame();
                modal.render(f, f.area(), &theme, &mut hits, &|a| a);
                interaction.end_frame(hits);
            })
            .unwrap();
    }

    /// Sends one event the way the app does and redraws.
    fn send(&mut self, ev: Event) -> LoginOutcome {
        let out = match &ev {
            Event::Key(k) => self.modal.handle_key(k),
            Event::Paste(t) => self.modal.paste(t),
            Event::Mouse(m) => match self.interaction.handle_mouse(m) {
                Some(Dispatch::Activate(a)) => self.modal.activate(a),
                Some(Dispatch::Back) => self.modal.activate(LoginAction::Back),
                _ => LoginOutcome::Ignored,
            },
            _ => LoginOutcome::Ignored,
        };
        self.draw();
        out
    }

    fn keys(&mut self, seq: &str) -> Vec<LoginOutcome> {
        keys(seq).into_iter().map(|e| self.send(e)).collect()
    }

    fn type_text(&mut self, s: &str) {
        for e in type_text(s) {
            self.send(e);
        }
    }

    fn click_text(&mut self, text: &str) -> LoginOutcome {
        let (x, y) = find_text(self.term.backend().buffer(), text)
            .unwrap_or_else(|| panic!("`{text}` not on screen:\n{}", self.text()));
        self.send(click(x, y))
    }

    fn text(&self) -> String {
        buffer_text(self.term.backend().buffer())
    }

    fn step(&self) -> LoginStep {
        self.modal.flow().step()
    }
}

#[test]
fn keyboard_path_masks_the_key_and_submits() {
    let mut m = Modal::new(None);
    let text = m.text();
    for s in [
        "Log in",
        "Log in to a provider",
        "Anthropic",
        "OpenAI",
        "OpenRouter",
    ] {
        assert!(text.contains(s), "{s}:\n{text}");
    }
    assert!(text.contains("[ cancel ]"), "{text}");
    // Arrows and Tab move the highlight; Enter chooses.
    m.keys("down tab up down");
    assert_eq!(m.modal.flow().selected(), 2);
    assert!(m.text().contains("▸ OpenRouter"), "{}", m.text());
    m.keys("enter");
    assert_eq!(m.step(), LoginStep::Method);
    assert!(m.text().contains("Paste an API key"), "{}", m.text());
    m.keys("enter");
    assert_eq!(m.step(), LoginStep::Value);
    m.type_text(KEY);
    m.keys("backspace");
    m.type_text("4");
    let text = m.text();
    assert!(!text.contains(KEY), "key on screen:\n{text}");
    assert!(!text.contains("never-on-screen"), "{text}");
    assert!(
        text.contains(&format!("API key › {}", "•".repeat(KEY.len()))),
        "{text}"
    );
    assert!(text.contains("[ submit ]") && text.contains("[ back ]"));
    let out = m.keys("enter").pop().unwrap();
    let LoginOutcome::Submit(sub) = out else {
        panic!("expected a submit, got {out:?}")
    };
    assert_eq!(sub.request().provider, "openrouter");
    let Credential::ApiKey(k) = &sub.request().credential else {
        panic!("inline key")
    };
    assert_eq!(k.expose(), KEY);
    let dbg = format!("{sub:?} {:?}", m.modal);
    assert!(!dbg.contains("never-on-screen"), "{dbg}");
    assert_eq!(dbg.split(' ').next(), Some("LoginSubmission(openrouter,"));
    assert_eq!(m.step(), LoginStep::Verifying);
    assert!(m.text().contains("checking the key"), "{}", m.text());

    m.modal
        .finish(Ok("OpenRouter accepted the key; saved api_key".into()));
    m.draw();
    assert!(m.text().contains("OpenRouter accepted the key"));
    assert!(matches!(m.keys("enter").pop(), Some(LoginOutcome::Close)));
}

#[test]
fn esc_backs_out_one_step_at_a_time() {
    let mut m = Modal::new(None);
    m.keys("enter enter");
    assert_eq!(m.step(), LoginStep::Value);
    m.type_text("abc");
    m.keys("esc");
    assert_eq!(m.step(), LoginStep::Method);
    m.keys("esc");
    assert_eq!(m.step(), LoginStep::Provider);
    assert!(matches!(m.keys("esc").pop(), Some(LoginOutcome::Close)));

    // A failed check: Enter tries again with an empty field, Esc closes.
    let mut m = Modal::new(Some("anthropic"));
    assert!(m.text().contains("subscription login"), "{}", m.text());
    m.keys("enter");
    m.type_text("sk-ant-x");
    assert!(matches!(
        m.keys("enter").pop(),
        Some(LoginOutcome::Submit(_))
    ));
    m.modal.finish(Err("Anthropic rejected the key".into()));
    m.draw();
    assert!(m.text().contains("Anthropic rejected the key"));
    assert!(m.text().contains("[ try again ]"));
    m.keys("enter");
    assert_eq!(m.step(), LoginStep::Value);
    assert_eq!(m.modal.flow().field().unwrap(), "");
    // Esc on the method step of a preset provider closes.
    m.keys("esc");
    assert!(matches!(m.keys("esc").pop(), Some(LoginOutcome::Close)));
}

#[test]
fn mouse_path_clicks_options_and_buttons() {
    let mut m = Modal::new(None);
    m.click_text("OpenAI");
    assert_eq!(m.modal.flow().provider().unwrap().provider, "openai");
    assert_eq!(m.step(), LoginStep::Method);
    m.click_text("Read the key from an environment variable");
    assert_eq!(m.step(), LoginStep::Value);
    // Variable names are not secret: shown as typed, prefilled.
    assert!(
        m.text().contains("Variable name › OPENAI_API_KEY"),
        "{}",
        m.text()
    );
    let LoginOutcome::Submit(sub) = m.click_text("[ submit ]") else {
        panic!("submit")
    };
    assert!(matches!(&sub.request().credential, Credential::Env(v) if v == "OPENAI_API_KEY"));
    m.modal
        .finish(Err("OPENAI_API_KEY is not set in this shell".into()));
    m.draw();
    m.click_text("[ try again ]");
    assert_eq!(m.step(), LoginStep::Value);
    m.click_text("[ back ]");
    assert_eq!(m.step(), LoginStep::Method);
    // A click outside the dialog backs out like Esc.
    m.send(click(0, 0));
    assert_eq!(m.step(), LoginStep::Provider);
    assert!(matches!(m.click_text("[ cancel ]"), LoginOutcome::Close));
}

#[test]
fn paste_fills_the_field_and_unknown_providers_are_refused() {
    let mut m = Modal::new(Some("openrouter"));
    m.click_text("Paste an API key");
    m.send(Event::Paste(format!("{KEY}\n")));
    assert_eq!(m.modal.flow().field().unwrap(), "•".repeat(KEY.len()));
    assert!(!m.text().contains(KEY));
    let e = LoginModal::new(Some("gemini")).unwrap_err();
    assert!(e.contains("anthropic, openai, openrouter"), "{e}");
}

#[test]
fn uses_theme_tokens() {
    let mut m = Modal::new(Some("openrouter"));
    m.keys("enter");
    m.type_text("abc");
    let theme = Theme::default();
    let buf = m.term.backend().buffer();
    let (x, y) = find_text(buf, "•••").unwrap();
    assert_eq!(buf[(x, y)].fg, theme.color(Token::Fg));
    let (x, y) = find_text(buf, "[ submit ]").unwrap();
    assert_eq!(buf[(x, y)].fg, theme.color(Token::Accent));
    let (x, y) = find_text(buf, "Log in ").unwrap();
    assert_eq!(buf[(x, y)].fg, theme.color(Token::Accent));
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
            term: Terminal::new(TestBackend::new(100, 30)).unwrap(),
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

    fn submissions(&mut self) -> Vec<LoginSubmission> {
        let mut out = Vec::new();
        while let Ok(r) = self.requests.try_recv() {
            match r {
                UiRequest::Login(s) => out.push(s),
                other => panic!("unexpected request {other:?}"),
            }
        }
        out
    }
}

#[test]
fn app_keyboard_path_sends_the_request_and_shows_the_result() {
    let mut ui = Ui::new();
    ui.send(type_text("draft"));
    ui.core(CoreEvent::Login { provider: None });
    assert!(ui.app.login().is_some());
    assert!(ui.text().contains("Log in to a provider"), "{}", ui.text());
    ui.send(keys("down down enter enter"));
    ui.send(type_text(KEY));
    // Typing went to the modal, not the chat input.
    assert_eq!(ui.app.draft(), "draft");
    assert!(!ui.text().contains(KEY), "{}", ui.text());
    ui.send(keys("enter"));
    let subs = ui.submissions();
    assert_eq!(subs.len(), 1);
    assert_eq!(subs[0].request().provider, "openrouter");
    ui.core(CoreEvent::LoginResult(Ok(
        "OpenRouter accepted the key; saved api_key".into(),
    )));
    assert!(ui.text().contains("OpenRouter: logged in"), "{}", ui.text());
    ui.send(keys("enter"));
    assert!(ui.app.login().is_none());
    assert!(!ui.text().contains(KEY));
}

#[test]
fn app_mouse_path_and_a_result_after_closing_becomes_a_toast() {
    let mut ui = Ui::new();
    ui.core(CoreEvent::Login {
        provider: Some("anthropic".into()),
    });
    assert!(ui.text().contains("subscription login"), "{}", ui.text());
    ui.click_text("Paste an API key");
    ui.app
        .handle(AppEvent::Terminal(Event::Paste("sk-ant-pasted-key".into())));
    ui.draw();
    assert!(!ui.text().contains("sk-ant-pasted-key"));
    ui.click_text("[ submit ]");
    assert_eq!(ui.submissions().len(), 1);
    // Closed while the key is checked; the result still reaches the user.
    ui.click_text("[ close ]");
    assert!(ui.app.login().is_none());
    ui.core(CoreEvent::LoginResult(Err(
        "Anthropic rejected the key (invalid x-api-key)".into(),
    )));
    assert!(ui.text().contains("login failed"), "{}", ui.text());
    // An unknown provider is a toast, not a modal.
    ui.core(CoreEvent::Login {
        provider: Some("gemini".into()),
    });
    assert!(ui.app.login().is_none());
    assert!(ui.text().contains("unknown provider"), "{}", ui.text());
}
