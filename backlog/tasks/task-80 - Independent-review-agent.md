---
id: TASK-80
title: Independent review agent
status: Done
assignee:
  - '@hygiene-verify'
created_date: '2026-10-22 14:30'
updated_date: '2026-10-22 16:05'
labels:
  - verify
  - problem-3
  - problem-5
milestone: m-7
dependencies:
  - TASK-51
  - TASK-72
priority: medium
type: feature
ordinal: 80000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Agents are poor judges of their own work. A separate reviewer, ideally on a different model, sees only the task and the diff.

## Loop guard (for agents)

Limit how many times you test and fix. If the same test, build or check still fails after **3 fix attempts**, or you have made no measurable progress in about 30 minutes, **stop**. Do not keep retrying, and never delete, skip, ignore or weaken a test or check to get a green run. Record what you tried, the exact failing output and your best hypothesis in the implementation notes, leave the task In Progress, and ask the user how to proceed.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 Reviewer receives task, scope and diff, not the implementer transcript
- [x] #2 Rubric covers correctness, unrequested changes, over-engineering, dead or compatibility code and test adequacy
- [x] #3 Returns a structured verdict usable by workflow loops
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
1. harness_hygiene::review: ReviewInput{task, scope, diff} (no transcript by construction) built from ledger/checkpoint diff.
2. Rubric (correctness, unrequested changes, over-engineering, dead/compat code, test adequacy) in the reviewer system prompt, JSON verdict schema.
3. review(provider, model, input) -> ReviewVerdict{verdict approve|request_changes, findings, scores}, outputs() for workflow loops.
4. Tests with FakeProvider.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Implemented harness_hygiene::review: ReviewInput{task, scope, diff} (new/from_ledger/from_checkpoint; no transcript field by construction), RUBRIC (correctness, unrequested_changes, over_engineering, dead_or_compat_code, test_adequacy), system_prompt() (lint clean), build_request(model, input) (single user message, no tools), ReviewVerdict{verdict Approve|RequestChanges, summary, criteria, findings} with parse() (fenced JSON ok, all criteria required, approve downgraded on failed criterion/blocker) and outputs() for workflow loops, async review(provider, model, input, cancel) with one repair retry. Tests: cargo test -p harness-hygiene review (5 pass, FakeProvider). Docs: docs/hygiene.md#independent-review. DoD#4 N/A.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Independent reviewer gets only task/scope/diff, grades a 5-point rubric and returns a structured verdict whose outputs drive workflow loops; verified with FakeProvider tests.
<!-- SECTION:FINAL_SUMMARY:END -->
