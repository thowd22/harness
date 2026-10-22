---
id: TASK-88
title: Cost tracking and budgets
status: Done
assignee:
  - '@context-cost-commands'
created_date: '2026-10-22 14:30'
updated_date: '2026-10-22 19:16'
labels:
  - context
  - problem-6
milestone: m-8
dependencies:
  - TASK-82
  - TASK-2
priority: high
type: feature
ordinal: 88000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Users report quota burning faster than expected and want control over spend.

## Loop guard (for agents)

Limit how many times you test and fix. If the same test, build or check still fails after **3 fix attempts**, or you have made no measurable progress in about 30 minutes, **stop**. Do not keep retrying, and never delete, skip, ignore or weaken a test or check to get a green run. Record what you tried, the exact failing output and your best hypothesis in the implementation notes, leave the task In Progress, and ask the user how to proceed.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 Budgets per session, run and day from the budget section, plus agent and workflow budgets
- [x] #2 Soft warning at 80 percent and on_exceed ask or stop at the limit
- [x] #3 /cost shows breakdown by model, agent and node
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
1. budget::{Budgets, BudgetScope, BudgetMeter (shared), BudgetGuard (RequestHook), BudgetApprover, DenyOverruns}: session/run/day from [budget], agent (with_agent_limit) and workflow limits.
2. 80% one-time warning event; at limit stop (StopReason::Budget) or ask approver (raise by original limit).
3. cost::CostReport for /cost by model, agent and node.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Tests: budget::tests, tests/budgets_and_done_gating.rs (session warn+stop, ask approve/refuse, daily baseline, agent limit, cost report). Open AC#3: CostReport::lines and the /cost built-in spec exist and are tested, but the TUI has no /cost view yet (needs rendering CostReport and mouse/keyboard tests, DoD#4). Also the binary should attach BudgetGuard(BudgetMeter::new(Budgets::from_config(&cfg.budget)).with_spent(Day, daily_spend(..))) to every loop, with a TUI BudgetApprover for on_exceed=ask.

Integration: /cost panel shows breakdown by model, agent and node; budget badges on map nodes. Tests: tui builtin_commands_open_panels, app panel tests (mouse and keyboard).
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Integration: /cost panel shows breakdown by model, agent and node; budget badges on map nodes. Tests: tui builtin_commands_open_panels, app panel tests (mouse and keyboard). Workspace green: cargo fmt --check, clippy -D warnings, cargo test --workspace (1142 passed, 0 failed).
<!-- SECTION:FINAL_SUMMARY:END -->
