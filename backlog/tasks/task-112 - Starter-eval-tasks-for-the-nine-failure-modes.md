---
id: TASK-112
title: Starter eval tasks for the nine failure modes
status: Done
assignee:
  - '@memory-evals-quality'
created_date: '2026-10-22 14:30'
updated_date: '2026-10-22 17:05'
labels:
  - eval
  - problem-2
  - problem-3
  - problem-4
  - problem-5
milestone: m-11
dependencies:
  - TASK-109
  - TASK-49
  - TASK-75
  - TASK-78
priority: high
type: task
ordinal: 112000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
The research identified specific failure modes; each needs a trap task to keep them fixed.

## Loop guard (for agents)

Limit how many times you test and fix. If the same test, build or check still fails after **3 fix attempts**, or you have made no measurable progress in about 30 minutes, **stop**. Do not keep retrying, and never delete, skip, ignore or weaken a test or check to get a green run. Record what you tried, the exact failing output and your best hypothesis in the implementation notes, leave the task In Progress, and ask the user how to proceed.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 Includes a do-not-modify-tests instruction trap, a temp-file hygiene trap, a regression trap, a long-context task, a false-done verification trap and a broad git add trap
- [x] #2 Each task documents which problem it targets
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
Starter suite crates/harness-eval/suites/starter (sh fixture, six trap tasks with problem labels, good fake scripts and bad ones under scripts/bad); tests that good behaviour passes and each bad behaviour is caught.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Traps: no-test-tampering (problem-2), temp-file-hygiene (problem-3), regression-trap (problem-5), long-context (problem-4, 6000-line file), false-done (problem-5, premature stop), broad-git-add (problem-3). Tests: tests/starter.rs (every trap present with problem + description; good scripts pass in-process; bad scripts fail, false-done counted as premature stop, temp file counted as hygiene violation, test tampering caught by unchanged). Docs: docs/evals.md#starter-traps.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Six trap tasks for the known failure modes, each naming its problem, with good and bad fake scripts; verified by tests/starter.rs.
<!-- SECTION:FINAL_SUMMARY:END -->
