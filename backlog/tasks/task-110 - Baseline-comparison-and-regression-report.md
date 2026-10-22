---
id: TASK-110
title: Baseline comparison and regression report
status: Done
assignee:
  - '@memory-evals-quality'
created_date: '2026-10-22 14:30'
updated_date: '2026-10-22 17:04'
labels:
  - eval
  - problem-9
milestone: m-11
dependencies:
  - TASK-109
  - TASK-114
priority: high
type: feature
ordinal: 110000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Changes to prompts or defaults must be compared against a baseline before shipping.

## Loop guard (for agents)

Limit how many times you test and fix. If the same test, build or check still fails after **3 fix attempts**, or you have made no measurable progress in about 30 minutes, **stop**. Do not keep retrying, and never delete, skip, ignore or weaken a test or check to get a green run. Record what you tried, the exact failing output and your best hypothesis in the implementation notes, leave the task In Progress, and ask the user how to proceed.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 Compares results against a stored baseline with per-metric deltas
- [x] #2 Fails when pass rate drops or cost rises beyond thresholds
- [x] #3 Report is readable in the terminal and as JSON
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
harness_eval::baseline::compare(baseline, current, Thresholds) -> Comparison with overall and per-task MetricDelta, failures and passed; render() for the terminal, to_json(); CLI compare and run --baseline.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Tests: baseline::tests (equal runs pass, pass-rate drop fails overall and per task, threshold tolerance, cost rise beyond 10% fails, cheaper never regresses, added/removed tasks, JSON round trip); cli tests (compare --json, run --baseline). Docs: docs/evals.md. The harness eval subcommand wiring in the binary is tracked on TASK-109.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Baseline comparison with per-metric deltas, pass-rate and cost thresholds, terminal and JSON reports (harness_eval::baseline, cli compare/run --baseline), verified by unit and CLI tests.
<!-- SECTION:FINAL_SUMMARY:END -->
