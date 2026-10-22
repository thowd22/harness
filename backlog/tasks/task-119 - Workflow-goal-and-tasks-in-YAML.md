---
id: TASK-119
title: Workflow goal and tasks in YAML
status: Done
assignee:
  - '@workflows-subagents'
created_date: '2026-10-22 14:39'
updated_date: '2026-10-22 17:14'
labels:
  - workflow
  - config
  - visibility
milestone: m-5
dependencies:
  - TASK-118
  - TASK-53
  - TASK-54
documentation:
  - backlog/docs/doc-3 - YAML-Spec-Prompts-Agents-Workflows-Skills.md
priority: medium
type: feature
ordinal: 119000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Workflows know their plan up front, so they should be able to seed the goal bar instead of relying on agents to create tasks.

## Loop guard (for agents)

Limit how many times you test and fix. If the same test, build or check still fails after **3 fix attempts**, or you have made no measurable progress in about 30 minutes, **stop**. Do not keep retrying, and never delete, skip, ignore or weaken a test or check to get a green run. Record what you tried, the exact failing output and your best hypothesis in the implementation notes, leave the task In Progress, and ask the user how to proceed.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 Workflow YAML accepts goal (templated) and tasks with id, title and optional step
- [x] #2 Tasks linked to a step take status and owning agent from that step automatically
- [x] #3 Agents in the workflow can add or update further tasks through the todo tool
- [x] #4 Unknown step references are load errors
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
1. Schema: goal (templated, refs checked) and tasks {id, title, step?}; unknown step refs and duplicate ids are load errors.
2. Executor seeds the TaskBoard (set_goal + seed) and keeps step-linked tasks in sync (status: pending/in_progress/blocked at a gate/done/failed/cancelled; owner = step node, agent = step agent).
3. Agents in the workflow get the same board through SubagentRuntime::with_board, so their todo tool adds/updates tasks.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Implemented in schema.rs (TaskSpec, validation) and executor.rs (seed/sync_task) on top of harness_agent::todo::TaskBoard::{set_goal, seed, sync_step}. Test goal_and_tasks_seed_the_board_and_follow_steps: the goal renders from inputs; linked tasks follow their step to done with agent and owner; an unlinked task stays pending; the workflow agent adds a task through the todo tool owned by its node; live events include in_progress. tasks_must_point_at_known_steps covers AC#4. Seeding is idempotent, so a resumed run keeps its progress. Validation: cargo test -p harness-workflow passes; clippy -D warnings clean.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Workflow goal and tasks are parsed and validated, seed the shared task board, follow their linked steps' status and owner, and agents can add or update tasks through todo. Verified by the schema and executor integration tests. DoD#4 N/A (the goal-bar view belongs to harness-tui).
<!-- SECTION:FINAL_SUMMARY:END -->
