//! Overlays: the text panel, toasts and the notification history, approval
//! cards and the ini confirmation, the login modal, the browser sign-in
//! modal, the update dialog, the key help, the selection bar and the memory card and browser.

use super::*;
use crate::approval::{ApprovalAction, ApprovalPanel, IniConfirm};
use crate::interaction::testing::{key, wheel_down};
use crate::interaction::{Dispatch, HitMap, Interaction};
use crate::keys::{HelpOverlay, Keymap};
use crate::login::{LoginAction, LoginModal};
use crate::memory::browser::{BrowserAction, CommitRow, FileRow, MemoryBrowser, MemoryData};
use crate::notify::{Level, Notifications};
use crate::panel::{Panel, PanelButton};
use crate::signin::{SignInAction, SignInModal, SignInPrompt};
use crate::theme::Theme;
use crate::update::{UpdateAction, UpdateModal, UpdateNotice};
use harness_events::schema::{EventKind, MemoryAction, RunEvent, RunId};
use harness_policy::approvals::{ApprovalPrompt, ApprovalRequest};
use harness_policy::rules::{Action, Decision, Layer, MatchedRule, Verdict};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use std::time::Instant;
use time::macros::datetime;

const TS: time::OffsetDateTime = datetime!(2026-10-22 14:32:00 UTC);

fn term(w: u16, h: u16) -> Terminal<TestBackend> {
    Terminal::new(TestBackend::new(w, h)).unwrap()
}

#[test]
fn panel_wraps_and_scrolls_to_every_line() {
    matrix("panel", |w, h, check| {
        let mut fx = Fx::new();
        let mut text: Vec<String> = (0..6).map(|i| fx.prose(&format!("p{i}"))).collect();
        text.extend((0..40).map(|i| format!("line {i}")));
        text.push(fx.word("last"));
        let label = fx.word("button");
        let mut panel = Panel::new(fx.word("title"), &text, Theme::default())
            .with_buttons(vec![PanelButton::new("b", label)]);
        let mut t = term(w, h);
        let mut frames = Vec::new();
        for _ in 0..400 {
            let buf = frame(&mut t, |f| panel.render(f, f.area()));
            if !check {
                return;
            }
            frames.push(checked("panel", &buf));
            let before = panel.scroll_offset();
            panel.handle(&key("down"));
            if panel.scroll_offset() == before {
                break;
            }
        }
        // A title too long for the border shows above the text.
        assert_reachable("panel", &frames, &fx.all());
    });
}

fn notifications(fx: &mut Fx, n: usize) -> Notifications {
    let mut notes = Notifications::new();
    let now = Instant::now();
    for i in 0..n {
        let title = fx.word(&format!("t{i}"));
        let body = fx.prose(&format!("b{i}"));
        notes.push(Level::Warning, title, Some(body), TS, now);
    }
    notes
}

#[test]
fn toasts_wrap_and_the_rest_is_in_the_history() {
    matrix("toasts", |w, h, check| {
        let mut fx = Fx::new();
        let mut notes = notifications(&mut fx, 3);
        let theme = Theme::default();
        let mut t = term(w, h);
        let buf = frame(&mut t, |f| {
            let mut hits: HitMap<()> = HitMap::new();
            notes.render_toasts(f, f.area(), &theme, &mut hits, &|_| ());
        });
        if !check {
            return;
        }
        let first = checked("toasts", &buf);
        // Cut toasts say where the rest is.
        let toasts = first.clone();
        let cut = fx.all().into_iter().any(|t| !shows(&toasts, t));
        if cut {
            assert!(
                toasts.contains("Notifications"),
                "a cut toast hints the history:\n{toasts}"
            );
        }
        // The documented action: the history shows everything.
        let mut frames = vec![first];
        notes.open_history();
        frames.extend(history_frames(&mut notes, &mut t, 3, &theme));
        assert_reachable("toasts", &frames, &fx.all());
    });
}

fn history_frames(
    notes: &mut Notifications,
    t: &mut Terminal<TestBackend>,
    n: usize,
    theme: &Theme,
) -> Vec<String> {
    let mut frames: Vec<String> = Vec::new();
    let draw = |notes: &mut Notifications, t: &mut Terminal<TestBackend>| {
        let buf = frame(t, |f| {
            let mut hits: HitMap<()> = HitMap::new();
            notes.render_history(
                f,
                f.area(),
                theme,
                &|_| Some("14:32".into()),
                &mut hits,
                &|_| (),
            );
        });
        checked("history", &buf)
    };
    // Select each item (it scrolls into view), then scroll through every
    // row (the wheel; PageUp/PageDown move a page of the same rows).
    notes.handle_key(&key_event("home"));
    for _ in 0..n {
        frames.push(draw(notes, t));
        notes.handle_key(&key_event("down"));
    }
    notes.handle_key(&key_event("home"));
    for _ in 0..500 {
        let f = draw(notes, t);
        if frames.last() == Some(&f) {
            break;
        }
        frames.push(f);
        // One wheel notch.
        notes.scroll(3);
    }
    frames
}

fn key_event(chord: &str) -> crossterm::event::KeyEvent {
    match key(chord) {
        crossterm::event::Event::Key(k) => k,
        _ => unreachable!(),
    }
}

#[test]
fn notification_history_wraps_rows_and_scrolls() {
    matrix("history", |w, h, check| {
        let mut fx = Fx::new();
        let mut notes = notifications(&mut fx, 6);
        notes.open_history();
        let theme = Theme::default();
        let mut t = term(w, h);
        if !check {
            frame(&mut t, |f| {
                let mut hits: HitMap<()> = HitMap::new();
                notes.render_history(f, f.area(), &theme, &|_| None, &mut hits, &|_| ());
            });
            return;
        }
        let frames = history_frames(&mut notes, &mut t, 6, &theme);
        assert_reachable("history", &frames, &fx.all());
    });
}

fn request(fx: &mut Fx, n: usize) -> ApprovalRequest {
    let bash = n == 0;
    let (action, command, preview) = if bash {
        let cmd = format!("{} --path {}", fx.word("cmd"), fx.path("cmd"));
        (
            Action::Bash {
                command: cmd.clone(),
            },
            Some(cmd),
            None,
        )
    } else {
        let path = fx.path(&format!("file{n}"));
        let body: String = (0..20).map(|i| format!("+line {i}\n")).collect();
        let last = fx.url(&format!("pv{n}"));
        (
            Action::Write {
                tool: "edit".into(),
                path: path.into(),
            },
            None,
            Some(format!("--- a/x\n+++ b/x\n@@ -1 +1,21 @@\n{body}+{last}\n")),
        )
    };
    ApprovalRequest {
        approval_id: format!("approval-1-{n}"),
        call_id: format!("call-{n}"),
        node_id: None,
        action,
        verdict: Verdict {
            decision: Decision::Ask,
            rule: MatchedRule {
                layer: Layer::Ini,
                origin: fx.path(&format!("origin{n}")),
                rule: fx.word(&format!("rule{n}")),
                reason: fx.wide(&format!("reason{n}")),
            },
            command,
            outward: false,
        },
        preview,
        suggested_pattern: Some(fx.word(&format!("pattern{n}"))),
    }
}

#[test]
fn approval_cards_wrap_and_scroll() {
    matrix("approvals", |w, h, check| {
        let mut fx = Fx::new();
        let reqs = vec![request(&mut fx, 0), request(&mut fx, 1)];
        let mut panel = ApprovalPanel::new(ApprovalPrompt {
            batch: 1,
            requests: reqs,
        });
        let theme = Theme::default();
        let mut t = term(w, h);
        let mut it: Interaction<ApprovalAction> = Interaction::new();
        let mut frames = Vec::new();
        let area_h = |w: u16, p: &ApprovalPanel| p.height(w, &theme).min(h / 2).max(1);
        // Expand every preview, then walk the cards and scroll each.
        for i in 0..2 {
            panel.activate(ApprovalAction::Preview(i));
        }
        // The first frame follows the focus; Home goes to the top.
        let mut hits = it.begin_frame();
        frame(&mut t, |f| {
            let ah = area_h(w, &panel);
            panel.render(
                f,
                ratatui::layout::Rect::new(0, 0, w, ah),
                &theme,
                &mut hits,
                &|a| a,
            );
        });
        it.end_frame(hits);
        panel.handle_key(&key_event("home"));
        for _ in 0..200 {
            let ah = area_h(w, &panel);
            let mut hits = it.begin_frame();
            let buf = frame(&mut t, |f| {
                panel.render(
                    f,
                    ratatui::layout::Rect::new(0, 0, w, ah),
                    &theme,
                    &mut hits,
                    &|a| a,
                );
            });
            it.end_frame(hits);
            if !check {
                return;
            }
            let f = checked("approvals", &buf);
            if frames.last() == Some(&f) {
                break;
            }
            frames.push(f);
            panel.scroll(1);
        }
        assert_reachable("approvals", &frames, &fx.all());
    });
}

#[test]
fn ini_confirmation_wraps() {
    matrix("ini-confirm", |w, h, check| {
        let mut fx = Fx::new();
        let pattern = fx.word("pattern");
        let proposal = harness_policy::approvals::IniRuleProposal {
            pattern: pattern.clone(),
        };
        let c = IniConfirm::new(proposal, std::path::PathBuf::from(fx.path("ini")));
        let theme = Theme::default();
        let mut t = term(w, h);
        let ch = c.height(w).min(h);
        let buf = frame(&mut t, |f| {
            let mut hits: HitMap<()> = HitMap::new();
            c.render(
                f,
                ratatui::layout::Rect::new(0, 0, w, ch),
                &theme,
                &mut hits,
                &|_| (),
            );
        });
        if check {
            let f = checked("ini-confirm", &buf);
            assert_reachable("ini-confirm", std::slice::from_ref(&f), &[&pattern]);
            assert!(f.contains("[y] yes") && f.contains("[n] no"), "{f}");
        }
    });
}

#[test]
fn login_modal_wraps_and_scrolls() {
    matrix("login", |w, h, check| {
        let mut fx = Fx::new();
        let theme = Theme::default();
        let mut t = term(w, h);
        let mut it: Interaction<LoginAction> = Interaction::new();
        let mut frames = Vec::new();
        let draw = |m: &mut LoginModal,
                    it: &mut Interaction<LoginAction>,
                    t: &mut Terminal<TestBackend>| {
            let mut out = Vec::new();
            // From the top (PageUp), then down a row at a time.
            m.scroll(-10_000);
            for _ in 0..40 {
                let mut hits = it.begin_frame();
                let buf = frame(t, |f| m.render(f, f.area(), &theme, &mut hits, &|a| a));
                it.end_frame(hits);
                if !check {
                    return out;
                }
                let s = checked("login", &buf);
                if out.last() == Some(&s) {
                    break;
                }
                out.push(s);
                m.scroll(1);
            }
            out
        };
        // The provider menu with its notes, then OpenAI's methods.
        let mut m = LoginModal::new(Some("openai")).unwrap();
        frames.extend(draw(&mut m, &mut it, &mut t));
        // The command field with a long command, then a long failure.
        m.activate(LoginAction::Option(1));
        let cmd = fx.prose("cmd");
        m.paste(&cmd);
        frames.extend(draw(&mut m, &mut it, &mut t));
        m.activate(LoginAction::Primary);
        m.finish(Err(fx.prose("err")));
        frames.extend(draw(&mut m, &mut it, &mut t));
        if !check {
            return;
        }
        let mut tokens = fx.all();
        tokens.extend([
            "Read the key from an environment variable",
            "use your ChatGPT plan; opens the browser, sets auth = chatgpt",
        ]);
        assert_reachable("login", &frames, &tokens);
    });
}

#[test]
fn sign_in_modal_wraps_and_scrolls() {
    matrix("sign-in", |w, h, check| {
        let mut fx = Fx::new();
        let theme = Theme::default();
        let mut t = term(w, h);
        let mut it: Interaction<SignInAction> = Interaction::new();
        let mut frames = Vec::new();
        let draw = |m: &mut SignInModal,
                    it: &mut Interaction<SignInAction>,
                    t: &mut Terminal<TestBackend>| {
            let mut out = Vec::new();
            m.scroll(-10_000);
            for _ in 0..60 {
                let mut hits = it.begin_frame();
                let mut links = Vec::new();
                let buf = frame(t, |f| {
                    m.render(f, f.area(), &theme, &mut hits, &|a| a, &mut links)
                });
                it.end_frame(hits);
                if !check {
                    return out;
                }
                let s = checked("sign-in", &buf);
                if out.last() == Some(&s) {
                    break;
                }
                out.push(s);
                m.scroll(1);
            }
            out
        };
        let mut m = SignInModal::new(SignInPrompt {
            id: "mcp.x".into(),
            title: fx.prose("title"),
            url: fx.url("link"),
            forward: Some(format!("ssh -N -L 1455:127.0.0.1:1455 {}", fx.word("host"))),
            no_browser: Some(fx.word("why")),
            device: None,
        });
        frames.extend(draw(&mut m, &mut it, &mut t));
        // A refused paste with a long reason, then a long failure.
        m.paste("http://127.0.0.1:1455/auth/callback?code=c&state=s");
        m.activate(SignInAction::Submit);
        m.answer(Err(fx.prose("refused")));
        frames.extend(draw(&mut m, &mut it, &mut t));
        m.finish(Err(fx.prose("err")));
        frames.extend(draw(&mut m, &mut it, &mut t));
        // A device authorization: two links and a code.
        let code = crate::signin::DeviceCode {
            user_code: zeroize::Zeroizing::new(fx.word("usercode")),
            verification_uri: fx.url("page"),
            verification_uri_complete: Some(zeroize::Zeroizing::new(fx.url("complete"))),
            expires_in: 600,
        };
        let mut m = SignInModal::new(SignInPrompt {
            id: "mcp.y".into(),
            title: fx.prose("devtitle"),
            url: code.link().to_owned(),
            forward: None,
            no_browser: Some(fx.word("devwhy")),
            device: Some(code),
        });
        frames.extend(draw(&mut m, &mut it, &mut t));
        if !check {
            return;
        }
        let mut tokens = fx.all();
        tokens.extend(["copy ssh command", "Address", "close", "copy code"]);
        assert_reachable("sign-in", &frames, &tokens);
    });
}

#[test]
fn update_dialog_wraps_and_scrolls() {
    matrix("update", |w, h, check| {
        let mut fx = Fx::new();
        let theme = Theme::default();
        let mut t = term(w, h);
        let mut it: Interaction<UpdateAction> = Interaction::new();
        let mut m = UpdateModal::new(UpdateNotice {
            current: fx.word("current"),
            latest: fx.word("latest"),
            url: fx.url("notes"),
            command: format!("curl -fsSL {} | bash", fx.url("install")),
        });
        let mut frames = Vec::new();
        m.scroll(-10_000);
        for _ in 0..60 {
            let mut hits = it.begin_frame();
            let mut links = Vec::new();
            let buf = frame(&mut t, |f| {
                m.render(f, f.area(), &theme, &mut hits, &|a| a, &mut links)
            });
            it.end_frame(hits);
            if !check {
                return;
            }
            let s = checked("update", &buf);
            if frames.last() == Some(&s) {
                break;
            }
            frames.push(s);
            m.scroll(1);
        }
        let mut tokens = fx.all();
        tokens.extend([
            "copy install command",
            "copy link",
            "[ close ]",
            "Esc close",
        ]);
        assert_reachable("update", &frames, &tokens);
    });
}

#[test]
fn help_overlay_wraps_and_scrolls() {
    matrix("help", |w, h, check| {
        let mut fx = Fx::new();
        let long: Vec<String> = vec![
            "ctrl+p".into(),
            "f1".into(),
            "ctrl+k".into(),
            "alt+p".into(),
        ];
        let keymap = Keymap::from_entries([("palette", long.as_slice(), None)]).unwrap();
        let _ = &mut fx;
        let theme = Theme::default();
        let mut t = term(w, h);
        let mut frames = Vec::new();
        for scroll in 0..60u16 {
            let mut hits: HitMap<()> = HitMap::new();
            let mut max = 0;
            let buf = frame(&mut t, |f| {
                max = HelpOverlay::new(&keymap, &theme).scrolled(scroll).render(
                    f,
                    f.area(),
                    &mut hits,
                    (),
                );
            });
            if !check {
                return;
            }
            let s = checked("help", &buf);
            frames.push(s);
            if scroll >= max {
                break;
            }
        }
        let mut tokens: Vec<String> = keymap
            .help_rows()
            .into_iter()
            // Keys wrap in their column, one chord per piece.
            .flat_map(|(k, d)| {
                let mut v: Vec<String> = k.split(", ").map(str::to_owned).collect();
                v.push(d.to_owned());
                v
            })
            .collect();
        tokens.push("esc to close".into());
        let tokens: Vec<&str> = tokens.iter().map(String::as_str).collect();
        assert_reachable("help", &frames, &tokens);
    });
}

#[test]
fn selection_bar_wraps_with_every_button() {
    matrix("select-bar", |w, h, check| {
        let theme = Theme::default();
        let mut t = term(w, h);
        let bh = crate::chat::copy::bar_height(w).min(h);
        let mut hits: HitMap<()> = HitMap::new();
        let buf = frame(&mut t, |f| {
            crate::chat::copy::render_bar(
                f,
                ratatui::layout::Rect::new(0, 0, w, bh),
                &theme,
                123,
                true,
                &mut hits,
                &|_| (),
            );
        });
        if check {
            let s = checked("select-bar", &buf);
            assert_reachable(
                "select-bar",
                &[s],
                &[
                    "SELECT",
                    "[copy]",
                    "[unmark]",
                    "[done]",
                    "123 lines",
                    "↑↓ move · space mark · y copy · esc done",
                ],
            );
            assert_eq!(hits.focusable().count(), 3, "every button is clickable");
        }
    });
}

fn memory_event(fx: &mut Fx) -> RunEvent {
    let note = fx.path("note");
    let (b1, b2) = (fx.prose("bullet1"), fx.word("bullet2"));
    RunEvent {
        seq: 0,
        ts: TS,
        run_id: RunId(uuid::Uuid::nil()),
        node_id: None,
        parent_node_id: None,
        kind: EventKind::Memory {
            action: MemoryAction::Written,
            memory_id: "0123456789abcdef0123456789abcdef01234567".into(),
            summary: format!("saved in {note}: {b1} | {b2}"),
        },
    }
}

#[test]
fn memory_card_wraps_inside_its_border() {
    matrix("memory-card", |w, h, check| {
        let mut fx = Fx::new();
        let card = crate::memory::card::MemoryCard::from_event(&memory_event(&mut fx)).unwrap();
        let theme = Theme::default();
        let mut t = term(w, h);
        let (lines, _) = card.lines(&theme, w as usize, false);
        let buf = frame(&mut t, |f| {
            f.render_widget(ratatui::widgets::Paragraph::new(lines.clone()), f.area());
        });
        if !check {
            return;
        }
        let mut frames = vec![checked("memory-card", &buf)];
        // Taller than the screen: the transcript scrolls it.
        for skip in 1..lines.len() {
            let rest: Vec<_> = lines.iter().skip(skip).cloned().collect();
            let buf = frame(&mut t, |f| {
                f.render_widget(ratatui::widgets::Paragraph::new(rest), f.area());
            });
            frames.push(checked("memory-card", &buf));
        }
        // The standalone card (in its own block).
        let ch = card.height_for(w).min(h);
        let buf = frame(&mut t, |f| {
            let mut hits: HitMap<()> = HitMap::new();
            card.render(
                f,
                ratatui::layout::Rect::new(0, 0, w, ch),
                &theme,
                &mut hits,
                &|_| (),
            );
        });
        frames.push(checked("memory-card block", &buf));
        assert_reachable("memory-card", &frames, &fx.all());
    });
}

#[test]
fn memory_browser_wraps_rows_detail_and_buttons() {
    matrix("memory-browser", |w, h, check| {
        let mut fx = Fx::new();
        let files: Vec<FileRow> = (0..8usize)
            .map(|i| FileRow {
                path: fx.path(&format!("f{i}")),
                title: fx.wide(&format!("title{i}")),
                memories: i,
            })
            .collect();
        let recent = vec![CommitRow {
            commit: "0123456789abcdef".into(),
            at: TS,
            subject: fx.prose("subject"),
        }];
        let mut b = MemoryBrowser::new(MemoryData {
            drive: fx.path("drive"),
            enabled: true,
            files,
            recent,
            dreams: Vec::new(),
        });
        b.open();
        let detail = fx.prose("detail");
        let theme = Theme::default();
        let mut t = term(w, h);
        let mut it: Interaction<BrowserAction> = Interaction::new();
        let mut frames = Vec::new();
        let mut buttons = std::collections::BTreeSet::new();
        let mut draw = |b: &mut MemoryBrowser, it: &mut Interaction<BrowserAction>| {
            let mut hits = it.begin_frame();
            let buf = frame(&mut t, |f| b.render(f, f.area(), &theme, &mut hits, &|a| a));
            for r in hits.focusable() {
                buttons.insert(r.id.0.clone());
            }
            it.end_frame(hits);
            buf
        };
        let first = draw(&mut b, &mut it);
        if !check {
            return;
        }
        frames.push(checked("memory-browser", &first));
        for i in 0..8 {
            b.activate(BrowserAction::Select(i));
            if i == 7 {
                let path = b.selected_path().unwrap().to_owned();
                b.set_detail(path, &detail);
            }
            frames.push(checked("memory-browser", &draw(&mut b, &mut it)));
        }
        // The detail (or, on a small screen, the whole browser) scrolls by
        // the wheel from the top (Home) down.
        let mut scroll_through = |b: &mut MemoryBrowser,
                                  it: &mut Interaction<BrowserAction>,
                                  frames: &mut Vec<String>| {
            b.handle_key(&key_event("home"), None);
            frames.push(checked("memory-browser", &draw(b, it)));
            for _ in 0..100 {
                let before = b.detail_scroll();
                let pane = it
                    .hits()
                    .pane_rect(&crate::interaction::PaneId::from(
                        crate::memory::browser::DETAIL_PANE,
                    ))
                    .expect("the detail pane");
                let (px, py) = (pane.x + pane.width / 2, pane.y + pane.height / 2);
                if let Some(Dispatch::Scroll { lines, .. }) =
                    it.handle_mouse(&mouse_wheel(&wheel_down(px, py)))
                {
                    b.scroll_detail(lines);
                }
                frames.push(checked("memory-browser", &draw(b, it)));
                if b.detail_scroll() == before {
                    break;
                }
            }
        };
        scroll_through(&mut b, &mut it, &mut frames);
        b.activate(BrowserAction::Tab(crate::memory::browser::Tab::Recent));
        scroll_through(&mut b, &mut it, &mut frames);
        // The drive is in the title: reachable in full on the Files tab.
        assert_reachable("memory-browser", &frames, &fx.all());
        for id in [
            "memory.btn.close",
            "memory.btn.dream",
            "memory.btn.toggle",
            "memory.btn.forget",
        ] {
            assert!(buttons.contains(id), "{id} not reachable: {buttons:?}");
        }
    });
}

fn mouse_wheel(e: &crossterm::event::Event) -> crossterm::event::MouseEvent {
    match e {
        crossterm::event::Event::Mouse(m) => *m,
        _ => unreachable!(),
    }
}

// ---- the scroll and expand affordances, by mouse and by keyboard ----

mod affordances {
    use super::*;
    use crate::app::{App, AppCommand, AppConfig, AppEvent, CoreEvent};
    use crate::chat::view::{ChatConfig, ChatView};
    use crate::interaction::testing::{click, find_text, wheel_up};
    use crate::status::StatusLine;
    use crossterm::event::Event;
    use harness_policy::approvals::{ApprovalUi, ChannelUi};
    use harness_policy::rules::Mode;
    use std::path::Path;
    use tokio::sync::mpsc;

    fn chat(w: u16, h: u16) -> (ChatView, Terminal<TestBackend>) {
        let now = Instant::now();
        let status = StatusLine::new(Path::new("/w"), None, Mode::AutoEdit);
        let view = ChatView::new(ChatConfig::default(), status, now);
        (view, term(w, h))
    }

    fn draw(view: &mut ChatView, t: &mut Terminal<TestBackend>) -> String {
        let now = Instant::now();
        screen(&frame(t, |f| view.render(f, f.area(), now)))
    }

    fn send(view: &mut ChatView, ev: Event) {
        view.handle(&ev, Instant::now(), TS);
    }

    #[test]
    fn notification_history_scrolls_by_wheel_and_page_keys() {
        let (mut view, mut t) = chat(60, 20);
        let mut fx = Fx::new();
        let now = Instant::now();
        for i in 0..6 {
            let b = fx.prose(&format!("n{i}"));
            view.notify(Level::Info, &format!("note {i}"), Some(b), now);
        }
        view.notifications_mut().open_history();
        let text = draw(&mut view, &mut t);
        assert!(text.contains("note 5"), "{text}");
        let (x, y) = find_text(t.backend().buffer(), "note 5").unwrap();
        // Mouse: the wheel scrolls rows; the selection stays.
        send(&mut view, wheel_down(x, y));
        draw(&mut view, &mut t);
        assert_eq!(view.notifications_mut().history_top(), 3);
        send(&mut view, wheel_up(x, y));
        draw(&mut view, &mut t);
        assert_eq!(view.notifications_mut().history_top(), 0);
        // Keyboard: PageDown / PageUp move a page.
        send(&mut view, key("pgdn"));
        draw(&mut view, &mut t);
        let top = view.notifications_mut().history_top();
        assert!(top > 3, "a page down: {top}");
        send(&mut view, key("pgup"));
        draw(&mut view, &mut t);
        assert_eq!(view.notifications_mut().history_top(), 0);
        // The position shows in the border while scrolled.
        send(&mut view, key("pgdn"));
        let text = draw(&mut view, &mut t);
        assert!(text.contains(&format!(" {}-", top + 1)), "{text}");
    }

    #[test]
    fn cut_toasts_point_to_the_history() {
        let (mut view, mut t) = chat(60, 20);
        let mut fx = Fx::new();
        view.notify(
            Level::Warning,
            &fx.word("title"),
            Some(fx.prose("body")),
            Instant::now(),
        );
        let text = draw(&mut view, &mut t);
        assert!(text.contains("more in Notifications"), "{text}");
        assert!(
            shows(&text, &fx.tokens[0]),
            "the title wraps in full:\n{text}"
        );
    }

    async fn approval(view: &mut ChatView, fx: &mut Fx) {
        let (chan, mut rx) = ChannelUi::new(2);
        let req = request(fx, 1);
        tokio::spawn(async move {
            chan.ask(ApprovalPrompt {
                batch: 1,
                requests: vec![req],
            })
            .await
        });
        view.push_approval(rx.recv().await.unwrap());
    }

    #[tokio::test]
    async fn approval_preview_expands_by_click_and_key() {
        let (mut view, mut t) = chat(70, 40);
        let mut fx = Fx::new();
        approval(&mut view, &mut fx).await;
        let text = draw(&mut view, &mut t);
        assert!(
            text.contains("more lines · e or click to show all"),
            "{text}"
        );
        assert!(!text.contains("line 15"), "{text}");
        // Mouse: click the row.
        let (x, y) = find_text(t.backend().buffer(), "e or click").unwrap();
        send(&mut view, click(x, y));
        let text = draw(&mut view, &mut t);
        assert!(text.contains("show less (e)"), "{text}");
        assert!(text.contains("line 15"), "{text}");
        // Keyboard: `e` collapses again.
        send(&mut view, key("e"));
        let text = draw(&mut view, &mut t);
        assert!(text.contains("e or click to show all"), "{text}");
    }

    #[tokio::test]
    async fn approval_cards_scroll_by_wheel_and_page_keys() {
        let (mut view, mut t) = chat(50, 20);
        let mut fx = Fx::new();
        approval(&mut view, &mut fx).await;
        send(&mut view, key("e"));
        let text = draw(&mut view, &mut t);
        let pos = |text: &str| -> String {
            text.lines()
                .find(|l| l.starts_with('↕'))
                .unwrap_or_else(|| panic!("no scroll hint:\n{text}"))
                .split_whitespace()
                .nth(1)
                .unwrap()
                .to_owned()
        };
        let auto = pos(&text);
        // Keyboard: Home to the top, PageDown a page.
        send(&mut view, key("home"));
        let top = pos(&draw(&mut view, &mut t));
        assert!(top.starts_with("1-"), "{top}");
        send(&mut view, key("pgdn"));
        let paged = pos(&draw(&mut view, &mut t));
        assert!(paged.starts_with("6-"), "{paged}");
        // Mouse: the wheel over the cards.
        let (_, y) = find_text(t.backend().buffer(), "↕").unwrap();
        send(&mut view, wheel_up(5, y - 1));
        let wheeled = pos(&draw(&mut view, &mut t));
        assert!(wheeled.starts_with("3-"), "{wheeled}");
        assert_ne!(auto, wheeled);
    }

    fn app(w: u16, h: u16) -> (App, Terminal<TestBackend>) {
        let (tx, rx) = mpsc::unbounded_channel();
        std::mem::forget(rx);
        (App::new(AppConfig::default(), tx), term(w, h))
    }

    fn app_draw(app: &mut App, t: &mut Terminal<TestBackend>) -> String {
        screen(&frame(t, |f| app.render(f)))
    }

    fn position(text: &str, prefix: &str) -> String {
        let i = text
            .find(prefix)
            .unwrap_or_else(|| panic!("no `{prefix}`:\n{text}"));
        text[i + prefix.len()..]
            .split(|c: char| c == '/' || c.is_whitespace())
            .next()
            .unwrap()
            .to_owned()
    }

    #[test]
    fn help_scrolls_by_wheel_and_keys() {
        let (mut app, mut t) = app(40, 12);
        app.handle(AppEvent::Command(AppCommand::ShowHelp));
        let text = app_draw(&mut app, &mut t);
        assert_eq!(position(&text, "↕ "), "1-8", "{text}");
        // Keyboard: Down, End, Home.
        app.handle(AppEvent::Terminal(key("down")));
        assert_eq!(position(&app_draw(&mut app, &mut t), "↕ "), "2-9");
        app.handle(AppEvent::Terminal(key("end")));
        let text = app_draw(&mut app, &mut t);
        assert!(text.contains("click activates"), "the last row: {text}");
        app.handle(AppEvent::Terminal(key("home")));
        assert_eq!(position(&app_draw(&mut app, &mut t), "↕ "), "1-8");
        // Mouse: the wheel over the list.
        let (x, y) = find_text(t.backend().buffer(), "send message").unwrap();
        app.handle(AppEvent::Terminal(wheel_down(x, y)));
        assert_eq!(position(&app_draw(&mut app, &mut t), "↕ "), "4-11");
        // Still closes with Esc.
        app.handle(AppEvent::Terminal(key("esc")));
        assert!(!app.help_open());
    }

    #[test]
    fn login_scrolls_by_wheel_and_page_keys() {
        let (mut app, mut t) = app(40, 12);
        app.handle(AppEvent::Core(CoreEvent::Login {
            provider: Some("openai".into()),
        }));
        let text = app_draw(&mut app, &mut t);
        assert_eq!(position(&text, "─ "), "1-7", "{text}");
        // Mouse: the wheel.
        let (x, y) = find_text(t.backend().buffer(), "Paste an API key").unwrap();
        app.handle(AppEvent::Terminal(wheel_down(x, y)));
        assert_eq!(position(&app_draw(&mut app, &mut t), "─ "), "4-10");
        // Keyboard: PageDown, PageUp.
        app.handle(AppEvent::Terminal(key("pgdn")));
        assert_eq!(position(&app_draw(&mut app, &mut t), "─ "), "9-15");
        app.handle(AppEvent::Terminal(key("pgup")));
        assert_eq!(position(&app_draw(&mut app, &mut t), "─ "), "4-10");
        // Choosing an option brings it back into view.
        app.handle(AppEvent::Terminal(key("down")));
        let text = app_draw(&mut app, &mut t);
        assert!(text.contains("▸ Use a command"), "{text}");
    }

    #[test]
    fn sign_in_scrolls_by_wheel_and_page_keys() {
        let (mut app, mut t) = app(40, 14);
        app.handle(AppEvent::Core(CoreEvent::SignIn(
            crate::signin::SignInPrompt {
                id: "chatgpt".into(),
                title: "Sign in with ChatGPT".into(),
                url: format!("https://auth.example/authorize?{}", "x".repeat(200)),
                forward: Some("ssh -N -L 1455:127.0.0.1:1455 dev@10.0.0.2".into()),
                no_browser: Some("this is an SSH session".into()),
                device: None,
            },
        )));
        // It opens with the address field in view.
        let text = app_draw(&mut app, &mut t);
        assert!(text.contains("Address ›"), "{text}");
        let start = position(&text, "─ ");
        // Keyboard: PageUp goes back to the top.
        for _ in 0..10 {
            app.handle(AppEvent::Terminal(key("pgup")));
            app_draw(&mut app, &mut t);
        }
        let text = app_draw(&mut app, &mut t);
        assert!(position(&text, "─ ").starts_with("1-"), "{text}");
        assert!(text.contains("Sign in with ChatGPT"), "{text}");
        // Mouse: the wheel scrolls down again.
        let (x, y) = find_text(t.backend().buffer(), "Sign in with").unwrap();
        app.handle(AppEvent::Terminal(wheel_down(x, y)));
        let text = app_draw(&mut app, &mut t);
        assert!(!position(&text, "─ ").starts_with("1-"), "{text}");
        // PageDown back to the field.
        for _ in 0..10 {
            app.handle(AppEvent::Terminal(key("pgdn")));
            app_draw(&mut app, &mut t);
        }
        assert_eq!(position(&app_draw(&mut app, &mut t), "─ "), start);
    }

    #[test]
    fn memory_detail_scrolls_by_wheel_and_page_keys() {
        let (mut app, mut t) = app(80, 24);
        let mut fx = Fx::new();
        let path = fx.path("note");
        app.handle(AppEvent::Core(CoreEvent::Memory(MemoryData {
            drive: "/m".into(),
            enabled: true,
            files: vec![FileRow {
                path: path.clone(),
                title: "note".into(),
                memories: 1,
            }],
            recent: Vec::new(),
            dreams: Vec::new(),
        })));
        let text: String = (0..40).map(|i| format!("- bullet {i}\n")).collect();
        app.handle(AppEvent::Core(CoreEvent::MemoryDetail { key: path, text }));
        let text = app_draw(&mut app, &mut t);
        assert_eq!(position(&text, " detail "), "1-8", "{text}");
        let (x, y) = find_text(t.backend().buffer(), "- bullet 1").unwrap();
        app.handle(AppEvent::Terminal(wheel_down(x, y)));
        assert_eq!(position(&app_draw(&mut app, &mut t), " detail "), "4-11");
        app.handle(AppEvent::Terminal(key("pgdn")));
        assert_eq!(position(&app_draw(&mut app, &mut t), " detail "), "9-16");
        app.handle(AppEvent::Terminal(key("end")));
        let text = app_draw(&mut app, &mut t);
        assert!(text.contains("- bullet 39"), "{text}");
        app.handle(AppEvent::Terminal(key("home")));
        assert_eq!(position(&app_draw(&mut app, &mut t), " detail "), "1-8");
    }

    #[test]
    fn panel_long_lines_wrap_and_scroll_by_wheel_and_keys() {
        let mut fx = Fx::new();
        let text = vec![fx.prose("a"), "short".into(), fx.word("end")];
        let mut panel = Panel::new("Cost", &text, Theme::default());
        let mut it_t = term(40, 8);
        let first = screen(&frame(&mut it_t, |f| panel.render(f, f.area())));
        assert!(shows(&first, &fx.tokens[0]), "{first}");
        assert!(first.contains("/"), "position indicator: {first}");
        // Mouse: the wheel scrolls wrapped rows.
        panel.handle(&wheel_down(5, 3));
        assert_eq!(panel.scroll_offset(), 3);
        // Keyboard: End shows the last wrapped row.
        panel.handle(&key("end"));
        let last = screen(&frame(&mut it_t, |f| panel.render(f, f.area())));
        assert!(shows(&last, &fx.tokens[5]), "{last}");
    }

    #[test]
    fn selection_bar_buttons_stay_clickable_when_it_wraps() {
        let (mut view, mut t) = chat(40, 12);
        view.transcript_mut().push_user("hello there", TS);
        draw(&mut view, &mut t);
        send(&mut view, key("ctrl+t"));
        let text = draw(&mut view, &mut t);
        assert!(
            text.contains("[done]") && text.contains("esc done"),
            "{text}"
        );
        let (x, y) = find_text(t.backend().buffer(), "[done]").unwrap();
        send(&mut view, click(x + 1, y));
        draw(&mut view, &mut t);
        assert!(view.transcript().selection().is_none(), "done by mouse");
        // Keyboard: Esc leaves too.
        send(&mut view, key("ctrl+t"));
        draw(&mut view, &mut t);
        assert!(view.transcript().selection().is_some());
        send(&mut view, key("esc"));
        assert!(view.transcript().selection().is_none(), "done by keyboard");
    }
}
