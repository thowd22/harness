//! `/diff`: review every changed file of the session or of one node.
//!
//! [`DiffView`] shows the net changes from the change ledger
//! (`harness_hygiene::ledger`): a file list on the left with the change
//! kind, `+N −M` counts, change sources and the hygiene classifications
//! inline (`out of scope`, `temp/debug`, `whitespace-only`,
//! `new untracked`), and the selected file's unified diff on the right with
//! syntax colours. Each file has a `[revert]` button that restores it
//! from a checkpoint through a [`FileReverter`]
//! ([`CheckpointReverter`] wraps `harness_hygiene::report::revert_files`).
//!
//! Mouse: click a file to show its diff, click `[revert]` to revert it.
//! Keyboard: Tab / arrows move focus over files and buttons, Enter
//! activates, `r` reverts the selected file, `y` (or the `copy` key, or the
//! `[⧉ copy diff]` button) copies the selected file's unified diff,
//! PgUp/PgDn scroll the diff, Esc backs out.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Arc;

use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers, MouseEventKind};
use harness_events::schema::{FileChangeKind, NodeId, RunEvent};
use harness_hygiene::checkpoints::CheckpointStore;
use harness_hygiene::ledger::{Ledger, NetChange};
use harness_hygiene::report::{Category, HygieneReport};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};

use crate::cards::{DiffKind, diff_lines, parse_unified};
use crate::chat::highlight::lang_for;
use crate::chat::text::{truncate, truncate_line};
use crate::interaction::{Dispatch, ElementId, HitMap, Interaction};
use crate::theme::{Theme, Token};

/// Restores files to their state before the changes.
pub trait FileReverter: Send + Sync {
    /// Reverts `paths` (workspace-relative). The error is shown to the user.
    fn revert(&self, paths: &[&str]) -> Result<(), String>;
}

/// Reverts from a checkpoint (`harness_hygiene::report::revert_files`).
pub struct CheckpointReverter {
    store: Arc<CheckpointStore>,
    checkpoint: u64,
}

impl CheckpointReverter {
    /// Reverts to checkpoint `checkpoint` of `store` (e.g. the node's first
    /// turn checkpoint).
    pub fn new(store: Arc<CheckpointStore>, checkpoint: u64) -> Self {
        CheckpointReverter { store, checkpoint }
    }
}

impl FileReverter for CheckpointReverter {
    fn revert(&self, paths: &[&str]) -> Result<(), String> {
        harness_hygiene::report::revert_files(&self.store, self.checkpoint, paths)
            .map_err(|e| e.to_string())
    }
}

/// One changed file.
#[derive(Debug, Clone, PartialEq)]
pub struct DiffFile {
    /// Workspace-relative path.
    pub path: String,
    /// Net change.
    pub kind: FileChangeKind,
    /// Change sources (`edit`, `bash`, ...).
    pub sources: Vec<String>,
    /// Unified diff text.
    pub diff: String,
    /// Added lines.
    pub added: usize,
    /// Removed lines.
    pub removed: usize,
    /// Hygiene classifications.
    pub categories: BTreeSet<Category>,
    /// Reverted in this view.
    pub reverted: bool,
}

impl DiffFile {
    fn from_change(c: &NetChange, report: Option<&HygieneReport>) -> DiffFile {
        let diff = c.diffs.join("\n");
        let rows = parse_unified(&diff);
        DiffFile {
            path: c.path.clone(),
            kind: c.kind,
            sources: c.sources.iter().map(|s| s.label().to_owned()).collect(),
            added: rows.iter().filter(|r| r.kind == DiffKind::Add).count(),
            removed: rows.iter().filter(|r| r.kind == DiffKind::Del).count(),
            diff,
            categories: report
                .and_then(|r| r.files.iter().find(|f| f.path == c.path))
                .map(|f| f.categories.clone())
                .unwrap_or_default(),
            reverted: false,
        }
    }

    /// Classifications worth flagging (everything except `in scope`).
    pub fn flags(&self) -> Vec<&'static str> {
        self.categories
            .iter()
            .filter(|c| **c != Category::InScope)
            .map(|c| c.label())
            .collect()
    }
}

/// Actions of the diff view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffAction {
    /// Show file `i`.
    Select(usize),
    /// Revert file `i`.
    Revert(usize),
    /// Copy the selected file's diff.
    Copy,
}

/// What the diff view asks the app to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiffOutput {
    /// A file was reverted.
    Reverted(String),
    /// Reverting failed.
    RevertFailed {
        /// File.
        path: String,
        /// Why.
        error: String,
    },
    /// Esc.
    Back,
    /// Copy this diff to the clipboard.
    Copy {
        /// File.
        path: String,
        /// Unified diff.
        diff: String,
    },
}

/// Element id of the copy button.
pub const COPY_ID: &str = "diff.copy";
/// Label of the copy button.
pub const COPY_LABEL: &str = "[⧉ copy diff]";

/// Element id of a file row.
pub fn file_id(i: usize) -> String {
    format!("diff.file.{i}")
}

/// Element id of a file's revert button.
pub fn revert_id(i: usize) -> String {
    format!("diff.revert.{i}")
}

/// The diff review view.
pub struct DiffView {
    title: String,
    files: Vec<DiffFile>,
    selected: usize,
    scroll: usize,
    height: usize,
    theme: Theme,
    reverter: Option<Box<dyn FileReverter>>,
    message: Option<(String, bool)>,
    interaction: Interaction<DiffAction>,
}

impl DiffView {
    /// A view of `changes` (from `Ledger::net_changes`), classified by
    /// `report` when given.
    pub fn new(
        title: impl Into<String>,
        changes: &[NetChange],
        report: Option<&HygieneReport>,
        theme: Theme,
    ) -> Self {
        DiffView {
            title: title.into(),
            files: changes
                .iter()
                .map(|c| DiffFile::from_change(c, report))
                .collect(),
            selected: 0,
            scroll: 0,
            height: 0,
            theme,
            reverter: None,
            message: None,
            interaction: Interaction::new(),
        }
    }

    /// The changes of the session (`node = None`) or one node, rebuilt
    /// from a run's `file_changed` events.
    pub fn from_events<'a>(
        root: impl Into<PathBuf>,
        events: impl IntoIterator<Item = &'a RunEvent>,
        node: Option<&NodeId>,
        report: Option<&HygieneReport>,
        theme: Theme,
    ) -> Self {
        let ledger = Ledger::from_events(root, events);
        let title = match node {
            Some(n) => format!("Changes of {n}"),
            None => "Session changes".to_owned(),
        };
        DiffView::new(title, &ledger.net_changes(node), report, theme)
    }

    /// Enables per-file revert.
    pub fn with_reverter(mut self, r: Box<dyn FileReverter>) -> Self {
        self.reverter = Some(r);
        self
    }

    /// The files.
    pub fn files(&self) -> &[DiffFile] {
        &self.files
    }

    /// The selected file index.
    pub fn selected(&self) -> usize {
        self.selected
    }

    fn activate(&mut self, a: DiffAction) -> Option<DiffOutput> {
        match a {
            DiffAction::Select(i) => {
                self.selected = i.min(self.files.len().saturating_sub(1));
                self.scroll = 0;
                None
            }
            DiffAction::Revert(i) => self.revert(i),
            DiffAction::Copy => self.copy(),
        }
    }

    /// Copies the selected file's diff (`None` when there is none).
    pub fn copy(&self) -> Option<DiffOutput> {
        let f = self.files.get(self.selected)?;
        (!f.diff.trim().is_empty()).then(|| DiffOutput::Copy {
            path: f.path.clone(),
            diff: f.diff.clone(),
        })
    }

    fn revert(&mut self, i: usize) -> Option<DiffOutput> {
        let f = self.files.get(i)?;
        if f.reverted {
            return None;
        }
        let path = f.path.clone();
        let res = match &self.reverter {
            Some(r) => r.revert(&[path.as_str()]),
            None => Err("no checkpoint to revert to".into()),
        };
        Some(match res {
            Ok(()) => {
                self.files[i].reverted = true;
                self.message = Some((format!("reverted {path}"), false));
                DiffOutput::Reverted(path)
            }
            Err(error) => {
                self.message = Some((format!("revert {path} failed: {error}"), true));
                DiffOutput::RevertFailed { path, error }
            }
        })
    }

    /// Handles input.
    pub fn handle(&mut self, ev: &Event) -> Option<DiffOutput> {
        let d = match ev {
            Event::Key(k) if k.kind != KeyEventKind::Release => {
                let plain = k.modifiers.difference(KeyModifiers::SHIFT).is_empty();
                match k.code {
                    KeyCode::Char('r') if plain => return self.revert(self.selected),
                    KeyCode::Char('y') if plain => return self.copy(),
                    KeyCode::PageDown => {
                        self.scroll += self.height.max(1);
                        return None;
                    }
                    KeyCode::PageUp => {
                        self.scroll = self.scroll.saturating_sub(self.height.max(1));
                        return None;
                    }
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
            Dispatch::Activate(a) => self.activate(a),
            Dispatch::Back => Some(DiffOutput::Back),
            Dispatch::Focus(id) => {
                // Focusing a file row shows it.
                if let Some(i) = id.0.strip_prefix("diff.file.").and_then(|n| n.parse().ok()) {
                    self.activate(DiffAction::Select(i));
                }
                None
            }
            _ => None,
        }
    }

    /// Renders the view.
    pub fn render(&mut self, frame: &mut Frame<'_>, area: Rect) {
        let t = self.theme.clone();
        let mut hits: HitMap<DiffAction> = self.interaction.begin_frame();
        let [head, body] =
            Layout::vertical([Constraint::Length(1), Constraint::Min(1)]).areas(area);
        let (added, removed) = self
            .files
            .iter()
            .fold((0, 0), |(a, r), f| (a + f.added, r + f.removed));
        let mut hs = vec![
            Span::styled(
                format!(" {} ", self.title),
                t.fg(Token::Fg).add_modifier(Modifier::BOLD),
            ),
            Span::styled(format!("{} files ", self.files.len()), t.fg(Token::Muted)),
            Span::styled(format!("+{added} "), t.fg(Token::DiffAdded)),
            Span::styled(format!("−{removed}"), t.fg(Token::DiffRemoved)),
        ];
        if let Some((m, err)) = &self.message {
            hs.push(Span::styled(
                format!("  {m}"),
                t.fg(if *err { Token::Error } else { Token::Ok }),
            ));
        }
        let bw = crate::chat::text::width(COPY_LABEL) as u16;
        let (head, button) = if head.width > bw + 20 {
            let [h, b] =
                Layout::horizontal([Constraint::Min(1), Constraint::Length(bw)]).areas(head);
            (h, Some(b))
        } else {
            (head, None)
        };
        frame.render_widget(Paragraph::new(Line::from(hs)), head);
        let list_w = (body.width / 3).clamp(24, 48).min(body.width);
        let [list, diff] =
            Layout::horizontal([Constraint::Length(list_w), Constraint::Min(1)]).areas(body);
        let block = Block::new()
            .borders(Borders::RIGHT)
            .border_style(t.fg(Token::Border));
        let inner = block.inner(list);
        frame.render_widget(block, list);
        let mut y = inner.y;
        let mut lines = Vec::new();
        for (i, f) in self.files.iter().enumerate() {
            if y + 1 >= inner.y + inner.height {
                break;
            }
            let fid = file_id(i);
            let focused = hits.is_focused(&ElementId::from(fid.as_str()));
            let sel = i == self.selected;
            let style = if focused || sel {
                t.focused()
            } else {
                Style::default()
            };
            let (sign, tok) = match f.kind {
                FileChangeKind::Created => ('+', Token::DiffAdded),
                FileChangeKind::Modified => ('~', Token::Warn),
                FileChangeKind::Deleted => ('-', Token::DiffRemoved),
            };
            let path_style = if f.reverted {
                t.fg(Token::Muted).add_modifier(Modifier::CROSSED_OUT)
            } else {
                t.fg(Token::Path)
            };
            lines.push(
                truncate_line(
                    Line::from(vec![
                        Span::styled(format!("{sign} "), t.fg(tok)),
                        Span::styled(f.path.clone(), path_style),
                        Span::styled(format!(" +{}", f.added), t.fg(Token::DiffAdded)),
                        Span::styled(format!(" −{}", f.removed), t.fg(Token::DiffRemoved)),
                    ]),
                    usize::from(inner.width),
                )
                .patch_style(style),
            );
            hits.register(
                fid,
                Rect::new(inner.x, y, inner.width, 1),
                DiffAction::Select(i),
            );
            // Second row: sources, flags and the revert button.
            let rid = revert_id(i);
            let label = if f.reverted { "reverted" } else { "[revert]" };
            let mut spans = vec![Span::styled(
                format!("  {}", f.sources.join(", ")),
                t.fg(Token::Muted),
            )];
            for flag in f.flags() {
                spans.push(Span::styled(format!(" ⚑ {flag}"), t.fg(Token::Warn)));
            }
            let used: usize = spans
                .iter()
                .map(|s| crate::chat::text::width(&s.content))
                .sum();
            let room = usize::from(inner.width).saturating_sub(10);
            let mut row = truncate_line(Line::from(spans), room);
            let pad = room.saturating_sub(used.min(room)) + 1;
            row.spans.push(Span::raw(" ".repeat(pad)));
            let bstyle = if hits.is_focused(&ElementId::from(rid.as_str())) {
                t.focused()
            } else {
                t.fg(Token::Error)
            };
            let bx = inner.x + (room + 1) as u16;
            row.spans.push(Span::styled(label, bstyle));
            lines.push(row);
            if !f.reverted && bx < inner.x + inner.width {
                hits.register(
                    rid,
                    Rect::new(bx, y + 1, 8.min(inner.x + inner.width - bx), 1),
                    DiffAction::Revert(i),
                );
            }
            y += 2;
        }
        if self.files.is_empty() {
            lines.push(Line::from(Span::styled("no changes", t.fg(Token::Muted))));
        }
        frame.render_widget(Paragraph::new(lines), inner);
        if let Some(b) = button.filter(|_| !self.files.is_empty()) {
            let id = ElementId::from(COPY_ID);
            let mut st = t.fg(Token::Accent);
            if hits.is_focused(&id) {
                st = st.patch(t.focused());
            }
            frame.render_widget(Paragraph::new(Span::styled(COPY_LABEL, st)), b);
            hits.register(id, b, DiffAction::Copy);
        }
        // Diff of the selected file.
        hits.register_pane("diff", diff);
        self.height = usize::from(diff.height);
        let mut dl = Vec::new();
        if let Some(f) = self.files.get(self.selected) {
            let rows = parse_unified(&f.diff);
            let lang = lang_for(&f.path);
            if rows.is_empty() {
                dl.push(Line::from(Span::styled(
                    truncate("no diff recorded for this file", usize::from(diff.width)),
                    t.fg(Token::Muted),
                )));
            }
            for l in diff_lines(&rows, lang, &t) {
                dl.push(truncate_line(l, usize::from(diff.width.saturating_sub(1))));
            }
        }
        self.scroll = self.scroll.min(dl.len().saturating_sub(self.height));
        let shown: Vec<Line<'static>> =
            dl.into_iter().skip(self.scroll).take(self.height).collect();
        frame.render_widget(
            Paragraph::new(shown),
            Rect::new(
                diff.x + 1,
                diff.y,
                diff.width.saturating_sub(1),
                diff.height,
            ),
        );
        self.interaction.end_frame(hits);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::demo::events::{EventBuilder, NodeSpec};
    use crate::interaction::testing::{buffer_text, click, find_text, key};
    use harness_hygiene::checkpoints::CheckpointKind;
    use harness_hygiene::report::ReportInput;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use std::sync::Mutex;

    const DIFF: &str = "--- a/src/parse.rs\n+++ b/src/parse.rs\n@@ -1,2 +1,2 @@\n fn parse() {\n-    a.unwrap();\n+    a?;\n";

    fn events() -> Vec<RunEvent> {
        let mut b = EventBuilder::new();
        b.start(NodeSpec::step("fix"));
        b.start(NodeSpec::step("other"));
        b.file_changed("fix", "src/parse.rs", FileChangeKind::Modified, Some(DIFF));
        b.file_changed(
            "fix",
            "debug.log",
            FileChangeKind::Created,
            Some("--- /dev/null\n+++ b/debug.log\n@@ -0,0 +1 @@\n+x\n"),
        );
        b.file_changed("other", "README.md", FileChangeKind::Modified, None);
        b.into_events()
    }

    #[derive(Default)]
    struct Recording(Mutex<Vec<String>>);

    impl FileReverter for Arc<Recording> {
        fn revert(&self, paths: &[&str]) -> Result<(), String> {
            self.0
                .lock()
                .unwrap()
                .extend(paths.iter().map(|p| (*p).to_owned()));
            Ok(())
        }
    }

    fn draw(v: &mut DiffView) -> Terminal<TestBackend> {
        let mut term = Terminal::new(TestBackend::new(110, 20)).unwrap();
        term.draw(|f| v.render(f, f.area())).unwrap();
        term
    }

    #[test]
    fn shows_changed_files_with_diffs_for_the_session_or_a_node() {
        let ev = events();
        let session = DiffView::from_events("/w", &ev, None, None, Theme::default());
        assert_eq!(session.files().len(), 3);
        let mut node = DiffView::from_events(
            "/w",
            &ev,
            Some(&NodeId::from("fix")),
            None,
            Theme::default(),
        );
        let paths: Vec<&str> = node.files().iter().map(|f| f.path.as_str()).collect();
        assert_eq!(paths, ["debug.log", "src/parse.rs"]);
        let term = draw(&mut node);
        let (x, y) = find_text(term.backend().buffer(), "src/parse.rs").unwrap();
        node.handle(&click(x, y));
        let text = buffer_text(draw(&mut node).backend().buffer());
        assert!(text.contains("Changes of fix"), "{text}");
        assert!(text.contains("~ src/parse.rs +1 −1"), "{text}");
        assert!(text.contains("a?;"), "{text}");
        // Keyboard: focusing a file row shows its diff.
        node.handle(&key("shift+tab"));
        node.handle(&key("shift+tab"));
        assert_eq!(node.selected(), 0);
    }

    #[test]
    fn hygiene_classifications_are_shown_inline() {
        let ev = events();
        let ledger = Ledger::from_events("/w", &ev);
        let changes = ledger.net_changes(Some(&NodeId::from("fix")));
        let report = HygieneReport::build(ReportInput {
            root: std::path::Path::new("/w"),
            node_id: None,
            changes: &changes,
            scope: None,
            tracked: None,
            before: None,
        });
        let mut v = DiffView::new("Changes", &changes, Some(&report), Theme::default());
        let text = buffer_text(draw(&mut v).backend().buffer());
        assert!(text.contains("⚑ temp/debug"), "{text}");
    }

    #[test]
    fn per_file_revert_by_click_and_keyboard() {
        let rec = Arc::new(Recording::default());
        let ev = events();
        let mut v = DiffView::from_events("/w", &ev, None, None, Theme::default())
            .with_reverter(Box::new(rec.clone()));
        let idx = |v: &DiffView, p: &str| v.files().iter().position(|f| f.path == p).unwrap();
        let term = draw(&mut v);
        // Click a file, then its revert button.
        let (x, y) = find_text(term.backend().buffer(), "debug.log").unwrap();
        v.handle(&click(x, y));
        let dbg = idx(&v, "debug.log");
        assert_eq!(v.selected(), dbg);
        draw(&mut v);
        let r = v
            .interaction
            .hits()
            .region(&ElementId::from(revert_id(dbg)))
            .unwrap()
            .rect;
        assert_eq!(
            v.handle(&click(r.x, r.y)),
            Some(DiffOutput::Reverted("debug.log".into()))
        );
        // Keyboard: r reverts the selected file.
        let parse = idx(&v, "src/parse.rs");
        v.handle(&click(0, 1 + 2 * parse as u16));
        draw(&mut v);
        assert_eq!(
            v.handle(&key("r")),
            Some(DiffOutput::Reverted("src/parse.rs".into()))
        );
        // Tab to README's revert button and press Enter.
        let readme = idx(&v, "README.md");
        draw(&mut v);
        for _ in 0..10 {
            if v.interaction
                .focused()
                .is_some_and(|f| f.0 == revert_id(readme))
            {
                break;
            }
            v.handle(&key("tab"));
        }
        assert_eq!(
            v.handle(&key("enter")),
            Some(DiffOutput::Reverted("README.md".into()))
        );
        assert_eq!(
            *rec.0.lock().unwrap(),
            ["debug.log", "src/parse.rs", "README.md"]
        );
        let text = buffer_text(draw(&mut v).backend().buffer());
        assert!(text.contains("reverted"), "{text}");
        assert_eq!(v.handle(&key("esc")), Some(DiffOutput::Back));
    }

    #[test]
    fn checkpoint_reverter_restores_the_file() {
        let work = tempfile::tempdir().unwrap();
        let cps = tempfile::tempdir().unwrap();
        std::fs::write(work.path().join("a.txt"), "before\n").unwrap();
        let store = Arc::new(CheckpointStore::open(cps.path(), work.path()).unwrap());
        let cp = store
            .snapshot(CheckpointKind::Manual, None, None, "start")
            .unwrap();
        std::fs::write(work.path().join("a.txt"), "after\n").unwrap();
        let r = CheckpointReverter::new(store, cp.id);
        r.revert(&["a.txt"]).unwrap();
        assert_eq!(
            std::fs::read_to_string(work.path().join("a.txt")).unwrap(),
            "before\n"
        );
    }
}
