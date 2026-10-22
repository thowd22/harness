---
id: TASK-69
title: Live metrics and blocked-node highlighting
status: Done
assignee:
  - '@visibility-tui'
created_date: '2026-10-22 14:30'
updated_date: '2026-10-22 16:57'
labels:
  - visibility
  - problem-6
  - problem-7
milestone: m-6
dependencies:
  - TASK-64
  - TASK-88
  - TASK-27
priority: medium
type: feature
ordinal: 69000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Users should see at a glance where time and money go and which nodes need them.

## Loop guard (for agents)

Limit how many times you test and fix. If the same test, build or check still fails after **3 fix attempts**, or you have made no measurable progress in about 30 minutes, **stop**. Do not keep retrying, and never delete, skip, ignore or weaken a test or check to get a green run. Record what you tried, the exact failing output and your best hypothesis in the implementation notes, leave the task In Progress, and ask the user how to proceed.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 Nodes show live token and cost totals and cache hit rate
- [x] #2 Nodes waiting on approval or budget are highlighted and Enter jumps to the prompt
- [x] #3 Run header shows totals and critical path duration
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
1. map/metrics.rs: blocked() (approval / budget stop), node cache rate, elapsed, critical path over needs edges. 2. Map nodes show live tokens, cost and cache %, blocked nodes get the approval border and Enter emits MapOutput::OpenPrompt. 3. Map header with totals and critical path.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Implemented in harness_tui::map (metrics.rs + MapView header/nodes). Tests: map::metrics critical_path_takes_the_longest_chain, running_nodes_count_elapsed_time, blocked_nodes_and_cache_rate; map::tests nodes_show_... (AC1 tokens/cost/cache), blocked_nodes_are_highlighted_and_enter_jumps_to_the_prompt (AC2, approval and budget), header_shows_totals_and_critical_path (AC3). App owner: on MapOutput::OpenPrompt{blocked: Approval} focus the approval queue (ChatView approvals), on Budget show the budget prompt.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Live per-node tokens/cost/cache rate, blocked-node highlighting with Enter-to-prompt and a run header with totals and critical path; verified by map and metrics tests.
<!-- SECTION:FINAL_SUMMARY:END -->
