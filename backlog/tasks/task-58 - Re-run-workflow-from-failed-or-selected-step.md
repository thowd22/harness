---
id: TASK-58
title: Re-run workflow from failed or selected step
status: Done
assignee:
  - '@workflows-subagents'
created_date: '2026-10-22 14:30'
updated_date: '2026-10-22 17:14'
labels:
  - workflow
milestone: m-5
dependencies:
  - TASK-55
priority: medium
type: feature
ordinal: 58000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Like re-running failed jobs in GitHub Actions, users should not pay to redo successful steps.

## Loop guard (for agents)

Limit how many times you test and fix. If the same test, build or check still fails after **3 fix attempts**, or you have made no measurable progress in about 30 minutes, **stop**. Do not keep retrying, and never delete, skip, ignore or weaken a test or check to get a green run. Record what you tried, the exact failing output and your best hypothesis in the implementation notes, leave the task In Progress, and ask the user how to proceed.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 Re-run from a selected step reuses upstream outputs and artifacts
- [x] #2 Re-runs are linked to the original run in history
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
1. harness_workflow::rerun: WorkflowState persisted as workflow.json, ReusePlan, plan_resume, plan_rerun(from steps or failed steps).
2. Executor takes over reused steps (records, outputs, copied artifacts) and runs the rest in a new run.
3. Origin{run, kind, from_steps} links runs; lineage() and reruns_of() for history; RunStarted name says re-run of <id>.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Implemented crates/harness-workflow/src/rerun.rs (5 tests) and the executor takeover; integration test rerun_from_a_selected_step_reuses_upstream_and_links_runs shows the upstream step is not run again, its output and copied artifact path feed the re-run step, and lineage links the runs. UI follow-up for harness-tui (not an AC of this task): a 're-run from here' palette action in node detail should call rerun::plan_rerun(run_dir, &wf, &[step]) and Executor::run with the plan. Validation: cargo test -p harness-workflow passes; clippy -D warnings clean.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Added harness_workflow::rerun (WorkflowState, StepRecord, StepStatus, Origin, ReusePlan, plan_resume, plan_rerun, lineage, reruns_of, BudgetState::badge). A re-run from selected or failed steps reuses upstream outputs and artifacts in a new run linked to the original. Verified by unit and integration tests. DoD#4 N/A (no UI in this task).
<!-- SECTION:FINAL_SUMMARY:END -->
