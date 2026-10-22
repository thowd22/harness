---
id: TASK-50
title: Built-in slash commands
status: Done
assignee:
  - '@context-cost-commands'
created_date: '2026-10-22 14:30'
updated_date: '2026-10-22 19:16'
labels:
  - commands
milestone: m-4
dependencies:
  - TASK-45
priority: high
type: feature
ordinal: 50000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Core actions need discoverable commands that also appear in the palette.

## Loop guard (for agents)

Limit how many times you test and fix. If the same test, build or check still fails after **3 fix attempts**, or you have made no measurable progress in about 30 minutes, **stop**. Do not keep retrying, and never delete, skip, ignore or weaken a test or check to get a green run. Record what you tried, the exact failing output and your best hypothesis in the implementation notes, leave the task In Progress, and ask the user how to proceed.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 Provides help, new, resume, model, mode, agents, workflows, skills, memory, runs, remote, cost, compact, undo, diff, context, config, doctor and quit
- [x] #2 Each built-in has a description and argument help in the palette
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
commands::builtin::Builtin enum with the 19 built-ins, descriptions and ArgSpec lists (choices, defaults, help); registered first in CommandRegistry.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Test every_builtin_has_a_description_and_argument_help and builtin_arguments_are_validated_with_helpful_errors. Open AC#2 (and DoD#4): the palette must list the registry's built-ins with description and arg_help (harness-tui palette::builtin_entries owner), with mouse/keyboard tests. The actions themselves are carried out by the core: /cost -> cost::CostReport::lines, /compact -> ContextManager::request_compaction/compact_now, /undo -> harness_hygiene CheckpointStore::undo, etc.

Integration: every built-in is a palette entry with description and argument help (tui tests palette_lists_every_builtin...; app palette tests cover mouse and keyboard).
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Integration: every built-in is a palette entry with description and argument help (tui tests palette_lists_every_builtin...; app palette tests cover mouse and keyboard). Workspace green: cargo fmt --check, clippy -D warnings, cargo test --workspace (1142 passed, 0 failed).
<!-- SECTION:FINAL_SUMMARY:END -->
