---
id: TASK-55
title: Workflow DAG executor
status: Done
assignee:
  - '@workflows-subagents'
created_date: '2026-10-22 14:30'
updated_date: '2026-10-22 17:14'
labels:
  - workflow
  - core
milestone: m-5
dependencies:
  - TASK-53
  - TASK-54
  - TASK-51
priority: high
type: feature
ordinal: 55000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
The engine that runs workflows reliably, in parallel where possible, with every step visible as a node.

## Loop guard (for agents)

Limit how many times you test and fix. If the same test, build or check still fails after **3 fix attempts**, or you have made no measurable progress in about 30 minutes, **stop**. Do not keep retrying, and never delete, skip, ignore or weaken a test or check to get a green run. Record what you tried, the exact failing output and your best hypothesis in the implementation notes, leave the task In Progress, and ask the user how to proceed.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 Runs ready steps in parallel within scheduler limits
- [x] #2 Honours if, retries, timeout, on_failure, loops and matrix expansion
- [x] #3 Cancellation stops running steps and marks the rest cancelled
- [x] #4 Workflow state is persisted so a crashed workflow can resume
- [x] #5 Integration tests with the fake provider cover each step type
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
1. harness_workflow::executor: Executor (recorder, Runners, Scheduler, library, board, hosts) runs WfRun: declares all step nodes queued, starts ready steps in parallel (JoinSet) holding scheduler slots, honours if/retries/timeout/on_failure/loop/matrix/for_each, cancellation, budgets.
2. StepRunner trait + Runners; standard runners in harness_workflow::runners (agent via SubagentRuntime, shell, verify, bugfix-proof, StoreCheckpointer).
3. Persist WorkflowState to workflow.json after each change; resume via rerun::plan_resume.
4. Integration tests with the fake provider for each step type.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Implemented crates/harness-workflow/src/executor.rs and runners.rs; tests in tests/executor.rs (+ tests/common). Node layout: workflow node, step nodes <wf>/<step> (needs edges, declared queued up front), instance nodes <wf>/<step>[i], agent loop <step node>.agent, nested workflow under its step. Approval and nested steps take no scheduler slot. The executor releases each slot explicitly (slot.suspend) because a runner may keep its StepRequest. Timeout cancels the attempt token and waits up to 10s for the runner. Resume and re-run create a new run (RunRecorder only creates runs) and copy the reused artifacts. Remote execution itself (sync push/pull around a placed step) is left to the runner the binary wires: the executor only places the step and passes StepRequest.host. Validation: cargo test -p harness-workflow (36 unit + 2 bundled + 12 executor integration tests pass); cargo clippy -p harness-workflow -p harness-agent --all-targets -D warnings clean.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Added harness_workflow::executor (Executor, Runners, StepRunner, StepRequest/StepAction/StepOutput/StepFailure, Checkpointer, WorkflowLibrary, resolve_inputs) and harness_workflow::runners. Ready steps run in parallel within scheduler limits, with if/retries/timeout/on_failure/loops/matrix, cancellation and persisted resumable state. Integration tests cover every step type with the fake provider and real processes. DoD#4 N/A (map rendering belongs to harness-tui).
<!-- SECTION:FINAL_SUMMARY:END -->
