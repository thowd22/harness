---
id: TASK-48
title: /context inspector
status: Done
assignee:
  - '@visibility-tui'
created_date: '2026-10-22 14:30'
updated_date: '2026-10-22 16:58'
labels:
  - prompts
  - tui
  - problem-2
  - problem-6
milestone: m-4
dependencies:
  - TASK-43
  - TASK-82
priority: medium
type: feature
ordinal: 48000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Users and maintainers need to see exactly what the model receives to debug instruction conflicts and token bloat.

## Loop guard (for agents)

Limit how many times you test and fix. If the same test, build or check still fails after **3 fix attempts**, or you have made no measurable progress in about 30 minutes, **stop**. Do not keep retrying, and never delete, skip, ignore or weaken a test or check to get a green run. Record what you tried, the exact failing output and your best hypothesis in the implementation notes, leave the task In Progress, and ask the user how to proceed.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 Shows each prompt layer, tool schema, memory index and skill listing with source file and token count
- [x] #2 Total initial context and percentage of window are shown
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
1. inspector/context.rs ContextReport: items for prompt layers (source = prompt YAML or file: path), tool schemas, memory index, skill listing, each with estimated tokens; total and % of the window. 2. ContextView with expandable, clickable/focusable rows. 3. Tests.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Implemented harness_tui::inspector::context (ContextKind, ContextItem, ContextReport::{new, push, add_prompt, add_tools, add_memory, add_skills, total, percent, by_kind}, ContextView, ContextOutput). Tests: lists_layers_tools_memory_and_skills_with_sources_and_tokens (AC1, AC2), rows_expand_by_click_and_keyboard. Wiring for the slash-command owner: on /context build ContextReport::new(Some(capabilities.context_window)), add_prompt(&catalog.prompts.get_entry(agent.prompt)), add_tools(&request_tools), add_memory(SessionIndex::text(), MEMORY.md path), add_skills(&skills::listing(..), dir), and show ContextView.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
/context inspector component listing prompt layers, tool schemas, memory index and skill listing with sources and token estimates, plus total and window percentage; verified by 2 tests (content, mouse/keyboard expansion).
<!-- SECTION:FINAL_SUMMARY:END -->
