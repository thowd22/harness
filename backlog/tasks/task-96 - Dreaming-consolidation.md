---
id: TASK-96
title: Dreaming consolidation
status: Done
assignee:
  - '@memory-evals-quality'
created_date: '2026-10-22 14:30'
updated_date: '2026-10-22 19:16'
labels:
  - memory
  - workflow
  - problem-4
milestone: m-9
dependencies:
  - TASK-92
  - TASK-94
  - TASK-55
documentation:
  - backlog/docs/doc-4 - Memory-System-Design.md
priority: high
type: feature
ordinal: 96000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Without consolidation memory grows noisy and stale. Devin runs a daily Dreaming pass; ours is a user-overridable built-in workflow.

## Loop guard (for agents)

Limit how many times you test and fix. If the same test, build or check still fails after **3 fix attempts**, or you have made no measurable progress in about 30 minutes, **stop**. Do not keep retrying, and never delete, skip, ignore or weaken a test or check to get a green run. Record what you tried, the exact failing output and your best hypothesis in the implementation notes, leave the task In Progress, and ask the user how to proceed.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 dream workflow merges duplicates, prunes stale unused notes after a grace period, adds missed lessons from recent runs and reorganizes the index
- [x] #2 Source links and explicit preferences are always preserved
- [x] #3 Runs about daily when there was activity or on /memory dream, using dream_model
- [x] #4 Result is one commit whose diff is viewable and revertable
- [x] #5 First run seeds memory from recent runs
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
1. dream::consolidate: pure passes (merge duplicates with source union, transient removal, stale prune with grace, drop empty + relink, MEMORY.md index block) + invariant check.
2. dream::runs: rebuild transcripts from the run store; dream::schedule: DreamLog + due().
3. Dreamer::dream: lessons via Extractor (dream_model), compute plan, write once through SessionCheckout::write (recompute on concurrent change), log the pass.
4. dream::history: dreams(), diff(), undo().
5. Built-in workflows/dream.yaml (type: dream step settings) and DreamSettings::from_workflow.
6. Tests with a real git drive, fake provider and recorded runs; docs/dreaming.md.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Done: harness_memory::dream::{Dreamer, DreamSettings, DreamReport, Lesson, DreamError, DREAM_SUBJECT, STEP_TYPE, MAX_ATTEMPTS}, dream::consolidate, dream::runs, dream::schedule::{due, Due, DreamLog, DreamEntry}, dream::history. The dream 'workflow' is the built-in workflows/dream.yaml whose type: dream step carries the settings; the pass itself is a library call (the workflow executor, TASK-55, should map step type dream to Dreamer::dream). Tests: dream unit tests (consolidate, schedule) and crates/harness-memory/tests/dream.rs (first pass seeds from runs, one commit, diff, undo; merge/prune/preserve; settings from the built-in workflow).
Remaining (AC#3, DoD#1): schedule::due implements daily-with-activity/manual/off, but nothing calls it yet. The binary/TUI must (a) on start-up and when idle call due(cfg.memory.dream, false, DreamLog::last().at, max(runs::last_activity(data_dir), drive head time), now, settings.interval()) and run Dreamer in a 'dream' SessionCheckout, (b) add /memory dream calling it with requested = true, (c) build the Extractor from the registry-resolved [memory] dream_model provider. Add a test of that trigger, then check AC#3 and DoD#1.

Integration: dream::due runs at TUI start when due, /memory dream runs a pass, lessons use [memory] dream_model; dream step type and bundled dream workflow. Tests: tui memory_dream_consolidates_in_the_background, a_scheduled_pass_runs_when_due_after_activity, lessons_use_the_configured_dream_model; commands.rs the_dream_workflow_loads_and_runs_headless_only_with_headless_access.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Integration: dream::due runs at TUI start when due, /memory dream runs a pass, lessons use [memory] dream_model; dream step type and bundled dream workflow. Tests: tui memory_dream_consolidates_in_the_background, a_scheduled_pass_runs_when_due_after_activity, lessons_use_the_configured_dream_model; commands.rs the_dream_workflow_loads_and_runs_headless_only_with_headless_access. Workspace green: cargo fmt --check, clippy -D warnings, cargo test --workspace (1142 passed, 0 failed).
<!-- SECTION:FINAL_SUMMARY:END -->
