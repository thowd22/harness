---
id: TASK-70
title: harness runs show
status: Done
assignee:
  - '@visibility-tui'
created_date: '2026-10-22 14:30'
updated_date: '2026-10-22 19:17'
labels:
  - visibility
  - cli
milestone: m-6
dependencies:
  - TASK-62
  - TASK-18
priority: low
type: feature
ordinal: 70000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Headless users and CI need the same visibility as the TUI.

## Loop guard (for agents)

Limit how many times you test and fix. If the same test, build or check still fails after **3 fix attempts**, or you have made no measurable progress in about 30 minutes, **stop**. Do not keep retrying, and never delete, skip, ignore or weaken a test or check to get a green run. Record what you tried, the exact failing output and your best hypothesis in the implementation notes, leave the task In Progress, and ask the user how to proceed.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 Prints a text tree summary of a run and --json emits the reducer state
- [x] #2 Works on live and finished runs
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
1. history/show.rs: tree_text(&RunState, now), state_json(&RunState) (the reducer state), resolve_run(id|prefix|last), run_show(data_dir, id, json, out, err) -> exit code. 2. Tests on a live (torn last line) and a finished run recorded with RunRecorder. 3. Binary owner wires the subcommand.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Library side done in harness_tui::history::show; tests prints_a_tree_and_the_reducer_json (insta snapshot runs_show_tree) and works_on_live_and_finished_runs. Remaining (AC1 + DoD1): the harness binary (crates/harness/src/main.rs / cli.rs, not owned here) must add 'harness runs show <id|prefix|last> [--json]' calling harness_tui::history::show::run_show(&data_dir, id, json, &mut stdout, &mut stderr) and exit with its code (0 ok, 1 error), plus a binary-level test. Then check AC1 and DoD1 and set Done.

Integration: `harness runs show <id|prefix|last> [--json]`. Test: commands.rs runs_show_prints_the_tree_and_json_state.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Integration: `harness runs show <id|prefix|last> [--json]`. Test: commands.rs runs_show_prints_the_tree_and_json_state. Workspace green: cargo fmt --check, clippy -D warnings, cargo test --workspace (1142 passed, 0 failed).
<!-- SECTION:FINAL_SUMMARY:END -->
