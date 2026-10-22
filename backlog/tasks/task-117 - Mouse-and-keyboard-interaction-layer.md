---
id: TASK-117
title: Mouse and keyboard interaction layer
status: Done
assignee:
  - '@tui-foundation'
created_date: '2026-10-22 14:39'
updated_date: '2026-10-22 15:31'
labels:
  - tui
  - interaction
milestone: m-3
dependencies:
  - TASK-32
  - TASK-33
documentation:
  - backlog/docs/doc-5 - Agent-Map-and-Run-Visibility.md
  - backlog/docs/doc-2 - harness.ini-Reference.md
priority: high
type: feature
ordinal: 117000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Everything visible in the TUI must be clickable and usable from the keyboard. Doing this per component leads to inconsistent behaviour, so one hit-test and focus layer is needed that every view registers its interactive regions with.

## Loop guard (for agents)

Limit how many times you test and fix. If the same test, build or check still fails after **3 fix attempts**, or you have made no measurable progress in about 30 minutes, **stop**. Do not keep retrying, and never delete, skip, ignore or weaken a test or check to get a green run. Record what you tried, the exact failing output and your best hypothesis in the implementation notes, leave the task In Progress, and ask the user how to proceed.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 Components register interactive regions with an action, and clicks dispatch to the topmost region under the cursor
- [x] #2 A focus model lets arrows and Tab move focus between registered elements, Enter activates and Esc backs out
- [x] #3 Mouse wheel scrolls the pane under the cursor, PgUp and PgDn scroll the focused pane
- [x] #4 ui mouse = false disables mouse capture so native terminal text selection works, and the palette can toggle it at runtime
- [x] #5 Test helpers simulate clicks and key sequences against rendered frames
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
1. interaction::HitMap: per-frame region registry with layers, topmost hit wins, modal layers block lower ones.
2. Focus model by stable element id: Tab/BackTab/arrows move, Enter activates, Esc -> Back.
3. Scroll panes: wheel scrolls pane under cursor, PgUp/PgDn the focused pane.
4. Mouse capture toggle through a TerminalControl trait; AppCommand::ToggleMouse for the palette; ui.mouse=false starts without capture.
5. interaction::testing helpers (click, scroll, key sequences, find text in rendered buffer).
6. Tests + docs.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Implemented crates/harness-tui/src/interaction.rs: ElementId, PaneId, Region, HitMap<A> (register, register_pane, push_layer, push_modal, hit = topmost by layer then registration order, pane_at, focusable, is_focused), Interaction<A> (begin_frame/end_frame per render, focus by stable id, handle_key: Tab/Down/Right next, Shift+Tab/Up/Left prev, Enter activate, Esc Back, PgUp/PgDn page the focused pane; handle_mouse: left click activates topmost region, click outside a modal -> Back, wheel scrolls pane under cursor by WHEEL_LINES), Dispatch<A>, Scrolls. Modal layers confine clicks and focus. Key precedence in the app: global keymap actions first; with an overlay open or a non-input element focused the focus layer gets keys first; in the input Tab is switch_view (documented). Mouse: [ui] mouse=false -> TerminalSession enters without capture and App ignores mouse events; AppCommand::ToggleMouse / SetMouse(bool) toggles at runtime via Effect::SetMouseCapture (the palette, TASK-36, should send AppCommand::ToggleMouse). interaction::testing: click, wheel_up/down, key, keys, type_text, buffer_text, find_text, styled_snapshot. Tests in interaction.rs and app.rs.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Shared hit-test/focus/scroll layer with modal layers, mouse-capture toggle and test helpers for clicks and key sequences against rendered frames. Verified by cargo test/clippy -p harness-tui.
<!-- SECTION:FINAL_SUMMARY:END -->
