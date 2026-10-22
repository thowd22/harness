---
id: TASK-68
title: Run replay with time scrubbing
status: Done
assignee:
  - '@visibility-tui'
created_date: '2026-10-22 14:30'
updated_date: '2026-10-22 16:57'
labels:
  - visibility
  - tui
milestone: m-6
dependencies:
  - TASK-67
  - TASK-64
priority: low
type: feature
ordinal: 68000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Replaying a run step by step makes it much easier to understand how an agent went wrong.

## Loop guard (for agents)

Limit how many times you test and fix. If the same test, build or check still fails after **3 fix attempts**, or you have made no measurable progress in about 30 minutes, **stop**. Do not keep retrying, and never delete, skip, ignore or weaken a test or check to get a green run. Record what you tried, the exact failing output and your best hypothesis in the implementation notes, leave the task In Progress, and ask the user how to proceed.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 Replay re-folds events up to a chosen point and the map and detail show that state
- [x] #2 Step forward and back by event and by node
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
1. history/replay.rs Replay: cursor over events, incremental re-fold forward, checkpoints every 512 events for seeking back. 2. step_event / step_node (node_started/node_finished boundaries). 3. ReplayBar with clickable buttons and scrubber track, keys. 4. Tests that map and detail rebuilt from Replay::visible show that state.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Implemented harness_tui::history::replay (Replay, ReplayBar, ReplayAction, ReplayOutput, CHECKPOINT_EVERY). Tests: refolds_up_to_the_cursor_and_views_show_that_state (AC1, MapView::set_state + DetailView::from_events on Replay::visible), steps_by_event_and_by_node (AC2), checkpoints_give_the_same_state_backwards, bar_buttons_and_track_work_by_click_and_keyboard.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Run replay with checkpointed re-folding, event and node stepping and a clickable scrubber bar; verified by 4 unit tests including map/detail state at the cursor.
<!-- SECTION:FINAL_SUMMARY:END -->
