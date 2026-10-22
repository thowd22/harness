---
id: TASK-73
title: Checkpoints and /undo
status: Done
assignee:
  - '@hygiene-verify'
created_date: '2026-10-22 14:30'
updated_date: '2026-10-22 19:16'
labels:
  - hygiene
  - problem-3
milestone: m-7
dependencies:
  - TASK-72
  - TASK-7
priority: high
type: feature
ordinal: 73000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Users need a cheap way to roll back an agent's changes without touching their own git history.

## Loop guard (for agents)

Limit how many times you test and fix. If the same test, build or check still fails after **3 fix attempts**, or you have made no measurable progress in about 30 minutes, **stop**. Do not keep retrying, and never delete, skip, ignore or weaken a test or check to get a green run. Record what you tried, the exact failing output and your best hypothesis in the implementation notes, leave the task In Progress, and ask the user how to proceed.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 Shadow git repository with a separate git dir snapshots the worktree before each mutating turn
- [x] #2 /undo restores the previous turn, and restore to any node checkpoint is available from the map
- [x] #3 User's git index, branches and stash are never modified
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
1. harness_hygiene::checkpoints: shadow git repo (separate --git-dir under DataDir::checkpoints()/<project-hash>.git, --work-tree=root, sanitized git env) .
2. snapshot via add -A + write-tree + commit-tree, checkpoint index (json) with node/turn/label; TurnCheckpointer snapshots once per mutating turn.
3. restore(id) (read-tree -u --reset after snapshotting current state so restore is undoable), undo() restores the last turn, restore_paths for report reverts, for_node() for the map.
4. Tests on a temp user git repo asserting index, branches, stash, HEAD untouched.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Implemented harness_hygiene::checkpoints: CheckpointStore::open(DataDir::checkpoints(), root) creates a shadow git dir <checkpoints>/<project-hash>.git used with explicit --git-dir/--work-tree, GIT_* env removed, global/system config ignored, hooks off. snapshot (add -A into the shadow index + write-tree + commit-tree, reuses the commit when the tree is unchanged), list/get/for_node, restore(id) (safety snapshot then read-tree -u --reset), undo() (latest not-undone Turn checkpoint, repeatable), restore_paths, file_text, changed_since, diff_since; TurnCheckpointer::before_mutation(node, turn) snapshots once per turn. Tests: cargo test -p harness-hygiene checkpoints (5 pass) assert the user's .git/index bytes, branches, stash, HEAD and staged diff are unchanged across snapshot/undo/restore; ignored files untouched; works outside a git repo. Docs: docs/hygiene.md#checkpoints-and-undo. REMAINING (AC#2 integration, DoD#1/#4): the /undo slash command (harness-agent commands / TUI) must call CheckpointStore::undo via spawn_blocking, and the map (harness-tui) needs a 'restore checkpoint' action listing CheckpointStore::for_node(node) and calling restore(id), with mouse + keyboard tests. harness-agent must call TurnCheckpointer::before_mutation before the first mutating tool call of each turn.

Integration: Core checkpoints before each turn; /undo restores the previous turn; map 'r' / restore action restores any turn checkpoint. Tests: tui undo_and_diff_use_the_turn_checkpoint, any_turn_checkpoint_can_be_restored, app restore tests (mouse and keyboard).
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Integration: Core checkpoints before each turn; /undo restores the previous turn; map 'r' / restore action restores any turn checkpoint. Tests: tui undo_and_diff_use_the_turn_checkpoint, any_turn_checkpoint_can_be_restored, app restore tests (mouse and keyboard). Workspace green: cargo fmt --check, clippy -D warnings, cargo test --workspace (1142 passed, 0 failed).
<!-- SECTION:FINAL_SUMMARY:END -->
