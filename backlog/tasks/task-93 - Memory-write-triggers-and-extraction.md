---
id: TASK-93
title: Memory write triggers and extraction
status: Done
assignee:
  - '@memory-core'
created_date: '2026-10-22 14:30'
updated_date: '2026-10-22 17:04'
labels:
  - memory
  - problem-4
milestone: m-9
dependencies:
  - TASK-91
documentation:
  - backlog/docs/doc-4 - Memory-System-Design.md
priority: medium
type: feature
ordinal: 93000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Agents fail to learn from corrections. The harness should capture corrections, preferences and gotchas reliably.

## Loop guard (for agents)

Limit how many times you test and fix. If the same test, build or check still fails after **3 fix attempts**, or you have made no measurable progress in about 30 minutes, **stop**. Do not keep retrying, and never delete, skip, ignore or weaken a test or check to get a green run. Record what you tried, the exact failing output and your best hypothesis in the implementation notes, leave the task In Progress, and ask the user how to proceed.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 Default prompts teach when to write: corrections with the rule, preferences, decisions with reasons, gotchas
- [x] #2 Optional end-of-session extraction pass proposes missed lessons using a cheap model
- [x] #3 What-not-to-save rules from the design doc are enforced
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
1. extract::PROMPT_SECTION and tool DESCRIPTION teach the triggers (corrections as rules, preferences, decisions with reasons, gotchas) and the not-saved list.
2. extract::Extractor: redacted transcript + existing memory -> cheap model (FakeProvider in tests) -> JSON candidates -> screen() through tool::guard; write_proposals() writes accepted ones via the tool.
3. Not-saved rules enforced by tool::guard for both tool writes and extraction.
4. Tests.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
AC#2 and AC#3 done and tested (extract unit tests, tests/memory.rs extraction_pass_proposes_screened_lessons_with_a_cheap_model, guard tests). AC#1 remains: the teaching text exists (harness_memory::extract::PROMPT_SECTION) and is in the memory tool description, which every memory-enabled request carries, but the bundled default prompts (TASK-49, harness-config builtins) do not exist yet. To finish: TASK-49 includes PROMPT_SECTION (or equivalent text) in the default prompt for memory: read-write agents, adds a test, then check AC#1 and DoD#1. The extraction pass is opt-in via API; there is no [memory] ini key to enable it (proposed: [memory] extract = true, owned by harness-config).

TASK-49 (memory-evals-quality): the built-in coder prompt (memory: read-write) includes crates/harness-config/assets/prompts/memory.md, identical to extract::PROMPT_SECTION. Tests: crates/harness-memory/tests/default_prompt.rs (memory_layer_matches_prompt_section, read_write_default_agents_render_the_memory_section) and harness-config builtins::tests::read_write_memory_agents_include_the_memory_layer.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Default prompts teach the memory triggers (PROMPT_SECTION in the coder prompt via TASK-49); extraction pass and not-saved rules were already done and tested.
<!-- SECTION:FINAL_SUMMARY:END -->
