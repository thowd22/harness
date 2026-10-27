//! Update dialog tests: what it shows, keyboard and mouse paths on the
//! dialog alone and inside the app, and that only Esc closes it.

use crossterm::event::Event;
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use tokio::sync::mpsc;

use super::*;
use crate::app::{App, AppConfig, AppEvent, CoreEvent, Effect, UiRequest};
use crate::interaction::testing::{
    buffer_text, click, find_text, key, keys, styled_snapshot, type_text, wheel_down,
};

const COMMAND: &str =
    "curl -fsSL https://raw.githubusercontent.com/thowd22/harness/main/install.sh | bash";
const URL: &str = "https://github.com/thowd22/harness/releases/tag/v0.5.0";

fn notice() -> UpdateNotice {
    UpdateNotice {
        current: "0.4.0".into(),
        latest: "0.5.0".into(),
        url: URL.into(),
        command: COMMAND.into(),
    }
}

fn copy_command() -> UpdateOutcome {
    UpdateOutcome::Copy {
        what: "install command",
        text: COMMAND.into(),
    }
}

fn copy_link() -> UpdateOutcome {
    UpdateOutcome::Copy {
        what: "release notes link",
        text: URL.into(),
    }
}

/// The dialog alone, with its hit map, like the app drives it.
struct Modal {
    modal: UpdateModal,
    interaction: Interaction<UpdateAction>,
    term: Terminal<TestBackend>,
    links: Vec<Hyperlink>,
}

impl Modal {
    fn new(w: u16, h: u16) -> Modal {
        let mut m = Modal {
            modal: UpdateModal::new(notice()),
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

    fn send(&mut self, ev: Event) -> UpdateOutcome {
        let out = match &ev {
            Event::Key(k) => self.modal.handle_key(k, &mut self.interaction),
            Event::Mouse(m) => self.modal.handle_mouse(m, &mut self.interaction),
            _ => UpdateOutcome::Ignored,
        };
        self.draw();
        out
    }

    fn key(&mut self, chord: &str) -> UpdateOutcome {
        self.send(key(chord))
    }

    fn click_text(&mut self, text: &str) -> UpdateOutcome {
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
fn shows_both_versions_the_release_notes_link_and_the_install_command() {
    let m = Modal::new(100, 30);
    let text = m.text();
    for s in [
        "Update available",
        "A new harness release is available",
        "Current version  0.4.0",
        "New version      0.5.0",
        "Release notes:",
        URL,
        "Install it with:",
        "curl -fsSL https://raw.githubusercontent.com/thowd22/harness/main/insta",
        "[ copy install command ]",
        "[ copy link ]",
        HINT,
    ] {
        assert!(text.contains(s), "{s}:\n{text}");
    }
    // The link is an OSC 8 hyperlink over its cells.
    assert_eq!(m.links.len(), 1);
    assert_eq!(
        (m.links[0].text.as_str(), m.links[0].url.as_str()),
        (URL, URL)
    );
    assert_eq!(
        find_text(m.term.backend().buffer(), URL),
        Some((m.links[0].x, m.links[0].y))
    );
    // The copy button has the focus when the dialog opens.
    assert_eq!(m.focused(), Some(COPY_COMMAND_ID));
    insta::assert_snapshot!("update_dialog", styled_snapshot(m.term.backend().buffer()));
}

#[test]
fn narrow_dialogs_cut_the_link_and_command_without_shortening_them() {
    let m = Modal::new(44, 30);
    let joined: String = m.links.iter().map(|l| l.text.as_str()).collect();
    assert!(m.links.len() > 1, "the link wraps");
    assert_eq!(joined, URL);
    let text = m.text().replace(['│', ' ', '\n'], "");
    assert!(
        text.contains(&COMMAND.replace(' ', "")),
        "the whole command is shown:\n{}",
        m.text()
    );
}

#[test]
fn keyboard_tab_and_enter_copy_and_only_esc_closes() {
    let mut m = Modal::new(100, 30);
    assert_eq!(m.key("enter"), copy_command());
    assert_eq!(m.key("tab"), UpdateOutcome::Changed);
    assert_eq!(m.focused(), Some(COPY_LINK_ID));
    assert_eq!(m.key("enter"), copy_link());
    // Tab wraps; the arrows and shift+tab move too.
    m.key("tab");
    assert_eq!(m.focused(), Some(COPY_COMMAND_ID));
    m.key("right");
    assert_eq!(m.focused(), Some(COPY_LINK_ID));
    m.key("shift+tab");
    assert_eq!(m.focused(), Some(COPY_COMMAND_ID));
    // Other keys do nothing.
    for k in [
        "q",
        "x",
        "space",
        "backspace",
        "ctrl+c",
        "ctrl+w",
        "f1",
        "home",
    ] {
        assert_eq!(m.key(k), UpdateOutcome::Ignored, "{k}");
    }
    assert_eq!(m.key("esc"), UpdateOutcome::Dismiss);
}

#[test]
fn mouse_clicks_the_buttons_and_clicks_outside_do_nothing() {
    let mut m = Modal::new(100, 30);
    assert_eq!(m.click_text("[ copy link ]"), copy_link());
    assert_eq!(m.focused(), Some(COPY_LINK_ID));
    assert_eq!(m.click_text("copy install command"), copy_command());
    assert_eq!(m.focused(), Some(COPY_COMMAND_ID));
    // Outside the dialog, and on its text: nothing.
    assert_eq!(m.send(click(0, 0)), UpdateOutcome::Ignored);
    assert_eq!(m.send(click(99, 29)), UpdateOutcome::Ignored);
    assert_eq!(m.click_text("Release notes:"), UpdateOutcome::Ignored);
}

#[test]
fn a_short_terminal_scrolls_to_every_line() {
    let mut m = Modal::new(60, 12);
    assert!(!m.text().contains("Install it with:"), "{}", m.text());
    // The buttons stay in view while the body scrolls.
    assert!(
        m.text().contains("[ copy install command ]"),
        "{}",
        m.text()
    );
    m.key("pagedown");
    assert!(m.text().contains("Install it with:"), "{}", m.text());
    m.key("pageup");
    assert!(m.text().contains("Current version"), "{}", m.text());
    let (x, y) = find_text(m.term.backend().buffer(), "Current version").unwrap();
    for _ in 0..4 {
        m.send(wheel_down(x, y));
    }
    assert!(m.text().contains("install.sh"), "{}", m.text());
}

/// The dialog inside the app.
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

    fn click_text(&mut self, text: &str) {
        let (x, y) = find_text(self.term.backend().buffer(), text)
            .unwrap_or_else(|| panic!("`{text}` not on screen:\n{}", self.text()));
        self.send(vec![click(x, y)]);
    }

    fn open(&mut self) {
        self.app
            .handle(AppEvent::Core(CoreEvent::UpdateAvailable(notice())));
        self.draw();
    }

    fn text(&self) -> String {
        buffer_text(self.term.backend().buffer())
    }

    fn requests(&mut self) -> Vec<UiRequest> {
        std::iter::from_fn(|| self.requests.try_recv().ok()).collect()
    }
}

#[test]
fn app_copies_through_the_clipboard_by_keyboard_and_mouse() {
    let mut ui = Ui::new();
    ui.send(type_text("draft"));
    ui.open();
    assert!(ui.text().contains("Update available"), "{}", ui.text());
    assert_eq!(ui.app.hyperlinks().len(), 1);

    ui.send(keys("enter"));
    assert_eq!(
        ui.app.take_effects(),
        vec![Effect::Copy {
            what: "install command".into(),
            text: COMMAND.into(),
        }]
    );
    ui.send(keys("tab enter"));
    assert_eq!(
        ui.app.take_effects(),
        vec![Effect::Copy {
            what: "release notes link".into(),
            text: URL.into(),
        }]
    );
    ui.click_text("[ copy install command ]");
    assert_eq!(
        ui.app.take_effects(),
        vec![Effect::Copy {
            what: "install command".into(),
            text: COMMAND.into(),
        }]
    );
    // Keys and pastes never reach the chat input below.
    ui.send(type_text("xyz"));
    ui.app
        .handle(AppEvent::Terminal(Event::Paste("pasted".into())));
    assert_eq!(ui.app.draft(), "draft");
    assert!(ui.requests().is_empty());
}

#[test]
fn app_closes_the_dialog_only_with_esc_and_records_the_dismissal() {
    let mut ui = Ui::new();
    ui.open();
    // Other keys, and clicks outside the dialog or on its text, leave it
    // open.
    ui.send(keys("q enter tab space backspace up down"));
    ui.send(vec![click(0, 0), click(99, 39)]);
    ui.click_text("Release notes:");
    assert!(ui.app.update_dialog().is_some());
    assert!(ui.text().contains("Update available"), "{}", ui.text());
    ui.app.take_effects();
    assert!(ui.requests().is_empty());

    ui.send(keys("esc"));
    assert!(ui.app.update_dialog().is_none());
    assert!(!ui.text().contains("Update available"), "{}", ui.text());
    assert!(ui.app.hyperlinks().is_empty());
    assert_eq!(
        ui.requests(),
        vec![UiRequest::DismissUpdate("0.5.0".into())]
    );
}

#[test]
fn app_a_sign_in_modal_goes_over_the_dialog() {
    let mut ui = Ui::new();
    ui.open();
    ui.app.handle(AppEvent::Core(CoreEvent::SignIn(
        crate::signin::SignInPrompt {
            id: "mcp.docs".into(),
            title: "Authorize MCP server docs".into(),
            url: "https://auth.example.com/authorize?x=1".into(),
            forward: None,
            no_browser: None,
            device: None,
        },
    )));
    ui.draw();
    // Esc goes to the sign-in on top; the dialog stays below.
    ui.send(keys("esc"));
    assert!(ui.app.sign_in().is_none());
    assert!(ui.app.update_dialog().is_some());
    assert_eq!(
        ui.requests(),
        vec![UiRequest::SignInCancel("mcp.docs".into())]
    );
    ui.send(keys("esc"));
    assert!(ui.app.update_dialog().is_none());
}
