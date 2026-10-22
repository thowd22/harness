---
id: TASK-67
title: Run history browser
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
  - TASK-117
  - TASK-4
  - TASK-62
priority: medium
type: feature
ordinal: 67000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Past runs need to be findable and comparable.

## Loop guard (for agents)

Limit how many times you test and fix. If the same test, build or check still fails after **3 fix attempts**, or you have made no measurable progress in about 30 minutes, **stop**. Do not keep retrying, and never delete, skip, ignore or weaken a test or check to get a green run. Record what you tried, the exact failing output and your best hypothesis in the implementation notes, leave the task In Progress, and ask the user how to proceed.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 Lists runs with status, agent or workflow, start, duration, cost and host
- [x] #2 Filter via palette and open any run in the map
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
1. history.rs: RunSummary from the shared reducer, load_runs over <data_dir>/runs. 2. HistoryView rows (status, workflow/agent, start, duration, cost, host) with palette filter entries and Open/Replay outputs. 3. Tests with RunRecorder-written runs in a temp dir; open a run into MapView::set_state.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Implemented harness_tui::history (RunSummary, load_runs, load_run, HistoryView, palette_entries, ids). Tests: lists_runs_with_status_agent_start_duration_cost_and_host (AC1), palette_filters_and_rows_open_by_click_and_keyboard (AC2: palette filter, click and keyboard open, the opened run folds into MapView). App owner: offer history::palette_entries(), route HistoryOutput::Open(id) to MapView::set_state(&load_run(data_dir, id)?.0).
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Run history browser over the run store with palette filtering and open-in-map; verified by two tokio tests using real RunRecorder logs and TestBackend mouse/keyboard input.
<!-- SECTION:FINAL_SUMMARY:END -->
