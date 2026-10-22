---
id: TASK-61
title: Workflow and step budgets
status: Done
assignee:
  - '@workflows-subagents'
created_date: '2026-10-22 14:30'
updated_date: '2026-10-22 19:16'
labels:
  - workflow
  - problem-6
milestone: m-5
dependencies:
  - TASK-55
  - TASK-88
priority: medium
type: feature
ordinal: 61000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Long workflows can burn quota quickly. Budgets must apply at workflow and step levels.

## Loop guard (for agents)

Limit how many times you test and fix. If the same test, build or check still fails after **3 fix attempts**, or you have made no measurable progress in about 30 minutes, **stop**. Do not keep retrying, and never delete, skip, ignore or weaken a test or check to get a green run. Record what you tried, the exact failing output and your best hypothesis in the implementation notes, leave the task In Progress, and ask the user how to proceed.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 Budget limits at workflow and step level with soft warning and hard stop per budget on_exceed
- [x] #2 Budget state is visible on map nodes
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
1. Schema budget {usd, tokens, warn_at, on_exceed: stop|warn} on workflow and steps.
2. Executor: accumulate step costs; soft warning once at warn_at (recoverable error event); at the cap stop (skip the rest, fail with budget:) or warn; pass the remaining stop budget and step stop budgets to runners as hard limits (agent LoopLimits).
3. BudgetState in workflow.json with badge() for map nodes; map rendering by harness-tui.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Library side done (executor + rerun::BudgetState; test workflow_and_step_budgets_warn_then_stop covers the workflow hard stop, the soft warning text, on_exceed warn, step budget stop/warn and the hard limits passed down; rerun budget_badge test). Remaining (AC#2, DoD#4, harness-tui map owner): show WorkflowState.budget.badge() on the workflow node (read <run dir>/workflow.json, or add the numbers to the run log once harness-events has a budget/metric event) and per-step cost against StepRecord / step budget, with mouse and keyboard tests. Validation: cargo test -p harness-workflow passes; clippy -D warnings clean.

Integration: workflow and step budget badges on map nodes (CoreEvent::NodeBadge, header 'budget ...'). Tests: tui workflow test asserts '/$5.00' badge; map badge tests.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Integration: workflow and step budget badges on map nodes (CoreEvent::NodeBadge, header 'budget ...'). Tests: tui workflow test asserts '/$5.00' badge; map badge tests. Workspace green: cargo fmt --check, clippy -D warnings, cargo test --workspace (1142 passed, 0 failed).
<!-- SECTION:FINAL_SUMMARY:END -->
