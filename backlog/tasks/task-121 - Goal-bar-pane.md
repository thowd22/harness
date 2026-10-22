---
id: TASK-121
title: Goal bar pane
status: Done
assignee:
  - '@visibility-tui'
created_date: '2026-10-22 14:39'
updated_date: '2026-10-22 19:16'
labels:
  - visibility
  - tui
milestone: m-6
dependencies:
  - TASK-117
  - TASK-118
  - TASK-37
  - TASK-39
  - TASK-62
  - TASK-64
  - TASK-65
documentation:
  - backlog/docs/doc-5 - Agent-Map-and-Run-Visibility.md
  - backlog/docs/doc-2 - harness.ini-Reference.md
priority: high
type: feature
ordinal: 121000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Users want an opencode-style panel on the right that shows the goal, tasks in progress, their status and which agent is working on each one. It should not take up space when there are no tasks.

## Loop guard (for agents)

Limit how many times you test and fix. If the same test, build or check still fails after **3 fix attempts**, or you have made no measurable progress in about 30 minutes, **stop**. Do not keep retrying, and never delete, skip, ignore or weaken a test or check to get a green run. Record what you tried, the exact failing output and your best hypothesis in the implementation notes, leave the task In Progress, and ask the user how to proceed.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 Right-side pane shows the goal and the task list with status icon, title, owning agent, model and host badge, plus done/total in the header
- [x] #2 Pane is only rendered when the session or run has at least one task and disappears when there are none
- [x] #3 In-progress tasks are highlighted and status changes appear live
- [x] #4 Clicking a task or pressing Enter on it jumps to the owning agent's node in the map or its node detail
- [x] #5 Visible in Chat and Map views; on narrow terminals it collapses into a progress indicator in the status line
- [x] #6 ui goal_bar and goal_bar_width in harness.ini control it and the palette can toggle it
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
1. goalbar.rs Goals: fold todo tool calls (full list) and workflow goal/tasks; owner = last node to change a task. 2. GoalBar: visibility (auto/off, palette toggle, width), split(area), render with node_glyph icons, highlight in-progress, task rows clickable/focusable -> OpenNode. 3. indicator() for the status line on narrow terminals. 4. Tests.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Implemented harness_tui::goalbar (Goals, GoalTask, TaskStatus, GoalBar, GoalBarOutput, palette_entry, TOGGLE_ID, MIN_MAIN_WIDTH, TODO_TOOL). Tests: shows_goal_tasks_owner_model_host_and_progress (AC1, AC3 highlight, snapshot), only_shown_with_tasks_and_updates_live (AC2, AC3), tasks_jump_to_their_owner_by_click_and_keyboard (AC4), narrow_terminals_collapse_to_a_status_indicator_and_config_and_palette_control_it (AC5 indicator part, AC6). Remaining for AC5 + DoD1: app.rs (not owned here) must compose the bar in both views: let (main, pane) = goalbar.split(body, &goals); render Chat/Map into main and GoalBar::render into pane; feed Goals::apply with every RunEvent; route clicks inside the pane to GoalBar::handle and GoalBarOutput::OpenNode to the map/detail; and the status line (status.rs owner) must show GoalBar::indicator(&goals, width) when it returns Some. Also add goalbar::palette_entry() to the palette and call GoalBar::run_palette. The todo tool (harness_agent::todo) must use the {goal?, todos:[{id?, content|title, status}]} argument shape (documented in docs/visibility.md).

Integration: App renders the goal bar in Chat and Map, collapses to a status-line indicator on narrow terminals, clicks/palette focus jump to owners; todo tool events feed it via EventKind::Tasks (record_tasks). Tests: app goal bar tests (mouse and keyboard), todo tasks_event test.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Integration: App renders the goal bar in Chat and Map, collapses to a status-line indicator on narrow terminals, clicks/palette focus jump to owners; todo tool events feed it via EventKind::Tasks (record_tasks). Tests: app goal bar tests (mouse and keyboard), todo tasks_event test. Workspace green: cargo fmt --check, clippy -D warnings, cargo test --workspace (1142 passed, 0 failed).
<!-- SECTION:FINAL_SUMMARY:END -->
