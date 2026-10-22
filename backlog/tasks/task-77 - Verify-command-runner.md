---
id: TASK-77
title: Verify command runner
status: Done
assignee:
  - '@hygiene-verify'
created_date: '2026-10-22 14:30'
updated_date: '2026-10-22 16:05'
labels:
  - verify
  - problem-5
milestone: m-7
dependencies:
  - TASK-23
  - TASK-2
priority: high
type: feature
ordinal: 77000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
External verification matters more than the base model (problem 5). Regressions should be detected by comparing against a baseline.

## Loop guard (for agents)

Limit how many times you test and fix. If the same test, build or check still fails after **3 fix attempts**, or you have made no measurable progress in about 30 minutes, **stop**. Do not keep retrying, and never delete, skip, ignore or weaken a test or check to get a green run. Record what you tried, the exact failing output and your best hypothesis in the implementation notes, leave the task In Progress, and ask the user how to proceed.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 Verify command comes from verify section, agent or workflow step
- [x] #2 Optional baseline run before the first edit
- [x] #3 Parses exit status and, when available, structured test output to compute failing test sets
- [x] #4 Reports newly failing tests separately from pre-existing failures
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
1. harness_hygiene::verify: VerifySpec resolution (workflow step > agent > [verify] > autodetect).
2. Runner over harness_tools::exec::Exec (sh -c), emits VerifyStarted/VerifyFinished.
3. Failing-test parsers: libtest text, libtest/nextest JSON, go test -json, pytest summary.
4. VerifyGate with optional baseline before first edit; compare() reports newly failing vs pre-existing vs fixed.
5. Tests + docs.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Implemented harness_hygiene::verify: VerifySpec::resolve (workflow step > agent > [verify] command > detect from Cargo.toml/go.mod/package.json/pyproject.toml/setup.py/Makefile); VerifyRunner over harness_tools::exec::Exec (sh -c, timeout, optional clean env, emits VerifyStarted/VerifyFinished); parse_test_output (libtest/nextest JSON lines and go test -json preferred, then libtest text and pytest summary); compare() -> Comparison{status Passed|PreExistingOnly|NewFailures|Failed, newly_failing, pre_existing, fixed, failing}; VerifyGate::before_first_edit (baseline once when [verify] baseline=true) and verify(). Tests: cargo test -p harness-hygiene verify (7 pass, real LocalExec with a fake suite script). Docs: docs/hygiene.md#verify-gate. Integration: harness-agent calls gate.before_first_edit before the first mutating tool call and gate.verify at end_turn; pass the bash tool's scrubbed env via VerifyRunner::with_env. DoD#4 N/A.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Verify runner with command resolution, optional baseline before first edit, structured/text failing-test parsing and newly-failing vs pre-existing comparison; verified with unit tests and LocalExec integration tests.
<!-- SECTION:FINAL_SUMMARY:END -->
