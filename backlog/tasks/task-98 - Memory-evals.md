---
id: TASK-98
title: Memory evals
status: Done
assignee:
  - '@memory-evals-quality'
created_date: '2026-10-22 14:30'
updated_date: '2026-10-22 17:04'
labels:
  - memory
  - eval
  - problem-4
milestone: m-9
dependencies:
  - TASK-96
  - TASK-109
priority: medium
type: task
ordinal: 98000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Memory only helps if corrections stick and retrieval is relevant.

## Loop guard (for agents)

Limit how many times you test and fix. If the same test, build or check still fails after **3 fix attempts**, or you have made no measurable progress in about 30 minutes, **stop**. Do not keep retrying, and never delete, skip, ignore or weaken a test or check to get a green run. Record what you tried, the exact failing output and your best hypothesis in the implementation notes, leave the task In Progress, and ask the user how to proceed.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 Eval: a correction in session 1 is applied in session 2
- [x] #2 Eval: retrieval returns relevant notes for seeded tasks
- [x] #3 Eval: dreaming does not drop explicit preferences or source links
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
harness_eval::memory_evals: correction_eval (two sessions with the memory tool; session 2 gets the index and read-only memory), retrieval_eval (recall@k, MRR over seeded notes/tasks), dream_preservation_eval; tests with the fake provider plus an ignored live test.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Tests: crates/harness-eval/tests/memory_evals.rs (correction applied in session 2 via search, via the MEMORY.md prefix, negative case; retrieval recall@3 >= 0.9 and MRR >= 0.8; dreaming keeps preferences and source links; eval runs get no memory; live_correction_eval #[ignore], needs ANTHROPIC_API_KEY). No TUI (DoD#4 N/A). Docs: docs/evals.md#memory-evals.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Added memory evals (correction carry-over, retrieval quality, dream preservation) in harness_eval::memory_evals, verified by tests/memory_evals.rs with the fake provider.
<!-- SECTION:FINAL_SUMMARY:END -->
