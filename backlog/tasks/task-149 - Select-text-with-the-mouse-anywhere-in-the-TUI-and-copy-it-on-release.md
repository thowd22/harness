---
id: TASK-149
title: Select text with the mouse anywhere in the TUI and copy it on release
status: Done
assignee:
  - '@claude'
created_date: '2026-10-23 15:35'
updated_date: '2026-10-23 19:22'
labels:
  - tui
  - clipboard
dependencies: []
priority: high
ordinal: 153000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
The TUI captures the mouse (EnableMouseCapture in crates/harness-tui/src/app.rs) so buttons are clickable, which disables the terminal's native drag-to-select. Today the only way to copy is the copy buttons on tool cards (`⧉ copy output`) and the copy action; dragging over text does nothing. The user expects copy-on-select everywhere: press, drag over any text, release, and it is in the clipboard.

Implement in-app text selection: track mouse down/drag/up over rendered text, highlight the selection (theme token, works in both themes), and on release copy the selected text through the existing clipboard backend (crates/harness-tui/src/clipboard.rs, OSC 52 by default, `[ui] clipboard`) with a short toast. It must work in every view that shows text: chat transcript (including scrolled content and wrapped lines, copying the logical text without wrap breaks or UI chrome like borders and gutters), tool cards and their output, agent map and node detail, diff view, panels/modals (/mcp, /model, sessions), and the input box. Drag past the top/bottom edge auto-scrolls. Clicks without drag still activate buttons. Pasting (bracketed paste, middle-click where the terminal supports it) keeps working into the input.
## Loop guard (for agents)

Limit how many times you test and fix. If the same test, build or check still fails after **3 fix attempts**, or you have made no measurable progress in about 30 minutes, **stop**. Do not keep retrying, and never delete, skip, ignore or weaken a test or check to get a green run. Record what you tried, the exact failing output and your best hypothesis in the implementation notes, leave the task In Progress, and ask the user how to proceed.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 Press-drag-release over text in any view (chat, tool cards, map/node detail, diff, panels and modals, input) highlights the selection and copies exactly that text to the clipboard on release, with a toast
- [x] #2 Copied text is the logical content: soft-wrapped lines are rejoined, and borders, gutters, icons and button labels are not included
- [x] #3 Dragging past the view edge scrolls and extends the selection; a click without drag still activates buttons and focus as before
- [x] #4 Double-click selects a word and triple-click a line; Esc or a click elsewhere clears the selection
- [x] #5 Keyboard path: a selection mode (palette entry and keybind) moves a cursor and extends the selection with Shift+arrows, Enter copies
- [x] #6 Paste into the input keeps working (bracketed paste); docs/tui.md and the clipboard docs describe copy-on-select
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [x] #1 Tests: automated tests cover every acceptance criterion (unit and/or integration as appropriate) and pass locally and in CI; no existing test is deleted, skipped, ignored or weakened to get a green run
- [x] #2 Docs: user-facing behaviour is documented (docs/ and, if the design changed, the relevant backlog doc), public APIs have rustdoc, and any new config keys appear in the harness.ini reference
- [x] #3 Loop guard respected: no more than 3 fix attempts per failing test or check; if still failing, stop, record attempts, output and hypothesis in implementation notes, and ask the user instead of looping
- [x] #4 UI interaction: if this task makes anything visible in the TUI, every visible interactive element is clickable with the mouse and fully operable with the keyboard (arrows/Tab to focus, Enter to activate, Esc to back out), with tests for both input paths; tasks with no visible UI mark this N/A
<!-- DOD:END -->

## Implementation Plan

<!-- SECTION:PLAN:BEGIN -->
1. New crate::selection module: per-frame text-region registry (thread-local collector filled during App::render; HitMap::register_pane auto-registers every scrollable pane, views refine it with scroll offset / soft-wrap rows / chrome mode; HitMap::register_button and selection::mask exclude buttons and overlays) and a Selector that maps screen cells back to logical text (row ordinals survive scrolling, row cache while selecting, soft-wrap rejoin, chrome stripping for borders/gutters/markers/icons/buttons).
2. Soft-wrap marker: wrap_line tags continuation lines with a zero-width marker span (space-consumed vs direct join) so transcript/markdown/detail rows know how to rejoin without parallel vectors.
3. App integration: press passes through as a click; drag starts a selection (swallowed); release copies via Effect::Copy (existing clipboard + toast); double/triple click = word/line (injectable clock); Esc/click clears; drag past a scrollable region edge synthesizes wheel scrolls (on drag and on ticks); highlight painted with a new theme token `selection`.
4. Keyboard: generic selection mode (select key / "Select text" palette entry on the map and screens): arrows move a cell cursor, Shift+arrows extend, Enter/y copies, Esc leaves, footer bar with clickable [copy]/[done]. The chat keeps its line selection mode, gaining Shift+Up/Down to extend.
5. Views: chat transcript (ordinals, soft rows, copy buttons masked), input box, panel, diff, detail log, inspector, map, pickers via panes; toasts/palette/help/pickers masked as overlays. Card bodies no longer toggle on press (the header toggles) so dragging over output works.
6. Tests (TestBackend mouse sequences per view, wrap rejoin, chrome exclusion, click-vs-drag, autoscroll, word/line, keyboard path, exact clipboard text via OSC 52 decode, paste) and docs (docs/tui.md, clipboard docs, doc-2/doc-7 token and key, CHANGELOG).

7. Review fixes: toasts register as transient overlays (selection::overlay snapshots the text under them: copied, not highlighted, no selection starts on them); the Selector keeps the last drawn buffer so drags, releases and double/triple clicks work without a frame between events; card bodies and /model, /resume picker rows act on the release when no drag happened.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Design: crates/harness-tui/src/selection.rs. Views register TextRegions per frame through a thread-local collector (begin_frame/end_frame in App::render); HitMap::register_pane auto-registers every pane, HitMap::push_modal registers the modal as a region (Drawing chrome) that covers what is below, HitMap::register_button masks button labels, toasts mask themselves. Views refine their pane with selection::update: chat transcript (row ordinals = (entry<<32)|line via Transcript::visible_ords, soft joins, Smart chrome), detail log (scroll + joins + Smart), diff (text rect without margin, scroll, Smart), panel/inspector/detail sidebar (scroll), map (pan + Drawing). The input registers its text without the prompt with Space/Direct joins. App adds a fallback region "screen" (main area) and "goalbar".
Soft wraps: wrap_line starts continuation lines with a zero-width marker span (HIDDEN = space consumed, HIDDEN|RAPID_BLINK = word split); chat::text::soft_join reads it. Zero width, so buffers/snapshots are unchanged.
Click vs drag: the press is passed on unchanged (click semantics as before); only Drag/Up after a press on a region are consumed. Card bodies (below the header row of a multi-row card / subagent hit) now focus instead of toggling on press so a drag over output does not collapse the card.
Autoscroll: drags past a scrolling region's edge (or on its edge row at the screen edge) synthesize wheel notches through the normal routing, on the drag and on every tick while held (App::needs_tick). Rows seen while selecting are cached by ordinal, so text spans scrolled-out lines.
Keyboard: App-level cursor mode on the map and screens (select key, palette Select text when the chat is covered, ChatView::set_covered); chat keeps its line mode with Shift+Up/Down extending. Footer SELECT bar with [copy]/[done].
Theme: new token selection = accent at 35% over bg (Ansi16 blue, reversed without colour), Theme::selection().

Validation: cargo nextest run -p harness-tui (396 passed, incl. 13 selection unit tests and 12 App-level selection tests: chat wrap rejoin + highlight token + toast, chrome exclusion, click-vs-drag, card header vs body, double/triple click with injected clock, chat autoscroll on ticks, input wrap + bracketed paste, map + node detail, diff gutter, panel autoscroll, /resume picker by mouse (title) and keyboard (row), keyboard mode with [copy]/[done] and palette entry, copy key re-copy; clipboard text checked by decoding the OSC 52 sequence the app writes); cargo clippy -p harness-tui --all-targets -D warnings clean; rustfmt clean; cargo nextest run -p harness and -p harness-config pass; defaults_changelog passes (no default changed). The help overlay's select description changed to 'select text with the keyboard' (same width; the three app snapshots updated for that line only). Known limit: a press on a /model or /resume picker row still picks it (existing click-on-press tests), so picker rows are copied with the select key; other picker text is mouse-selectable.

Review fixes (5 findings, all confirmed real and fixed):
1. Toast truncation: notify.rs now calls selection::overlay(rect, buf) before drawing a toast instead of mask. FrameText keeps the symbols drawn under the overlay; capture uses them (Cover::Overlay), so text under a toast is copied in full, also for rows cached while autoscrolling. A press on a toast starts no selection (region_at None) and the highlight skips covered cells. Opaque masks (buttons, palette, modals) are unchanged.
2. Frame timing: Selector stores a clone of the last drawn buffer (end_frame). Drag start, release and double/triple click capture from it immediately, so pending_click is gone and a press/drag/release batch copies on the release. The real-binary resume_pty test (press+release in one write) also passes through this path.
3+5. Pickers: App::handle_mouse holds a press on a /model or /resume picker element (picker_row_at) and delivers it with the release when the selection layer passed the release (no drag). A drag over a row copies its text; nothing is switched or resumed. Presses outside the picker still close it on press. Note: a double click on a row is a word selection (AC4), not a pick.
4. Card bodies: ChatView remembers a press on a card body (focus + action) and runs ToggleCard/OpenNode on a release over the same card; the app keeps the release of a drag, so a drag over output does not toggle. The expand/collapse hint row is in the body, so clicking it toggles again.
Tests: new app tests text_under_a_toast_is_copied_and_not_highlighted, a_gesture_delivered_without_frames_in_between_still_copies (drag + double + triple click without draws), dragging_over_picker_rows_copies_them_and_a_click_still_picks (/model and /resume); unit test a_transient_overlay_keeps_the_text_under_it_and_blocks_presses; subagent_cards a_click_on_the_card_body_opens_the_subagent_on_release_and_a_drag_does_not. a_click_without_drag_still_activates_buttons_and_cards previously asserted the reviewed-as-wrong behaviour (body click does not toggle); it now asserts the AC3 behaviour (press focuses, release toggles, the hint toggles, a drag over the body copies and does not toggle). app/tests.rs click_text now sends press+release (a real click), no assertion changed.
Docs: docs/tui.md (cards, picker rows, toasts), docs/tui-views.md, CHANGELOG entry amended. No defaults changed (defaults_changelog passes).
Validation: cargo nextest run -p harness-tui 405 passed; cargo nextest run -p harness 152 passed (incl. resume_pty); clippy -p harness-tui --all-targets -D warnings clean; rustfmt --check clean on touched files.

Integration check (both tasks together, 2026-10-23): cargo fmt --all --check clean; cargo clippy --workspace --all-targets --locked -D warnings clean; cargo nextest run --workspace --locked --profile ci 1812 passed, 10 skipped; cargo test --workspace --locked --doc ok; cargo deny check ok; scripts/hermetic-guard.sh nextest run -p harness -p harness-eval 213 passed, nothing written to HOME/XDG. No conflicts found between TASK-149 and TASK-150 in the TUI app. DoD #1 'in CI' is pending until the orchestrator pushes; nothing committed. Also updated README.md (Keys note: drag to select, copied on release).

Orchestrator fixes after the real-TUI verification (wf_292e042e-b32):
- Node detail did not rejoin soft-wrapped text: detail.rs put the indent span before the zero-width wrap marker, so soft_join saw a hard line. New prefixed() keeps the marker first. Test node_detail_rejoins_soft_wrapped_text (fails without the fix).
- The copy toast was hidden under the /model and /resume pickers and the login modal. Over a modal the newest toast is now drawn on top at the bottom right (over the modal's help line, not its rows). dragging_over_picker_rows_copies_them_and_a_click_still_picks asserts the toast is visible (fails without the fix).
- Map copies included a leading status icon ('✔ Coder'). Chrome::Drawing now blanks an icon that leads a label (first visible cell after a border or row start, followed by a blank); icons inside text are kept. Unit test drawing_chrome_drops_status_icons_in_front_of_labels_only.
- pty test helpers (resume_pty, tui_pty) now kill their harness child on Drop; failed pty tests had left TUI processes orphaned for hours.

Committed in 58b1512; CI green on Ubuntu and macOS: https://github.com/thowd22/harness/actions/runs/40009831788.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Added a selection layer (crates/harness-tui/src/selection.rs) that works in every view. Views register the text they draw on each frame: panes and modals automatically, buttons as masks, toasts as transient overlays. Chat, input, detail, diff, panel, inspector and map add scroll ordinals, soft-wrap joins and chrome rules. The app maps drags back to logical text. A drag copies on release through the existing clipboard with a toast and is highlighted with a new selection theme token. Soft wraps rejoin; borders, gutters, icons and button labels are left out; text under a toast is still copied. Dragging past an edge autoscrolls (also on ticks). Double/triple click selects a word/line. Esc or a click clears. Mouse gestures read the last drawn frame, so events that arrive in one batch copy the same text. A press without movement is still a click. Card bodies (including the expand hint) and /model, /resume picker rows act on the release, so a drag over them selects text and does not toggle, pick or resume. Keyboard: the select key / Select text palette entry gives a cursor mode on the map, screens and pickers (Shift extends, Enter copies, footer [copy]/[done]); the chat line mode gained Shift+Up/Down. Docs: docs/tui.md, docs/tui-views.md, guide, doc-2, doc-7, CHANGELOG. Verified with cargo nextest -p harness-tui (405 pass, including tests for toasts, batched events, picker drags and card-body clicks), -p harness (152 pass, including the resume PTY click), clippy and rustfmt.
<!-- SECTION:FINAL_SUMMARY:END -->
