---
id: TASK-62
title: Run state reducer
status: Done
assignee:
  - '@events-engineer'
created_date: '2026-10-22 14:30'
updated_date: '2026-10-22 15:08'
labels:
  - visibility
  - events
milestone: m-6
dependencies:
  - TASK-4
documentation:
  - backlog/docs/doc-5 - Agent-Map-and-Run-Visibility.md
priority: high
type: feature
ordinal: 62000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Live, history, replay and headless views must share one fold over events so they never disagree.

## Loop guard (for agents)

Limit how many times you test and fix. If the same test, build or check still fails after **3 fix attempts**, or you have made no measurable progress in about 30 minutes, **stop**. Do not keep retrying, and never delete, skip, ignore or weaken a test or check to get a green run. Record what you tried, the exact failing output and your best hypothesis in the implementation notes, leave the task In Progress, and ask the user how to proceed.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 Incremental reducer builds nodes, edges, status, timings, tokens, cost, retries and host per node
- [x] #2 Reducer is deterministic and snapshot-tested on recorded runs
- [x] #3 Applying an event is O(1) amortized
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
1. reducer.rs: RunState with Vec<NodeState> + HashMap index, edges (needs/spawn), status, timings, tokens, cost, retries, host. 2. apply(&RunEvent) O(1) amortized, deterministic (no hash iteration in output). 3. insta snapshot of a recorded fixture run, determinism + live-vs-replay equality, scaling test. 4. Docs.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
reducer.rs: RunState { nodes Vec + HashMap index, edges (needs/spawn), status, timings, duration, tokens, cost, retries, model/tool call counts, files_changed, pending approvals, last verify, host, topology_version }. apply() does only map lookups and pushes. Tests: unit tests + tests/reducer.rs (insta YAML snapshot of recorded fixture tests/fixtures/fix-bug/events.jsonl, determinism incl. prefix folds, live-vs-replay equality via RunRecorder, per-event cost scaling test 500 vs 8000 nodes). DoD#4 N/A (no UI).
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Deterministic incremental RunState reducer shared by live/history/replay. Verified with insta snapshot on a recorded run, determinism and live==replay tests, and an O(1) scaling test; clippy clean.
<!-- SECTION:FINAL_SUMMARY:END -->
