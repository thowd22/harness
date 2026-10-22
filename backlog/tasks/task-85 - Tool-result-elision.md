---
id: TASK-85
title: Tool-result elision
status: Done
assignee:
  - '@context-cost-commands'
created_date: '2026-10-22 14:30'
updated_date: '2026-10-22 16:46'
labels:
  - context
  - problem-4
  - problem-6
milestone: m-8
dependencies:
  - TASK-24
  - TASK-82
priority: medium
type: feature
ordinal: 85000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Old, large tool results dominate context in long sessions.

## Loop guard (for agents)

Limit how many times you test and fix. If the same test, build or check still fails after **3 fix attempts**, or you have made no measurable progress in about 30 minutes, **stop**. Do not keep retrying, and never delete, skip, ignore or weaken a test or check to get a green run. Record what you tried, the exact failing output and your best hypothesis in the implementation notes, leave the task In Progress, and ask the user how to proceed.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 Tool results older than N turns and larger than elide_tool_results_over are replaced with stubs and blob pointers
- [x] #2 Elision happens at cache-friendly boundaries to avoid invalidating the prefix every turn
- [x] #3 The model can re-read elided content
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
1. compaction::elide::Elider replaces text tool results over elide_tool_results_over tokens and older than elide_after_turns with a stub pointing at spill:<blob id> (stored via harness_tools BlobSink).
2. Batching: only when the oldest eligible result is 2N turns old, then elide all >= N (prefix changes at most once per N turns).
3. ContextManager runs it before each request and calls Conversation::invalidate_log_from so resume rebuilds the stubs.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Tests: tests/context_management.rs old_large_tool_results_are_elided_in_batches_and_can_be_reread (stubs, bounded prefix breaks, re-read through ReadTool spill:<id>, rebuild_conversation equals history). Production wiring (binary/TUI must add ContextManager::with_elision(Arc<RunBlobSink>) as a loop hook, using the same sink as the read tool) is listed as a cross-crate need.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Batched, cache-friendly tool-result elision with spill:<id> blob pointers re-readable via the read tool, recorded in the log for resume. Verified with cargo test -p harness-agent --test context_management. No UI (DoD#4 N/A).
<!-- SECTION:FINAL_SUMMARY:END -->
