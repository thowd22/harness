---
id: TASK-84
title: Reasoning preservation tests
status: Done
assignee:
  - '@context-cost-commands'
created_date: '2026-10-22 14:30'
updated_date: '2026-10-24 01:15'
labels:
  - context
  - problem-4
  - problem-9
milestone: m-8
dependencies:
  - TASK-11
  - TASK-12
  - TASK-13
priority: high
type: task
ordinal: 84000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Dropping or clearing reasoning content incorrectly silently degrades quality.

## Loop guard (for agents)

Limit how many times you test and fix. If the same test, build or check still fails after **3 fix attempts**, or you have made no measurable progress in about 30 minutes, **stop**. Do not keep retrying, and never delete, skip, ignore or weaken a test or check to get a green run. Record what you tried, the exact failing output and your best hypothesis in the implementation notes, leave the task In Progress, and ask the user how to proceed.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 Tests assert reasoning blocks or items are retained across tool loops as each provider requires
- [x] #2 Any intentional clearing is explicit, configurable and covered by a test
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
1. Integration tests run a tool loop with the fake provider emitting signed thinking / OpenAI encrypted reasoning items / OpenRouter reasoning_details, then build each provider's real request body.
2. Explicit clearing: context::ReasoningRetention {KeepAll (default), DropCompletedTurns} applied by ContextManager, tested.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
tests/request_layout.rs: anthropic_signed_thinking_is_resent_unchanged_across_the_tool_loop, openai_encrypted_reasoning_items_are_resent_verbatim, openrouter_reasoning_details_are_resent_on_assistant_messages, clearing_reasoning_is_explicit_and_only_touches_finished_turns; unit test in context::tests. Clearing is configured via ContextSettings::reasoning (library setting); no ini key added. A [context] keep_reasoning = all|current_turn key could be added by the harness-config owner if users should control it.

Audit 2026-10-24: DoD#3 (loop guard) checked. It is a process item: the notes record no unresolved failing check and no fix loop. Re-verified: cargo nextest run -p harness-agent -E 'binary(request_layout)' passes the anthropic/openai/openrouter reasoning tests and clearing_reasoning_is_explicit_and_only_touches_finished_turns; context::tests::reasoning_is_kept_by_default_and_dropped_only_when_configured passes; main CI green at cfb7027 (https://github.com/thowd22/harness/actions/runs/40048623347).
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Reasoning preservation is tested end to end for Anthropic, OpenAI and OpenRouter request bodies across tool loops; the only clearing is the explicit ReasoningRetention::DropCompletedTurns setting, which never touches the current tool loop. Verified with cargo test -p harness-agent --test request_layout. No UI (DoD#4 N/A).
<!-- SECTION:FINAL_SUMMARY:END -->
