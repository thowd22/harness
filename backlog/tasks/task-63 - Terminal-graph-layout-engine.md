---
id: TASK-63
title: Terminal graph layout engine
status: Done
assignee:
  - '@visibility-tui'
created_date: '2026-10-22 14:30'
updated_date: '2026-10-22 16:56'
labels:
  - visibility
  - tui
milestone: m-6
dependencies:
  - TASK-62
documentation:
  - backlog/docs/doc-5 - Agent-Map-and-Run-Visibility.md
priority: high
type: feature
ordinal: 63000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
The agent view should look like a map. Workflow DAGs and subagent trees need a readable, stable layout in a character grid.

## Loop guard (for agents)

Limit how many times you test and fix. If the same test, build or check still fails after **3 fix attempts**, or you have made no measurable progress in about 30 minutes, **stop**. Do not keep retrying, and never delete, skip, ignore or weaken a test or check to get a green run. Record what you tried, the exact failing output and your best hypothesis in the implementation notes, leave the task In Progress, and ask the user how to proceed.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 Layered DAG layout with crossing reduction for workflow steps
- [x] #2 Subagent trees attach to their parent node
- [x] #3 Layout is stable as nodes are added and only recomputes on topology change
- [x] #4 Snapshot tests cover linear, fan-out, fan-in, matrix and nested cases
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
1. layout.rs: classify nodes into DAG steps (roots / needs-connected / children of workflow containers) and spawn trees (children of agent nodes).
2. Longest-path layering over needs edges with dummy nodes for long edges; barycenter crossing reduction that only accepts strictly better orders, seeded from the previous order (stability).
3. Place boxes on a character grid for full and compact densities; subagent trees hang below their parent's box.
4. LayoutCache recomputes only when RunState::topology_version changes.
5. Insta snapshot tests (ASCII render of the layout) for linear, fan-out, fan-in, matrix and nested cases.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Implemented crates/harness-tui/src/layout.rs: classify() (steps / workflow containers / spawn-tree items), longest-path layering over needs edges with dummy slots for long edges, barycenter crossing reduction that only accepts strict improvements and is seeded from the previous layout's order (stability), placement for Full and Compact densities with subagent trees hanging below their parent box, orthogonal edge routing with per-target turning columns, LayoutCache keyed by RunState::topology_version + density, and Canvas (clipped grid with junction merging) used by the map. Tests: insta snapshots layout_linear, layout_fan, layout_matrix, layout_nested, layout_nested_compact; crossing_reduction_reorders_a_crossed_layer, layout_is_stable_when_nodes_are_added, cache_recomputes_only_on_topology_change, neighbour_follows_arrow_directions, workflow_containers_are_not_drawn, junctions_merge. DoD#4: the layout itself has no interactive UI (the map view registers every node; see TASK-64). cargo test/clippy -p harness-tui clean.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Layered DAG layout engine with crossing reduction, subagent trees, stable incremental layout and a cached Canvas renderer in harness_tui::layout; verified by 11 unit/snapshot tests (linear, fan-out/in, matrix, nested).
<!-- SECTION:FINAL_SUMMARY:END -->
