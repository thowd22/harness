---
id: TASK-79
title: Bug-fix proof step
status: Done
assignee:
  - '@hygiene-verify'
created_date: '2026-10-22 14:30'
updated_date: '2026-10-22 16:05'
labels:
  - verify
  - workflow
  - problem-5
milestone: m-7
dependencies:
  - TASK-77
  - TASK-73
  - TASK-53
priority: medium
type: feature
ordinal: 79000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
A strong signal that a bug fix is real: the test fails before, passes after, and fails again when the fix is reverted.

## Loop guard (for agents)

Limit how many times you test and fix. If the same test, build or check still fails after **3 fix attempts**, or you have made no measurable progress in about 30 minutes, **stop**. Do not keep retrying, and never delete, skip, ignore or weaken a test or check to get a green run. Record what you tried, the exact failing output and your best hypothesis in the implementation notes, leave the task In Progress, and ask the user how to proceed.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 bugfix-proof runs the test on original code (expect fail), with fix (expect pass), with fix reverted (expect fail), then restores the fix
- [x] #2 Each phase is a visible sub-step with logs
- [x] #3 Worktree is always restored even on cancel
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
1. harness_hygiene::proof: BugfixProof runs phases original(expect fail), fixed(expect pass), reverted(expect fail), restore, over a ProofWorktree trait implemented by checkpoints.
2. Each phase is a child node with VerifyStarted/Finished events and logs.
3. Restore guaranteed on cancel (CancellationToken) and on drop.
4. Tests with a temp git worktree and a fake test command.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Implemented harness_hygiene::proof: Phase{Original,Fixed,Reverted,Restore}, ProofWorktree trait, CheckpointWorktree::prepare(store, original_checkpoint, keep globs) (snapshots the fix, swaps only changed non-kept paths; DEFAULT_KEEP=tests/**), BugfixProof::new(runner, worktree, test_command, proof_node).run(&CancellationToken) -> ProofReport{phases, proven} with outputs(). Each phase is a child node <proof>.<phase> (NodeStarted with parent, VerifyStarted/Finished logs, NodeFinished). Fix restored after the last phase, on error, on cancel (select on token) and on drop/panic (RestoreGuard). Tests: cargo test -p harness-hygiene proof (6 pass, incl. reducer showing 4 child nodes, cancel and dropped-future restore). Docs: docs/hygiene.md#bug-fix-proof. Integration: harness-workflow executor maps the bugfix-proof step type to BugfixProof using the fix step's first turn checkpoint. DoD#4 N/A (map shows the child nodes via existing reducer).
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Bug-fix proof runs fail-before/pass-after/fail-on-revert then restores, each phase a child node with verify logs; restore is guaranteed on cancel/drop. Verified by integration tests on a temp worktree.
<!-- SECTION:FINAL_SUMMARY:END -->
