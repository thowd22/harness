---
id: TASK-87
title: Prevent context anxiety
status: Done
assignee:
  - '@context-cost-commands'
created_date: '2026-10-22 14:30'
updated_date: '2026-10-22 19:16'
labels:
  - context
  - problem-4
milestone: m-8
dependencies:
  - TASK-86
priority: medium
type: feature
ordinal: 87000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Models wrap up early when they believe they are near the context limit.

## Loop guard (for agents)

Limit how many times you test and fix. If the same test, build or check still fails after **3 fix attempts**, or you have made no measurable progress in about 30 minutes, **stop**. Do not keep retrying, and never delete, skip, ignore or weaken a test or check to get a green run. Record what you tried, the exact failing output and your best hypothesis in the implementation notes, leave the task In Progress, and ask the user how to proceed.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 Remaining context is never exposed to the model
- [x] #2 Default prompts tell the model the harness handles context, so it should keep going
- [x] #3 Eval measures premature stops on long tasks
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
1. Never expose remaining context: no token counts/percentages in any request; context::guard::mentions_remaining_context checker used in tests over every request (agent and summarizer).
2. context::CONTEXT_PROMPT_SECTION text for default prompts.
3. Eval for premature stops: needs harness-eval.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
AC#1 checked: compaction test asserts no request (agent or summarizer) mentions remaining context; stubs and handoffs carry no context numbers. Open AC#2: the default prompts live in harness-config assets (crates/harness-config/assets/prompts/base.md, owned by the TASK-49 engineer); they need CONTEXT_PROMPT_SECTION or equivalent text plus a test. Open AC#3: harness-eval is a stub; it needs an eval measuring premature stops on long tasks.

Integration: assets/prompts/context.md (harness manages context, keep going) is part of coder/researcher/reviewer; long-tasks eval measures premature stops (no-premature-stop task with a failing bad variant). Tests: harness-agent default_prompt_context.rs, harness-eval tests/long_tasks.rs. Docs: docs/context.md, docs/evals.md.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Integration: assets/prompts/context.md (harness manages context, keep going) is part of coder/researcher/reviewer; long-tasks eval measures premature stops (no-premature-stop task with a failing bad variant). Tests: harness-agent default_prompt_context.rs, harness-eval tests/long_tasks.rs. Docs: docs/context.md, docs/evals.md. Workspace green: cargo fmt --check, clippy -D warnings, cargo test --workspace (1142 passed, 0 failed).
<!-- SECTION:FINAL_SUMMARY:END -->
