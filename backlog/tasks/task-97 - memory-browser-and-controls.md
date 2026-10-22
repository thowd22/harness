---
id: TASK-97
title: /memory browser and controls
status: Done
assignee:
  - '@memory-evals-quality'
created_date: '2026-10-22 14:30'
updated_date: '2026-10-22 19:16'
labels:
  - memory
  - tui
milestone: m-9
dependencies:
  - TASK-117
  - TASK-95
  - TASK-96
priority: medium
type: feature
ordinal: 97000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Memory must be inspectable and controllable.

## Loop guard (for agents)

Limit how many times you test and fix. If the same test, build or check still fails after **3 fix attempts**, or you have made no measurable progress in about 30 minutes, **stop**. Do not keep retrying, and never delete, skip, ignore or weaken a test or check to get a green run. Record what you tried, the exact failing output and your best hypothesis in the implementation notes, leave the task In Progress, and ask the user how to proceed.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 Shows files, search, recent writes and dream history with diffs
- [x] #2 Open a note in EDITOR or ask the agent to change or forget it
- [x] #3 memory enabled toggle keeps notes, and agents or workflows can set memory off
- [x] #4 Headless, eval and automation runs do not access memory unless headless_access is true
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
1. harness_tui::memory::browser::MemoryBrowser: tabs Files/Search/Recent writes/Dreams, detail pane (note text or diff), buttons, BrowserOutcome for the core.
2. Data from harness-memory (list_notes, SearchIndex, dream::history).
3. Tests for mouse and keyboard; controls tests in harness-memory (enabled toggle keeps notes, agent memory off, headless access) and eval runs without memory.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Done: MemoryBrowser (pure view; set_data/set_search_results/set_detail; outcomes Search, Preview, ShowDiff, OpenInEditor, Draft, Undo, SetEnabled, Dream, Closed), tested by mouse and keyboard (crates/harness-tui/src/memory/browser.rs tests). Data sources: harness_memory::drive::list_notes, index::SearchIndex, dream::history::{recent_writes, dreams, diff, undo}. crates/harness-memory/tests/controls.rs: disabling memory keeps notes, agents can set memory off, headless needs headless_access; crates/harness-eval/tests/memory_evals.rs eval_runs_do_not_access_memory. Docs: docs/memory-ui.md.
Remaining (AC#3 and DoD#1): workflows cannot set memory off yet because the workflow schema (TASK-53) has no memory key; add a workflow/step memory: off that feeds effective_access. Integration: the TUI app must add a /memory slash command that opens MemoryBrowser, fills MemoryData from the drive, and handles the outcomes (spawn $EDITOR on drive.root().join(path) with the terminal suspended, put Draft text in the input, history::undo, toggle cfg.memory.enabled for the session, run the dream pass); add an app-level test.

Integration: /memory opens MemoryBrowser as an app screen (Interaction focus model); search/preview/diff/undo/toggle/dream go to the core; open in editor suspends the UI and runs $VISUAL/$EDITOR (Effect::Edit). The toggle restarts the session with the same conversation (notes kept); workflows get a top-level `memory: off|read|read-write` cap. Tests: app memory_browser_by_mouse_and_keyboard, tui the_memory_toggle_keeps_notes_and_removes_the_tool_for_the_session, commands.rs workflows_can_turn_memory_off, executor workflows_can_cap_memory. Docs: docs/memory-ui.md, docs/workflows.md.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Integration: /memory opens MemoryBrowser as an app screen (Interaction focus model); search/preview/diff/undo/toggle/dream go to the core; open in editor suspends the UI and runs $VISUAL/$EDITOR (Effect::Edit). The toggle restarts the session with the same conversation (notes kept); workflows get a top-level `memory: off|read|read-write` cap. Tests: app memory_browser_by_mouse_and_keyboard, tui the_memory_toggle_keeps_notes_and_removes_the_tool_for_the_session, commands.rs workflows_can_turn_memory_off, executor workflows_can_cap_memory. Docs: docs/memory-ui.md, docs/workflows.md. Workspace green: cargo fmt --check, clippy -D warnings, cargo test --workspace (1142 passed, 0 failed).
<!-- SECTION:FINAL_SUMMARY:END -->
