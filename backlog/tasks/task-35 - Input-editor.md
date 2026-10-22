---
id: TASK-35
title: Input editor
status: Done
assignee:
  - '@tui-views'
created_date: '2026-10-22 14:30'
updated_date: '2026-10-22 16:18'
labels:
  - tui
milestone: m-3
dependencies:
  - TASK-32
priority: high
type: feature
ordinal: 35000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Typing prompts and slash commands is the primary interaction.

## Loop guard (for agents)

Limit how many times you test and fix. If the same test, build or check still fails after **3 fix attempts**, or you have made no measurable progress in about 30 minutes, **stop**. Do not keep retrying, and never delete, skip, ignore or weaken a test or check to get a green run. Record what you tried, the exact failing output and your best hypothesis in the implementation notes, leave the task In Progress, and ask the user how to proceed.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 Multiline editing, history recall and bracketed paste
- [x] #2 Large pastes collapse to a placeholder that expands on demand
- [x] #3 Typing / opens inline completion of slash commands with descriptions and arguments
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
1. input::Input editor with byte cursor over a String; multi-line editing and line-aware Up/Down. 2. History recall with draft stash. 3. Bracketed paste: large pastes become one private-use placeholder char expanded on send, Ctrl+E or click. 4. Slash completion popup (prefix then substring match) with args/description/scope, keyboard and mouse. 5. Tests incl. TestBackend click/focus.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Implemented crates/harness-tui/src/input.rs. Keys come pre-mapped through Keymap (send/newline). Pastes over PASTE_COLLAPSE_LINES (8) lines or PASTE_COLLAPSE_BYTES (1000) collapse to '[paste #N · L lines]'; Backspace deletes it atomically; Ctrl+E (fixed, EXPAND_PASTE_CHORD) expands all; each placeholder is a registered focusable/clickable element (paste.N). Completion rows are registered as completion.N. Tests: multiline_editing_and_cursor_movement, history_recall, small_pastes_insert_and_large_pastes_collapse, slash_opens_completion_with_descriptions_and_args, completion_rows_and_placeholders_are_clickable_and_focusable, long_lines_wrap_and_cursor_follows, plus ChatView slash_completion_from_the_input. App currently has its own simple draft; the app owner should route input through ChatView/Input.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Multi-line prompt editor with history, bracketed paste with collapsing placeholders (expand by Ctrl+E/click/Enter) and inline slash completion (keyboard + mouse). Verified by harness-tui unit and TestBackend tests; documented in docs/tui-views.md.
<!-- SECTION:FINAL_SUMMARY:END -->
