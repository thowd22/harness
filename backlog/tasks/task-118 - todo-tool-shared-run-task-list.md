---
id: TASK-118
title: 'todo tool: shared run task list'
status: Done
assignee:
  - '@workflows-subagents'
created_date: '2026-10-22 14:39'
updated_date: '2026-10-22 16:33'
labels:
  - agent
  - tools
  - visibility
  - problem-6
milestone: m-5
dependencies:
  - TASK-16
  - TASK-4
  - TASK-51
documentation:
  - backlog/docs/doc-3 - YAML-Spec-Prompts-Agents-Workflows-Skills.md
  - backlog/docs/doc-5 - Agent-Map-and-Run-Visibility.md
priority: high
type: feature
ordinal: 118000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
The goal bar needs structured, live task data. As in opencode, agents should maintain an explicit task list; here it is shared by every agent in a run so the user can see who is working on what.

## Loop guard (for agents)

Limit how many times you test and fix. If the same test, build or check still fails after **3 fix attempts**, or you have made no measurable progress in about 30 minutes, **stop**. Do not keep retrying, and never delete, skip, ignore or weaken a test or check to get a green run. Record what you tried, the exact failing output and your best hypothesis in the implementation notes, leave the task In Progress, and ask the user how to proceed.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 todo tool supports add, update and list with id, title, status (pending, in_progress, blocked, done, failed, cancelled) and owner
- [x] #2 Owner defaults to the calling agent node, and subagents can claim and update tasks in the same run
- [x] #3 Tool appears only for agents whose tools list includes todo, with a compact schema
- [x] #4 Every change emits a task event so views update live, and task state survives resume
- [x] #5 Concurrent updates from parallel agents are applied in order without lost writes
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
1. harness_agent::todo: TaskBoard (goal + tasks) applies changes under one lock with an increasing version, broadcasts a TaskEvent and appends it to <run dir>/tasks.jsonl.
2. TodoTool (add/update/list, claim) bound to an Actor (node + agent); owner defaults to the caller.
3. SubagentRuntime::tools_for adds todo only when the agent lists it.
4. Resume via TaskBoard::open / continue_from; workflow seeding via seed/sync_step.
5. Tests for every AC; docs/subagents.md.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Implemented crates/harness-agent/src/todo.rs (+ todo/tests.rs). The compact schema is under 400 bytes. The tool is read_only=true because the board serializes changes itself. Task events: harness-events has no task EventKind, and I cannot add one (other crate). Every change is therefore broadcast to TaskBoard::subscribe() for live views and journaled in the run dir (TASKS_FILE=tasks.jsonl, written while the lock is held, so journal order = apply order). Resume uses TaskBoard::open(path) or continue_from(prev, new). Follow-up for harness-events: add EventKind::TaskUpdated{version, change, task} so task changes also reach events.jsonl; a RecorderTaskSink can then mirror them. Concurrency is tested with 8 tasks x 25 adds on a multi-thread runtime: versions 1..=200 with no lost writes. Validation: cargo test -p harness-agent --lib todo subagents pass.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Added harness_agent::todo with TaskBoard, TodoTool, TaskEvent and a tasks.jsonl journal. The shared run task list supports add/update/list with id, title, status and owner; owner defaults to the caller and agents can claim tasks. Changes are ordered and lossless, broadcast live and survive resume. The tool is offered only when listed (SubagentRuntime::tools_for). Verified by 8 todo unit tests and the tools_for test. DoD#4 N/A here: the goal-bar view belongs to harness-tui.
<!-- SECTION:FINAL_SUMMARY:END -->
