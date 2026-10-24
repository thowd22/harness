//! Layout matrix cases for the screens: agent map (and live pane), node
//! detail, goal bar, diff review, request inspector, context view, run
//! history, replay bar and the ui-demo gallery tabs.

use super::*;
use crate::demo::events::{EventBuilder, NodeSpec, usage};
use crate::detail::DetailView;
use crate::diff::DiffView;
use crate::goalbar::{GoalBar, Goals};
use crate::history::{HistoryView, RunSummary};
use crate::inspector::{Inspection, InspectorView, Tab};
use crate::interaction::testing::{key, wheel_down};
use crate::map::{MapConfig, MapView};
use crate::theme::Theme;
use harness_events::schema::{DeltaChannel, NodeId, RunEvent, RunId, RunStatus};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use serde_json::json;
use time::OffsetDateTime;

fn term(w: u16, h: u16) -> Terminal<TestBackend> {
    Terminal::new(TestBackend::new(w, h)).unwrap()
}

fn now() -> OffsetDateTime {
    crate::demo::events::T0 + time::Duration::minutes(5)
}

/// The long-content run plus a workflow of steps with long names, agents,
/// models and hosts, so the map shows full boxes and edges.
fn map_events() -> (Vec<RunEvent>, Fx) {
    let (mut events, mut fx) = run_events();
    let mut b = EventBuilder::new();
    let (s1, a1, m1, h1) = (
        fx.word("step1"),
        fx.word("agent1"),
        fx.word("model1"),
        fx.word("host1"),
    );
    b.start(
        NodeSpec::step("s1")
            .name(&s1)
            .agent(&a1)
            .model(&m1)
            .host(&h1),
    );
    let s2 = fx.wide("step2");
    b.start(NodeSpec::step("s2").name(&s2).needs(&["s1"]));
    let offset = events.len() as u64;
    for mut e in b.into_events() {
        e.seq += offset;
        e.run_id = events[0].run_id;
        events.push(e);
    }
    (events, fx)
}

/// Copies `buf`, turning the map's edge corners (`┌┐└┘`, drawn as plain
/// line junctions) into `┼` so the border check only sees the rounded node
/// boxes (`╭╮╰╯`) and the live pane: an edge bend is not a box.
fn without_edge_corners(buf: &Buffer) -> Buffer {
    let mut b = buf.clone();
    let area = b.area;
    for y in area.top()..area.bottom() {
        for x in area.left()..area.right() {
            let c = &mut b[(x, y)];
            if matches!(c.symbol(), "┌" | "┐" | "└" | "┘") {
                c.set_symbol("┼");
            }
        }
    }
    b
}

/// Scrolls to the top (`step(true)` scrolls up), then to the bottom, each
/// until the frame stops changing, checking every frame. Returns the
/// frames from the top down.
fn sweep(
    what: &str,
    check: bool,
    mut draw: impl FnMut() -> Buffer,
    mut step: impl FnMut(bool),
) -> Vec<Buffer> {
    let mut out: Vec<Buffer> = Vec::new();
    for up in [true, false] {
        let mut last: Option<String> = None;
        for _ in 0..400 {
            let buf = draw();
            if check {
                assert_clean(what, &buf, buf.area);
            }
            let s = screen(&buf);
            if last.as_ref() == Some(&s) {
                break;
            }
            last = Some(s);
            if !up {
                out.push(buf);
            }
            step(up);
        }
    }
    out
}

/// The text of each column band of `buf`, split at `cols`: panes side by
/// side wrap on their own, so tokens are matched per pane.
fn bands(buf: &Buffer, cols: &[u16]) -> Vec<String> {
    let mut edges = vec![0];
    edges.extend(cols.iter().copied().filter(|c| *c < buf.area.width));
    edges.push(buf.area.width);
    edges
        .windows(2)
        .map(|w| text_in(buf, Rect::new(w[0], 0, w[1] - w[0], buf.area.height)))
        .collect()
}

/// One text from the frames of a pane scrolled step by step: the first
/// frame, then for each next frame the rows after its longest overlap with
/// the text so far (skipping rows above the overlap, such as a header that
/// changed). A token taller than the pane is matched across the scroll.
fn stitch(frames: &[String]) -> String {
    let rows: Vec<Vec<&str>> = frames.iter().map(|f| f.lines().collect()).collect();
    let Some(first) = rows.first() else {
        return String::new();
    };
    let mut out: Vec<&str> = first.clone();
    for r in &rows[1..] {
        let mut best = (0usize, 0usize); // (overlap, skipped)
        for j in 0..r.len() {
            let max = (r.len() - j).min(out.len());
            if max <= best.0 {
                break;
            }
            if let Some(k) = (best.0 + 1..=max)
                .rev()
                .find(|&k| out[out.len() - k..] == r[j..j + k])
            {
                best = (k, j);
            }
        }
        let (k, j) = best;
        if k == 0 {
            // Pages that follow each other: drop the rows shared with the
            // first frame's top (a header) and append the rest.
            let head = r.iter().zip(first).take_while(|(a, b)| a == b).count();
            out.extend(&r[head.min(r.len().saturating_sub(1))..]);
        } else {
            out.extend(&r[j + k..]);
        }
    }
    out.join("\n")
}

#[test]
fn map_fits_and_selected_node_details_are_reachable() {
    matrix("map", |w, h, check| {
        let (events, fx) = map_events();
        let mut map = MapView::new(MapConfig {
            theme: Theme::default(),
            animations: false,
            live_preview: true,
        });
        for e in &events {
            map.apply_run_event(e);
        }
        let mut t = term(w, h);
        let mut frames = Vec::new();
        let ids: Vec<NodeId> = map.run_state().nodes.iter().map(|n| n.id.clone()).collect();
        for live in [false, true] {
            map.set_live_open(live);
            for id in &ids {
                map.select(id.clone());
                let buf = frame(&mut t, |f| map.render(f, f.area(), now()));
                if check {
                    assert_clean("map", &without_edge_corners(&buf), buf.area);
                }
                let s = screen(&buf);
                // The info line had no room for everything: its documented
                // action, Enter, opens the node detail, which shows it all.
                if check && s.contains("Enter opens the detail") {
                    frames.push(detail_text(&events, id, w, h));
                }
                frames.push(s);
            }
        }
        if !check {
            return;
        }
        let tokens: Vec<&str> = fx
            .all()
            .into_iter()
            .filter(|t| {
                // Agents show as their display names, so `agent1` is not
                // matched literally.
                ["model1", "host1", "step1", "step2", "sub", "host"]
                    .iter()
                    .any(|k| t.ends_with(k) || t.ends_with(&format!("_{k}")))
            })
            .collect();
        assert_reachable("map", &frames, &tokens);
    });
}

/// The node detail of `node`, scrolled from the top to the bottom and
/// stitched into one text (the log pane only, beside any sidebar).
fn detail_text(events: &[RunEvent], node: &NodeId, w: u16, h: u16) -> String {
    let d = std::cell::RefCell::new(DetailView::from_events(
        node.clone(),
        Theme::default(),
        Default::default(),
        false,
        events,
    ));
    let t = std::cell::RefCell::new(term(w, h));
    let frames = sweep(
        "detail",
        true,
        || {
            frame(&mut t.borrow_mut(), |f| {
                d.borrow_mut().render(f, f.area(), now())
            })
        },
        |up| {
            let x = w.saturating_sub(1);
            d.borrow_mut().handle(&if up {
                crate::interaction::testing::wheel_up(x, h / 2)
            } else {
                wheel_down(x, h / 2)
            });
        },
    );
    let split: &[u16] = if w >= crate::detail::SIDEBAR_WIDTH + 40 {
        &[crate::detail::SIDEBAR_WIDTH]
    } else {
        &[]
    };
    let pane: Vec<String> = frames
        .iter()
        .map(|b| bands(b, split).pop().unwrap_or_default())
        .collect();
    stitch(&pane)
}

#[test]
fn node_detail_fits_and_everything_is_reachable() {
    matrix("detail", |w, h, check| {
        let (events, fx) = run_events();
        for node in ["main", "r1"] {
            let mut d = DetailView::from_events(
                NodeId::from(node),
                Theme::default(),
                Default::default(),
                false,
                &events,
            );
            let mut t = term(w, h);
            // Expand the collapsed reasoning (a click on its toggle).
            // Scroll from the top until the toggle shows, then click it.
            frame(&mut t, |f| d.render(f, f.area(), now()));
            d.handle(&key("home"));
            for _ in 0..100 {
                let buf = frame(&mut t, |f| d.render(f, f.area(), now()));
                if let Some((x, y)) = crate::interaction::testing::find_text(&buf, "▸ thinking") {
                    d.handle(&crate::interaction::testing::click(x, y));
                    break;
                }
                d.handle(&wheel_down(w.saturating_sub(1), h / 2));
            }
            let d = std::cell::RefCell::new(d);
            let t = std::cell::RefCell::new(t);
            let frames = sweep(
                "detail",
                check,
                || {
                    frame(&mut t.borrow_mut(), |f| {
                        d.borrow_mut().render(f, f.area(), now())
                    })
                },
                |up| {
                    let ev = if up {
                        crate::interaction::testing::wheel_up(w.saturating_sub(1), h / 2)
                    } else {
                        wheel_down(w.saturating_sub(1), h / 2)
                    };
                    d.borrow_mut().handle(&ev);
                },
            );
            if !check {
                continue;
            }
            // The sidebar sits beside the log from 70 columns.
            let split: &[u16] = if w >= crate::detail::SIDEBAR_WIDTH + 40 {
                &[crate::detail::SIDEBAR_WIDTH]
            } else {
                &[]
            };
            let n = split.len() + 1;
            // Whole screens for the full-width header, and each pane
            // stitched over the scroll.
            let mut texts: Vec<String> = frames.iter().map(screen).collect();
            texts.extend((0..n).map(|k| {
                let pane: Vec<String> = frames.iter().map(|b| bands(b, split)[k].clone()).collect();
                stitch(&pane)
            }));
            let tokens: Vec<&str> = if node == "r1" {
                fx.all()
                    .into_iter()
                    .filter(|t| {
                        t.contains("subread") || t.ends_with("_sub") || t.ends_with("_host")
                    })
                    .collect()
            } else {
                fx.all()
                    .into_iter()
                    .filter(|t| {
                        t.ends_with("_model") || t.contains("verify") || t.contains("think")
                    })
                    .collect()
            };
            assert_reachable(&format!("detail of {node}"), &texts, &tokens);
        }
    });
}

#[test]
fn goal_bar_fits_and_tasks_are_reachable() {
    matrix("goalbar", |w, h, check| {
        let (events, fx) = run_events();
        let mut goals = Goals::new();
        let mut run = harness_events::reducer::RunState::new();
        for e in &events {
            goals.apply(e);
            run.apply(e);
        }
        let mut bar = GoalBar::new(
            Theme::default(),
            harness_config::ini::config::GoalBar::Auto,
            32,
            false,
        );
        let mut t = term(w, h);
        let mut frames = Vec::new();
        let area = Rect::new(0, 0, w, h);
        let (_, pane) = bar.split(area, &goals);
        let Some(pane) = pane else {
            // Too narrow for the pane: the status-line indicator stands in.
            let ind = bar.indicator(&goals, w).expect("indicator when collapsed");
            let text: String = ind.iter().map(|s| s.content.as_ref()).collect();
            if check {
                // The in-progress task's title, whole (the status line wraps).
                let tokens: Vec<&str> = fx
                    .all()
                    .into_iter()
                    .filter(|t| t.ends_with("todo2"))
                    .collect();
                assert_reachable("goal indicator", &[text], &tokens);
            }
            return;
        };
        for _ in 0..40 {
            let buf = frame(&mut t, |f| bar.render(f, pane, &goals, &run));
            if check {
                assert_clean("goal bar", &buf, buf.area);
            }
            let s = screen(&buf);
            if frames.last() == Some(&s) {
                break;
            }
            frames.push(s);
            bar.handle(&wheel_down(pane.x + 1, pane.y + 1));
        }
        if !check {
            return;
        }
        let tokens: Vec<&str> = fx
            .all()
            .into_iter()
            .filter(|t| t.contains("goal") || t.contains("todo"))
            .collect();
        assert_reachable("goal bar", &frames, &tokens);
    });
}

#[test]
fn diff_review_fits_and_every_file_is_reachable() {
    matrix("diff", |w, h, check| {
        let (mut events, mut fx) = run_events();
        // Many files, so the list must scroll.
        let mut b = EventBuilder::new();
        let mut paths = Vec::new();
        for i in 0..12 {
            let p = fx.path(&format!("many{i}"));
            b.file_changed(
                "main",
                &p,
                harness_events::schema::FileChangeKind::Created,
                Some(&format!(
                    "--- /dev/null\n+++ b{p}\n@@ -0,0 +1 @@\n+line {i}\n"
                )),
            );
            paths.push(p);
        }
        let off = events.len() as u64;
        for mut e in b.into_events() {
            e.seq += off;
            e.run_id = events[0].run_id;
            events.push(e);
        }
        let d = std::cell::RefCell::new(DiffView::from_events(
            "/",
            &events,
            None,
            None,
            Theme::default(),
        ));
        let t = std::cell::RefCell::new(term(w, h));
        let n = d.borrow().files().len();
        let mut frames = Vec::new();
        let mut per_file = Vec::new();
        // Each file (Down steps through them), its diff scrolled through
        // with the wheel.
        for i in 0..n {
            let f = sweep(
                "diff",
                check,
                || frame(&mut t.borrow_mut(), |f| d.borrow_mut().render(f, f.area())),
                |up| {
                    let x = w.saturating_sub(1);
                    d.borrow_mut().handle(&if up {
                        crate::interaction::testing::wheel_up(x, h / 2)
                    } else {
                        wheel_down(x, h / 2)
                    });
                },
            );
            frames.extend(f.iter().cloned());
            per_file.push(f);
            if i + 1 < n {
                d.borrow_mut().handle(&key("down"));
            }
        }
        if !check {
            return;
        }
        let list_w = (w / 3).clamp(24, 48).min(w);
        // The list as shown, and each file's diff stitched over its scroll.
        let mut texts: Vec<String> = frames
            .iter()
            .map(|b| bands(b, &[list_w])[0].clone())
            .collect();
        for f in &per_file {
            let pane: Vec<String> = f.iter().map(|b| bands(b, &[list_w])[1].clone()).collect();
            texts.push(stitch(&pane));
        }
        let frames = texts;
        let tokens: Vec<&str> = fx
            .all()
            .into_iter()
            .filter(|t| {
                t.contains("_edit_")
                    || t.contains("many")
                    || t.ends_with("_old")
                    || t.ends_with("_new")
                    || t.ends_with("ctx")
            })
            .collect();
        assert_reachable("diff", &frames, &tokens);
    });
}

fn inspected(fx: &mut Fx) -> Inspection {
    let mut b = EventBuilder::new();
    b.start(NodeSpec::step("main").agent("coder"));
    let (model, system, msg, reply) = (
        fx.word("imodel"),
        fx.prose("isys"),
        fx.prose("imsg"),
        fx.prose("ireply"),
    );
    b.model_call_with(
        "main",
        "c1",
        &model,
        json!({
            "provider": "anthropic",
            "model": model,
            "system": system,
            "tools": ["read", "bash"],
            "messages_from": 0,
            "messages": [{"role": "user", "content": [{"type": "text", "text": msg}]}],
        }),
    );
    b.delta("main", "c1", DeltaChannel::Text, &reply);
    b.finish_call("main", "c1", usage(10, 20, 30, 0), 100);
    Inspection::from_events(b.events(), "c1", None).expect("call c1")
}

#[test]
fn inspector_fits_and_every_tab_is_reachable() {
    matrix("inspector", |w, h, check| {
        let mut fx = Fx::new();
        let data = inspected(&mut fx);
        let mut v = InspectorView::new(data, Theme::default(), std::env::temp_dir(), None);
        let mut t = term(w, h);
        let mut frames = Vec::new();
        for tab in ["1", "2", "3"] {
            v.handle(&key(tab));
            let mut pages = Vec::new();
            for _ in 0..300 {
                let buf = frame(&mut t, |f| v.render(f, f.area()));
                if check {
                    assert_clean("inspector", &buf, buf.area);
                }
                let s = screen(&buf);
                if pages.last() == Some(&s) {
                    break;
                }
                pages.push(s);
                v.handle(&key("pgdn"));
            }
            // Pages follow each other; a token can span two.
            frames.push(stitch(&pages));
        }
        if !check {
            return;
        }
        let _ = Tab::Request;
        // The request JSON shows a tab as its `\\t` escape, exactly as sent.
        let mut tokens: Vec<&str> = fx
            .all()
            .into_iter()
            .filter(|t| !t.contains('\t') || t.contains("ireply"))
            .collect();
        tokens.extend(["[Request]", "[Response]", "[Layers]", "[export]"]);
        assert_reachable("inspector", &frames, &tokens);
    });
}

fn summaries(fx: &mut Fx) -> Vec<RunSummary> {
    (0..8)
        .map(|i| RunSummary {
            run_id: RunId::new(),
            name: format!("run{i}"),
            workflow: None,
            agent: Some("coder".into()),
            who: fx.word(&format!("who{i}")),
            title: Some(fx.prose(&format!("title{i}x"))),
            files_changed: 3,
            error: None,
            status: RunStatus::Succeeded,
            started_at: Some(crate::demo::events::T0),
            duration_ms: Some(134_000),
            cost_usd: 0.12,
            host: Some(fx.word(&format!("rhost{i}"))),
            nodes: 2,
            truncated: false,
        })
        .collect()
}

#[test]
fn history_fits_and_every_run_is_reachable() {
    matrix("history", |w, h, check| {
        let mut fx = Fx::new();
        let v = std::cell::RefCell::new(HistoryView::new(summaries(&mut fx), Theme::default()));
        let t = std::cell::RefCell::new(term(w, h));
        // The wheel scrolls the whole list.
        let frames = sweep(
            "history",
            check,
            || {
                frame(&mut t.borrow_mut(), |f| {
                    v.borrow_mut().render(f, f.area(), now())
                })
            },
            |up| {
                v.borrow_mut().handle(&if up {
                    crate::interaction::testing::wheel_up(1, h / 2)
                } else {
                    wheel_down(1, h / 2)
                });
            },
        );
        if !check {
            return;
        }
        let texts: Vec<String> = frames.iter().map(screen).collect();
        assert_reachable("history", &[stitch(&texts)], &fx.all());
    });
}

#[test]
fn gallery_tabs_fit() {
    matrix("gallery", |w, h, check| {
        let mut g = crate::demo::gallery::Gallery::new(Theme::default());
        let mut t = term(w, h);
        let buf = frame(&mut t, |f| g.render(f, f.area()));
        if !check {
            return;
        }
        let s = screen(&buf);
        let labels: Vec<&str> = crate::demo::gallery::Page::ALL
            .iter()
            .map(|p| p.label())
            .collect();
        assert_reachable("gallery tabs", std::slice::from_ref(&s), &labels);
    });
}
