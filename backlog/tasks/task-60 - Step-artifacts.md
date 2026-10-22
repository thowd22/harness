---
id: TASK-60
title: Step artifacts
status: Done
assignee:
  - '@workflows-subagents'
created_date: '2026-10-22 14:30'
updated_date: '2026-10-22 19:16'
labels:
  - workflow
milestone: m-5
dependencies:
  - TASK-55
priority: medium
type: feature
ordinal: 60000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Steps need to hand files such as plans, patches or reports to later steps without polluting the repo (problem 3).

## Loop guard (for agents)

Limit how many times you test and fix. If the same test, build or check still fails after **3 fix attempts**, or you have made no measurable progress in about 30 minutes, **stop**. Do not keep retrying, and never delete, skip, ignore or weaken a test or check to get a green run. Record what you tried, the exact failing output and your best hypothesis in the implementation notes, leave the task In Progress, and ask the user how to proceed.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 Steps write artifacts into the run directory and later steps reference them by name
- [x] #2 Artifacts are viewable from the node detail view
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
1. harness_workflow::artifacts: ArtifactStore over <run dir>/artifacts/<step>/<name> (write atomic, list, copy_from), inline_section for agent briefs, for_node and view_lines for node detail.
2. Executor stores each step's declared artifacts (agent JSON fields, shell $ARTIFACTS_DIR files, verify output, proof report) and exposes steps.<id>.artifacts.<name> paths.
3. Node detail view in harness-tui.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Library side done: crates/harness-workflow/src/artifacts.rs (3 tests) and executor integration (artifacts_are_written_to_the_run_dir_and_referenced_by_name: agent and shell artifacts are written under the run dir, referenced by name from later steps, inlined into agent briefs; a missing declared artifact fails the step). Remaining (AC#2, DoD#4, harness-tui node detail owner): list harness_workflow::artifacts::for_node(run_dir, node_id) (or view_lines) in node detail and open an artifact's content on click/Enter, with mouse and keyboard tests. Validation: cargo test -p harness-workflow passes; clippy -D warnings clean.

Integration: node detail lists a step's artifacts (DetailView::set_artifacts) and opens one on click/Enter. Tests: tui workflow test ('Artifact plan' panel), app artifacts tests (mouse and keyboard).
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Integration: node detail lists a step's artifacts (DetailView::set_artifacts) and opens one on click/Enter. Tests: tui workflow test ('Artifact plan' panel), app artifacts tests (mouse and keyboard). Workspace green: cargo fmt --check, clippy -D warnings, cargo test --workspace (1142 passed, 0 failed).
<!-- SECTION:FINAL_SUMMARY:END -->
