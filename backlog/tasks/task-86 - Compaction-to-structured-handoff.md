---
id: TASK-86
title: Compaction to structured handoff
status: Done
assignee:
  - '@context-cost-commands'
created_date: '2026-10-22 14:30'
updated_date: '2026-10-22 19:16'
labels:
  - context
  - problem-4
milestone: m-8
dependencies:
  - TASK-82
  - TASK-85
priority: high
type: feature
ordinal: 86000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Coherence drops as context fills. Deliberate resets with a structured handoff work better than letting the model wrap up early.

## Loop guard (for agents)

Limit how many times you test and fix. If the same test, build or check still fails after **3 fix attempts**, or you have made no measurable progress in about 30 minutes, **stop**. Do not keep retrying, and never delete, skip, ignore or weaken a test or check to get a green run. Record what you tried, the exact failing output and your best hypothesis in the implementation notes, leave the task In Progress, and ask the user how to proceed.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 At compact_at or via /compact, history is summarized into a handoff with goal, decisions, current state, files touched, next steps and open questions
- [x] #2 The most recent keep_recent_turns turns are kept verbatim
- [x] #3 Compaction is an event visible in the map and detail
- [x] #4 Eval checks task completion after compaction
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
1. compaction::handoff::Compactor summarizes messages before cut_index(keep_recent_turns) with a dedicated summarization call into Handoff{goal, decisions, current_state, files_touched (harness-recorded paths first), next_steps, open_questions}.
2. ContextManager triggers it at compact_at * window (ContextTracker estimate) or on request_compaction (/compact); compact_now for idle /compact.
3. Each compaction is a 'compaction' child node with its own model call + cost; history re-logged via invalidate_log_from(0).
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Tests: tests/context_management.rs compaction_at_threshold_writes_a_handoff_keeps_recent_turns_and_the_task_completes (sections, verbatim tail, child node in events and in reducer RunState with parent/children/model_calls, resume rebuild, task completes after compaction), slash_compact_compacts_below_the_threshold_with_instructions, compact_now_works_while_idle_and_reports_nothing_to_compact. Open: AC#4 asks for an eval; a deterministic eval-style scenario test exists here, but the harness-eval suite (crate is still a stub) needs a real 'long task survives compaction' eval case. AC#3 is checked: the compaction appears as a child node (map) with its model call (detail); a dedicated EventKind::Compaction would be a nicer optional addition in harness-events. Production wiring (binary/TUI adding ContextManager with a Compactor as a loop hook and dispatching /compact) is a cross-crate need.

Integration: long-tasks eval suite (survives-compaction, no-premature-stop, with bad variants that must fail). Tests: harness-eval tests/long_tasks.rs; binary commands.rs long_task_evals_pass_through_the_binary. Docs: docs/evals.md 'Long tasks'.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Integration: long-tasks eval suite (survives-compaction, no-premature-stop, with bad variants that must fail). Tests: harness-eval tests/long_tasks.rs; binary commands.rs long_task_evals_pass_through_the_binary. Docs: docs/evals.md 'Long tasks'. Workspace green: cargo fmt --check, clippy -D warnings, cargo test --workspace (1142 passed, 0 failed).
<!-- SECTION:FINAL_SUMMARY:END -->
