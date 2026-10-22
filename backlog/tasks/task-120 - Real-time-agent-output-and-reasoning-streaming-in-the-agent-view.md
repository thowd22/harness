---
id: TASK-120
title: Real-time agent output and reasoning streaming in the agent view
status: Done
assignee:
  - '@visibility-tui'
created_date: '2026-10-22 14:39'
updated_date: '2026-10-22 16:57'
labels:
  - visibility
  - tui
  - provider
milestone: m-6
dependencies:
  - TASK-11
  - TASK-117
  - TASK-12
  - TASK-122
  - TASK-123
  - TASK-13
  - TASK-39
  - TASK-64
  - TASK-65
documentation:
  - backlog/docs/doc-5 - Agent-Map-and-Run-Visibility.md
priority: high
type: feature
ordinal: 120000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Users want to watch what every agent is writing and thinking as it happens, not only after a turn completes. With many parallel agents this must stay smooth and must be honest about what each provider exposes.

## Loop guard (for agents)

Limit how many times you test and fix. If the same test, build or check still fails after **3 fix attempts**, or you have made no measurable progress in about 30 minutes, **stop**. Do not keep retrying, and never delete, skip, ignore or weaken a test or check to get a green run. Record what you tried, the exact failing output and your best hypothesis in the implementation notes, leave the task In Progress, and ask the user how to proceed.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 Running map nodes show a one-line live preview of the latest output or reasoning line, dimmed for reasoning
- [x] #2 Selecting a node (Space, Enter or click) opens a live pane under the map streaming that agent's output, reasoning and tool calls token by token
- [x] #3 Node detail streams the current turn live: reasoning expanded while streaming then collapsed per ui show_thinking, text, and tool calls with live bash output
- [x] #4 Streams from 10 or more parallel agents render within the 16 ms frame budget using per-node buffering and throttled redraws
- [x] #5 When a provider hides or encrypts reasoning the view states that explicitly instead of showing nothing
- [x] #6 Live streams and the persisted event log show identical content after the turn completes
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
1. map/live.rs LiveStreams: bounded per-node segments (text, reasoning, tool calls with live output, notes), O(1) appends, FrameThrottle. 2. Map nodes show the latest line (reasoning dimmed); Space/click opens a LivePane under the map. 3. DetailView streams the current turn (reasoning expanded while streaming then per show_thinking, live bash output). 4. hidden_reasoning(): harness ReasoningHidden notices or reasoning tokens without reasoning text -> explicit note. 5. Perf test with 10 parallel streams; live vs log equality test through RunRecorder.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Implemented harness_tui::map::live (LiveStreams, NodeStream, Segment, LivePane, FrameThrottle, hidden_reasoning, REASONING_HIDDEN) plus MapView live pane and DetailView streaming. Tests: map running_nodes_show_a_live_preview_dimmed_for_reasoning (AC1), space_and_click_open_the_live_pane_esc_and_close_button_close_it (AC2, mouse + keyboard), detail current_turn_streams_reasoning_then_collapses_and_streams_bash_output (AC3), tests/perf.rs ten_live_streams_and_the_goal_bar_render_within_budget (AC4), map::live hidden_reasoning_is_stated + detail hidden_reasoning_is_stated (AC5), tests/live_vs_log.rs live_and_logged_streams_match (AC6, RunRecorder subscribe vs read_run incl. redaction). Live bash output has no event kind yet: the core must call MapView::tool_output(node, call_id, chunk) / DetailView::tool_output until harness-events adds a tool-output event.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Real-time per-node output/reasoning streaming: map previews, live pane, streaming node detail, honest hidden-reasoning notes, throttled per-node buffers; verified by map/detail/live unit tests, a 10-stream perf test and a live-vs-log RunRecorder test.
<!-- SECTION:FINAL_SUMMARY:END -->
