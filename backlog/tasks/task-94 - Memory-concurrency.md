---
id: TASK-94
title: Memory concurrency
status: Done
assignee:
  - '@memory-core'
created_date: '2026-10-22 14:30'
updated_date: '2026-10-22 16:02'
labels:
  - memory
milestone: m-9
dependencies:
  - TASK-91
documentation:
  - backlog/docs/doc-4 - Memory-System-Design.md
priority: medium
type: feature
ordinal: 94000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Parallel sessions and subagents may write memory at the same time.

## Loop guard (for agents)

Limit how many times you test and fix. If the same test, build or check still fails after **3 fix attempts**, or you have made no measurable progress in about 30 minutes, **stop**. Do not keep retrying, and never delete, skip, ignore or weaken a test or check to get a green run. Record what you tried, the exact failing output and your best hypothesis in the implementation notes, leave the task In Progress, and ask the user how to proceed.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 Each session uses its own worktree of the drive
- [x] #2 Stale writes are rejected and retried after rebase
- [x] #3 Conflicting edits are merged, never overwritten, and tests cover concurrent writers
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
1. sync.rs: SessionCheckout::open creates .harness/worktrees/<session> on branch session/<session> (resume rebases onto main); write(msg, edit) rebases, edits, commits, publishes.
2. Publish: under .harness/drive.lock, fast-forward main if main is an ancestor; else stale -> rebase under lock and retry (max 8).
3. .gitattributes '*.md merge=union' so concurrent edits merge rather than conflict/overwrite.
4. Tests: own worktrees, deterministic stale-write test, 6 threads x 5 writes to the same notes.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
First attempt rebased outside the lock and livelocked under 6 concurrent writers (StaleWrite after 8 attempts, ~2/15 runs). Fixed by rebasing while holding the drive lock on retries; 25/25 repeated runs pass. Known limit of union merge: a forget racing an adjacent add can resurrect the forgotten bullet (documented in docs/memory.md).
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Implemented harness_memory::sync (SessionCheckout, Published, drive lock). Verified by tests/memory.rs sessions_use_their_own_worktrees, stale_write_is_rejected_then_rebased_and_retried, concurrent_writers_merge_never_overwrite (run 25x without failure). clippy clean. DoD#4 N/A.
<!-- SECTION:FINAL_SUMMARY:END -->
