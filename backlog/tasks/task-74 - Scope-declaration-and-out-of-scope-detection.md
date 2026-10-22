---
id: TASK-74
title: Scope declaration and out-of-scope detection
status: Done
assignee:
  - '@hygiene-verify'
created_date: '2026-10-22 14:30'
updated_date: '2026-10-22 16:05'
labels:
  - hygiene
  - problem-2
  - problem-3
milestone: m-7
dependencies:
  - TASK-72
  - TASK-44
priority: high
type: feature
ordinal: 74000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Positive scope replaces blanket prohibitions (problem 2) and makes unrequested changes detectable (problem 3).

## Loop guard (for agents)

Limit how many times you test and fix. If the same test, build or check still fails after **3 fix attempts**, or you have made no measurable progress in about 30 minutes, **stop**. Do not keep retrying, and never delete, skip, ignore or weaken a test or check to get a green run. Record what you tried, the exact failing output and your best hypothesis in the implementation notes, leave the task In Progress, and ask the user how to proceed.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 Scope globs come from agent, workflow step or session
- [x] #2 Edits outside scope ask or are flagged per policy mode
- [x] #3 Scope is shown to the model as allowed areas, never as prohibitions
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
1. harness_hygiene::scope: Scope{globs, source} resolved from session > workflow step > agent.
2. Scope::check_write(path, Mode) -> InScope | OutOfScope{Ask|Flag} (ask in ask/auto-edit, flag in auto), classify ledger paths.
3. prompt_section(): allowed areas only, verified with harness_config::lint::blanket_prohibitions.
4. Tests + docs.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Implemented harness_hygiene::scope (Scope::new/resolve/contains/contains_path/check_write/prompt_section, ScopeSource, ScopeDecision{InScope,Ask,Flag}). Resolution order session > workflow step > agent. check_write asks in ask/auto-edit and flags in auto. prompt_section phrases scope as allowed areas and is checked with harness_config::lint::blanket_prohibitions in tests. Tests: cargo test -p harness-hygiene scope (4 pass). Docs: docs/hygiene.md#scope. Integration for harness-agent: resolve the Scope per node, call check_write before write/edit tool calls (Ask -> ApprovalBroker, Flag -> allow), append prompt_section() to the system prompt. DoD#4 N/A (no UI in this crate).
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Added positive scope globs (session/step/agent) with ask-or-flag out-of-scope detection per mode and an allowed-areas prompt section; verified by unit tests including a lint check for blanket prohibitions.
<!-- SECTION:FINAL_SUMMARY:END -->
