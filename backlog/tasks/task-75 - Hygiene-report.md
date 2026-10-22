---
id: TASK-75
title: Hygiene report
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
  - TASK-74
priority: high
type: feature
ordinal: 75000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Temporary debug scripts, helper files, unrelated edits and churn are the second-largest failure cluster (problem 3).

## Loop guard (for agents)

Limit how many times you test and fix. If the same test, build or check still fails after **3 fix attempts**, or you have made no measurable progress in about 30 minutes, **stop**. Do not keep retrying, and never delete, skip, ignore or weaken a test or check to get a green run. Record what you tried, the exact failing output and your best hypothesis in the implementation notes, leave the task In Progress, and ask the user how to proceed.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 At node end, classifies changes into in scope, out of scope, new untracked, temp or debug-like, and whitespace-only
- [x] #2 Offers revert of selected files and auto-removal of temp files
- [x] #3 Hygiene score is recorded as a metric
- [x] #4 Report is shown in node detail and before done
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
1. harness_hygiene::report: classify node net changes into in-scope, out-of-scope, new untracked, temp/debug-like, whitespace-only.
2. revert selected files via checkpoints restore_paths; remove temp files.
3. HygieneScore metric (0-100, serde) on the report.
4. render_lines() for node detail / pre-done summary.
5. Tests + docs.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Implemented harness_hygiene::report: HygieneReport::build(ReportInput{root, node_id, changes (Ledger::net_changes), scope, tracked (tracked_files(root), read-only git ls-files), before (CheckpointBefore{store,id})}) -> files with Category set {InScope, OutOfScope, NewUntracked, TempOrDebug, WhitespaceOnly}; temp_or_debug heuristics; whitespace check from checkpoint content or diffs; HygieneScore (0-100 + counts); revert_files(store, checkpoint, paths); remove_temp_files(root, report); record_metric(run_dir, report) appends {name: hygiene.score, node_id, value, counts, ts} to <run_dir>/metrics.jsonl; lines() for display. Tests: cargo test -p harness-hygiene report (6 pass, real git repo + checkpoints). Docs: docs/hygiene.md#hygiene-report. REMAINING (AC#4, DoD#1/#4): harness-tui node detail (TASK-65) and the before-done summary must show HygieneReport::lines() with clickable/keyboard revert and remove-temp actions (calling revert_files / remove_temp_files), with tests for both input paths; harness-agent builds the report at node end. If harness-events later adds a metric EventKind, record_metric should also emit it.

Integration: hygiene report computed after each turn, sent as a Hygiene event to node detail with revert and remove-temp actions, and a notify before done. Tests: tui hygiene_reports_reach_node_detail..., app node detail hygiene tests (both input paths).
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Integration: hygiene report computed after each turn, sent as a Hygiene event to node detail with revert and remove-temp actions, and a notify before done. Tests: tui hygiene_reports_reach_node_detail..., app node detail hygiene tests (both input paths). Workspace green: cargo fmt --check, clippy -D warnings, cargo test --workspace (1142 passed, 0 failed).
<!-- SECTION:FINAL_SUMMARY:END -->
