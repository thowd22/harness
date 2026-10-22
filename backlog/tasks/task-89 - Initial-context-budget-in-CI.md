---
id: TASK-89
title: Initial context budget in CI
status: Done
assignee:
  - '@context-cost-commands'
created_date: '2026-10-22 14:30'
updated_date: '2026-10-22 16:46'
labels:
  - context
  - testing
  - problem-6
milestone: m-8
dependencies:
  - TASK-49
  - TASK-82
priority: medium
type: task
ordinal: 89000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Large initial contexts and tool schemas are a major harness tax (problem 6).

## Loop guard (for agents)

Limit how many times you test and fix. If the same test, build or check still fails after **3 fix attempts**, or you have made no measurable progress in about 30 minutes, **stop**. Do not keep retrying, and never delete, skip, ignore or weaken a test or check to get a green run. Record what you tried, the exact failing output and your best hypothesis in the implementation notes, leave the task In Progress, and ask the user how to proceed.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 CI measures the default agent's initial tokens (system, tools, memory index, skills listing) and fails above the threshold
- [x] #2 Threshold and current value are reported in CI output
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [x] #1 Tests: automated tests cover every acceptance criterion (unit and/or integration as appropriate) and pass locally and in CI; no existing test is deleted, skipped, ignored or weakened to get a green run
- [x] #2 Docs: user-facing behaviour is documented (docs/ and, if the design changed, the relevant backlog doc), public APIs have rustdoc, and any new config keys appear in the harness.ini reference
- [ ] #3 Loop guard respected: no more than 3 fix attempts per failing test or check; if still failing, stop, record attempts, output and hypothesis in implementation notes, and ask the user instead of looping
- [x] #4 UI interaction: if this task makes anything visible in the TUI, every visible interactive element is clickable with the mouse and fully operable with the keyboard (arrows/Tab to focus, Enter to activate, Esc to back out), with tests for both input paths; tasks with no visible UI mark this N/A
<!-- DOD:END -->

## Implementation Plan

<!-- SECTION:PLAN:BEGIN -->
context::initial::InitialContext measures system/tools/memory index/skills of a SessionPrefix; tests/initial_context.rs builds the built-in coder agent (harness_config::builtins), renders its prompt, builds its tools (read/write/edit/bash/todo/memory), a fresh memory drive's SessionIndex and the skill listing, and checks DEFAULT_INITIAL_BUDGET=4000.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
CI runs cargo test --workspace, so the test gates CI. The report line is written straight to stderr (bypassing libtest capture) so it appears in CI output even on success, e.g. 'initial context 1308 tokens (threshold 4000): system 475, tools 818, memory index 15, skills 0'. Added harness-memory as a dev-dependency of harness-agent.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
CI test measures the default agent's initial context and fails above 4000 tokens, always printing threshold and value. Verified with cargo test -p harness-agent --test initial_context (1308 tokens). No UI (DoD#4 N/A).
<!-- SECTION:FINAL_SUMMARY:END -->
