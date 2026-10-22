---
id: TASK-123
title: Tool call cards
status: Done
assignee:
  - '@tui-views'
created_date: '2026-10-22 14:43'
updated_date: '2026-10-22 16:19'
labels:
  - tui
  - visibility
milestone: m-3
dependencies:
  - TASK-117
  - TASK-34
  - TASK-39
  - TASK-4
documentation:
  - backlog/docs/doc-7 - Visual-Design-Dracula-Theme-Indicators-Tool-Cards.md
priority: high
type: feature
ordinal: 123000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Tool calls are where most of the agent's real work and failures happen. They should render as detailed, scannable cards, as in pi and oh-my-pi, so the user can follow and audit the work without opening logs.

## Loop guard (for agents)

Limit how many times you test and fix. If the same test, build or check still fails after **3 fix attempts**, or you have made no measurable progress in about 30 minutes, **stop**. Do not keep retrying, and never delete, skip, ignore or weaken a test or check to get a green run. Record what you tried, the exact failing output and your best hypothesis in the implementation notes, leave the task In Progress, and ask the user how to proceed.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 Each call renders as a card with status icon, tool name, argument summary, duration and timestamp in the header, tinted by pending, ok, error or approval state
- [x] #2 read shows path, line range and line count; write shows path, bytes and a new-file badge; edit shows a syntax-colored unified diff with line numbers and +/- counts
- [x] #3 bash shows the command, streams output live while running, and shows exit code, timeout or killed badges and a pointer when output was truncated
- [x] #4 Output is collapsed past ui tool_preview_lines with a count of hidden lines, and Enter or a click expands or collapses the card
- [x] #5 Errors show the exact actionable message returned to the model
- [x] #6 Parallel calls from one turn are grouped under one header with a count
- [x] #7 todo, memory and spawn_agent calls render as compact summaries
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
1. cards::ToolCard model (status, args, output, file change, approval note, expanded) and CardGroup for parallel calls. 2. Bordered card layout with header summary/badges/duration/timestamp and status tint. 3. Per-tool bodies: read ranges, write bytes/new file, edit diff with line numbers and syntax colours, bash live output + exit/timeout/killed badges + spill pointer, compact todo/memory/spawn_agent. 4. Collapse past tool_preview_lines; toggle via transcript actions. 5. Tests.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Implemented crates/harness-tui/src/cards.rs; the transcript builds cards from ToolCall/ToolResult/FileChanged events and groups calls of one model turn. Bash exit status is parsed from the bash tool's trailing status lines; the spill pointer from the spiller's '[... output truncated ... spill:<id>' notice. Live bash output arrives via Transcript::tool_output(call_id, chunk) because harness-events has no tool-output delta event yet. Tests: header_has_icon_name_summary_duration_timestamp_and_tint, read_shows_range_count_and_truncated_badge, write_shows_path_bytes_and_new_file_badge, edit_shows_colored_diff_with_line_numbers_and_counts, bash_streams_and_shows_exit_badges_and_spill_pointer, output_collapses_past_preview_and_toggles, errors_show_the_full_message, parallel_calls_are_grouped_with_a_count, todo_memory_and_spawn_agent_are_compact, plus ChatView cards_toggle_by_click_and_by_keyboard.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Tool call cards with status tint, per-tool headers/bodies (read/write/edit diff/bash streaming and badges), collapse past tool_preview_lines with Enter/click toggle, full error messages, grouped parallel calls and compact todo/memory/spawn_agent. Verified by harness-tui tests; documented in docs/tui-views.md.
<!-- SECTION:FINAL_SUMMARY:END -->
