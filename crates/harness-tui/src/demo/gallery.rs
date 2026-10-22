//! The UI gallery: every component rendered from scripted data.
//!
//! [`Gallery`] has one page per component ([`Page`]): the chat (tool
//! cards, working indicator, timestamps, subagent cards), the agent map,
//! node detail, the goal bar, the command palette, approvals, the diff
//! review and the request inspector. A tab row at the top switches pages
//! by click; ←/→ or Tab/Enter on a tab do the same from the keyboard, and
//! `q` / Esc quits. Each page has a styled snapshot test.

use std::path::Path;
use std::time::Instant;

use crossterm::event::{Event, KeyCode, KeyEventKind};
use harness_events::schema::{NodeId, RunEvent};
use harness_policy::approvals::{ApprovalPrompt, ApprovalRequest};
use harness_policy::rules::{Action as PolicyAction, Decision, Layer, MatchedRule, Mode, Verdict};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::Modifier;
use ratatui::text::Span;
use ratatui::widgets::Paragraph;
use serde_json::json;

use super::events::{EventBuilder, NodeSpec, T0, usage};
use crate::approval::ApprovalPanel;
use crate::chat::view::{ChatConfig, ChatView};
use crate::detail::DetailView;
use crate::diff::DiffView;
use crate::goalbar::{GoalBar, Goals};
use crate::inspector::{Inspection, InspectorView};
use crate::interaction::{Dispatch, ElementId, HitMap, Interaction};
use crate::keys::Keymap;
use crate::map::{MapConfig, MapView};
use crate::palette::{Palette, builtin_entries};
use crate::status::StatusLine;
use crate::theme::{Theme, Token};

/// Gallery pages.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Page {
    /// Chat: cards, indicators, timestamps, subagent cards.
    Chat,
    /// Agent map.
    Map,
    /// Node detail.
    Detail,
    /// Goal bar.
    GoalBar,
    /// Command palette.
    Palette,
    /// Approval prompt.
    Approvals,
    /// Diff review.
    Diff,
    /// Request inspector.
    Inspector,
}

impl Page {
    /// Every page, in tab order.
    pub const ALL: [Page; 8] = [
        Page::Chat,
        Page::Map,
        Page::Detail,
        Page::GoalBar,
        Page::Palette,
        Page::Approvals,
        Page::Diff,
        Page::Inspector,
    ];

    /// Tab label.
    pub fn label(self) -> &'static str {
        match self {
            Page::Chat => "chat",
            Page::Map => "map",
            Page::Detail => "detail",
            Page::GoalBar => "goal bar",
            Page::Palette => "palette",
            Page::Approvals => "approvals",
            Page::Diff => "diff",
            Page::Inspector => "inspector",
        }
    }
}

/// Element id of a gallery tab.
pub fn tab_id(p: Page) -> String {
    format!("gallery.tab.{}", p.label().replace(' ', "_"))
}

/// The chat part of the scripted session: a main agent with reasoning,
/// an edit, a failing bash call, a subagent and a turn in progress.
pub fn chat_events() -> Vec<RunEvent> {
    let mut b = EventBuilder::new();
    b.run_started("chat", None);
    b.start(NodeSpec::step("main").agent("coder").model("sonnet"));
    b.model_call_with(
        "main",
        "c1",
        "sonnet",
        json!({
            "provider": "anthropic",
            "model": "claude-sonnet-5-5",
            "system": "You are a careful coding agent.",
            "tools": ["read", "write", "edit", "bash"],
            "messages_from": 0,
            "messages": [{"role": "user", "content": [{"type": "text", "text": "Fix the header parsing bug (#142)"}]}],
        }),
    );
    b.delta(
        "main",
        "c1",
        harness_events::schema::DeltaChannel::Reasoning,
        "The colon case panics.",
    );
    b.delta(
        "main",
        "c1",
        harness_events::schema::DeltaChannel::Text,
        "I'll update `parse_header` to return an error.",
    );
    b.advance(4_200);
    b.finish_call("main", "c1", usage(500, 11_900, 812, 120), 4_200);
    b.cost("main", 0.021);
    b.tool_call("main", "t1", "edit", json!({"path": "src/parse.rs", "old_string": ".unwrap()", "new_string": ".ok_or(ParseError::MissingColon)?"}));
    b.advance(3);
    b.file_changed(
        "main",
        "src/parse.rs",
        harness_events::schema::FileChangeKind::Modified,
        Some("--- a/src/parse.rs\n+++ b/src/parse.rs\n@@ -40,2 +40,2 @@\n     let hdr = buf.split_once(':')\n-        .unwrap();\n+        .ok_or(ParseError::MissingColon)?;\n"),
    );
    b.tool_result("main", "t1", true, "ok", 3);
    b.tool_call("main", "t2", "bash", json!({"command": "cargo test parse"}));
    b.advance(8_100);
    b.tool_result(
        "main",
        "t2",
        false,
        "test parse::header_colon ... FAILED\nexit code 101",
        8_100,
    );
    b.start(
        NodeSpec::subagent("r1", "main")
            .name("researcher")
            .model("haiku")
            .host("gpu-box"),
    );
    b.tool_call("r1", "t3", "read", json!({"path": "src/net/mod.rs"}));
    b.cost("r1", 0.004);
    b.model_call("main", "c2", "sonnet");
    b.delta(
        "main",
        "c2",
        harness_events::schema::DeltaChannel::Text,
        "Looking at the failure",
    );
    b.into_events()
}

/// The approval prompt of the gallery.
pub fn approval_prompt() -> ApprovalPrompt {
    ApprovalPrompt {
        batch: 1,
        requests: vec![ApprovalRequest {
            approval_id: "approval-1-1".into(),
            call_id: "t4".into(),
            node_id: Some(NodeId::from("fix")),
            action: PolicyAction::Bash {
                command: "git push origin main".into(),
            },
            verdict: Verdict {
                decision: Decision::Ask,
                rule: MatchedRule {
                    layer: Layer::BuiltIn,
                    origin: "outward guard".into(),
                    rule: "git push*".into(),
                    reason: "outward action".into(),
                },
                command: Some("git push origin main".into()),
                outward: true,
            },
            preview: None,
            suggested_pattern: Some("git push *".into()),
        }],
    }
}

/// Fake-provider script that replays the gallery's chat turns through the
/// real agent loop (`harness ui-demo` feeds it to
/// `harness_providers::fake::FakeProvider`).
pub const FAKE_SCRIPT: &str = r#"responses:
  - chunk_delay_ms: 30
    content:
      - reasoning: { text: "The colon case panics." }
      - text: "I'll update `parse_header` to return an error."
      - tool_use: { name: edit, input: { path: src/parse.rs, old_string: ".unwrap()", new_string: ".ok_or(ParseError::MissingColon)?" } }
    usage: { input_tokens: 500, cache_read_tokens: 11900, output_tokens: 812, reasoning_tokens: 120 }
  - error: { kind: rate_limited, message: slow down, retry_after_ms: 2000 }
  - chunk_delay_ms: 30
    content:
      - text: "Running the tests."
      - tool_use: { name: bash, input: { command: "cargo test parse" } }
    usage: { input_tokens: 300, cache_read_tokens: 12000, output_tokens: 100 }
  - content:
      - text: "The fix is in; one test still fails, see the card above."
    usage: { input_tokens: 50, cache_read_tokens: 12300, output_tokens: 40 }
"#;

/// What the gallery asks the caller to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GalleryOutput {
    /// Quit the demo.
    Quit,
}

/// The gallery.
pub struct Gallery {
    theme: Theme,
    page: Page,
    chat: ChatView,
    map: MapView,
    detail: DetailView,
    goals: Goals,
    goalbar: GoalBar,
    palette: Palette,
    approval: ApprovalPanel,
    diff: DiffView,
    inspector: InspectorView,
    now: Instant,
    interaction: Interaction<Page>,
}

impl Gallery {
    /// The gallery with Dracula colours and static spinners (stable
    /// snapshots).
    pub fn new(theme: Theme) -> Gallery {
        let now = Instant::now();
        let run = super::scripted_run();
        let cfg = ChatConfig {
            theme: theme.clone(),
            animations: false,
            ..ChatConfig::default()
        };
        let status = StatusLine::new(
            Path::new("/home/u/CODE/harness"),
            Some(Path::new("/home/u")),
            Mode::AutoEdit,
        );
        let mut chat = ChatView::new(cfg, status, now);
        chat.status_mut().set_model("sonnet", Some(200_000));
        chat.transcript_mut()
            .push_user("Fix the header parsing bug (#142)", T0);
        for e in chat_events() {
            chat.apply_run_event(&e, now);
        }
        let mut map = MapView::new(MapConfig {
            theme: theme.clone(),
            animations: false,
            live_preview: true,
        });
        for e in &run {
            map.apply_run_event(e);
        }
        let chat_ev = chat_events();
        let detail = DetailView::from_events(
            NodeId::from("main"),
            theme.clone(),
            Default::default(),
            false,
            &chat_ev,
        );
        let mut goals = Goals::new();
        let mut gb = EventBuilder::new();
        gb.start(NodeSpec::step("fix").agent("implementer").model("sonnet"));
        gb.tool_call(
            "fix",
            "todo1",
            "todo",
            json!({"goal": "Fix header parsing bug (#142)", "todos": [
                {"content": "Reproduce with failing test", "status": "completed"},
                {"content": "Fix parse_header", "status": "in_progress"},
                {"content": "Bug-fix proof", "status": "pending"},
                {"content": "Independent review", "status": "pending"},
            ]}),
        );
        for e in gb.events() {
            goals.apply(e);
        }
        for e in gb.events() {
            map.apply_run_event(e);
        }
        let mut palette = Palette::new(builtin_entries(&Keymap::defaults()));
        palette.open();
        let diff = DiffView::from_events("/w", &chat_ev, None, None, theme.clone());
        let insp = Inspection::from_events(&chat_ev, "c1", None).expect("scripted call c1");
        let inspector = InspectorView::new(insp, theme.clone(), std::env::temp_dir(), None);
        Gallery {
            goalbar: GoalBar::new(
                theme.clone(),
                harness_config::ini::config::GoalBar::Auto,
                36,
                false,
            ),
            approval: ApprovalPanel::new(approval_prompt()),
            theme,
            page: Page::Chat,
            chat,
            map,
            detail,
            goals,
            palette,
            diff,
            inspector,
            now,
            interaction: Interaction::new(),
        }
    }

    /// The shown page.
    pub fn page(&self) -> Page {
        self.page
    }

    /// Shows a page.
    pub fn show(&mut self, p: Page) {
        self.page = p;
    }

    fn step(&mut self, forward: bool) {
        let i = Page::ALL.iter().position(|p| *p == self.page).unwrap_or(0);
        let n = Page::ALL.len();
        self.page = Page::ALL[if forward {
            (i + 1) % n
        } else {
            (i + n - 1) % n
        }];
    }

    /// Handles input: ←/→ switch pages, click a tab, Tab + Enter on a tab,
    /// `q` or Esc quits.
    pub fn handle(&mut self, ev: &Event) -> Option<GalleryOutput> {
        let d = match ev {
            Event::Key(k) if k.kind != KeyEventKind::Release => match k.code {
                KeyCode::Char('q') | KeyCode::Esc => return Some(GalleryOutput::Quit),
                KeyCode::Left => {
                    self.step(false);
                    return None;
                }
                KeyCode::Right => {
                    self.step(true);
                    return None;
                }
                _ => self.interaction.handle_key(k),
            },
            Event::Mouse(m) => self.interaction.handle_mouse(m),
            _ => None,
        }?;
        if let Dispatch::Activate(p) = d {
            self.page = p;
        }
        None
    }

    /// Renders the tab row and the current page.
    pub fn render(&mut self, frame: &mut Frame<'_>, area: Rect) {
        let t = self.theme.clone();
        frame.render_widget(ratatui::widgets::Block::new().style(t.base()), area);
        let [tabs, body] =
            Layout::vertical([Constraint::Length(1), Constraint::Min(1)]).areas(area);
        let mut hits: HitMap<Page> = self.interaction.begin_frame();
        let mut x = tabs.x;
        frame.render_widget(
            Paragraph::new(Span::styled(
                " ui-demo ",
                t.fg(Token::Accent).add_modifier(Modifier::BOLD),
            )),
            Rect::new(x, tabs.y, 9.min(tabs.width), 1),
        );
        x += 9;
        for p in Page::ALL {
            let label = format!(" {} ", p.label());
            let w = label.chars().count() as u16;
            if x + w > tabs.x + tabs.width {
                break;
            }
            let id = tab_id(p);
            let style = if hits.is_focused(&ElementId::from(id.as_str())) {
                t.focused()
            } else if p == self.page {
                t.fg_on(Token::Bg, Token::Accent)
            } else {
                t.fg(Token::Muted)
            };
            let r = Rect::new(x, tabs.y, w, 1);
            frame.render_widget(Paragraph::new(Span::styled(label, style)), r);
            hits.register(id, r, p);
            x += w + 1;
        }
        self.interaction.end_frame(hits);
        let wall = T0 + time::Duration::seconds(30);
        match self.page {
            Page::Chat => self.chat.render(frame, body, self.now),
            Page::Map => self.map.render(frame, body, wall),
            Page::Detail => self.detail.render(frame, body, wall),
            Page::GoalBar => {
                let (main, pane) = self.goalbar.split(body, &self.goals);
                self.map.render(frame, main, wall);
                if let Some(p) = pane {
                    self.goalbar
                        .render(frame, p, &self.goals, self.map.run_state());
                }
            }
            Page::Palette => {
                let mut h: HitMap<()> = HitMap::new();
                self.palette.render(frame, body, &t, &mut h, &|_| ());
            }
            Page::Approvals => {
                let mut h: HitMap<()> = HitMap::new();
                let height = self.approval.height(body.width, &t).min(body.height);
                let r = Rect::new(body.x, body.y, body.width, height);
                self.approval.render(frame, r, &t, &mut h, &|_| ());
            }
            Page::Diff => self.diff.render(frame, body),
            Page::Inspector => self.inspector.render(frame, body),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interaction::testing::{buffer_text, click, find_text, key, styled_snapshot};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn snap(page: Page) -> String {
        let mut g = Gallery::new(Theme::default());
        g.show(page);
        let mut term = Terminal::new(TestBackend::new(120, 32)).unwrap();
        term.draw(|f| g.render(f, f.area())).unwrap();
        styled_snapshot(term.backend().buffer())
    }

    #[test]
    fn snapshot_chat() {
        insta::assert_snapshot!("gallery_chat", snap(Page::Chat));
    }

    #[test]
    fn snapshot_map() {
        insta::assert_snapshot!("gallery_map", snap(Page::Map));
    }

    #[test]
    fn snapshot_detail() {
        insta::assert_snapshot!("gallery_detail", snap(Page::Detail));
    }

    #[test]
    fn snapshot_goal_bar() {
        insta::assert_snapshot!("gallery_goal_bar", snap(Page::GoalBar));
    }

    #[test]
    fn snapshot_palette() {
        insta::assert_snapshot!("gallery_palette", snap(Page::Palette));
    }

    #[test]
    fn snapshot_approvals() {
        insta::assert_snapshot!("gallery_approvals", snap(Page::Approvals));
    }

    #[test]
    fn snapshot_diff() {
        insta::assert_snapshot!("gallery_diff", snap(Page::Diff));
    }

    #[test]
    fn snapshot_inspector() {
        insta::assert_snapshot!("gallery_inspector", snap(Page::Inspector));
    }

    #[test]
    fn pages_show_their_components() {
        let mut g = Gallery::new(Theme::default());
        let mut term = Terminal::new(TestBackend::new(120, 32)).unwrap();
        let mut text = |g: &mut Gallery, p: Page| {
            g.show(p);
            term.draw(|f| g.render(f, f.area())).unwrap();
            buffer_text(term.backend().buffer())
        };
        let chat = text(&mut g, Page::Chat);
        assert!(chat.contains("╭ ✔ edit"), "cards: {chat}");
        assert!(chat.contains("◐ researcher"), "subagent card: {chat}");
        assert!(chat.contains("14:32:"), "timestamps: {chat}");
        assert!(
            chat.contains("esc to interrupt") || chat.contains("writing"),
            "indicator: {chat}"
        );
        assert!(text(&mut g, Page::Map).contains("prove-fail"));
        assert!(text(&mut g, Page::GoalBar).contains("Fix parse_header"));
        assert!(text(&mut g, Page::Approvals).contains("git push origin main"));
    }

    #[test]
    fn tabs_switch_by_click_and_keyboard() {
        let mut g = Gallery::new(Theme::default());
        let mut term = Terminal::new(TestBackend::new(120, 32)).unwrap();
        term.draw(|f| g.render(f, f.area())).unwrap();
        let (x, y) = find_text(term.backend().buffer(), " map ").unwrap();
        g.handle(&click(x + 1, y));
        assert_eq!(g.page(), Page::Map);
        g.handle(&key("right"));
        assert_eq!(g.page(), Page::Detail);
        g.handle(&key("left"));
        g.handle(&key("left"));
        assert_eq!(g.page(), Page::Chat);
        term.draw(|f| g.render(f, f.area())).unwrap();
        // Focus stays on the clicked map tab; Tab moves to detail.
        g.handle(&key("tab"));
        g.handle(&key("enter"));
        assert_eq!(g.page(), Page::Detail);
        assert_eq!(g.handle(&key("q")), Some(GalleryOutput::Quit));
    }

    #[test]
    fn fake_script_parses() {
        let s = harness_providers::fake::FakeScript::from_yaml(FAKE_SCRIPT).unwrap();
        assert_eq!(s.responses.len(), 4);
    }
}
