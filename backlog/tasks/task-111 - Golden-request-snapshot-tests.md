---
id: TASK-111
title: Golden request snapshot tests
status: Done
assignee:
  - '@memory-evals-quality'
created_date: '2026-10-22 14:30'
updated_date: '2026-10-22 17:04'
labels:
  - testing
  - problem-9
milestone: m-11
dependencies:
  - TASK-49
  - TASK-11
  - TASK-12
  - TASK-13
priority: high
type: task
ordinal: 111000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Any change to prompt assembly, tool schemas or defaults should show up as a reviewable diff.

## Loop guard (for agents)

Limit how many times you test and fix. If the same test, build or check still fails after **3 fix attempts**, or you have made no measurable progress in about 30 minutes, **stop**. Do not keep retrying, and never delete, skip, ignore or weaken a test or check to get a green run. Record what you tried, the exact failing output and your best hypothesis in the implementation notes, leave the task In Progress, and ask the user how to proceed.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 Snapshot tests of serialized requests for default agents on each provider
- [x] #2 Snapshot changes require explicit acceptance in review
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
insta snapshots in crates/harness-eval/tests/golden_requests.rs: rendered default prompts and the Anthropic/OpenAI/OpenRouter request bodies for every built-in agent; guard tests for agent coverage and unaccepted .snap.new files.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Snapshots under crates/harness-eval/tests/snapshots/. CI=true cargo test -p harness-eval --test golden_requests passes without writing. Changing anything fails until accepted with cargo insta review; no_unreviewed_snapshots_are_left_behind fails on committed .snap.new files; every_builtin_agent_has_a_request_snapshot pins the agent list. The todo tool is listed under tools_not_built_yet until harness-agent provides it (its snapshot will then change, by design). Docs: docs/golden-snapshots.md.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Golden request snapshots for every default agent on each provider plus rendered prompts, with explicit acceptance via insta review; verified by tests/golden_requests.rs.
<!-- SECTION:FINAL_SUMMARY:END -->
