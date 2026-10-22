---
id: TASK-81
title: Diff review UI
status: Done
assignee:
  - '@visibility-tui'
created_date: '2026-10-22 14:30'
updated_date: '2026-10-22 19:16'
labels:
  - hygiene
  - tui
milestone: m-7
dependencies:
  - TASK-117
  - TASK-32
  - TASK-39
  - TASK-72
priority: medium
type: feature
ordinal: 81000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Users review every change today. A good diff view with per-file actions makes that fast.

## Loop guard (for agents)

Limit how many times you test and fix. If the same test, build or check still fails after **3 fix attempts**, or you have made no measurable progress in about 30 minutes, **stop**. Do not keep retrying, and never delete, skip, ignore or weaken a test or check to get a green run. Record what you tried, the exact failing output and your best hypothesis in the implementation notes, leave the task In Progress, and ask the user how to proceed.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 /diff shows changed files with per-file diffs for the session or a node
- [x] #2 Per-file revert, with hygiene classifications shown inline
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
1. diff.rs DiffView over ledger NetChanges (session or node via Ledger::from_events), hygiene categories from HygieneReport inline. 2. FileReverter trait + CheckpointReverter (report::revert_files). 3. File rows and [revert] buttons clickable/focusable, r key. 4. Tests incl. a real CheckpointStore revert.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Implemented harness_tui::diff (DiffView, DiffFile, FileReverter, CheckpointReverter, DiffAction, DiffOutput, file_id, revert_id). Tests: shows_changed_files_with_diffs_for_the_session_or_a_node, hygiene_classifications_are_shown_inline, per_file_revert_by_click_and_keyboard, checkpoint_reverter_restores_the_file (real shadow-git CheckpointStore). Remaining for AC1 + DoD1: the '/diff' slash command lives with the slash-command/app owner: on /diff [node] build DiffView::from_events(root, &events, node, report.as_ref(), theme).with_reverter(Box::new(CheckpointReverter::new(store, first_turn_checkpoint))) and show it; add a test that /diff opens it. Then check AC1 and DoD1.

Integration: /diff opens DiffView for the session (turn checkpoint) or a node with revert. Tests: tui undo_and_diff_use_the_turn_checkpoint, app diff_screen_reverts_files_and_detail_asks_for_it.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Integration: /diff opens DiffView for the session (turn checkpoint) or a node with revert. Tests: tui undo_and_diff_use_the_turn_checkpoint, app diff_screen_reverts_files_and_detail_asks_for_it. Workspace green: cargo fmt --check, clippy -D warnings, cargo test --workspace (1142 passed, 0 failed).
<!-- SECTION:FINAL_SUMMARY:END -->
