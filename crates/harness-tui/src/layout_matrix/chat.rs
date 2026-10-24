//! The composed app: chat transcript, cards, activity, input, status line
//! and footer, with long content everywhere.

use super::*;
use crate::app::{App, AppConfig, AppEvent, CoreEvent};
use crate::chat::transcript::{EntryKind, TranscriptAction};
use crate::interaction::testing::wheel_up;
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use std::path::PathBuf;
use tokio::sync::mpsc;

/// The app at `w`x`h` with the long-content run and session, and the
/// fixtures it shows.
pub fn app(w: u16, h: u16) -> (App, Terminal<TestBackend>, Fx) {
    let (tx, rx) = mpsc::unbounded_channel();
    std::mem::forget(rx);
    let (events, mut fx) = run_events();
    let cwd = fx.path("cwd");
    let cfg = AppConfig {
        animations: false,
        ..AppConfig::default()
    }
    .with_dirs(PathBuf::from(cwd), Some(PathBuf::from("/home/u")));
    let mut app = App::new(cfg, tx);
    app.handle(AppEvent::Core(CoreEvent::Session {
        model: fx.word("session"),
        context_window: Some(200_000),
        branch: Some(fx.word("branch")),
    }));
    app.handle(AppEvent::Core(CoreEvent::McpStatus {
        failed: vec![fx.word("mcp")],
    }));
    for e in events {
        app.handle(AppEvent::Run(Box::new(e)));
    }
    let term = Terminal::new(TestBackend::new(w, h)).unwrap();
    (app, term, fx)
}

/// The screen text, with the goal bar pane (when it shows) split off the
/// main view so text wrapped in either reads as one run.
fn panes(buf: &Buffer) -> String {
    match crate::interaction::testing::find_text(buf, "┌ Goal") {
        Some((x, _)) => {
            let a = buf.area;
            format!(
                "{}\n{}",
                text_in(buf, Rect::new(0, 0, x, a.height)),
                text_in(buf, Rect::new(x, 0, a.width - x, a.height))
            )
        }
        None => screen(buf),
    }
}

/// Draws the app; returns the frame.
pub fn draw(app: &mut App, term: &mut Terminal<TestBackend>) -> Buffer {
    frame(term, |f| app.render(f))
}

#[test]
fn chat_screen_fits_and_everything_is_reachable() {
    matrix("chat", |w, h, check| {
        let (mut app, mut term, fx) = app(w, h);
        let first = screen(&draw(&mut app, &mut term));
        if !check {
            return;
        }
        let mut frames = vec![first];
        // Expand every card, then scroll from the bottom to the top.
        let ids: Vec<String> = app
            .chat()
            .transcript()
            .entries()
            .iter()
            .flat_map(|e| match &e.kind {
                EntryKind::Tools { group, .. } => {
                    group.cards.iter().map(|c| c.call_id.clone()).collect()
                }
                _ => Vec::new(),
            })
            .collect();
        for id in ids {
            app.chat_mut()
                .transcript_mut()
                .activate(&TranscriptAction::ToggleCard(id));
        }
        let turns: Vec<u64> = app
            .chat()
            .transcript()
            .entries()
            .iter()
            .filter(|e| matches!(e.kind, EntryKind::Assistant(_)))
            .map(|e| e.id)
            .collect();
        for id in turns {
            app.chat_mut()
                .transcript_mut()
                .activate(&TranscriptAction::ToggleThinking(id));
        }
        let mut last = String::new();
        for _ in 0..400 {
            let buf = draw(&mut app, &mut term);
            assert_clean("chat", &buf, buf.area);
            let f = panes(&buf);
            // Timers tick between frames; the view stopped when only they change.
            let key: String = f.chars().filter(|c| !c.is_ascii_digit()).collect();
            if key == last {
                break;
            }
            frames.push(f);
            last = key;
            app.handle(AppEvent::Terminal(wheel_up(w / 2, h / 2)));
        }
        // The status line's `+N more` (or the palette's "Show status")
        // opens the full status.
        // The panel scrolls (Down) when it does not fit.
        app.command(crate::app::AppCommand::ShowStatus);
        let mut last = String::new();
        for _ in 0..100 {
            let buf = draw(&mut app, &mut term);
            assert_clean("status panel", &buf, buf.area);
            let f = screen(&buf);
            if f == last {
                break;
            }
            frames.push(f.clone());
            last = f;
            app.handle(AppEvent::Terminal(crate::interaction::testing::key("down")));
        }
        {
            // The subagent's own tool calls show in its detail, not the chat.
            let tokens: Vec<&str> = fx
                .all()
                .into_iter()
                .filter(|t| !t.contains("subread"))
                .collect();
            assert_reachable("chat", &frames, &tokens)
        }
    });
}

#[test]
fn status_details_open_from_the_more_button_and_the_palette() {
    use crate::interaction::testing::{click, find_text, key, release, type_text};
    let (mut app, mut term, fx) = app(40, 12);
    let cwd = fx
        .tokens
        .iter()
        .find(|t| t.contains("cwd_"))
        .unwrap()
        .clone();
    let buf = draw(&mut app, &mut term);
    let (x, y) = find_text(&buf, "more").expect("+N more on the status line");
    app.handle(AppEvent::Terminal(click(x, y)));
    app.handle(AppEvent::Terminal(release(x, y)));
    assert_eq!(app.screen(), Some("panel"));
    assert_eq!(app.panel().map(|p| p.title()), Some("Status"));
    let shown = screen(&draw(&mut app, &mut term));
    assert!(shows(&shown, &cwd), "{shown}");
    // Keyboard: Esc closes; the palette's "Show status" opens it again.
    app.handle(AppEvent::Terminal(key("esc")));
    assert_eq!(app.screen(), None);
    draw(&mut app, &mut term);
    app.handle(AppEvent::Terminal(key("ctrl+p")));
    for e in type_text("show status") {
        app.handle(AppEvent::Terminal(e));
    }
    app.handle(AppEvent::Terminal(key("enter")));
    assert_eq!(app.panel().map(|p| p.title()), Some("Status"));
    // With the goal bar collapsed (too narrow), its progress indicator on
    // the status line opens the same panel, which lists the goal and tasks.
    app.handle(AppEvent::Terminal(key("esc")));
    let (mut app, mut term, fx) = super::chat::app(60, 30);
    // Short status values, so the indicator has room on the status line.
    let status = app.chat_mut().status_mut();
    status.cwd = "~".into();
    status.set_branch(None);
    status.set_model("m", None);
    status.set_mcp_failed(Vec::new());
    let buf = draw(&mut app, &mut term);
    let (x, y) =
        find_text(&buf, "☰").unwrap_or_else(|| panic!("goal indicator:\n{}", screen(&buf)));
    app.handle(AppEvent::Terminal(click(x, y)));
    app.handle(AppEvent::Terminal(release(x, y)));
    assert_eq!(app.panel().map(|p| p.title()), Some("Status"));
    let text = app.panel().unwrap().text().join("\n");
    for t in fx
        .tokens
        .iter()
        .filter(|t| t.contains("goal") || t.contains("todo"))
    {
        assert!(text.contains(t.as_str()), "{t}: {text}");
    }
}

#[test]
fn footer_wraps_on_narrow_terminals_and_keeps_its_buttons() {
    use crate::interaction::testing::{click, find_text, key, release};
    let (mut app, mut term, _) = app(24, 12);
    app.command(crate::app::AppCommand::SetMouse(false));
    let buf = draw(&mut app, &mut term);
    let text = screen(&buf);
    for want in [" chat ", " map ", "running", "mouse off", " keys "] {
        assert!(text.contains(want), "{want}:\n{text}");
    }
    let (_, y) = find_text(&buf, " keys ").unwrap();
    let (_, tabs) = find_text(&buf, " chat ").unwrap();
    assert_eq!(y, 11, "{text}");
    assert_eq!(tabs, 10, "the footer took a second row:\n{text}");
    // Clicks need mouse capture.
    app.command(crate::app::AppCommand::SetMouse(true));
    let buf = draw(&mut app, &mut term);
    let (x, y) = find_text(&buf, " keys ").unwrap();
    app.handle(AppEvent::Terminal(click(x + 1, y)));
    app.handle(AppEvent::Terminal(release(x + 1, y)));
    assert!(app.help_open());
    app.handle(AppEvent::Terminal(key("esc")));
    assert!(!app.help_open());
    // The keyboard selection bar wraps its hints too.
    app.handle(AppEvent::Terminal(key("ctrl+t")));
    let text = screen(&draw(&mut app, &mut term));
    for want in ["SELECT", "[copy]", "[done]", "esc done"] {
        assert!(text.contains(want), "{want}:\n{text}");
    }
}
