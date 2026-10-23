//! A full-screen text panel with buttons, for command output such as
//! `/cost`, `/config`, `/remote` and `/help`.
//!
//! The text scrolls with the wheel, PageUp/PageDown, Home/End and Up/Down;
//! buttons (`[ test gpu ]`, `[ close ]`) are focusable with Tab / Shift+Tab
//! or Left/Right, activate with Enter or a click, and Esc closes the panel.

use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Paragraph};

use crate::chat::text::{sanitize, width};
use crate::interaction::{Dispatch, ElementId, Interaction};
use crate::theme::{Theme, Token};

/// Element id of the close button.
pub const CLOSE_ID: &str = "panel.close";
/// Scroll pane id of the text.
pub const PANEL_PANE: &str = "panel";

/// A button below the text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PanelButton {
    /// Returned in [`PanelOutput::Action`].
    pub id: String,
    /// Label, shown as `[ label ]`.
    pub label: String,
}

impl PanelButton {
    /// A button.
    pub fn new(id: impl Into<String>, label: impl Into<String>) -> PanelButton {
        PanelButton {
            id: id.into(),
            label: label.into(),
        }
    }
}

/// Element id of a button.
pub fn button_id(id: &str) -> String {
    format!("panel.button.{id}")
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum PanelAction {
    Button(String),
    Close,
}

/// What the panel asks the app to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PanelOutput {
    /// A button was activated.
    Action(String),
    /// Close the panel.
    Close,
}

/// The panel.
pub struct Panel {
    title: String,
    lines: Vec<Line<'static>>,
    buttons: Vec<PanelButton>,
    scroll: usize,
    view_height: usize,
    theme: Theme,
    interaction: Interaction<PanelAction>,
}

impl std::fmt::Debug for Panel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Panel")
            .field("title", &self.title)
            .field("lines", &self.lines.len())
            .field("buttons", &self.buttons)
            .finish_non_exhaustive()
    }
}

impl Panel {
    /// A panel showing `text` (one entry per line).
    pub fn new(title: impl Into<String>, text: &[String], theme: Theme) -> Panel {
        let fg = theme.fg(Token::Fg);
        Panel::with_lines(
            title,
            text.iter()
                .map(|l| Line::from(Span::styled(sanitize(l), fg)))
                .collect(),
            theme,
        )
    }

    /// A panel showing styled lines.
    pub fn with_lines(title: impl Into<String>, lines: Vec<Line<'static>>, theme: Theme) -> Panel {
        Panel {
            title: title.into(),
            lines,
            buttons: Vec::new(),
            scroll: 0,
            view_height: 1,
            theme,
            interaction: Interaction::new(),
        }
    }

    /// Adds buttons before the close button.
    pub fn with_buttons(mut self, buttons: Vec<PanelButton>) -> Panel {
        self.buttons = buttons;
        self
    }

    /// The title.
    pub fn title(&self) -> &str {
        &self.title
    }

    /// Replaces the text (e.g. after a button ran), keeping the scroll
    /// position where possible.
    pub fn set_text(&mut self, text: &[String]) {
        let fg = self.theme.fg(Token::Fg);
        self.lines = text
            .iter()
            .map(|l| Line::from(Span::styled(sanitize(l), fg)))
            .collect();
        self.scroll = self.scroll.min(self.max_scroll());
    }

    /// The plain text, for tests.
    pub fn text(&self) -> Vec<String> {
        self.lines
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect()
    }

    /// The focused element.
    pub fn focused(&self) -> Option<&ElementId> {
        self.interaction.focused()
    }

    /// First visible line.
    pub fn scroll_offset(&self) -> usize {
        self.scroll
    }

    fn max_scroll(&self) -> usize {
        self.lines.len().saturating_sub(self.view_height)
    }

    fn scroll_by(&mut self, lines: i32) {
        let next = self.scroll as i64 + i64::from(lines);
        self.scroll = next.clamp(0, self.max_scroll() as i64) as usize;
    }

    /// Handles a terminal event.
    pub fn handle(&mut self, ev: &Event) -> Option<PanelOutput> {
        let d = match ev {
            Event::Key(k) => {
                if k.kind == KeyEventKind::Release
                    || !k.modifiers.difference(KeyModifiers::SHIFT).is_empty()
                {
                    return None;
                }
                let page = self.view_height.saturating_sub(1).max(1) as i32;
                match k.code {
                    KeyCode::PageUp => return self.scrolled(-page),
                    KeyCode::PageDown => return self.scrolled(page),
                    KeyCode::Home => return self.scrolled(-(self.lines.len() as i32)),
                    KeyCode::End => return self.scrolled(self.lines.len() as i32),
                    // Up/Down scroll the text; Tab and Left/Right move
                    // between buttons.
                    KeyCode::Up => return self.scrolled(-1),
                    KeyCode::Down => return self.scrolled(1),
                    _ => self.interaction.handle_key(k)?,
                }
            }
            Event::Mouse(m) => self.interaction.handle_mouse(m)?,
            _ => return None,
        };
        match d {
            Dispatch::Activate(PanelAction::Button(id)) => Some(PanelOutput::Action(id)),
            Dispatch::Activate(PanelAction::Close) | Dispatch::Back => Some(PanelOutput::Close),
            Dispatch::Scroll { lines, .. } => self.scrolled(lines),
            Dispatch::Focus(_) => None,
        }
    }

    fn scrolled(&mut self, lines: i32) -> Option<PanelOutput> {
        self.scroll_by(lines);
        None
    }

    /// Draws the panel into `area`.
    pub fn render(&mut self, frame: &mut Frame<'_>, area: Rect) {
        let theme = self.theme.clone();
        let mut hits = self.interaction.begin_frame();
        hits.push_modal(area);
        let block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(theme.border(true))
            .style(theme.base())
            .title(Span::styled(
                format!(" {} ", self.title),
                theme.fg(Token::Accent),
            ));
        let inner = block.inner(area);
        frame.render_widget(block, area);

        // Buttons wrap onto as many rows as they need, at the bottom.
        let mut all: Vec<(String, String, PanelAction)> = self
            .buttons
            .iter()
            .map(|b| {
                (
                    button_id(&b.id),
                    format!("[ {} ]", b.label),
                    PanelAction::Button(b.id.clone()),
                )
            })
            .collect();
        all.push((CLOSE_ID.into(), "[ close ]".into(), PanelAction::Close));
        let mut rows: Vec<Vec<(String, String, PanelAction)>> = vec![Vec::new()];
        let mut used = 0usize;
        for b in all {
            let w = width(&b.1) + 1;
            if used + w > inner.width as usize && !rows.last().is_some_and(Vec::is_empty) {
                rows.push(Vec::new());
                used = 0;
            }
            used += w;
            rows.last_mut().expect("a row").push(b);
        }
        let button_rows = (rows.len() as u16).min(inner.height.saturating_sub(1));
        let text_area = Rect {
            height: inner.height.saturating_sub(button_rows),
            ..inner
        };
        self.view_height = text_area.height.max(1) as usize;
        self.scroll = self.scroll.min(self.max_scroll());
        hits.register_pane(PANEL_PANE, text_area);
        let first = self.scroll;
        crate::selection::update(PANEL_PANE, |r| r.set_scroll(first));
        let visible: Vec<Line<'static>> = self
            .lines
            .iter()
            .skip(self.scroll)
            .take(text_area.height as usize)
            .cloned()
            .collect();
        frame.render_widget(Paragraph::new(visible), text_area);

        for (r, row) in rows.iter().take(button_rows as usize).enumerate() {
            let y = text_area.bottom() + r as u16;
            let mut x = inner.x;
            for (id, label, action) in row {
                let w = (width(label) as u16).min(inner.right().saturating_sub(x));
                let rect = Rect::new(x, y, w, 1);
                let style = if hits.is_focused(&ElementId(id.clone())) {
                    theme.focused()
                } else {
                    theme.fg(Token::Link)
                };
                frame.render_widget(Paragraph::new(Span::styled(label.clone(), style)), rect);
                hits.register_button(id.clone(), rect, action.clone());
                x = x.saturating_add(w + 1);
            }
        }
        if self.lines.len() > self.view_height {
            let pos = format!(
                " {}-{}/{} ",
                self.scroll + 1,
                (self.scroll + self.view_height).min(self.lines.len()),
                self.lines.len()
            );
            let w = width(&pos) as u16;
            if area.width > w + 2 {
                frame.render_widget(
                    Paragraph::new(Span::styled(pos, theme.fg(Token::Muted))),
                    Rect::new(area.right() - w - 1, area.bottom() - 1, w, 1),
                );
            }
        }
        self.interaction.end_frame(hits);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interaction::testing::{buffer_text, click, find_text, key, wheel_down};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    struct Ui {
        panel: Panel,
        term: Terminal<TestBackend>,
    }

    impl Ui {
        fn new(lines: usize) -> Ui {
            let text: Vec<String> = (0..lines).map(|i| format!("line {i}")).collect();
            let panel = Panel::new("Hosts", &text, Theme::default())
                .with_buttons(vec![PanelButton::new("test.gpu", "test gpu")]);
            let mut ui = Ui {
                panel,
                term: Terminal::new(TestBackend::new(40, 10)).unwrap(),
            };
            ui.draw();
            ui
        }

        fn draw(&mut self) {
            let p = &mut self.panel;
            self.term.draw(|f| p.render(f, f.area())).unwrap();
        }

        fn send(&mut self, ev: Event) -> Option<PanelOutput> {
            let o = self.panel.handle(&ev);
            self.draw();
            o
        }

        fn text(&self) -> String {
            buffer_text(self.term.backend().buffer())
        }
    }

    #[test]
    fn buttons_work_with_mouse_and_keyboard() {
        let mut ui = Ui::new(3);
        assert!(ui.text().contains("Hosts"));
        let (x, y) = find_text(ui.term.backend().buffer(), "[ test gpu ]").unwrap();
        assert_eq!(
            ui.send(click(x + 2, y)),
            Some(PanelOutput::Action("test.gpu".into()))
        );
        // Keyboard: Tab to the next button (close), Enter closes.
        assert_eq!(ui.send(key("tab")), None);
        assert_eq!(ui.panel.focused(), Some(&ElementId::from(CLOSE_ID)));
        assert_eq!(ui.send(key("enter")), Some(PanelOutput::Close));
        // Shift+Tab back to the test button.
        ui.send(key("shift+tab"));
        assert_eq!(
            ui.send(key("enter")),
            Some(PanelOutput::Action("test.gpu".into()))
        );
        assert_eq!(ui.send(key("esc")), Some(PanelOutput::Close));
        let (x, y) = find_text(ui.term.backend().buffer(), "[ close ]").unwrap();
        assert_eq!(ui.send(click(x, y)), Some(PanelOutput::Close));
    }

    #[test]
    fn long_text_scrolls_with_keys_and_wheel() {
        let mut ui = Ui::new(30);
        assert!(ui.text().contains("line 0"));
        ui.send(key("down"));
        assert_eq!(ui.panel.scroll_offset(), 1);
        ui.send(key("pgdn"));
        assert!(ui.panel.scroll_offset() > 1);
        ui.send(key("end"));
        assert!(ui.text().contains("line 29"), "{}", ui.text());
        ui.send(key("home"));
        assert_eq!(ui.panel.scroll_offset(), 0);
        ui.send(wheel_down(3, 3));
        assert_eq!(ui.panel.scroll_offset(), 3);
        assert!(
            ui.text().contains("/30"),
            "position indicator: {}",
            ui.text()
        );
    }
}
