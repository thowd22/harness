---
id: TASK-159
title: 'Copy-on-select polish: card header rule and panel-border presses'
status: Done
assignee:
  - '@claude'
created_date: '2026-10-23 19:43'
updated_date: '2026-10-24 00:41'
labels:
  - tui
  - clipboard
dependencies: []
priority: low
ordinal: 166000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Leftovers from TASK-149's real-TUI verification: (1) copying a tool card's header line keeps a border character, e.g. 'line-th…─ 7ms · 17:24:02' (the header rule is part of the card frame and must be chrome); (2) a press on a panel or modal border (e.g. the /mcp panel's left border at column 1) followed by a drag starts no selection, while a press on the chat card border does; it should start a selection at the nearest text cell, consistently across panels, pickers and cards. Keep button clicks, focus and existing selection behaviour unchanged.
## Loop guard (for agents)

Limit how many times you test and fix. If the same test, build or check still fails after **3 fix attempts**, or you have made no measurable progress in about 30 minutes, **stop**. Do not keep retrying, and never delete, skip, ignore or weaken a test or check to get a green run. Record what you tried, the exact failing output and your best hypothesis in the implementation notes, leave the task In Progress, and ask the user how to proceed.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 Copying across a tool card header yields the header text without frame characters
- [x] #2 A press on a panel, picker or card border followed by a drag selects from the nearest text cell, in every view
- [x] #3 Existing selection, click and paste tests stay green; new TestBackend tests cover both cases, and the real TUI is checked in tmux
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
1. Header rule: in selection smart_mask, on a card header row (╭ … ╮) blank the header's fill run of ─ even when shorter than 3 cells (truncated headers leave 1-2 cells); mirror in clipboard::plain_line for consistency.
2. Border presses: in Selector::mouse, when a press lands on a box-drawing cell of region R (modal/screen fallback) and a region registered on top of R and inside it lies next to the cell (same row for │ side borders, same column for ─ top/bottom borders, both for corners/junctions), the drag anchors at the nearest cell of that inner region; clicks, multi-clicks and presses inside a region are unchanged.
3. Tests: selection unit tests for both rules; app TestBackend tests for a tool card header with a truncated title, /mcp-style panel border, model picker border and the chat card border.
4. Verify in the real TUI via a private tmux server (SGR mouse, set-clipboard on, show-buffer).
5. Docs: user guide selection section + CHANGELOG fix entry.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Header rule: selection::smart_mask blanks the last run of ─ before a blank on a card header row (╭ … ╮), any width; clipboard::plain_line does the same for transcript copies. Border presses: FrameText::inside_border snaps a drag's anchor from a box-drawing cell of region R into a region registered on top of R and inside it (│ side borders: same row, up to 2 cells; ─ top/bottom: same column, 1 row; corners/junctions: both), only when that cell is visible; presses inside a region, clicks, multi-clicks, masks and toasts are unchanged (a press on a toast still starts nothing).
Tests: selection::tests smart_chrome_drops_a_card_header_rule_of_any_width, a_press_on_a_border_selects_from_the_nearest_text_cell_inside_it, a_press_on_text_or_a_border_inside_a_region_is_unchanged; app::selection_tests copying_a_truncated_tool_card_header_leaves_out_its_rule, a_press_on_a_panel_or_picker_border_selects_from_the_nearest_text (panel left/right border, model picker border without picking, chat card border); clipboard plain_line cases. All four new tests fail with the fix disabled. cargo nextest -p harness-tui: 417/417 at the time; clippy -p harness-tui clean.
Real TUI (private tmux -L server, set-clipboard on, TMUX unset so OSC 52 lands in a tmux buffer, hermetic HOME/XDG/HARNESS_CONFIG, --fake-script and ui-demo): truncated approval card header copied as 'bash  … commands need approval in this mode ·  · 19:55:36' (no ─), also from the ╭ corner; /mcp panel left border at column 1 -> 'No MCP servers. Add'; top border -> text in that column; /model picker left border on a row -> the row's model name, filter-row/title border still selects the modal's own text. UI interaction DoD: no new visible elements (behaviour of existing mouse selection only; keyboard selection unchanged).
Note: two layout_matrix tests currently fail from another engineer's in-progress layout_matrix module (created during this run), unrelated to this change. An early cargo fmt --all ran workspace-wide.

Integration validation (2026-10-23, tree including other engineers' in-progress TASK-160/161 work): cargo fmt --all --check, cargo clippy --workspace --all-targets --locked -D warnings, cargo nextest run --workspace --locked --profile ci x3 (1948/1948 passed each run, no flakes), cargo test --workspace --doc, cargo deny check, scripts/hermetic-guard.sh nextest run -p harness -p harness-eval (230 passed, nothing written to HOME/XDG), no .snap.new files, defaults_changelog passes (no defaults changed). CI part of DoD#1 pending until the orchestrator pushes.

The layout_matrix failures noted above were from TASK-160's in-progress work and pass in the integration run.

Committed in 63eb3bf; hosted CI green on Ubuntu and macOS: https://github.com/thowd22/harness/actions/runs/40045278391
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Copy-on-select polish. Tool card headers no longer leak a piece of the header rule when the title is truncated (smart chrome blanks the header's last ─ run of any width; plain_line mirrors it). A drag that starts on a panel, picker or modal border now anchors at the nearest text cell of the region that border frames (FrameText::inside_border, used by Selector::mouse on press), matching chat card borders; clicks, focus, multi-clicks and existing selection are unchanged. Verified with new unit and TestBackend tests (failing before the fix), the full harness-tui suite and clippy, and in the real TUI through a private tmux server (SGR mouse via send-keys -l, show-buffer). Docs: docs/tui.md selection section, CHANGELOG Fixed.
<!-- SECTION:FINAL_SUMMARY:END -->
