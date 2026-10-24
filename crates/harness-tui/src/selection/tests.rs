use super::*;
use crate::interaction::testing::{click, drag, key, release};
use crossterm::event::Event;
use ratatui::style::Color; // raw-color-ok

/// A buffer showing `lines` from row 0.
fn buffer(w: u16, h: u16, lines: &[&str]) -> Buffer {
    let mut buf = Buffer::empty(Rect::new(0, 0, w, h));
    for (y, l) in lines.iter().enumerate() {
        buf.set_string(0, y as u16, l, Style::new());
    }
    buf
}

struct Ui {
    sel: Selector,
    buf: Buffer,
    now: Instant,
    regions: Vec<TextRegion>,
    masks: Vec<Rect>,
}

impl Ui {
    fn new(lines: &[&str], regions: Vec<TextRegion>) -> Ui {
        let mut ui = Ui {
            sel: Selector::new(),
            buf: buffer(40, 12, lines),
            now: Instant::now(),
            regions,
            masks: Vec::new(),
        };
        ui.frame();
        ui
    }

    /// Draws a frame: registers the regions and masks, then finishes it.
    fn frame(&mut self) {
        let mut ft = FrameText::default();
        for r in &self.regions {
            ft.register(r.clone());
        }
        for m in &self.masks {
            ft.mask(*m);
        }
        // A fresh frame: no highlight from the last one.
        for cell in self.buf.content.iter_mut() {
            cell.bg = Color::Reset; // raw-color-ok
            cell.modifier = Modifier::empty();
        }
        let st = Style::new().bg(Color::Indexed(4)); // raw-color-ok
        self.sel.end_frame(ft, &mut self.buf, st, st);
    }

    fn mouse(&mut self, ev: Event) -> (MouseOutcome, Option<Scroll>) {
        let Event::Mouse(m) = ev else { panic!() };
        let out = self.sel.mouse(&m, self.now);
        self.frame();
        out
    }

    /// Press at `from`, drag to `to`, release there; returns the release.
    fn select(&mut self, from: (u16, u16), to: (u16, u16)) -> MouseOutcome {
        assert_eq!(self.mouse(click(from.0, from.1)).0, MouseOutcome::Pass);
        assert_eq!(self.mouse(drag(to.0, to.1)).0, MouseOutcome::Consumed);
        self.mouse(release(to.0, to.1)).0
    }

    fn key(&mut self, chord: &str) -> KeyOutcome {
        let out = self.key_only(chord);
        self.frame();
        out
    }

    /// A key without drawing a frame after it (the app scrolls first).
    fn key_only(&mut self, chord: &str) -> KeyOutcome {
        let Event::Key(k) = key(chord) else { panic!() };
        self.sel.key(&k)
    }

    fn highlighted(&self, y: u16) -> String {
        (0..self.buf.area.width)
            .filter(|x| self.buf[(*x, y)].bg == Color::Indexed(4)) // raw-color-ok
            .map(|x| self.buf[(x, y)].symbol().to_owned())
            .collect()
    }
}

fn copied(out: MouseOutcome) -> String {
    match out {
        MouseOutcome::Copy(t) => t,
        o => panic!("expected a copy, got {o:?}"),
    }
}

#[test]
fn press_drag_release_copies_exactly_the_range_and_highlights_it() {
    let lines = ["alpha beta gamma", "delta epsilon", "zeta eta theta"];
    let mut ui = Ui::new(
        &lines,
        vec![TextRegion::new("text", Rect::new(0, 0, 40, 3))],
    );
    // From `beta` on row 0 to `eta` on row 2.
    let out = ui.select((6, 0), (7, 2));
    assert_eq!(copied(out), "beta gamma\ndelta epsilon\nzeta eta");
    assert_eq!(ui.highlighted(0), "beta gamma");
    assert_eq!(ui.highlighted(1), "delta epsilon");
    assert_eq!(ui.highlighted(2), "zeta eta");
    // Dragging backwards selects the same text.
    let out = ui.select((7, 2), (6, 0));
    assert_eq!(copied(out), "beta gamma\ndelta epsilon\nzeta eta");
}

#[test]
fn a_press_without_movement_is_a_click_and_clears_the_selection() {
    let mut ui = Ui::new(
        &["one two"],
        vec![TextRegion::new("t", Rect::new(0, 0, 40, 1))],
    );
    assert_eq!(ui.mouse(click(1, 0)).0, MouseOutcome::Pass);
    // A drag event on the same cell is no movement yet.
    assert_eq!(ui.mouse(drag(1, 0)).0, MouseOutcome::Consumed);
    assert!(ui.sel.selection().is_none());
    assert_eq!(ui.mouse(release(1, 0)).0, MouseOutcome::Pass);
    // A selection stays highlighted until the next press.
    copied(ui.select((0, 0), (2, 0)));
    assert!(ui.sel.selection().is_some());
    assert_eq!(ui.mouse(click(5, 0)).0, MouseOutcome::Pass);
    assert!(ui.sel.selection().is_none());
    assert_eq!(ui.highlighted(0), "");
}

#[test]
fn soft_wrapped_rows_are_rejoined() {
    let lines = ["the quick", "brown fox", "abcde", "fghij", "next line"];
    let mut r = TextRegion::new("t", Rect::new(0, 0, 40, 5));
    r.set_joins([
        Join::Hard,
        Join::Space,
        Join::Hard,
        Join::Direct,
        Join::Hard,
    ]);
    let mut ui = Ui::new(&lines, vec![r]);
    let out = ui.select((0, 0), (20, 4));
    assert_eq!(copied(out), "the quick brown fox\nabcdefghij\nnext line");
    // Continuation rows lose their wrap indent.
    let lines = ["- one two", "  three"];
    let mut r = TextRegion::new("t", Rect::new(0, 0, 40, 2));
    r.set_joins([Join::Hard, Join::Space]);
    let mut ui = Ui::new(&lines, vec![r]);
    assert_eq!(copied(ui.select((0, 0), (30, 1))), "- one two three");
}

#[test]
fn smart_chrome_leaves_out_frames_gutters_markers_icons_and_buttons() {
    let lines = [
        "◆ assistant              ⧉ copy · 12:00",
        "Run this:",
        "╭ ✔ bash $ seq 2 ───────── 3ms ╮",
        "│    1 │ first                 │",
        "│    2 │ second                │",
        "│ ⧉ copy output                │",
        "╰──────────────────────────────╯",
        "▌ user words",
        "  12 │-let a = 1;",
        "Enter to expand",
    ];
    let r = TextRegion::new("t", Rect::new(0, 0, 40, 10)).chrome(Chrome::Smart);
    let mut ui = Ui::new(&lines, vec![r]);
    let out = ui.select((0, 0), (39, 9));
    assert_eq!(
        copied(out),
        "assistant\nRun this:\nbash $ seq 2  3ms\nfirst\nsecond\nuser words\n-let a = 1;"
    );
    // Chrome is not highlighted either.
    assert_eq!(ui.highlighted(3), "first");
}

#[test]
fn masks_exclude_buttons_and_overlays_and_a_press_on_them_selects_nothing() {
    let lines = ["text [ close ] more"];
    let mut ui = Ui::new(&lines, vec![TextRegion::new("t", Rect::new(0, 0, 40, 1))]);
    ui.masks.push(Rect::new(5, 0, 9, 1));
    ui.frame();
    assert_eq!(copied(ui.select((0, 0), (18, 0))), "text  more");
    // Pressing on the button and dragging does not select.
    assert_eq!(ui.mouse(click(7, 0)).0, MouseOutcome::Pass);
    assert_eq!(ui.mouse(drag(18, 0)).0, MouseOutcome::Consumed);
    assert_eq!(ui.mouse(release(18, 0)).0, MouseOutcome::Pass);
    assert!(ui.sel.selection().is_none());
}

#[test]
fn later_regions_cover_earlier_ones_and_drawing_chrome_drops_borders() {
    let lines = ["┌──────┐ under", "│ node │ under", "└──────┘"];
    let base = TextRegion::new("screen", Rect::new(0, 0, 40, 3)).chrome(Chrome::Drawing);
    let top = TextRegion::new("box", Rect::new(0, 0, 8, 3)).chrome(Chrome::Drawing);
    let mut ui = Ui::new(&lines, vec![base, top]);
    // Inside the box only the box's text.
    assert_eq!(copied(ui.select((0, 0), (7, 2))), "node");
    // The screen region sees the rest, not the covered box.
    assert_eq!(copied(ui.select((9, 0), (14, 1))), "under\nunder");
}

#[test]
fn drawing_chrome_drops_status_icons_in_front_of_labels_only() {
    let lines = ["┌─────────┐", "│ ✔ Coder │", "│ ok ✔ x  │", "└─────────┘"];
    let r = TextRegion::new("map", Rect::new(0, 0, 11, 4)).chrome(Chrome::Drawing);
    let mut ui = Ui::new(&lines, vec![r]);
    // The icon before a label goes; one between words is text.
    assert_eq!(copied(ui.select((0, 1), (10, 2))), "Coder\nok ✔ x");
}

#[test]
fn dragging_past_an_edge_scrolls_and_the_selection_keeps_every_line() {
    let content: Vec<String> = (0..20).map(|i| format!("line {i}")).collect();
    let show = |first: usize| -> Vec<&str> {
        content[first..first + 4]
            .iter()
            .map(String::as_str)
            .collect()
    };
    // Rows 1..5 show content lines from `first`.
    let region = |first: usize| TextRegion::new("log", Rect::new(0, 1, 40, 4)).scrolled(first);
    let mut ui = Ui::new(&[], vec![region(0)]);
    let paint = |ui: &mut Ui, first: usize| {
        ui.buf = Buffer::empty(Rect::new(0, 0, 40, 12));
        for (i, l) in show(first).iter().enumerate() {
            ui.buf.set_string(0, 1 + i as u16, l, Style::new());
        }
        ui.regions = vec![region(first)];
        ui.frame();
    };
    paint(&mut ui, 0);
    assert_eq!(ui.mouse(click(0, 2)).0, MouseOutcome::Pass); // line 1
    // The drag that starts the selection does not scroll yet: the frame
    // after it must show the starting row.
    assert_eq!(ui.mouse(drag(3, 8)), (MouseOutcome::Consumed, None));
    let (out, scroll) = ui.mouse(drag(3, 9));
    assert_eq!(out, MouseOutcome::Consumed);
    assert_eq!(
        scroll,
        Some(Scroll {
            x: 0,
            y: 1,
            up: false
        })
    );
    assert!(ui.sel.autoscroll().is_some(), "ticks keep scrolling");
    // The app scrolls by three lines, twice; the pointer stays below.
    paint(&mut ui, 3);
    paint(&mut ui, 6);
    let text = copied(ui.mouse(release(3, 8)).0);
    let want: Vec<String> = (1..10).map(|i| format!("line {i}")).collect();
    assert_eq!(text, want.join("\n"));
    assert!(ui.sel.autoscroll().is_none());
    // Regions that do not scroll never ask to.
    let mut ui = Ui::new(
        &["a", "b"],
        vec![TextRegion::new("t", Rect::new(0, 1, 40, 2))],
    );
    ui.mouse(click(0, 1));
    assert_eq!(ui.mouse(drag(0, 9)).1, None);
}

#[test]
fn double_click_selects_a_word_and_triple_click_a_logical_line() {
    let lines = ["open src/parse.rs now", "and then", "more"];
    let mut r = TextRegion::new("t", Rect::new(0, 0, 40, 3));
    r.set_joins([Join::Hard, Join::Space, Join::Hard]);
    let mut ui = Ui::new(&lines, vec![r]);
    ui.mouse(click(8, 0));
    ui.mouse(release(8, 0));
    ui.now += Duration::from_millis(100);
    assert_eq!(ui.mouse(click(8, 0)).0, MouseOutcome::Pass, "still a click");
    assert_eq!(copied(ui.mouse(release(8, 0)).0), "src/parse.rs");
    ui.now += Duration::from_millis(100);
    ui.mouse(click(8, 0));
    assert_eq!(
        copied(ui.mouse(release(8, 0)).0),
        "open src/parse.rs now and then"
    );
    // Too slow: two single clicks.
    ui.now += Duration::from_secs(5);
    ui.mouse(click(1, 2));
    ui.mouse(release(1, 2));
    ui.now += MULTI_CLICK + Duration::from_millis(1);
    ui.mouse(click(1, 2));
    assert_eq!(ui.mouse(release(1, 2)).0, MouseOutcome::Pass);
    assert!(ui.sel.selection().is_none());
}

#[test]
fn keyboard_mode_moves_a_cursor_extends_with_shift_and_copies_on_enter() {
    let lines = ["header", "first line", "second line", "third"];
    let small = TextRegion::new("head", Rect::new(0, 0, 40, 1));
    let body = TextRegion::new("body", Rect::new(0, 1, 40, 3)).scrolled(0);
    let mut ui = Ui::new(&lines, vec![small, body]);
    assert!(ui.sel.start_keyboard());
    assert!(ui.sel.keyboard());
    ui.frame();
    // The cursor starts in the largest region.
    assert_eq!(ui.sel.selection().unwrap().region, "body");
    // Plain arrows move without selecting.
    assert_eq!(ui.key("right"), KeyOutcome::Changed);
    let s = ui.sel.selection().unwrap();
    assert_eq!(s.anchor, s.head);
    // Shift extends.
    ui.key("shift+down");
    ui.key("shift+end");
    assert_eq!(ui.highlighted(2), "second line");
    assert_eq!(
        ui.key("enter"),
        KeyOutcome::Copy("irst line\nsecond line".into())
    );
    assert!(!ui.sel.keyboard(), "copy leaves the mode");
    // Enter without extending copies the line under the cursor.
    ui.sel.start_keyboard();
    ui.frame();
    ui.key("down");
    assert_eq!(ui.key("enter"), KeyOutcome::Copy("second line".into()));
    // Tab moves to the next region; Esc leaves.
    ui.sel.start_keyboard();
    ui.frame();
    ui.key("tab");
    assert_eq!(ui.sel.selection().unwrap().region, "head");
    assert_eq!(ui.key("esc"), KeyOutcome::Exit);
    assert!(ui.sel.selection().is_none());
}

#[test]
fn keyboard_cursor_past_the_bottom_scrolls_a_scrolling_region() {
    let lines = ["a", "b"];
    let mut ui = Ui::new(
        &lines,
        vec![TextRegion::new("log", Rect::new(0, 0, 40, 2)).scrolled(0)],
    );
    ui.sel.start_keyboard();
    ui.frame();
    ui.key("shift+down");
    assert_eq!(
        ui.key_only("shift+down"),
        KeyOutcome::Scroll(Scroll {
            x: 0,
            y: 0,
            up: false
        })
    );
    // The view scrolled by one line: the cursor lands on the new row.
    ui.buf = buffer(40, 12, &["b", "c"]);
    ui.regions = vec![TextRegion::new("log", Rect::new(0, 0, 40, 2)).scrolled(1)];
    ui.frame();
    assert_eq!(ui.sel.selection().unwrap().head.ord, 2);
    assert_eq!(ui.key("enter"), KeyOutcome::Copy("a\nb\nc".into()));
}

#[test]
fn a_selection_whose_region_is_gone_is_cleared() {
    let mut ui = Ui::new(&["x y"], vec![TextRegion::new("t", Rect::new(0, 0, 40, 1))]);
    copied(ui.select((0, 0), (2, 0)));
    ui.regions.clear();
    ui.frame();
    assert!(ui.sel.selection().is_none());
}

#[test]
fn wide_characters_are_copied_once() {
    let mut ui = Ui::new(
        &["日本語 ok"],
        vec![TextRegion::new("t", Rect::new(0, 0, 40, 1))],
    );
    assert_eq!(copied(ui.select((0, 0), (9, 0))), "日本語 ok");
}

#[test]
fn the_collector_only_records_inside_a_frame() {
    register(TextRegion::new("lost", Rect::new(0, 0, 1, 1)));
    begin_frame();
    register(TextRegion::new("a", Rect::new(0, 0, 4, 2)));
    update("a", |r| r.set_scroll(10));
    mask(Rect::new(0, 0, 1, 1));
    // Same id: replaced in place.
    register(TextRegion::new("b", Rect::new(0, 0, 4, 1)));
    register(TextRegion::new("b", Rect::new(0, 1, 4, 1)));
    let ft = end_frame();
    assert_eq!(ft.region_ids(), vec!["a", "b"]);
    assert_eq!(ft.region("a").unwrap().rows[1].ord, 11);
    assert!(ft.region_at(0, 0).is_none(), "masked");
    assert_eq!(ft.region_at(1, 0).unwrap().id, "a");
    assert_eq!(ft.region_at(1, 1).unwrap().id, "b");
    assert_eq!(end_frame(), FrameText::default());
}

#[test]
fn a_transient_overlay_keeps_the_text_under_it_and_blocks_presses() {
    let lines = ["alpha beta gamma delta"];
    let mut buf = buffer(40, 12, &lines);
    let mut ft = FrameText::default();
    ft.register(TextRegion::new("r", Rect::new(0, 0, 40, 4)));
    // A toast over "beta gamma", drawn after the snapshot.
    let toast = Rect::new(6, 0, 10, 2);
    ft.overlay(toast, &buf);
    buf.set_string(6, 0, "[toast!!!]", Style::new());
    assert!(ft.region_at(8, 0).is_none(), "a press on it starts nothing");
    assert!(ft.region_at(2, 0).is_some());
    let mut sel = Selector::new();
    let st = Style::new().bg(Color::Indexed(4)); // raw-color-ok
    sel.end_frame(ft, &mut buf, st, st);
    let now = Instant::now();
    let ev = |e: Event| match e {
        Event::Mouse(m) => m,
        _ => panic!(),
    };
    // One batch, no frame in between.
    sel.mouse(&ev(click(0, 0)), now);
    sel.mouse(&ev(drag(21, 0)), now);
    let (out, _) = sel.mouse(&ev(release(21, 0)), now);
    assert_eq!(out, MouseOutcome::Copy("alpha beta gamma delta".into()));
    // An opaque mask (a button) on top still hides its own cells.
    let mut buf = buffer(40, 12, &lines);
    let mut ft = FrameText::default();
    ft.register(TextRegion::new("r", Rect::new(0, 0, 40, 4)));
    ft.overlay(toast, &buf);
    ft.mask(Rect::new(6, 0, 5, 1));
    let mut sel = Selector::new();
    sel.end_frame(ft, &mut buf, st, st);
    sel.mouse(&ev(click(0, 0)), now);
    sel.mouse(&ev(drag(21, 0)), now);
    let (out, _) = sel.mouse(&ev(release(21, 0)), now);
    assert_eq!(out, MouseOutcome::Copy("alpha gamma delta".into()));
}

#[test]
fn smart_chrome_drops_a_card_header_rule_of_any_width() {
    let lines = [
        "╭ ✔ bash  $ line-th…─ 7ms · 17:24:02 ╮",
        "│ out                                │",
        "╭ ✔ read  a.rs ── 7ms ╮",
        "╭ ✔ bash  $ echo a ─ b ─ ╮",
    ];
    let r = TextRegion::new("t", Rect::new(0, 0, 40, 4)).chrome(Chrome::Smart);
    let mut ui = Ui::new(&lines, vec![r]);
    assert_eq!(
        copied(ui.select((0, 0), (39, 0))),
        "bash  $ line-th… 7ms · 17:24:02"
    );
    assert_eq!(copied(ui.select((0, 2), (39, 2))), "read  a.rs  7ms");
    // A rule inside the title is text; only the fill before the end goes.
    assert_eq!(copied(ui.select((0, 3), (39, 3))), "bash  $ echo a ─ b");
    assert!(!ui.highlighted(3).contains("b ─"), "{}", ui.highlighted(3));
}

/// A modal (its own filter row and a list pane inside its border) over
/// the screen.
fn modal_ui() -> Ui {
    let lines = [
        "╭ Servers ─────────────╮",
        "│ filter: abc          │",
        "│ entry 00             │",
        "│ entry 01             │",
        "│ entry 02             │",
        "╰──────────────────────╯",
    ];
    let screen = TextRegion::new("screen", Rect::new(0, 0, 40, 12)).chrome(Chrome::Drawing);
    let modal = TextRegion::new("modal", Rect::new(0, 0, 24, 6)).chrome(Chrome::Drawing);
    let pane = TextRegion::new("list", Rect::new(2, 2, 20, 3));
    Ui::new(&lines, vec![screen, modal, pane])
}

#[test]
fn a_press_on_a_border_selects_from_the_nearest_text_cell_inside_it() {
    let mut ui = modal_ui();
    // The left border, on a list row.
    assert_eq!(copied(ui.select((0, 3), (9, 4))), "entry 01\nentry 02");
    assert_eq!(ui.sel.selection().unwrap().region, "list");
    // The right border, dragging left.
    assert_eq!(copied(ui.select((23, 2), (2, 2))), "entry 00");
    // The bottom border, dragging up: from the list's last row.
    assert_eq!(
        copied(ui.select((5, 5), (2, 2))),
        "entry 00\nentry 01\nentr"
    );
    // A corner.
    assert_eq!(copied(ui.select((0, 5), (9, 4))), "entry 02");
    // A press on the border next to the modal's own text stays in the
    // modal, as before.
    assert_eq!(copied(ui.select((0, 1), (10, 1))), "filter: a");
    assert_eq!(ui.sel.selection().unwrap().region, "modal");
    // A press without movement on a border is still a click.
    assert_eq!(ui.mouse(click(0, 3)).0, MouseOutcome::Pass);
    assert_eq!(ui.mouse(release(0, 3)).0, MouseOutcome::Pass);
    assert!(ui.sel.selection().is_none());
}

#[test]
fn a_press_on_text_or_a_border_inside_a_region_is_unchanged() {
    // A box drawn inside a single region (a chat card, a map node): the
    // border is that region's own and the drag starts on it.
    let lines = ["┌──────┐", "│ node │", "└──────┘"];
    let r = TextRegion::new("map", Rect::new(0, 0, 40, 3)).chrome(Chrome::Drawing);
    let mut ui = Ui::new(&lines, vec![r]);
    assert_eq!(copied(ui.select((0, 1), (7, 1))), "node");
    assert_eq!(ui.sel.selection().unwrap().anchor, Point { ord: 1, col: 0 });
    // A border next to a region that is not inside the pressed one (the
    // input box under the chat) does not jump into it.
    let lines = ["╰──────╯", "input text"];
    let chat = TextRegion::new("chat", Rect::new(0, 0, 40, 1)).chrome(Chrome::Smart);
    let input = TextRegion::new("input", Rect::new(0, 1, 40, 1));
    let mut ui = Ui::new(&lines, vec![chat, input]);
    assert_eq!(ui.mouse(click(3, 0)).0, MouseOutcome::Pass);
    ui.mouse(drag(5, 1));
    ui.mouse(release(5, 1));
    assert!(ui.sel.selection().is_none_or(|s| s.region == "chat"));
}
