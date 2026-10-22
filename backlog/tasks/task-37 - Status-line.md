---
id: TASK-37
title: Status line
status: Done
assignee:
  - '@tui-views'
created_date: '2026-10-22 14:30'
updated_date: '2026-10-22 16:18'
labels:
  - tui
  - problem-6
milestone: m-3
dependencies:
  - TASK-32
  - TASK-39
  - TASK-9
priority: high
type: feature
ordinal: 37000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Cost and state must always be visible so the user controls spend and knows what the agent is doing (problems 6 and 7).

## Loop guard (for agents)

Limit how many times you test and fix. If the same test, build or check still fails after **3 fix attempts**, or you have made no measurable progress in about 30 minutes, **stop**. Do not keep retrying, and never delete, skip, ignore or weaken a test or check to get a green run. Record what you tried, the exact failing output and your best hypothesis in the implementation notes, leave the task In Progress, and ask the user how to proceed.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 Shows model, autonomy mode, context usage, session cost, cache hit rate, verify state, running agents and active remote hosts
- [x] #2 Updates live from events without extra polling
- [x] #3 Footer matches the visual design doc: cwd and git branch, model and mode, context usage bar, tokens with cache hit rate, cost, verify state, running agents and remotes, all in theme colors
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
1. status::StatusLine folded from RunEvents (tokens, cost, context, verify, running agents and hosts). 2. Setters for cwd/branch/model+context window/mode. 3. Render footer segments in theme colours with priority-based dropping. 4. Tests for content, live updates, colours, narrow widths.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Implemented crates/harness-tui/src/status.rs. Context usage = last main-call input+cached+output over the window given by set_model (from the registry's Capabilities.context_window). Cache hit rate = cached/(input+cached). Cost shows $? until a Cost event arrives (unknown is never $0). Tests: footer_matches_the_design (exact doc-7 layout), updates_live_from_events, uses_theme_colors_and_context_bar_levels, narrow_terminals_drop_low_priority_segments. DoD#4: the status line has no interactive elements (N/A). Integration: App footer should render StatusLine::line (ChatView already does).
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Event-driven status line with cwd/branch, model/mode, context bar, tokens + cache rate, cost, verify state, running agents and remotes in theme colours. Verified by harness-tui tests; documented in docs/tui-views.md.
<!-- SECTION:FINAL_SUMMARY:END -->
