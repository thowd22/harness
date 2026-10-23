//! `/context`: what the model receives before the first user message.
//!
//! A [`ContextReport`] lists every part of the initial context with its
//! source file and an estimated token count: each system prompt layer,
//! each tool schema, the memory index and the skill listing. The header
//! shows the total and its share of the model's context window.
//! [`ContextView`] renders it; every row is clickable and focusable and
//! expands to a preview of its text.

use std::collections::HashSet;

use crossterm::event::{Event, KeyCode, KeyEventKind, MouseEventKind};
use harness_config::discovery::Entry;
use harness_config::prompts::{LayerSource, Prompt};
use harness_providers::model::ToolSpec;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Modifier;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use super::estimate_tokens;
use crate::chat::text::{fmt_tokens, right_align, sanitize, truncate};
use crate::interaction::{Dispatch, HitMap, Interaction};
use crate::theme::{Theme, Token};

/// What a context item is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContextKind {
    /// A system prompt layer.
    PromptLayer,
    /// A tool schema.
    Tool,
    /// The memory index.
    Memory,
    /// The skill listing.
    Skills,
}

impl ContextKind {
    /// Short label.
    pub fn label(self) -> &'static str {
        match self {
            ContextKind::PromptLayer => "prompt",
            ContextKind::Tool => "tool",
            ContextKind::Memory => "memory",
            ContextKind::Skills => "skills",
        }
    }
}

/// One part of the initial context.
#[derive(Debug, Clone, PartialEq)]
pub struct ContextItem {
    /// Kind.
    pub kind: ContextKind,
    /// Name, e.g. `coder layer 2` or `bash`.
    pub name: String,
    /// Source file, when it came from one.
    pub source: Option<String>,
    /// The text sent.
    pub text: String,
    /// Estimated tokens.
    pub tokens: u64,
}

/// The initial context, item by item.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ContextReport {
    /// Items in prompt order.
    pub items: Vec<ContextItem>,
    /// The model's context window in tokens, when known.
    pub window: Option<u32>,
}

impl ContextReport {
    /// An empty report for a model with `window` tokens of context.
    pub fn new(window: Option<u32>) -> Self {
        ContextReport {
            items: Vec::new(),
            window,
        }
    }

    /// Adds an item; its tokens are estimated from `text`.
    pub fn push(
        &mut self,
        kind: ContextKind,
        name: impl Into<String>,
        source: Option<String>,
        text: impl Into<String>,
    ) {
        let text = text.into();
        self.items.push(ContextItem {
            kind,
            name: name.into(),
            source,
            tokens: estimate_tokens(&text),
            text,
        });
    }

    /// Adds every layer of a prompt that is sent. Inline layers name the
    /// prompt's file; `file:` layers name their file next to it; `builtin:`
    /// layers (the identity layer) are named `built-in <name> layer` and
    /// skipped when the prompt's identity layer is off.
    pub fn add_prompt(&mut self, entry: &Entry<Prompt>) {
        let origin = entry.origin.display.clone();
        let dir = entry
            .origin
            .path
            .as_ref()
            .and_then(|p| p.parent())
            .map(|p| p.display().to_string());
        for (i, layer) in entry.item.layers.iter().enumerate() {
            if !entry.item.identity && layer.source.is_identity() {
                continue;
            }
            let source = match &layer.source {
                LayerSource::Inline => origin.clone(),
                LayerSource::File(f) => match &dir {
                    Some(d) => format!("{d}/{f}"),
                    None => f.clone(),
                },
                LayerSource::Builtin(b) => format!("built-in {b} layer"),
            };
            self.push(
                ContextKind::PromptLayer,
                format!("{} layer {}", entry.item.name, i + 1),
                Some(source),
                layer.template.clone(),
            );
        }
    }

    /// Adds a rendered system prompt. A leading built-in identity layer
    /// ([`harness_config::prompts::IDENTITY`]) becomes its own `identity
    /// layer` item, so its token count is visible; the rest is one `system
    /// prompt (frozen prefix)` item.
    pub fn add_system_prompt(&mut self, system: &str) {
        use harness_config::prompts::{IDENTITY, LAYER_SEPARATOR};
        let rest = match system.strip_prefix(IDENTITY) {
            Some(rest) => {
                self.push(
                    ContextKind::PromptLayer,
                    "identity layer",
                    Some("built-in identity layer".into()),
                    IDENTITY,
                );
                rest.strip_prefix(LAYER_SEPARATOR).unwrap_or(rest)
            }
            None => system,
        };
        if !rest.is_empty() {
            self.push(
                ContextKind::PromptLayer,
                "system prompt (frozen prefix)",
                None,
                rest,
            );
        }
    }

    /// Adds one item per tool schema (name, description and JSON schema).
    pub fn add_tools(&mut self, tools: &[ToolSpec]) {
        for t in tools {
            let text = serde_json::to_string(t).unwrap_or_default();
            self.push(ContextKind::Tool, t.name.clone(), None, text);
        }
    }

    /// Adds the memory index text (`SessionIndex::text`) read from `source`.
    pub fn add_memory(&mut self, text: &str, source: &str) {
        if !text.is_empty() {
            self.push(
                ContextKind::Memory,
                "memory index",
                Some(source.to_owned()),
                text,
            );
        }
    }

    /// Adds the skill listing (`harness_config::skills::listing`).
    pub fn add_skills(&mut self, listing: &str, source: Option<String>) {
        if !listing.is_empty() {
            self.push(ContextKind::Skills, "skill listing", source, listing);
        }
    }

    /// Total estimated tokens.
    pub fn total(&self) -> u64 {
        self.items.iter().map(|i| i.tokens).sum()
    }

    /// Total as a percentage of the window, when known.
    pub fn percent(&self) -> Option<f64> {
        self.window
            .filter(|w| *w > 0)
            .map(|w| self.total() as f64 * 100.0 / f64::from(w))
    }

    /// Total per kind, in item order of first appearance.
    pub fn by_kind(&self) -> Vec<(ContextKind, u64)> {
        let mut out: Vec<(ContextKind, u64)> = Vec::new();
        for i in &self.items {
            match out.iter_mut().find(|(k, _)| *k == i.kind) {
                Some((_, t)) => *t += i.tokens,
                None => out.push((i.kind, i.tokens)),
            }
        }
        out
    }
}

/// Element id of a context row.
pub fn row_id(i: usize) -> String {
    format!("context.item.{i}")
}

/// The `/context` view.
pub struct ContextView {
    report: ContextReport,
    theme: Theme,
    expanded: HashSet<usize>,
    scroll: usize,
    interaction: Interaction<usize>,
}

/// What the context view asks the app to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContextOutput {
    /// Esc.
    Back,
}

impl ContextView {
    /// A view of `report`.
    pub fn new(report: ContextReport, theme: Theme) -> Self {
        ContextView {
            report,
            theme,
            expanded: HashSet::new(),
            scroll: 0,
            interaction: Interaction::new(),
        }
    }

    /// The report.
    pub fn report(&self) -> &ContextReport {
        &self.report
    }

    /// Whether row `i` is expanded.
    pub fn is_expanded(&self, i: usize) -> bool {
        self.expanded.contains(&i)
    }

    fn toggle(&mut self, i: usize) {
        if !self.expanded.remove(&i) {
            self.expanded.insert(i);
        }
    }

    /// Handles input: click or Enter on a row expands it, Tab/arrows move
    /// focus, the wheel and PgUp/PgDn scroll, Esc backs out.
    pub fn handle(&mut self, ev: &Event) -> Option<ContextOutput> {
        let d = match ev {
            Event::Key(k) if k.kind != KeyEventKind::Release => {
                match k.code {
                    KeyCode::PageDown => self.scroll += 10,
                    KeyCode::PageUp => self.scroll = self.scroll.saturating_sub(10),
                    _ => {}
                }
                self.interaction.handle_key(k)
            }
            Event::Mouse(m) => match m.kind {
                MouseEventKind::ScrollUp => {
                    self.scroll = self.scroll.saturating_sub(3);
                    None
                }
                MouseEventKind::ScrollDown => {
                    self.scroll += 3;
                    None
                }
                _ => self.interaction.handle_mouse(m),
            },
            _ => None,
        }?;
        match d {
            Dispatch::Activate(i) => {
                self.toggle(i);
                None
            }
            Dispatch::Back => Some(ContextOutput::Back),
            _ => None,
        }
    }

    /// Renders the view.
    pub fn render(&mut self, frame: &mut Frame<'_>, area: Rect) {
        let t = self.theme.clone();
        let mut hits: HitMap<usize> = self.interaction.begin_frame();
        hits.register_pane("context", area);
        let w = usize::from(area.width);
        let r = &self.report;
        let mut lines: Vec<(Option<usize>, Line<'static>)> = Vec::new();
        let mut head = vec![
            Span::styled(
                "Initial context ",
                t.fg(Token::Fg).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!("≈{} tokens", fmt_tokens(r.total())),
                t.fg(Token::Number),
            ),
        ];
        match (r.percent(), r.window) {
            (Some(p), Some(win)) => head.push(Span::styled(
                format!(" · {p:.1}% of {} window", fmt_tokens(u64::from(win))),
                t.fg(Token::Muted),
            )),
            _ => head.push(Span::styled(" · window unknown", t.fg(Token::Muted))),
        }
        lines.push((None, Line::from(head)));
        let kinds: Vec<String> = r
            .by_kind()
            .iter()
            .map(|(k, n)| format!("{} {}", k.label(), fmt_tokens(*n)))
            .collect();
        lines.push((
            None,
            Line::from(Span::styled(kinds.join(" · "), t.fg(Token::Muted))),
        ));
        for (i, item) in r.items.iter().enumerate() {
            let focused = hits.is_focused(&crate::interaction::ElementId::from(row_id(i)));
            let arrow = if self.expanded.contains(&i) {
                "▾ "
            } else {
                "▸ "
            };
            let left = Line::from(vec![
                Span::styled(arrow, t.fg(Token::Accent)),
                Span::styled(format!("{:<7}", item.kind.label()), t.fg(Token::Muted)),
                Span::styled(item.name.clone(), t.fg(Token::Agent)),
                Span::styled(
                    item.source
                        .as_ref()
                        .map(|s| format!("  {s}"))
                        .unwrap_or_default(),
                    t.fg(Token::Path),
                ),
            ]);
            let right = vec![Span::styled(fmt_tokens(item.tokens), t.fg(Token::Number))];
            let mut line = right_align(left, right, w, Span::raw(" "));
            if focused {
                line = line.patch_style(t.focused());
            }
            lines.push((Some(i), line));
            if self.expanded.contains(&i) {
                for l in item.text.lines().take(12) {
                    lines.push((
                        None,
                        Line::from(Span::styled(
                            truncate(&format!("    {}", sanitize(l)), w),
                            t.fg(Token::Muted),
                        )),
                    ));
                }
            }
        }
        let h = usize::from(area.height);
        self.scroll = self.scroll.min(lines.len().saturating_sub(h));
        let mut shown = Vec::new();
        for (row, (item, line)) in lines.into_iter().skip(self.scroll).take(h).enumerate() {
            if let Some(i) = item {
                hits.register(
                    row_id(i),
                    Rect::new(area.x, area.y + row as u16, area.width, 1),
                    i,
                );
            }
            shown.push(line);
        }
        frame.render_widget(Paragraph::new(shown), area);
        self.interaction.end_frame(hits);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interaction::testing::{buffer_text, click, find_text, key};
    use harness_config::discovery::Artifact;
    use harness_config::discovery::{Kind, Scope, SourceFile};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use serde_json::json;

    fn report() -> ContextReport {
        let src = SourceFile::inline(
            Scope::Project,
            Kind::Prompt,
            "prompts/coder.yaml",
            "name: coder\nlayers:\n  - text: You are a careful coding agent.\n  - text: Prefer small diffs.\n",
        );
        let prompt = Prompt::load(&src).expect("prompt parses");
        let entry = Entry {
            item: prompt,
            origin: harness_config::discovery::Origin {
                scope: Scope::Project,
                display: ".harness/prompts/coder.yaml".into(),
                path: None,
            },
            text: String::new(),
        };
        let mut r = ContextReport::new(Some(200_000));
        r.add_prompt(&entry);
        r.add_tools(&[ToolSpec {
            name: "bash".into(),
            description: "Run a shell command".into(),
            input_schema: json!({"type": "object", "properties": {"command": {"type": "string"}}}),
        }]);
        r.add_memory(
            "# Memory\n- prefers cargo nextest",
            "~/.local/share/harness/memory/MEMORY.md",
        );
        r.add_skills(
            "Skills:\n- /review: review the diff",
            Some("~/.config/harness/skills".into()),
        );
        r
    }

    #[test]
    fn shows_the_identity_layer_with_its_token_count() {
        let src = SourceFile::inline(
            Scope::Builtin,
            Kind::Prompt,
            "prompts/coder.yaml",
            "name: coder\nlayers:\n  - builtin: identity\n  - text: Prefer small diffs.\n",
        );
        let mut entry = Entry {
            item: Prompt::load(&src).expect("prompt parses"),
            origin: harness_config::discovery::Origin {
                scope: Scope::Builtin,
                display: "<built-in>/prompts/coder.yaml".into(),
                path: None,
            },
            text: String::new(),
        };
        let mut r = ContextReport::new(Some(200_000));
        r.add_prompt(&entry);
        let first = &r.items[0];
        assert_eq!(first.name, "coder layer 1");
        assert_eq!(first.source.as_deref(), Some("built-in identity layer"));
        assert_eq!(first.text, harness_config::prompts::IDENTITY);
        assert_eq!(
            first.tokens,
            estimate_tokens(harness_config::prompts::IDENTITY)
        );
        assert!(first.tokens > 0 && first.tokens <= 120);

        entry.item.identity = false;
        let mut off = ContextReport::new(Some(200_000));
        off.add_prompt(&entry);
        assert_eq!(off.items.len(), 1);
        assert_eq!(off.items[0].name, "coder layer 2");
    }

    #[test]
    fn system_prompt_splits_off_the_identity_layer() {
        use harness_config::prompts::IDENTITY;
        let mut r = ContextReport::new(None);
        r.add_system_prompt(&format!("{IDENTITY}\n\nBe careful."));
        let names: Vec<&str> = r.items.iter().map(|i| i.name.as_str()).collect();
        assert_eq!(names, ["identity layer", "system prompt (frozen prefix)"]);
        assert_eq!(r.items[0].tokens, estimate_tokens(IDENTITY));
        assert_eq!(
            r.items[0].source.as_deref(),
            Some("built-in identity layer")
        );
        assert_eq!(r.items[1].text, "Be careful.");

        let mut off = ContextReport::new(None);
        off.add_system_prompt("Be careful.");
        assert_eq!(off.items.len(), 1);
        assert_eq!(off.items[0].name, "system prompt (frozen prefix)");
    }

    #[test]
    fn lists_layers_tools_memory_and_skills_with_sources_and_tokens() {
        let r = report();
        let kinds: Vec<ContextKind> = r.items.iter().map(|i| i.kind).collect();
        assert_eq!(
            kinds,
            [
                ContextKind::PromptLayer,
                ContextKind::PromptLayer,
                ContextKind::Tool,
                ContextKind::Memory,
                ContextKind::Skills
            ]
        );
        assert_eq!(
            r.items[0].source.as_deref(),
            Some(".harness/prompts/coder.yaml")
        );
        assert_eq!(
            r.items[0].tokens,
            estimate_tokens("You are a careful coding agent.")
        );
        let total: u64 = r.items.iter().map(|i| i.tokens).sum();
        assert_eq!(r.total(), total);
        let pct = r.percent().unwrap();
        assert!((pct - total as f64 / 2000.0).abs() < 1e-9);

        let mut v = ContextView::new(r, Theme::default());
        let mut term = Terminal::new(TestBackend::new(100, 20)).unwrap();
        term.draw(|f| v.render(f, f.area())).unwrap();
        let text = buffer_text(term.backend().buffer());
        assert!(text.contains("Initial context ≈"), "{text}");
        assert!(text.contains("% of 200k window"), "{text}");
        assert!(
            text.contains("coder layer 1  .harness/prompts/coder.yaml"),
            "{text}"
        );
        assert!(text.contains("tool   bash"), "{text}");
        assert!(
            text.contains("memory index  ~/.local/share/harness/memory/MEMORY.md"),
            "{text}"
        );
        assert!(text.contains("skill listing"), "{text}");
    }

    #[test]
    fn rows_expand_by_click_and_keyboard() {
        let mut v = ContextView::new(report(), Theme::default());
        let mut term = Terminal::new(TestBackend::new(100, 30)).unwrap();
        term.draw(|f| v.render(f, f.area())).unwrap();
        let (x, y) = find_text(term.backend().buffer(), "coder layer 2").unwrap();
        v.handle(&click(x, y));
        assert!(v.is_expanded(1));
        term.draw(|f| v.render(f, f.area())).unwrap();
        assert!(buffer_text(term.backend().buffer()).contains("    Prefer small diffs."));
        term.draw(|f| v.render(f, f.area())).unwrap();
        v.handle(&key("up"));
        v.handle(&key("enter"));
        assert!(v.is_expanded(0));
        assert_eq!(v.handle(&key("esc")), Some(ContextOutput::Back));
    }
}
