---
id: TASK-148
title: Coder keeps a todo list by default for multi-step work
status: In Progress
assignee:
  - '@claude'
created_date: '2026-10-23 15:25'
updated_date: '2026-10-23 15:26'
labels:
  - agent
  - tui
dependencies: []
priority: high
ordinal: 152000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
The goal bar only fills when the agent calls the `todo` tool. The built-in coder agent has the tool and the shared coder prompt says to plan multi-step work with it, but in practice models skip it: a real 0.3.0 session on a large repo ("index this codebase and store it in memory") made 131 tool calls over 92 model turns and never called `todo`, so the user saw no goals in the sidebar.

Make todo use the default for the coder:
- Stronger, more direct wording in the coder prompt (`crates/harness-config/assets/prompts/coder.md`) and the `todo` tool description (`crates/harness-agent/src/todo.rs`): plan first for any task with more than one step.
- A harness-side nudge in the agent loop: when an agent that has the `todo` tool has made N tool-using turns and the run's task list is still empty, append a short reminder to the next request (once, then again only after another N turns). Configurable in harness.ini; 0 disables.

## Loop guard (for agents)

Limit how many times you test and fix. If the same test, build or check still fails after **3 fix attempts**, or you have made no measurable progress in about 30 minutes, **stop**. Do not keep retrying, and never delete, skip, ignore or weaken a test or check to get a green run. Record what you tried, the exact failing output and your best hypothesis in the implementation notes, leave the task In Progress, and ask the user how to proceed.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 The coder prompt and the todo tool description tell the model to write a todo list before starting any task with more than one step
- [ ] #2 When an agent with the todo tool has made [general] todo_nudge_after tool-using turns (default 3) and the task list is empty, the next model request carries a short reminder to use todo; it repeats at most every todo_nudge_after turns and stops once a task exists
- [ ] #3 todo_nudge_after = 0 disables the reminder; agents without the todo tool never get it
- [ ] #4 The reminder is visible in the run's events/request and does not break prompt caching of earlier turns
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 Tests: automated tests cover every acceptance criterion (unit and/or integration as appropriate) and pass locally and in CI; no existing test is deleted, skipped, ignored or weakened to get a green run
- [ ] #2 Docs: user-facing behaviour is documented (docs/ and, if the design changed, the relevant backlog doc), public APIs have rustdoc, and any new config keys appear in the harness.ini reference
- [ ] #3 Loop guard respected: no more than 3 fix attempts per failing test or check; if still failing, stop, record attempts, output and hypothesis in implementation notes, and ask the user instead of looping
- [ ] #4 UI interaction: if this task makes anything visible in the TUI, every visible interactive element is clickable with the mouse and fully operable with the keyboard (arrows/Tab to focus, Enter to activate, Esc to back out), with tests for both input paths; tasks with no visible UI mark this N/A
<!-- DOD:END -->

## Implementation Plan

<!-- SECTION:PLAN:BEGIN -->
1. Stronger wording: coder.md working-method bullet and todo DESCRIPTION (plan first for multi-step work; add each step, mark in_progress/done).
2. [general] todo_nudge_after (u32, default 3, 0 = off) in harness-config: General, defaults, read_general, reference.ini fixture, doc-2, docs/config.md; CHANGELOG Defaults entry with the new fingerprint.
3. harness_agent::todo::TodoNudge RequestHook: when the request carries the todo tool, the board has no tasks, the run has made >= N model calls since the last nudge (reset per node), and the last message is a tool-result user message, append a short reminder text block to that last message (unlogged tail only, so caching and logs stay exact). Unit tests: fires at N, repeats only after N more, stops once a task exists, never without the todo tool, 0 disables.
4. Wire it into core.rs hooks for the top-level loop; integration test via fake provider that the reminder appears in model_call_started.
5. Docs: docs/subagents.md (todo section), docs/visibility.md goal bar note; run full gates.
<!-- SECTION:PLAN:END -->
