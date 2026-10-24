//! Model picker tests: layout at several terminal sizes, filtering, keys,
//! mouse and refresh.

use std::collections::BTreeMap;
use std::time::Instant;

use crossterm::event::{Event, KeyModifiers};
use harness_providers::catalog::cache::LiveList;
use harness_providers::catalog::live::LiveModel;
use harness_providers::catalog::{AuthMethod, CredentialState, Credentials};
use harness_providers::pricing::Price;
use harness_providers::registry::Registry;
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;

use super::*;
use crate::interaction::testing::{buffer_text, click, find_text, styled_snapshot, wheel_down};
use crate::interaction::{Dispatch, Interaction};

fn live(id: &str, ctx: u32, price: Option<(f64, f64)>, tools: bool) -> LiveModel {
    LiveModel {
        id: id.into(),
        context_window: Some(ctx),
        price: price.map(|(i, o)| Price::new(i, o)),
        tools: Some(tools),
        reasoning: Some(tools),
        vision: Some(ctx >= 1_000_000),
        ..LiveModel::default()
    }
}

/// A small catalog: Anthropic and OpenRouter have keys, OpenAI does not.
/// One OpenRouter model has no tool calling and one has a very long id.
pub(crate) fn sample(refreshing: bool) -> Arc<Catalog> {
    sample_with(refreshing, 0)
}

/// [`sample`] plus `extra` OpenRouter models.
pub(crate) fn sample_with(refreshing: bool, extra: usize) -> Arc<Catalog> {
    let registry = Registry::empty()
        .with_aliases([
            ("sonnet", "anthropic/claude-sonnet-5-5"),
            ("opus", "anthropic/claude-opus-5-5"),
            ("gpt", "openai/gpt-5.5"),
        ])
        .unwrap()
        .with_price_overrides([
            ("anthropic/claude-sonnet-5-5", Price::new(3.0, 15.0)),
            ("anthropic/claude-opus-5-5", Price::new(5.0, 25.0)),
            ("anthropic/claude-haiku-5-5", Price::new(1.0, 5.0)),
        ]);
    let creds = Credentials::new()
        .with("anthropic", AuthMethod::ApiKey, CredentialState::Ready)
        .with("openai", AuthMethod::ApiKey, CredentialState::Missing)
        .with("openrouter", AuthMethod::ApiKey, CredentialState::Ready);
    let mut lists = BTreeMap::new();
    lists.insert(
        "anthropic".to_owned(),
        LiveList::new(
            "anthropic",
            "https://api.anthropic.com",
            1,
            vec![
                live("claude-opus-5-5", 1_000_000, None, true),
                live("claude-sonnet-5-5", 200_000, None, true),
                live("claude-haiku-5-5", 200_000, None, true),
            ],
        ),
    );
    lists.insert(
        "openai".to_owned(),
        LiveList::new(
            "openai",
            "https://api.openai.com",
            1,
            vec![
                live("gpt-5.5", 400_000, Some((1.25, 10.0)), true),
                live("gpt-5.5-mini", 400_000, Some((0.25, 2.0)), true),
            ],
        ),
    );
    let mut or = vec![
        live(
            "anthropic/claude-sonnet-5.5",
            200_000,
            Some((3.0, 15.0)),
            true,
        ),
        live(
            "google/gemini-2.5-pro-preview-with-a-very-long-model-identifier-2026-10",
            1_048_576,
            Some((1.25, 10.0)),
            true,
        ),
        live(
            "meta-llama/llama-4-scout:free",
            128_000,
            Some((0.0, 0.0)),
            true,
        ),
        live(
            "perplexity/sonar-deep-research",
            128_000,
            Some((2.0, 8.0)),
            false,
        ),
    ];
    for i in 0..extra {
        or.push(live(
            &format!("vendor{}/model-{i:04}", i % 40),
            32_000 + i as u32,
            Some((0.5, 1.5)),
            true,
        ));
    }
    lists.insert(
        "openrouter".to_owned(),
        LiveList::new("openrouter", "https://openrouter.ai/api/v1", 1, or),
    );
    let catalog = Catalog::build(&registry, &creds, &lists);
    if !refreshing {
        return Arc::new(catalog);
    }
    let mut v = serde_json::to_value(&catalog).unwrap();
    v["refreshing"] = serde_json::Value::Bool(true);
    Arc::new(serde_json::from_value(v).unwrap())
}

fn picker() -> ModelPicker {
    ModelPicker::new(sample(false), Some("anthropic/claude-sonnet-5-5".into()))
}

fn key(chord: &str) -> KeyEvent {
    let Event::Key(k) = crate::interaction::testing::key(chord) else {
        unreachable!()
    };
    k
}

fn typ(p: &mut ModelPicker, s: &str) {
    for c in s.chars() {
        p.handle_key(&KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
    }
}

fn draw(
    p: &mut ModelPicker,
    it: &mut Interaction<PickerAction>,
    w: u16,
    h: u16,
) -> Terminal<TestBackend> {
    let theme = Theme::default();
    let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
    let mut hits = it.begin_frame();
    term.draw(|f| p.render(f, f.area(), &theme, &mut hits, &|a| a))
        .unwrap();
    it.end_frame(hits);
    term
}

fn selected(p: &ModelPicker) -> String {
    p.selected_entry().unwrap().reference.to_string()
}

/// The dialog's border rectangle found in the buffer.
fn dialog(buf: &Buffer) -> Rect {
    let (x, y) = find_text(buf, "╭").expect("dialog top-left corner");
    let (_, bottom) = find_text(buf, "╰").expect("dialog bottom-left corner");
    let right = (x..buf.area.right())
        .find(|&cx| buf[(cx, y)].symbol() == "╮")
        .expect("top-right corner");
    Rect::new(x, y, right - x + 1, bottom - y + 1)
}

fn row_text(buf: &Buffer, r: Rect, y: u16) -> String {
    (r.x..r.right()).map(|x| buf[(x, y)].symbol()).collect()
}

#[test]
fn dialog_height_is_ninety_percent_rounded() {
    assert_eq!(dialog_height(10), 9);
    assert_eq!(dialog_height(24), 22);
    assert_eq!(dialog_height(30), 27);
    assert_eq!(dialog_height(40), 36);
    assert_eq!(dialog_height(55), 50);
    assert_eq!(dialog_height(1), 1);
}

/// Renders at `w`x`h` and checks the layout contract: 90 % height,
/// centered, width fits the widest row or is capped at the terminal minus
/// margins, every row on exactly one line inside the borders.
fn check_layout(w: u16, h: u16) -> (Rect, String) {
    let mut p = picker();
    let mut it = Interaction::new();
    let term = draw(&mut p, &mut it, w, h);
    let buf = term.backend().buffer();
    let r = dialog(buf);
    assert_eq!(r.height, dialog_height(h), "{w}x{h}: 90% height");
    assert_eq!(r.y, (h - r.height) / 2, "{w}x{h}: vertically centered");
    assert_eq!(r.x, (w - r.width) / 2, "{w}x{h}: horizontally centered");
    let natural = p.natural_width() as u16;
    if natural + 2 * MARGIN <= w {
        assert_eq!(r.width, natural, "{w}x{h}: fits the widest row");
    } else {
        assert_eq!(r.width, w - 2 * MARGIN, "{w}x{h}: capped with margins");
    }
    // Every line between the borders starts and ends with the side border,
    // so no row overflows; and each visible model is on exactly one line,
    // so no row wraps.
    let lines: Vec<String> = (r.y + 1..r.bottom() - 1)
        .map(|y| row_text(buf, r, y))
        .collect();
    for l in &lines {
        assert!(l.starts_with('│') && l.ends_with('│'), "{w}x{h}: {l}");
    }
    // Query line and column headers above, the key hints (wrapped to
    // the content width) below.
    let hint_rows =
        crate::chat::text::wrap_text(HINTS, r.width as usize - 2 - 2 * PAD as usize).len();
    let rows = r.height as usize - 2 - 2 - hint_rows;
    let shown = lines[2..2 + rows]
        .iter()
        .filter(|l| {
            let c: Vec<char> = l.chars().collect();
            (c[2] == ' ' || c[2] == '▸') && !l.trim_matches(|ch| ch == '│' || ch == ' ').is_empty()
        })
        .count();
    let headers = p
        .items
        .iter()
        .skip(p.scroll)
        .take(rows)
        .filter(|i| matches!(i, Item::Header { .. }))
        .count();
    assert_eq!(
        shown,
        p.items.len().min(rows) - headers,
        "{w}x{h}: one line per row"
    );
    let text = buffer_text(buf);
    if r.width == natural {
        // Nothing is cut when the dialog fits.
        assert!(
            text.contains(
                "google/gemini-2.5-pro-preview-with-a-very-long-model-identifier-2026-10"
            )
        );
    } else {
        assert!(text.contains('…'), "{w}x{h}: long cells end in an ellipsis");
    }
    (r, styled_snapshot(buf))
}

#[test]
fn snapshot_wide_terminal_fits_the_widest_row() {
    let mut p = picker();
    let natural = p.natural_width() as u16;
    let (r, snap) = check_layout(natural + 2 * MARGIN + 10, 40);
    assert_eq!(r.width, natural);
    let term = draw(&mut p, &mut Interaction::new(), natural + 14, 40);
    assert!(
        !buffer_text(term.backend().buffer()).contains('…'),
        "nothing cut"
    );
    insta::assert_snapshot!("picker_wide", snap);
}

#[test]
fn snapshot_medium_terminal_caps_width_and_truncates() {
    let (r, snap) = check_layout(120, 30);
    assert_eq!(r.width, 116);
    insta::assert_snapshot!("picker_120x30", snap);
}

#[test]
fn snapshot_small_terminal_caps_width_and_truncates() {
    let (r, snap) = check_layout(80, 24);
    assert_eq!(r.width, 76);
    insta::assert_snapshot!("picker_80x24", snap);
}

#[test]
fn tiny_terminals_keep_the_layout_rules() {
    for (w, h) in [(60, 16), (50, 12), (200, 60)] {
        check_layout(w, h);
    }
    // Degenerate sizes do not panic.
    for (w, h) in [(1, 1), (10, 3), (30, 4), (3, 40)] {
        let mut p = picker();
        draw(&mut p, &mut Interaction::new(), w, h);
    }
}

#[test]
fn rows_show_every_column_grouped_by_provider() {
    let mut p = picker();
    let term = draw(&mut p, &mut Interaction::new(), 240, 40);
    let text = buffer_text(term.backend().buffer());
    for col in COLUMNS {
        assert!(text.contains(col), "header {col}: {text}");
    }
    let sonnet = text
        .lines()
        .find(|l| l.contains("claude-sonnet-5-5"))
        .unwrap();
    for cell in [
        "anthropic",
        "sonnet",
        "200k",
        "$3.00",
        "$15.00",
        "tools reasoning",
        "API key",
    ] {
        assert!(sonnet.contains(cell), "{cell} in {sonnet}");
    }
    // Groups in provider order, each with a header.
    let pos = |s: &str| text.find(s).unwrap_or_else(|| panic!("{s}: {text}"));
    assert!(pos("anthropic · 3 models") < pos("openai · 2 models"));
    assert!(pos("openai · 2 models") < pos("openrouter · 4 models"));
    assert!(text.contains("1M") && text.contains("1.05M") && text.contains("free"));
}

#[test]
fn current_model_is_marked_and_unusable_models_are_dimmed_with_the_reason() {
    let mut p = picker();
    let theme = Theme::default();
    let term = draw(&mut p, &mut Interaction::new(), 240, 40);
    let buf = term.backend().buffer();
    let text = buffer_text(buf);
    let sonnet = text
        .lines()
        .find(|l| l.contains("claude-sonnet-5-5"))
        .unwrap();
    assert!(sonnet.contains("▸●"), "current and selected: {sonnet}");
    let opus = text
        .lines()
        .find(|l| l.contains("claude-opus-5-5"))
        .unwrap();
    assert!(!opus.contains('●'));
    // OpenAI has no key: dimmed, with the fix.
    let gpt = text.lines().find(|l| l.contains("gpt-5.5-mini")).unwrap();
    assert!(gpt.contains("no openai API key"), "{gpt}");
    let (x, y) = find_text(buf, "gpt-5.5-mini").unwrap();
    assert_eq!(buf[(x, y)].fg, theme.color(Token::Muted));
    let (x, y) = find_text(buf, "claude-opus-5-5").unwrap();
    assert_eq!(buf[(x, y)].fg, theme.color(Token::Fg));
    // No tool calling: usable for chat, flagged.
    let sonar = text
        .lines()
        .find(|l| l.contains("sonar-deep-research"))
        .unwrap();
    assert!(sonar.contains("chat only: no tool calling"), "{sonar}");
}

#[test]
fn typing_filters_with_fuzzy_matching() {
    let mut p = picker();
    assert_eq!(p.visible().len(), 9);
    typ(&mut p, "gem25pro");
    let v = p.visible();
    assert_eq!(v.len(), 1, "{v:?}");
    assert!(v[0].model().starts_with("google/gemini-2.5-pro"));
    assert_eq!(selected(&p), v[0].reference.to_string());
    // Aliases match too.
    p.set_query("opus");
    assert!(p.visible().iter().any(|e| e.model() == "claude-opus-5-5"));
    p.handle_key(&key("backspace"));
    assert_eq!(p.query(), "opu");
    p.set_query("zzzzqqq");
    assert!(p.visible().is_empty());
    let term = draw(&mut p, &mut Interaction::new(), 120, 30);
    assert!(buffer_text(term.backend().buffer()).contains("no models match `zzzzqqq`"));
    assert_eq!(p.handle_key(&key("enter")), PickerOutcome::Ignored);
    p.set_query("");
    assert_eq!(p.visible().len(), 9);
}

#[test]
fn keys_move_the_selection_and_enter_switches() {
    let mut p = picker();
    let mut it = Interaction::new();
    draw(&mut p, &mut it, 120, 30);
    assert_eq!(selected(&p), "anthropic/claude-sonnet-5-5");
    p.handle_key(&key("up"));
    assert_eq!(selected(&p), "anthropic/claude-opus-5-5");
    p.handle_key(&key("up"));
    p.handle_key(&key("up"));
    assert_eq!(
        selected(&p),
        "anthropic/claude-haiku-5-5",
        "clamped at the top"
    );
    p.handle_key(&key("down"));
    p.handle_key(&key("tab"));
    assert_eq!(selected(&p), "anthropic/claude-sonnet-5-5");
    p.handle_key(&key("shift+tab"));
    assert_eq!(selected(&p), "anthropic/claude-opus-5-5");
    p.handle_key(&key("end"));
    assert_eq!(selected(&p), "openrouter/perplexity/sonar-deep-research");
    p.handle_key(&key("home"));
    assert_eq!(selected(&p), "anthropic/claude-haiku-5-5");
    p.handle_key(&key("pagedown"));
    assert_eq!(selected(&p), "openrouter/perplexity/sonar-deep-research");
    p.handle_key(&key("pageup"));
    assert_eq!(selected(&p), "anthropic/claude-haiku-5-5");
    p.handle_key(&key("down"));
    assert_eq!(
        p.handle_key(&key("enter")),
        PickerOutcome::Switch("anthropic/claude-opus-5-5".into())
    );
    assert_eq!(p.handle_key(&key("esc")), PickerOutcome::Close);
}

#[test]
fn page_keys_move_by_a_screenful() {
    let mut p = ModelPicker::new(sample_with(false, 200), None);
    let mut it = Interaction::new();
    draw(&mut p, &mut it, 120, 30);
    let page = p.page;
    assert!(page > 5);
    let start = selected(&p);
    p.handle_key(&key("pagedown"));
    let entries = p.visible();
    let i = entries
        .iter()
        .position(|e| e.reference.to_string() == selected(&p))
        .unwrap();
    assert_eq!(i, page, "moved one page from {start}");
    let last = entries.last().unwrap().model().to_owned();
    // The selection stays on screen.
    p.handle_key(&key("end"));
    let term = draw(&mut p, &mut it, 120, 30);
    let text = buffer_text(term.backend().buffer());
    assert!(text.contains(&last), "{last} visible:\n{text}");
}

#[test]
fn choosing_an_unusable_model_keeps_the_picker_open_with_the_reason() {
    let mut p = picker();
    p.set_query("gpt-5.5-mini");
    assert_eq!(selected(&p), "openai/gpt-5.5-mini");
    assert_eq!(p.handle_key(&key("enter")), PickerOutcome::Changed);
    let notice = p.notice().unwrap().to_owned();
    assert!(
        notice.contains("cannot switch to openai/gpt-5.5-mini"),
        "{notice}"
    );
    assert!(notice.contains("no openai API key"), "{notice}");
    let term = draw(&mut p, &mut Interaction::new(), 200, 30);
    assert!(buffer_text(term.backend().buffer()).contains("cannot switch to openai/gpt-5.5-mini"));
    // Moving clears it.
    p.handle_key(&key("up"));
    assert!(p.notice().is_none());
}

#[test]
fn rows_are_clickable_and_the_wheel_scrolls() {
    let mut p = picker();
    let mut it = Interaction::new();
    let term = draw(&mut p, &mut it, 120, 30);
    let (x, y) = find_text(term.backend().buffer(), "claude-haiku-5-5").unwrap();
    let Event::Mouse(m) = click(x, y) else {
        unreachable!()
    };
    let Some(Dispatch::Activate(a)) = it.handle_mouse(&m) else {
        panic!("row not clickable")
    };
    assert_eq!(
        p.activate(a),
        PickerOutcome::Switch("anthropic/claude-haiku-5-5".into())
    );
    // Clicking an unusable row explains instead of switching.
    let (x, y) = find_text(term.backend().buffer(), "gpt-5.5-mini").unwrap();
    let Event::Mouse(m) = click(x, y) else {
        unreachable!()
    };
    let Some(Dispatch::Activate(a)) = it.handle_mouse(&m) else {
        panic!("row not clickable")
    };
    assert_eq!(p.activate(a), PickerOutcome::Changed);
    assert!(p.notice().is_some());
    // Wheel over the list moves the selection down.
    let before = selected(&p);
    let Event::Mouse(m) = wheel_down(x, y) else {
        unreachable!()
    };
    let Some(Dispatch::Scroll { pane, lines }) = it.handle_mouse(&m) else {
        panic!("list not scrollable")
    };
    assert_eq!(pane.0, LIST_PANE);
    p.scroll(lines);
    assert_ne!(selected(&p), before);
    // A click outside the dialog backs out.
    let Event::Mouse(m) = click(0, 0) else {
        unreachable!()
    };
    assert_eq!(it.handle_mouse(&m), Some(Dispatch::Back));
}

#[test]
fn refresh_indicator_shows_while_lists_update_and_selection_survives() {
    let mut p = ModelPicker::new(sample(true), Some("sonnet".into()));
    assert_eq!(
        selected(&p),
        "anthropic/claude-sonnet-5-5",
        "alias is current"
    );
    let mut it = Interaction::new();
    let term = draw(&mut p, &mut it, 120, 30);
    assert!(buffer_text(term.backend().buffer()).contains(REFRESHING));
    p.handle_key(&key("end"));
    typ(&mut p, "o");
    let chosen = selected(&p);
    p.set_catalog(sample_with(false, 3));
    assert_eq!(selected(&p), chosen);
    assert_eq!(p.query(), "o");
    let term = draw(&mut p, &mut it, 120, 30);
    let text = buffer_text(term.backend().buffer());
    assert!(!text.contains(REFRESHING), "{text}");
}

#[test]
fn paste_appends_to_the_filter() {
    let mut p = picker();
    p.paste("haiku\nignored");
    assert_eq!(p.query(), "haiku");
    assert_eq!(p.visible().len(), 1);
}

#[test]
fn opens_from_a_large_cached_catalog_in_under_50ms() {
    let catalog = sample_with(false, 800);
    let theme = Theme::default();
    // One open and first frame on fresh state.
    let open = || {
        let mut term = Terminal::new(TestBackend::new(200, 50)).unwrap();
        let start = Instant::now();
        let mut p = ModelPicker::new(catalog.clone(), Some("anthropic/claude-sonnet-5-5".into()));
        let mut it: Interaction<PickerAction> = Interaction::new();
        let mut hits = it.begin_frame();
        term.draw(|f| p.render(f, f.area(), &theme, &mut hits, &|a| a))
            .unwrap();
        it.end_frame(hits);
        (start.elapsed(), p, term)
    };
    // Best of five runs: a slow implementation is slow every run, while
    // a scheduler hiccup on a shared CI runner only hits some of them.
    let (took, p, term) = (0..5).map(|_| open()).min_by_key(|r| r.0).unwrap();
    assert!(
        took.as_millis() < 50,
        "opening took {took:?} for {} models",
        p.catalog().entries().len()
    );
    assert!(buffer_text(term.backend().buffer()).contains("claude-sonnet-5-5"));
}

fn click_at(it: &mut Interaction<PickerAction>, x: u16, y: u16) -> Option<PickerAction> {
    let Event::Mouse(m) = click(x, y) else {
        unreachable!()
    };
    match it.handle_mouse(&m) {
        Some(Dispatch::Activate(a)) => Some(a),
        _ => None,
    }
}

/// A cut row offers `→ full text`; Right or a click on it shows every
/// cell of the selected model labelled and wrapped, Shift+Down or ▼
/// scrolls it, Right or `← hide full text` closes it.
#[test]
fn cut_rows_open_their_full_text_by_keyboard_and_mouse() {
    let mut p = picker();
    let mut it = Interaction::new();
    let term = draw(&mut p, &mut it, 80, 24);
    let text = buffer_text(term.backend().buffer());
    assert!(text.contains("→ full text"), "{text}");
    assert!(!text.contains("Model: "), "{text}");
    assert_eq!(p.handle_key(&key("right")), PickerOutcome::Changed);
    let term = draw(&mut p, &mut it, 80, 24);
    let text = buffer_text(term.backend().buffer());
    assert!(text.contains("─ full text"), "{text}");
    assert!(text.contains("Model: claude-sonnet-5-5"), "{text}");
    assert!(text.contains("Provider: anthropic"), "{text}");
    p.handle_key(&key("right"));
    let term = draw(&mut p, &mut it, 80, 24);
    assert!(!buffer_text(term.backend().buffer()).contains("Model: "));
    // Mouse, in a short terminal where the text overflows.
    let term = draw(&mut p, &mut it, 70, 16);
    let (x, y) = find_text(term.backend().buffer(), "→ full text").unwrap();
    let a = click_at(&mut it, x, y).expect("the hint is clickable");
    assert_eq!(a, PickerAction::ToggleDetail);
    p.activate(a);
    let term = draw(&mut p, &mut it, 70, 16);
    let text = buffer_text(term.backend().buffer());
    assert!(text.contains("shift+↑↓ 1-"), "{text}");
    let (x, y) = find_text(term.backend().buffer(), "▼").unwrap();
    let a = click_at(&mut it, x, y).expect("▼ is clickable");
    assert_eq!(a, PickerAction::Detail(1));
    p.activate(a);
    let term = draw(&mut p, &mut it, 70, 16);
    assert!(buffer_text(term.backend().buffer()).contains("shift+↑↓ 2-"));
    p.handle_key(&key("shift+down"));
    let term = draw(&mut p, &mut it, 70, 16);
    assert!(buffer_text(term.backend().buffer()).contains("shift+↑↓ 3-"));
    p.handle_key(&key("shift+up"));
    let term = draw(&mut p, &mut it, 70, 16);
    assert!(buffer_text(term.backend().buffer()).contains("shift+↑↓ 2-"));
    let (x, y) = find_text(term.backend().buffer(), "← hide full text").unwrap();
    let a = click_at(&mut it, x + 2, y).expect("hide is clickable");
    p.activate(a);
    let term = draw(&mut p, &mut it, 70, 16);
    assert!(!buffer_text(term.backend().buffer()).contains("─ full text"));
}
