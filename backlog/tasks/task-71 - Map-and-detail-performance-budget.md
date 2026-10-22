---
id: TASK-71
title: Map and detail performance budget
status: Done
assignee:
  - '@visibility-tui'
created_date: '2026-10-22 14:30'
updated_date: '2026-10-22 16:57'
labels:
  - visibility
  - tui
  - testing
milestone: m-6
dependencies:
  - TASK-64
  - TASK-65
priority: medium
type: task
ordinal: 71000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Visibility must not make the UI slow.

## Loop guard (for agents)

Limit how many times you test and fix. If the same test, build or check still fails after **3 fix attempts**, or you have made no measurable progress in about 30 minutes, **stop**. Do not keep retrying, and never delete, skip, ignore or weaken a test or check to get a green run. Record what you tried, the exact failing output and your best hypothesis in the implementation notes, leave the task In Progress, and ask the user how to proceed.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 Benchmarks show map render under 16 ms at 200 nodes and detail under 16 ms at 10k log entries
- [x] #2 Benchmarks run in CI with a regression threshold
- [x] #3 Benchmark includes 10 concurrent live streams and the goal bar
- [x] #4 Benchmark includes spinner animation and verifies idle CPU stays near zero when nothing is running
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
1. tests/perf.rs: steady-state frame timing via TestBackend for map at 200 nodes, detail at 10k entries, 10 live streams + goal bar, spinner frames. 2. Budget 16 ms in release, scaled x4 in debug (CI cargo test), HARNESS_PERF_BUDGET_MS override. 3. Assert idle map requests no ticks and no stream redraws.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
crates/harness-tui/tests/perf.rs. Release medians (cargo test --release -p harness-tui --test perf): map 200 nodes 0.91 ms, detail 10k entries 3.66 ms, 10 live streams + goal bar 0.43 ms, spinner frames 0.19 ms. Debug medians ~9.5 ms / 14.3 ms against the 64 ms debug budget. The CI test job (cargo test --workspace) runs them, so a regression over the threshold fails CI. Idle: spinner_animates_only_while_something_runs asserts needs_tick is false when nothing runs or the map is hidden, ticks never recompute layout, and no stream changes are pending. DoD4 N/A (benchmarks only). Optional CI follow-up (ci.yml not owned here): a release-mode perf job for the 16 ms number.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Frame-budget benchmarks for map (200 nodes), detail (10k entries), 10 live streams + goal bar and spinner/idle behaviour, run by the CI test job with a regression threshold; release medians all under 4 ms.
<!-- SECTION:FINAL_SUMMARY:END -->
