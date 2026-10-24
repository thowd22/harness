//! Text selection over the composed app: mouse drags, clicks and the
//! keyboard mode in every view, copied through the real clipboard path.

use super::*;
use crate::demo::events::{EventBuilder, NodeSpec};
use crate::interaction::testing::*;
use harness_events::schema::DeltaChannel;
use ratatui::backend::TestBackend;
use serde_json::json;
use std::sync::Mutex;

struct Ui {
    app: App,
    term: Terminal<TestBackend>,
    control: RecordingControl,
    _requests: mpsc::UnboundedReceiver<UiRequest>,
    now: Arc<Mutex<Instant>>,
}

impl Ui {
    fn sized(w: u16, h: u16) -> Ui {
        let (tx, requests) = mpsc::unbounded_channel();
        let mut app = App::new(AppConfig::default(), tx);
        app.set_clipboard(Clipboard::osc52_only());
        // An injected clock: double clicks never depend on test speed.
        let now = Arc::new(Mutex::new(Instant::now()));
        let clock = now.clone();
        app.set_clock(Arc::new(move || *clock.lock().unwrap()));
        let mut ui = Ui {
            app,
            term: Terminal::new(TestBackend::new(w, h)).unwrap(),
            control: RecordingControl::default(),
            _requests: requests,
            now,
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
            if self.app.is_dirty() {
                self.draw();
            }
        }
    }

    fn feed(&mut self, events: &[RunEvent]) {
        for e in events {
            self.app.handle(AppEvent::Run(Box::new(e.clone())));
        }
        self.draw();
    }

    fn core(&mut self, c: CoreEvent) {
        self.app.handle(AppEvent::Core(c));
        self.draw();
    }

    fn advance(&mut self, ms: u64) {
        *self.now.lock().unwrap() += Duration::from_millis(ms);
    }

    fn text(&self) -> String {
        buffer_text(self.term.backend().buffer())
    }

    fn find(&self, s: &str) -> (u16, u16) {
        find_text(self.term.backend().buffer(), s)
            .unwrap_or_else(|| panic!("`{s}` not on screen:\n{}", self.text()))
    }

    /// The cell of the last character of `s`.
    fn end_of(&self, s: &str) -> (u16, u16) {
        let (x, y) = self.find(s);
        (x + s.chars().count() as u16 - 1, y)
    }

    /// Press at `from`, drag through the middle to `to`, release there.
    fn drag(&mut self, from: (u16, u16), to: (u16, u16)) {
        self.send(vec![
            click(from.0, from.1),
            drag(to.0, to.1),
            release(to.0, to.1),
        ]);
    }

    /// Performs the pending effects and returns the texts copied.
    fn copies(&mut self) -> Vec<String> {
        let before = self.control.calls().len();
        flush(&mut self.term, &mut self.control, &mut self.app).unwrap();
        self.control.calls()[before..]
            .iter()
            .filter_map(|c| c.strip_prefix("osc "))
            .map(decode_osc52)
            .collect()
    }

    fn copied(&mut self) -> String {
        let c = self.copies();
        assert_eq!(c.len(), 1, "one copy expected: {c:?}\n{}", self.text());
        c[0].clone()
    }
}

/// The text of a logged (escaped) OSC 52 sequence.
fn decode_osc52(escaped: &str) -> String {
    let b64 = escaped
        .strip_prefix("\\u{1b}]52;c;")
        .and_then(|s| s.strip_suffix("\\u{7}"))
        .unwrap_or_else(|| panic!("not OSC 52: {escaped}"));
    let table = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut bits = 0u32;
    let mut n = 0;
    let mut out = Vec::new();
    for c in b64.bytes().filter(|c| *c != b'=') {
        let v = table.iter().position(|t| *t == c).unwrap() as u32;
        bits = (bits << 6) | v;
        n += 6;
        if n >= 8 {
            n -= 8;
            out.push((bits >> n) as u8);
            bits &= (1 << n) - 1;
        }
    }
    let text = String::from_utf8(out).unwrap();
    // Round trip: exactly what the clipboard writes for this text.
    assert_eq!(
        crate::clipboard::base64_encode(text.as_bytes()),
        b64_padded(b64)
    );
    text
}

fn b64_padded(s: &str) -> String {
    let mut s = s.to_owned();
    while !s.len().is_multiple_of(4) {
        s.push('=');
    }
    s
}

const PARAGRAPH: &str = "The parser reads the header first, then every section in order, \
and stops at the first malformed line so the error names it.";

fn session() -> Vec<RunEvent> {
    let mut b = EventBuilder::new();
    b.start(NodeSpec::step("main").agent("coder"));
    b.model_call("main", "c1", "sonnet");
    b.delta(
        "main",
        "c1",
        DeltaChannel::Text,
        &format!("{PARAGRAPH}\n\nNext paragraph here.\n"),
    );
    b.finish_call("main", "c1", crate::demo::events::usage(10, 0, 5, 0), 20);
    b.tool_call("main", "b1", "bash", json!({"command": "seq 3"}));
    b.tool_result(
        "main",
        "b1",
        true,
        "first output line\nsecond output line\nthird",
        3,
    );
    b.into_events()
}

/// A session with one long tool card (collapsed by default).
fn long_session() -> Vec<RunEvent> {
    let mut b = EventBuilder::new();
    b.start(NodeSpec::step("main").agent("coder"));
    b.tool_call("main", "b2", "bash", json!({"command": "seq 12"}));
    let out: Vec<String> = (1..=12).map(|i| format!("n{i}")).collect();
    b.tool_result("main", "b2", true, &out.join("\n"), 3);
    b.into_events()
}

#[test]
fn dragging_over_wrapped_chat_text_copies_the_logical_text_with_a_toast() {
    let mut ui = Ui::sized(60, 30);
    ui.feed(&session());
    let from = ui.find("The parser reads");
    let to = ui.end_of("names it.");
    assert!(to.1 > from.1 + 1, "the paragraph wraps:\n{}", ui.text());
    ui.drag(from, to);
    assert_eq!(ui.copied(), PARAGRAPH, "soft wraps are rejoined");
    assert!(ui.text().contains("copied selection"), "{}", ui.text());
    // The selection is highlighted with the selection token until cleared.
    let theme = Theme::default();
    let buf = ui.term.backend().buffer();
    assert_eq!(buf[from].bg, theme.color(Token::Selection));
    ui.send(keys("esc"));
    let buf = ui.term.backend().buffer();
    assert_ne!(buf[from].bg, theme.color(Token::Selection));
    assert!(ui.app.selection().selection().is_none());
}

#[test]
fn chat_chrome_is_not_copied_and_card_bodies_do_not_toggle_on_press() {
    let mut ui = Ui::sized(60, 30);
    ui.feed(&session());
    // From the assistant header to the card's last output line: no icons,
    // copy buttons, timestamps, borders or gutters.
    let from = ui.find("◆ assistant");
    let to = ui.end_of("third");
    ui.drag((0, from.1), (59, to.1));
    let text = ui.copied();
    for chrome in ["◆", "⧉", "╭", "│", "╰", "✔"] {
        assert!(!text.contains(chrome), "{chrome} in {text:?}");
    }
    assert!(text.starts_with("assistant\nThe parser reads"), "{text:?}");
    assert!(
        text.ends_with("first output line\nsecond output line\nthird"),
        "{text:?}"
    );
    // The press on the card body did not collapse it.
    assert!(ui.text().contains("second output line"), "{}", ui.text());
    // Only the card's output, from inside the card.
    let a = ui.find("second output line");
    ui.drag(a, ui.end_of("third"));
    assert_eq!(ui.copied(), "second output line\nthird");
}

#[test]
fn a_click_without_drag_still_activates_buttons_and_cards() {
    let mut ui = Ui::sized(60, 30);
    ui.feed(&session());
    // The message's copy button: a click copies the message, not a selection.
    let (x, y) = ui.find("⧉ copy");
    ui.send(vec![click(x, y), release(x, y)]);
    let c = ui.copies();
    assert_eq!(c.len(), 1);
    assert!(c[0].starts_with("The parser reads"), "{c:?}");
    assert!(ui.text().contains("copied message"), "{}", ui.text());
    // A drag that starts on a button selects nothing: the press is the
    // button's click (it copies the message again), the drag does nothing.
    ui.send(vec![click(x, y), drag(x + 4, y + 2), release(x + 4, y + 2)]);
    let c = ui.copies();
    assert_eq!(c.len(), 1, "{c:?}");
    assert!(c[0].starts_with("The parser reads"), "{c:?}");
    assert!(ui.app.selection().selection().is_none());
    // A click on a card's header toggles it, and so does a click on its
    // body (on the release) and on its expand hint; a press on the body
    // alone only focuses it, and a drag over the body does not toggle it.
    let mut ui = Ui::sized(60, 40);
    ui.feed(&long_session());
    assert!(ui.text().contains("more lines"), "{}", ui.text());
    let (hx, hy) = ui.find("$ seq 12");
    ui.send(vec![click(hx, hy), release(hx, hy)]);
    assert!(ui.copies().is_empty());
    assert!(ui.text().contains("Enter to collapse"), "{}", ui.text());
    let (bx, by) = ui.find("n5");
    ui.send(vec![click(bx, by)]);
    assert!(ui.text().contains("Enter to collapse"), "the press waits");
    assert_eq!(ui.app.chat().focused(), Some(&ElementId::from("card.b2")));
    ui.send(vec![release(bx, by)]);
    assert!(ui.text().contains("Enter to expand"), "{}", ui.text());
    assert!(ui.copies().is_empty());
    let (ex, ey) = ui.find("Enter to expand");
    ui.send(vec![click(ex, ey), release(ex, ey)]);
    assert!(ui.text().contains("Enter to collapse"), "{}", ui.text());
    let (bx, by) = ui.find("n5");
    ui.drag((bx, by), ui.end_of("n7"));
    assert_eq!(ui.copied(), "n5\nn6\nn7");
    assert!(
        ui.text().contains("Enter to collapse"),
        "a drag does not toggle"
    );
}

#[test]
fn text_under_a_toast_is_copied_and_not_highlighted() {
    // Short enough that the paragraph is on the top rows.
    let mut ui = Ui::sized(60, 19);
    ui.feed(&session());
    let from = ui.find("The parser reads");
    let to = ui.end_of("names it.");
    // A copy shows a toast in the top right, over the paragraph's rows.
    let n = ui.find("Next paragraph");
    ui.drag(n, ui.end_of("Next paragraph"));
    assert_eq!(ui.copied(), "Next paragraph");
    let (tx, ty) = ui.find("copied selection");
    // The toast is three rows high from the row above its title.
    assert!(
        ty + 2 >= from.1 && ty <= to.1 + 1 && tx > from.0,
        "the toast covers the paragraph:\n{}",
        ui.text()
    );
    assert!(!ui.text().contains("header first"), "{}", ui.text());
    // Selecting across the covered rows copies the whole text, not the
    // toast, and does not highlight the toast.
    ui.drag(from, (59, to.1));
    assert_eq!(ui.copied(), PARAGRAPH);
    let theme = Theme::default();
    let (tx, ty) = ui.find("copied selection");
    assert_ne!(
        ui.term.backend().buffer()[(tx, ty)].bg,
        theme.color(Token::Selection)
    );
    // A press on the toast does not start a selection.
    ui.send(vec![
        click(tx + 2, ty),
        drag(tx + 6, ty),
        release(tx + 6, ty),
    ]);
    assert!(ui.copies().is_empty());
}

#[test]
fn a_gesture_delivered_without_frames_in_between_still_copies() {
    let mut ui = Ui::sized(60, 30);
    ui.feed(&session());
    let from = ui.find("The parser reads");
    let to = ui.end_of("names it.");
    let mid = ui.end_of("header");
    // Press, drag, drag and release in one batch: no frame in between.
    for e in [
        click(from.0, from.1),
        drag(mid.0, mid.1),
        drag(to.0, to.1),
        release(to.0, to.1),
    ] {
        ui.app.handle(AppEvent::Terminal(e));
    }
    assert_eq!(ui.copied(), PARAGRAPH);
    ui.draw();
    assert!(ui.text().contains("copied selection"), "{}", ui.text());
    // A double click and a triple click in one batch each.
    let (x, y) = ui.find("header");
    let at = (x + 2, y);
    ui.advance(1000);
    for e in [click(at.0, at.1), release(at.0, at.1)] {
        ui.app.handle(AppEvent::Terminal(e));
    }
    ui.advance(100);
    for e in [click(at.0, at.1), release(at.0, at.1)] {
        ui.app.handle(AppEvent::Terminal(e));
    }
    assert_eq!(ui.copied(), "header");
    ui.advance(100);
    for e in [click(at.0, at.1), release(at.0, at.1)] {
        ui.app.handle(AppEvent::Terminal(e));
    }
    assert_eq!(ui.copied(), PARAGRAPH);
    ui.draw();
}

#[test]
fn dragging_over_picker_rows_copies_them_and_a_click_still_picks() {
    let mut ui = Ui::sized(120, 30);
    ui.core(CoreEvent::ModelPicker {
        catalog: crate::model_picker::tests::sample(true),
        current: Some("anthropic/claude-sonnet-5-5".into()),
    });
    let name = "claude-haiku-5-5";
    ui.drag(ui.find(name), ui.end_of(name));
    assert_eq!(ui.copied(), name);
    // The copy toast is drawn above the picker, not hidden under it.
    ui.draw();
    assert!(ui.text().contains("copied selection"), "{}", ui.text());
    assert!(ui.app.model_picker().is_some(), "the drag did not pick");
    assert!(ui._requests.try_recv().is_err(), "no model switch");
    // A press alone picks nothing; the release picks the row.
    let (x, y) = ui.find("claude-opus-5-5");
    ui.send(vec![click(x, y)]);
    assert!(ui.app.model_picker().is_some());
    ui.send(vec![release(x, y)]);
    assert!(ui.app.model_picker().is_none());
    assert_eq!(
        ui._requests.try_recv().ok(),
        Some(UiRequest::SetModel("anthropic/claude-opus-5-5".into()))
    );
    // The /resume picker: dragging over a session's name copies it.
    ui.core(CoreEvent::SessionPicker(
        crate::session_picker::tests::sample(),
    ));
    let name = "Fix header parser";
    ui.drag(ui.find(name), ui.end_of(name));
    assert_eq!(ui.copied(), name);
    assert!(ui.app.session_picker().is_some(), "the drag did not resume");
    assert!(ui._requests.try_recv().is_err(), "no resume");
    // (Not a double click on the drag's start.)
    ui.advance(1000);
    let (x, y) = ui.find(name);
    ui.send(vec![click(x, y), release(x, y)]);
    assert_eq!(
        ui._requests.try_recv().ok(),
        Some(UiRequest::Resume("s-000000bbbbbb".into()))
    );
}

#[test]
fn double_click_selects_a_word_and_triple_click_the_line() {
    let mut ui = Ui::sized(60, 30);
    ui.feed(&session());
    let (x, y) = ui.find("header");
    let at = (x + 2, y);
    ui.send(vec![click(at.0, at.1), release(at.0, at.1)]);
    assert!(ui.copies().is_empty(), "one click copies nothing");
    ui.advance(150);
    ui.send(vec![click(at.0, at.1), release(at.0, at.1)]);
    assert_eq!(ui.copied(), "header");
    ui.advance(150);
    ui.send(vec![click(at.0, at.1), release(at.0, at.1)]);
    assert_eq!(ui.copied(), PARAGRAPH, "the whole wrapped line");
    // A click elsewhere clears the selection.
    ui.advance(1000);
    let (nx, ny) = ui.find("Next paragraph");
    ui.send(vec![click(nx, ny), release(nx, ny)]);
    assert!(ui.app.selection().selection().is_none());
}

#[test]
fn dragging_past_the_top_of_the_chat_scrolls_and_extends_the_selection() {
    let mut ui = Ui::sized(60, 16);
    let mut b = EventBuilder::new();
    b.start(NodeSpec::step("main").agent("coder"));
    b.model_call("main", "c1", "sonnet");
    let body: Vec<String> = (1..=40).map(|i| format!("row {i:02}")).collect();
    b.delta("main", "c1", DeltaChannel::Text, &body.join("\n\n"));
    b.finish_call("main", "c1", crate::demo::events::usage(10, 0, 5, 0), 20);
    ui.feed(b.events());
    let start = ui.end_of("row 40");
    // Hold the pointer on the top row: every tick scrolls up.
    ui.send(vec![click(start.0, start.1), drag(2, 0)]);
    assert!(ui.app.needs_tick(), "autoscroll ticks");
    for _ in 0..20 {
        ui.app.tick();
        ui.draw();
    }
    let top = ui.text().lines().next().unwrap().to_owned();
    ui.send(vec![release(2, 0)]);
    assert!(!ui.app.needs_tick() || ui.app.is_running());
    let text = ui.copied();
    let lines: Vec<&str> = text.lines().filter(|l| !l.is_empty()).collect();
    assert_eq!(lines.last(), Some(&"row 40"), "{text}");
    assert!(
        lines.len() > 20,
        "extended past the first screen: {lines:?}"
    );
    // Every row between the top and the start, in order, none missing.
    // The selection starts at the pointer's column, so when the view
    // stops on a text row (not the gap between two) that row is copied
    // from its middle.
    let first: usize = lines[0]
        .rsplit(' ')
        .next()
        .and_then(|n| n.parse().ok())
        .unwrap_or(1);
    let want: Vec<String> = (first..=40).map(|i| format!("row {i:02}")).collect();
    assert!(
        want[0].ends_with(lines[0]),
        "top row was {top:?}: {lines:?}"
    );
    assert_eq!(lines[1..], want[1..], "top row was {top:?}");
}

#[test]
fn the_input_box_is_selectable_and_wrapped_input_rejoins() {
    let mut ui = Ui::sized(40, 20);
    let draft = "please explain how the header parser handles tabs";
    ui.send(type_text(draft));
    let (x, y) = ui.find("please");
    let to = ui.end_of("tabs");
    assert!(to.1 > y, "the draft wraps:\n{}", ui.text());
    ui.drag((x, y), to);
    assert_eq!(ui.copied(), draft);
    // Pasting still goes into the input (and clears the selection).
    ui.send(vec![Event::Paste(" now".into())]);
    assert_eq!(ui.app.draft(), format!("{draft} now"));
}

#[test]
fn the_map_and_node_detail_are_selectable() {
    let mut ui = Ui::sized(100, 30);
    ui.feed(&session());
    ui.send(keys("tab"));
    assert_eq!(ui.app.view(), View::Map);
    let (x, y) = ui.find("main");
    ui.drag((x, y), (x + 3, y));
    let text = ui.copied();
    assert_eq!(text, "main", "no box drawing: {text:?}");
    // Node detail: the log, with chrome left out.
    ui.app.open_detail(NodeId::from("main"));
    ui.draw();
    let from = ui.find("first output line");
    ui.drag(from, ui.end_of("second output line"));
    assert_eq!(ui.copied(), "first output line\nsecond output line");
}

#[test]
fn node_detail_rejoins_soft_wrapped_text() {
    // The detail pane indents its log lines; the soft-wrap marker must stay
    // first so the copy rejoins wrapped rows (seen in the real TUI).
    let mut ui = Ui::sized(70, 40);
    ui.feed(&session());
    ui.app.open_detail(NodeId::from("main"));
    ui.draw();
    let from = ui.find("The parser reads");
    let to = ui.end_of("names it.");
    assert!(to.1 > from.1, "the paragraph wraps:\n{}", ui.text());
    ui.drag(from, to);
    assert_eq!(ui.copied(), PARAGRAPH);
}

#[test]
fn the_diff_screen_copies_diff_lines_without_the_gutter() {
    let mut ui = Ui::sized(100, 30);
    let mut b = EventBuilder::new();
    b.start(NodeSpec::step("main"));
    b.file_changed(
        "main",
        "src/a.rs",
        harness_events::schema::FileChangeKind::Modified,
        Some("--- a/src/a.rs\n+++ b/src/a.rs\n@@ -1 +1 @@\n-old line\n+new line\n"),
    );
    ui.feed(b.events());
    ui.core(CoreEvent::Diff(DiffSpec {
        root: PathBuf::from("/p"),
        node: None,
        report: None,
        reverter: None,
    }));
    assert_eq!(ui.app.screen(), Some("diff"));
    let (x, y) = ui.find("-old line");
    let to = ui.end_of("+new line");
    ui.drag((x.saturating_sub(6), y), to);
    assert_eq!(ui.copied(), "-old line\n+new line");
}

#[test]
fn panels_and_pickers_are_selectable_and_panels_autoscroll() {
    let mut ui = Ui::sized(60, 16);
    let lines: Vec<String> = (0..40).map(|i| format!("entry {i:02}")).collect();
    ui.core(CoreEvent::Panel {
        title: "Servers".into(),
        lines: lines.clone(),
        buttons: Vec::new(),
    });
    let from = ui.find("entry 01");
    ui.drag(from, ui.end_of("entry 03"));
    assert_eq!(ui.copied(), "entry 01\nentry 02\nentry 03");
    // Dragging onto the panel's bottom border scrolls it.
    let (x, y) = ui.find("entry 02");
    let bottom = ui.find("[ close ]").1 + 1;
    ui.send(vec![click(x, y), drag(x, bottom)]);
    for _ in 0..4 {
        ui.app.tick();
        ui.draw();
    }
    ui.send(vec![release(x, bottom)]);
    let text = ui.copied();
    let got: Vec<&str> = text.lines().collect();
    assert_eq!(got.first(), Some(&"entry 02"));
    assert!(got.len() > 14, "scrolled past the visible rows: {got:?}");
    let want: Vec<&str> = lines[2..2 + got.len()].iter().map(String::as_str).collect();
    assert_eq!(got, want);
    ui.send(keys("esc"));
    ui.send(keys("esc"));
    assert_eq!(ui.app.screen(), None);
    // The /resume picker: its text by mouse (its title, and a row, which
    // a drag copies without resuming) and a row's text by keyboard.
    ui.core(CoreEvent::SessionPicker(
        crate::session_picker::tests::sample(),
    ));
    let title = crate::session_picker::TITLE;
    ui.drag(ui.find(title), ui.end_of(title));
    assert_eq!(ui.copied(), title);
    let text = "parser docs and the guide";
    ui.drag(ui.find(text), ui.end_of(text));
    assert_eq!(ui.copied(), text);
    ui.send(keys("ctrl+t"));
    assert!(ui.app.selection().keyboard());
    ui.send(keys("shift+end enter"));
    let row = ui.copied();
    assert!(!row.is_empty());
    assert!(ui.text().contains(row.trim()), "{row:?}\n{}", ui.text());
    assert!(ui.app.session_picker().is_some(), "copying did not resume");
}

#[test]
fn keyboard_selection_on_a_screen_moves_extends_and_copies() {
    let mut ui = Ui::sized(60, 16);
    let lines: Vec<String> = (0..5).map(|i| format!("line {i}")).collect();
    ui.core(CoreEvent::Panel {
        title: "Output".into(),
        lines,
        buttons: Vec::new(),
    });
    ui.send(keys("ctrl+t"));
    assert!(ui.app.selection().keyboard());
    assert!(ui.text().contains("SELECT"), "{}", ui.text());
    ui.send(keys("down shift+down shift+end"));
    ui.send(keys("enter"));
    assert_eq!(ui.copied(), "line 1\nline 2");
    assert!(!ui.app.selection().keyboard());
    // Esc leaves; the panel stays open.
    ui.send(keys("ctrl+t esc"));
    assert!(!ui.app.selection().keyboard());
    assert_eq!(ui.app.screen(), Some("panel"));
    // The bar's buttons are clickable: [copy] copies the cursor's line.
    ui.send(keys("ctrl+t down down"));
    let (x, y) = ui.find("[copy]");
    ui.send(vec![click(x, y)]);
    assert_eq!(ui.copied(), "line 2");
    ui.send(keys("ctrl+t"));
    let (x, y) = ui.find("[done]");
    ui.send(vec![click(x, y)]);
    assert!(!ui.app.selection().keyboard());
    assert!(ui.copies().is_empty());
    // The palette entry starts it too, over the screen.
    ui.send(keys("ctrl+p"));
    ui.send(type_text("select text with the keyboard"));
    ui.send(keys("enter"));
    assert!(ui.app.selection().keyboard(), "{}", ui.text());
    ui.send(keys("shift+right shift+right ctrl+y"));
    assert_eq!(ui.copied(), "lin");
}

#[test]
fn the_chat_line_selection_mode_extends_with_shift_arrows() {
    let mut ui = Ui::sized(60, 30);
    ui.feed(&session());
    ui.send(keys("ctrl+t"));
    assert!(ui.text().contains("SELECT"), "{}", ui.text());
    // From the bottom line (blank, then the card's border) up over the
    // three output lines.
    ui.send(keys("shift+up shift+up shift+up shift+up enter"));
    assert_eq!(ui.copied(), "first output line\nsecond output line\nthird");
}

#[test]
fn a_mouse_selection_is_copied_again_with_the_copy_key() {
    let mut ui = Ui::sized(60, 30);
    ui.feed(&session());
    let from = ui.find("Next paragraph");
    ui.drag(from, ui.end_of("Next paragraph"));
    assert_eq!(ui.copied(), "Next paragraph");
    ui.send(keys("ctrl+y"));
    assert_eq!(ui.copied(), "Next paragraph");
    // Any other key clears it.
    ui.send(type_text("x"));
    assert!(ui.app.selection().selection().is_none());
    assert_eq!(ui.app.draft(), "x");
}

/// The column of the nearest `│` left of `(x, y)` on screen.
fn border_left_of(ui: &Ui, (x, y): (u16, u16)) -> u16 {
    let buf = ui.term.backend().buffer();
    (0..x)
        .rev()
        .find(|&bx| buf[(bx, y)].symbol() == "│")
        .unwrap_or_else(|| panic!("no border left of ({x}, {y}):\n{}", ui.text()))
}

#[test]
fn copying_a_truncated_tool_card_header_leaves_out_its_rule() {
    let mut ui = Ui::sized(44, 20);
    let mut b = EventBuilder::new();
    b.start(NodeSpec::step("main").agent("coder"));
    b.tool_call(
        "main",
        "b1",
        "bash",
        json!({"command": "printf 'line-the-first line-the-second line-the-third'"}),
    );
    b.tool_result("main", "b1", true, "out", 7);
    ui.feed(b.events());
    let (_, y) = ui.find("╭ ✔ bash");
    let row: String = ui.text().lines().nth(usize::from(y)).unwrap().to_owned();
    // TASK-160: a header too long for the border is no longer cut; the
    // command wraps onto the rows below it, inside the box.
    assert!(!row.contains('…'), "the title is not cut: {row}");
    ui.drag((0, y), (43, y + 1));
    let text = ui.copied();
    for chrome in ["─", "╭", "╮", "│", "✔"] {
        assert!(!text.contains(chrome), "{chrome} in {text:?}");
    }
    assert!(text.starts_with("bash"), "{text:?}");
    assert!(text.contains("7ms"), "{text:?}");
    assert!(text.contains("$ printf"), "{text:?}");
}

#[test]
fn a_press_on_a_panel_or_picker_border_selects_from_the_nearest_text() {
    let mut ui = Ui::sized(60, 16);
    let lines: Vec<String> = (0..5).map(|i| format!("entry {i:02}")).collect();
    ui.core(CoreEvent::Panel {
        title: "Servers".into(),
        lines,
        buttons: Vec::new(),
    });
    // The left border, then the right border dragging left.
    let (x, y) = ui.find("entry 01");
    let left = border_left_of(&ui, (x, y));
    ui.drag((left, y), ui.end_of("entry 02"));
    assert_eq!(ui.copied(), "entry 01\nentry 02");
    // (Below the copy toast, which blocks presses on the rows it covers.)
    let (x4, y4) = ui.find("entry 04");
    let buf = ui.term.backend().buffer();
    let right = (x4..60).find(|&bx| buf[(bx, y4)].symbol() == "│").unwrap();
    ui.drag((right, y4), (x4, y4));
    assert_eq!(ui.copied(), "entry 04");
    // A click on the border still does nothing; the panel stays open.
    ui.send(vec![click(left, y), release(left, y)]);
    assert!(ui.copies().is_empty());
    assert_eq!(ui.app.screen(), Some("panel"));
    ui.send(keys("esc"));
    assert_eq!(ui.app.screen(), None);

    // The model picker: from its left border over a row, without picking.
    ui = Ui::sized(120, 30);
    ui.core(CoreEvent::ModelPicker {
        catalog: crate::model_picker::tests::sample(true),
        current: Some("anthropic/claude-sonnet-5-5".into()),
    });
    let name = "claude-haiku-5-5";
    let at = ui.find(name);
    let left = border_left_of(&ui, at);
    ui.drag((left, at.1), ui.end_of(name));
    let text = ui.copied();
    assert!(text.ends_with(name), "{text:?}");
    assert!(!text.contains('│'), "{text:?}");
    assert!(ui.app.model_picker().is_some(), "the drag did not pick");
    assert!(ui._requests.try_recv().is_err(), "no model switch");

    // The chat card's border selects as before.
    let mut ui = Ui::sized(60, 30);
    ui.feed(&session());
    let (x, y) = ui.find("second output line");
    let left = border_left_of(&ui, (x, y));
    ui.drag((left, y), ui.end_of("third"));
    assert_eq!(ui.copied(), "second output line\nthird");
}
