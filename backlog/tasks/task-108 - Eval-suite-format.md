---
id: TASK-108
title: Eval suite format
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
  - TASK-53
  - TASK-14
priority: high
type: feature
ordinal: 108000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Small harness changes move quality noticeably and existing evals miss them (problem 9). The harness needs its own evals.

## Loop guard (for agents)

Limit how many times you test and fix. If the same test, build or check still fails after **3 fix attempts**, or you have made no measurable progress in about 30 minutes, **stop**. Do not keep retrying, and never delete, skip, ignore or weaken a test or check to get a green run. Record what you tried, the exact failing output and your best hypothesis in the implementation notes, leave the task In Progress, and ask the user how to proceed.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 YAML suites define tasks with repo fixture, prompt or workflow, checks (commands, file expectations, hygiene limits) and repeats
- [x] #2 Fixtures can use the fake provider or real providers
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
harness_eval::suite: Suite/Defaults/Task/Check (command with expect, file, unchanged, hygiene)/ProviderSpec (fake | model)/WorkflowSpec, validation, ResolvedTask with defaults applied.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Tests: suite::tests (parse tasks/checks/defaults, inconsistent suites rejected, located YAML errors, filtering); fake-provider tasks run end to end in tests/starter.rs, real-model tasks map to --model in ProcessExecutor (exec tests). Docs: docs/evals.md. No TUI (DoD#4 N/A).
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
YAML eval suites (fixture, inline files, setup, prompt or workflow, fake or real provider, mode/allow, repeats, command/file/unchanged/hygiene checks) in harness_eval::suite, verified by unit tests and the starter suite.
<!-- SECTION:FINAL_SUMMARY:END -->
