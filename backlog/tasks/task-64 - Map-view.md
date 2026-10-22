---
id: TASK-64
title: Map view
status: Done
assignee:
  - '@visibility-tui'
created_date: '2026-10-22 14:30'
updated_date: '2026-10-22 19:16'
labels:
  - visibility
  - tui
milestone: m-6
dependencies:
  - TASK-117
  - TASK-32
  - TASK-39
  - TASK-63
documentation:
  - backlog/docs/doc-5 - Agent-Map-and-Run-Visibility.md
priority: high
type: feature
ordinal: 64000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
The primary visibility surface: a live map of every agent and step in a run.

## Loop guard (for agents)

Limit how many times you test and fix. If the same test, build or check still fails after **3 fix attempts**, or you have made no measurable progress in about 30 minutes, **stop**. Do not keep retrying, and never delete, skip, ignore or weaken a test or check to get a green run. Record what you tried, the exact failing output and your best hypothesis in the implementation notes, leave the task In Progress, and ask the user how to proceed.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 Nodes show status icon, name, agent and model, host badge, duration, cost and retry or loop counts
- [x] #2 Edges render with box drawing characters
- [x] #3 Arrow keys move selection, the viewport pans to selection, selection follows the active node until the user moves it
- [x] #4 Large graphs switch to compact nodes automatically
- [x] #5 Tab toggles chat and map; Enter opens node detail
- [x] #6 Every node is clickable and keyboard-selectable through the interaction layer
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
1. map.rs MapView owning RunState + LayoutCache (full/compact) + live streams. 2. Draw nodes (status glyph via indicators::node_glyph, name, agent/model, host badge, duration, cost, retries, tokens) and edges on layout::Canvas. 3. Arrow/hjkl neighbour navigation, pan-to-selection, follow-active until moved. 4. Auto compact density. 5. Enter -> MapOutput::OpenDetail; every node registered in the HitMap. 6. Tests with TestBackend for mouse and keyboard.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Implemented harness_tui::map::MapView (map.rs, map/tests.rs). Tests: nodes_show_status_name_agent_model_host_duration_cost_retries_and_tokens (AC1, AC2 box-drawing edges), arrows_move_selection_and_selection_follows_active_node_until_moved + viewport_pans_to_the_selection (AC3), large_graphs_switch_to_compact_nodes (AC4), enter_opens_detail_and_tab_is_left_to_the_app (AC5 map side), every_node_is_clickable (AC6), layout_is_only_recomputed_on_topology_changes, snapshot_map_with_subagents. Docs: docs/visibility.md, doc-5 implementation notes.
Remaining for AC5: crates/harness-tui/src/app.rs (owned by the app/tui-foundation owner) still renders a placeholder body for View::Map. It must hold a MapView, call apply_run_event for AppEvent::Run, render it for View::Map, route terminal events to MapView::handle while the map is shown (Tab stays with the app, which already toggles chat/map), and on MapOutput::OpenDetail(node) show harness_tui::detail::DetailView::from_events(node, ...). Then check AC5 and set Done.

Integration: App composes MapView; Tab toggles chat/map, Enter or click opens node detail. Tests: app tab_switches_view_and_view_tabs_are_clickable and the map/detail tests.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Integration: App composes MapView; Tab toggles chat/map, Enter or click opens node detail. Tests: app tab_switches_view_and_view_tabs_are_clickable and the map/detail tests. Workspace green: cargo fmt --check, clippy -D warnings, cargo test --workspace (1142 passed, 0 failed).
<!-- SECTION:FINAL_SUMMARY:END -->
