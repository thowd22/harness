---
id: TASK-65
title: Node detail view in GitHub Actions log style
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
  - TASK-123
  - TASK-124
  - TASK-39
  - TASK-64
documentation:
  - backlog/docs/doc-5 - Agent-Map-and-Run-Visibility.md
priority: high
type: feature
ordinal: 65000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Users want GitHub-Actions-level detail for each agent: what it did, how long it took and what it cost.

## Loop guard (for agents)

Limit how many times you test and fix. If the same test, build or check still fails after **3 fix attempts**, or you have made no measurable progress in about 30 minutes, **stop**. Do not keep retrying, and never delete, skip, ignore or weaken a test or check to get a green run. Record what you tried, the exact failing output and your best hypothesis in the implementation notes, leave the task In Progress, and ask the user how to proceed.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 Sidebar lists turns, tool calls, verify runs and hygiene report
- [x] #2 Log groups per turn and tool call are collapsible, failures auto-expand
- [x] #3 Each entry shows duration, tokens (in, cached, out, reasoning) and cost
- [x] #4 Follow mode tails a running node
- [x] #5 Palette actions: search log, copy entry, open diff, open request inspector
- [x] #6 Every log group and entry can be expanded or collapsed by click and by keyboard
- [x] #7 Log entries reuse the tool call cards and per-turn metadata from the chat view
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
1. detail.rs DetailView folding one node's events into turn groups (reasoning, text, ToolCards, TurnMeta line), verify groups, notices and a hygiene group. 2. Sidebar of turns/tool calls/verify/hygiene with jump actions. 3. Auto-expand failures, per-group render cache (virtualized), follow mode. 4. Palette entries + run_palette (search, copy, open diff, open inspector, follow). 5. Mouse and keyboard tests.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Implemented harness_tui::detail::DetailView (detail.rs, detail/tests.rs). Tests: sidebar_lists_turns_tool_calls_verify_runs_and_hygiene (AC1), groups_collapse_and_failures_auto_expand (AC2), entries_show_duration_tokens_and_cost_and_reuse_chat_cards_and_meta (AC3, AC7), follow_mode_tails_a_running_node (AC4), palette_actions_search_copy_diff_and_inspector (AC5), groups_and_entries_toggle_by_click_and_keyboard (AC6), hygiene_actions_by_click_and_keyboard, current_turn_streams_reasoning_then_collapses_and_streams_bash_output, rendering_cost_does_not_grow_with_log_length, snapshot_detail; perf: tests/perf.rs detail 10k entries 3.7 ms release. Integration for the app owner: add detail::palette_entries() to the palette while detail is shown and pass Invocations to DetailView::run_palette; map DetailOutput::{OpenDiff, OpenInspector, Copy, RemoveTempFiles, Back}. Hygiene: call DetailView::set_hygiene(ledger::summary_lines(..), HygieneReport::lines(), report.temp_files().len()) (TASK-72 AC3 / TASK-75 AC4 UI side).
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
GitHub-Actions-style node detail (sidebar + collapsible, virtualized log reusing chat ToolCards and TurnMeta, auto-expanded failures, follow mode, palette actions, hygiene section) in harness_tui::detail; verified by 11 TestBackend tests covering mouse and keyboard.
<!-- SECTION:FINAL_SUMMARY:END -->
