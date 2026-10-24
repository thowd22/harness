//! Pickers: the command palette (rows, past runs, argument prompts), slash
//! completion, `/model` and `/resume`, with long content. Rows keep their
//! columns; the selected item's full text opens below the list (Right),
//! scrolled with Shift+Down, so moving the selection through the list
//! reaches every token.

use super::*;
use crate::input::{Input, SlashCommand};
use crate::interaction::HitMap;
use crate::model_picker::ModelPicker;
use crate::palette::{ArgSpec, EntryKind, Palette, PaletteEntry};
use crate::session_picker::{SessionPicker, SessionRow};
use crate::theme::Theme;
use crossterm::event::{Event, KeyEvent};
use ratatui::Terminal;
use ratatui::backend::TestBackend;

fn k(chord: &str) -> KeyEvent {
    let Event::Key(k) = crate::interaction::testing::key(chord) else {
        unreachable!()
    };
    k
}

fn entries(fx: &mut Fx) -> Vec<PaletteEntry> {
    let mut v = Vec::new();
    for i in 0..4 {
        v.push(
            PaletteEntry::new(
                format!("e{i}"),
                format!("Entry {}", fx.word(&format!("pt{i}"))),
                fx.prose(&format!("pd{i}")),
                EntryKind::Workflow,
            )
            .with_scope(fx.path(&format!("ps{i}")))
            .with_key(format!("ctrl+{i}")),
        );
    }
    v.push(
        PaletteEntry::new(
            "run",
            format!("Coder · {} · 14:02 Oct 22 · ✔", fx.wide("run")),
            fx.url("rund"),
            EntryKind::Run,
        )
        .with_keep_tail(17),
    );
    for i in 0..12 {
        v.push(PaletteEntry::new(
            format!("b{i}"),
            format!("Short {i}"),
            "short",
            EntryKind::Builtin,
        ));
    }
    v
}

/// The rows of the open full-text detail in a screen: below its divider,
/// as many as its position (`a-b/n`) says, without the side borders.
fn detail_rows(screen: &str) -> Vec<String> {
    let mut lines = screen.lines().skip_while(|l| !l.contains("─ full text"));
    let Some(divider) = lines.next() else {
        return Vec::new();
    };
    let view = divider
        .split_whitespace()
        .find_map(|w| {
            let (range, _) = w.split_once('/')?;
            let (a, b) = range.split_once('-')?;
            Some(b.parse::<usize>().ok()? + 1 - a.parse::<usize>().ok()?)
        })
        .unwrap_or(usize::MAX);
    lines
        .take(view)
        .take_while(|l| !l.trim_start().starts_with('╰'))
        .map(|l| l.trim().trim_matches('│').to_owned())
        .collect()
}

/// Frames of `draw` with the full text open (Right) after each selection
/// step and each Shift+Down of the detail, checking every frame; the
/// first frame (detail closed) is checked too. Each item's detail, which
/// Shift+Down scrolls one row at a time, is also stitched back together
/// into one extra frame, so a token longer than the detail's rows counts
/// as reachable.
fn walk(
    name: &str,
    items: usize,
    mut draw: impl FnMut() -> ratatui::buffer::Buffer,
    mut key: impl FnMut(KeyEvent),
) -> Vec<String> {
    let buf = draw();
    assert_clean(name, &buf, buf.area);
    let mut frames = vec![screen(&buf)];
    key(k("right"));
    for _ in 0..items {
        let mut last = String::new();
        let mut stitched: Vec<String> = Vec::new();
        for _ in 0..400 {
            let buf = draw();
            assert_clean(name, &buf, buf.area);
            let f = screen(&buf);
            if f == last {
                break;
            }
            let rows = detail_rows(&f);
            if stitched.is_empty() {
                stitched = rows;
            } else if let Some(r) = rows.last() {
                stitched.push(r.clone());
            }
            frames.push(f.clone());
            last = f;
            key(k("shift+down"));
        }
        frames.push(stitched.join("\n"));
        key(k("down"));
    }
    frames
}

#[test]
fn palette_rows_detail_and_prompts_fit() {
    matrix("palette", |w, h, check| {
        let mut fx = Fx::new();
        let list = entries(&mut fx);
        let n = list.len();
        let mut p = Palette::new(list);
        p.open();
        let theme = Theme::default();
        let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
        let p = std::cell::RefCell::new(p);
        let draw = |term: &mut Terminal<TestBackend>| {
            frame(term, |f| {
                let mut hits: HitMap<()> = HitMap::new();
                p.borrow_mut()
                    .render(f, f.area(), &theme, &mut hits, &|_| ());
            })
        };
        if !check {
            draw(&mut term);
            p.borrow_mut().handle_key(&k("down"));
            draw(&mut term);
            return;
        }
        let mut frames = walk(
            "palette",
            n,
            || draw(&mut term),
            |e| {
                p.borrow_mut().handle_key(&e);
            },
        );
        // A long query wraps and stays whole.
        let q = fx.word("query");
        p.borrow_mut().set_query(&q);
        frames.push(checked("palette query", &draw(&mut term)));
        assert_reachable("palette", &frames, &fx.all());

        // An argument prompt for a long entry: title, argument name, hint,
        // then the typed value and an error.
        let mut fx = Fx::new();
        let mut spec = ArgSpec::required(fx.word("argname"));
        spec.hint = Some(fx.url("hint"));
        let e = PaletteEntry::new("w", fx.wide("ptitle"), "d", EntryKind::Workflow)
            .with_args(vec![spec]);
        p.borrow_mut().prompt_entry(e);
        let mut frames = vec![checked("prompt", &draw(&mut term))];
        p.borrow_mut().paste(&fx.path("typed"));
        frames.push(checked("prompt typed", &draw(&mut term)));
        assert_reachable("palette prompt", &frames, &fx.all());
    });
}

fn commands(fx: &mut Fx) -> Vec<SlashCommand> {
    (0..10)
        .map(|i| SlashCommand {
            name: format!("cmd{i}-{}", fx.word(&format!("cn{i}"))),
            description: format!(
                "{} {}",
                fx.url(&format!("cd{i}")),
                fx.wide(&format!("cw{i}"))
            ),
            args: Some(format!("<{}>", fx.word(&format!("ca{i}")))),
            scope: Some(fx.path(&format!("cs{i}"))),
        })
        .collect()
}

#[test]
fn slash_completion_shows_the_selected_command_in_full() {
    matrix("completion", |w, h, check| {
        let mut fx = Fx::new();
        let cmds = commands(&mut fx);
        let n = cmds.len();
        let mut input = Input::new();
        input.set_commands(cmds);
        input.insert_char('/');
        let theme = Theme::default();
        let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
        let mut frames = Vec::new();
        for _ in 0..n {
            let buf = frame(&mut term, |f| {
                let ch = input
                    .completion_height_at(f.area().width)
                    .min(f.area().height);
                let r = Rect::new(0, f.area().height - ch, f.area().width, ch);
                let mut hits: HitMap<()> = HitMap::new();
                input.render_completion(f, r, &theme, &mut hits, &|_| ());
            });
            if check {
                assert_clean("completion", &buf, buf.area);
            }
            frames.push(screen(&buf));
            input.handle_key(&k("down"), None);
        }
        if check && h >= 20 {
            // Below 20 rows the popup is shorter than one command in full
            // (it shares the screen with the transcript in the app).
            assert_reachable("completion", &frames, &fx.all());
        }
    });
}

fn sessions(fx: &mut Fx) -> Vec<SessionRow> {
    (0..5)
        .map(|i| SessionRow {
            id: format!("s-00000000000{i}"),
            name: fx.word(&format!("sn{i}")),
            description: fx.prose(&format!("sd{i}")),
            when: "14:40 today".into(),
            project: fx.path(&format!("sp{i}")),
            model: fx.wide(&format!("sm{i}")),
            messages: "12 msgs".into(),
            cost: "$0.04".into(),
            tag: format!("#00000{i}"),
            current: i == 0,
        })
        .collect()
}

#[test]
fn resume_picker_shows_the_selected_session_in_full() {
    matrix("resume", |w, h, check| {
        let mut fx = Fx::new();
        let rows = sessions(&mut fx);
        let n = rows.len();
        let p = std::cell::RefCell::new(SessionPicker::new(rows));
        // The first session that is not the open one is selected.
        p.borrow_mut().handle_key(&k("home"));
        let theme = Theme::default();
        let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
        let draw = |term: &mut Terminal<TestBackend>| {
            frame(term, |f| {
                let mut hits: HitMap<()> = HitMap::new();
                p.borrow_mut()
                    .render(f, f.area(), &theme, &mut hits, &|_| ());
            })
        };
        if !check {
            draw(&mut term);
            return;
        }
        let frames = walk(
            "resume",
            n,
            || draw(&mut term),
            |e| {
                p.borrow_mut().handle_key(&e);
            },
        );
        assert_reachable("resume", &frames, &fx.all());
    });
}

fn catalog(fx: &mut Fx) -> std::sync::Arc<harness_providers::catalog::Catalog> {
    use harness_providers::catalog::cache::LiveList;
    use harness_providers::catalog::live::LiveModel;
    use harness_providers::catalog::{AuthMethod, Catalog, CredentialState, Credentials};
    use harness_providers::registry::Registry;
    let models: Vec<LiveModel> = (0..3)
        .map(|i| LiveModel {
            id: fx.word(&format!("mid{i}")),
            context_window: Some(200_000),
            tools: Some(true),
            ..LiveModel::default()
        })
        .collect();
    let creds = Credentials::new().with("openrouter", AuthMethod::ApiKey, CredentialState::Ready);
    let mut lists = std::collections::BTreeMap::new();
    lists.insert(
        "openrouter".to_owned(),
        LiveList::new("openrouter", "https://openrouter.ai/api/v1", 1, models),
    );
    let c = Catalog::build(&Registry::empty(), &creds, &lists);
    // A failed refresh with a long reason.
    let mut v = serde_json::to_value(&c).unwrap();
    let err = fx.url("err");
    v["providers"] = serde_json::json!([{
        "provider": "openrouter",
        "auth": [],
        "fetched_at": null,
        "live_models": 3,
        "stale": false,
        "error": err,
    }]);
    std::sync::Arc::new(serde_json::from_value(v).unwrap())
}

#[test]
fn model_picker_shows_the_selected_model_in_full() {
    matrix("model", |w, h, check| {
        let mut fx = Fx::new();
        let cat = catalog(&mut fx);
        let n = cat.entries().len();
        let p = std::cell::RefCell::new(ModelPicker::new(cat, None));
        p.borrow_mut().handle_key(&k("home"));
        let theme = Theme::default();
        let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
        let draw = |term: &mut Terminal<TestBackend>| {
            frame(term, |f| {
                let mut hits: HitMap<()> = HitMap::new();
                p.borrow_mut()
                    .render(f, f.area(), &theme, &mut hits, &|_| ());
            })
        };
        if !check {
            draw(&mut term);
            return;
        }
        let frames = walk(
            "model",
            n,
            || draw(&mut term),
            |e| {
                p.borrow_mut().handle_key(&e);
            },
        );
        assert_reachable("model", &frames, &fx.all());
    });
}

#[test]
fn input_box_wraps_long_text_and_scrolls_to_every_row() {
    matrix("input", |w, h, check| {
        let mut fx = Fx::new();
        let mut input = Input::new();
        input.set_text(&format!("{}\n{}", fx.prose("in"), fx.prose("in2")));
        let theme = Theme::default();
        let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
        let mut frames = Vec::new();
        // The cursor starts at the end; Up walks it to the first row.
        for _ in 0..40 {
            let buf = frame(&mut term, |f| {
                let ih = input.height(f.area().width).min(f.area().height);
                let r = Rect::new(0, 0, f.area().width, ih);
                let mut hits: HitMap<()> = HitMap::new();
                input.render(f, r, &theme, true, &mut hits, &|_| ());
            });
            if check {
                assert_clean("input", &buf, buf.area);
            }
            frames.push(screen(&buf));
            input.handle_key(&k("up"), None);
        }
        if check {
            // Rows scroll by one, so consecutive frames overlap: stitch
            // each frame's first row in front of the previous text.
            let mut stitched: Vec<String> = Vec::new();
            for f in frames.iter().rev() {
                let rows: Vec<String> = f
                    .lines()
                    .map(|l| l.trim_start_matches('›').to_owned())
                    .filter(|l| !l.trim().is_empty())
                    .collect();
                if stitched.is_empty() {
                    stitched = rows;
                } else if let Some(r) = rows.last()
                    && !stitched.contains(r)
                {
                    stitched.push(r.clone());
                }
            }
            let mut all = frames.clone();
            all.push(stitched.join("\n"));
            assert_reachable("input", &all, &fx.all());
        }
    });
}

#[test]
fn completion_wraps_the_selected_command_and_its_rows_stay_clickable() {
    let mut input = Input::new();
    input.set_commands(vec![
        SlashCommand::new("short", "a short one"),
        SlashCommand {
            name: "long".into(),
            description: "a description that does not fit in one row ".repeat(3) + "tail_end",
            args: None,
            scope: Some("project".into()),
        },
    ]);
    input.insert_char('/');
    // Keyboard: the selection moves with Down/Up; only a cut command
    // takes extra rows ("long" sorts first and is selected).
    input.handle_key(&k("down"), None);
    assert_eq!(input.completion_height_at(40), 2, "`short` fits its row");
    input.handle_key(&k("up"), None);
    let ch = input.completion_height_at(40);
    assert!(ch > 2, "the selected command takes extra rows: {ch}");
    let theme = Theme::default();
    let mut term = Terminal::new(TestBackend::new(40, 10)).unwrap();
    let mut it: crate::interaction::Interaction<crate::input::InputAction> =
        crate::interaction::Interaction::new();
    let mut hits = it.begin_frame();
    let buf = frame(&mut term, |f| {
        input.render_completion(f, Rect::new(0, 0, 40, ch), &theme, &mut hits, &|a| a)
    });
    it.end_frame(hits);
    let text = screen(&buf);
    assert!(
        shows(&text, "tail_end") && text.contains("project"),
        "{text}"
    );
    // Mouse: a click on the last row of the wrapped command accepts it.
    let crossterm::event::Event::Mouse(m) = crate::interaction::testing::click(5, ch - 2) else {
        unreachable!()
    };
    let Some(crate::interaction::Dispatch::Activate(a)) = it.handle_mouse(&m) else {
        panic!("wrapped rows are clickable:\n{text}")
    };
    input.activate(&a);
    assert_eq!(input.raw(), "/long ");
}

/// In the app, the `→ full text` hints of the palette and the `/model`
/// and `/resume` pickers open the full text by click and by Right.
#[test]
fn app_pickers_open_the_full_text_by_click_and_by_key() {
    use crate::app::{App, AppConfig, AppEvent, CoreEvent};
    use crate::interaction::testing::{click, find_text, key, release};
    let mut fx = Fx::new();
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    std::mem::forget(rx);
    let mut app = App::new(AppConfig::default(), tx);
    let mut term = Terminal::new(TestBackend::new(80, 24)).unwrap();
    let draw = |app: &mut App, term: &mut Terminal<TestBackend>| frame(term, |f| app.render(f));
    let click_on = |app: &mut App, term: &mut Terminal<TestBackend>, text: &str| {
        let buf = draw(app, term);
        let (x, y) = find_text(&buf, text).unwrap_or_else(|| panic!("{text}:\n{}", screen(&buf)));
        app.handle(AppEvent::Terminal(click(x + 1, y)));
        app.handle(AppEvent::Terminal(release(x + 1, y)));
        screen(&draw(app, term))
    };
    // Palette (mouse).
    app.handle(AppEvent::Core(CoreEvent::PaletteEntries(entries(&mut fx))));
    app.handle(AppEvent::Terminal(key("ctrl+p")));
    app.handle(AppEvent::Terminal(key("end")));
    let mut n = 0;
    while !screen(&draw(&mut app, &mut term)).contains("→ full text") && n < 40 {
        app.handle(AppEvent::Terminal(key("up")));
        n += 1;
    }
    let text = click_on(&mut app, &mut term, "→ full text");
    assert!(text.contains("─ full text"), "{text}");
    app.handle(AppEvent::Terminal(key("esc")));
    // /model (keyboard, then mouse to close).
    app.handle(AppEvent::Core(CoreEvent::ModelPicker {
        catalog: catalog(&mut fx),
        current: None,
    }));
    let text = screen(&draw(&mut app, &mut term));
    assert!(text.contains("→ full text"), "{text}");
    app.handle(AppEvent::Terminal(key("right")));
    let text = screen(&draw(&mut app, &mut term));
    assert!(text.contains("─ full text"), "{text}");
    let text = click_on(&mut app, &mut term, "← hide full text");
    assert!(!text.contains("─ full text"), "{text}");
    app.handle(AppEvent::Terminal(key("esc")));
    // /resume (mouse).
    app.handle(AppEvent::Core(CoreEvent::SessionPicker(sessions(&mut fx))));
    let text = click_on(&mut app, &mut term, "→ full text");
    assert!(text.contains("─ full text"), "{text}");
    app.handle(AppEvent::Terminal(key("right")));
    let text = screen(&draw(&mut app, &mut term));
    assert!(!text.contains("─ full text"), "{text}");
}
