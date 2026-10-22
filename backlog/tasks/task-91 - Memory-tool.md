---
id: TASK-91
title: Memory tool
status: Done
assignee:
  - '@memory-core'
created_date: '2026-10-22 14:30'
updated_date: '2026-10-22 16:02'
labels:
  - memory
milestone: m-9
dependencies:
  - TASK-90
  - TASK-25
documentation:
  - backlog/docs/doc-4 - Memory-System-Design.md
priority: high
type: feature
ordinal: 91000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Agents need a small tool to search, read, write and forget memories, enabled per agent.

## Loop guard (for agents)

Limit how many times you test and fix. If the same test, build or check still fails after **3 fix attempts**, or you have made no measurable progress in about 30 minutes, **stop**. Do not keep retrying, and never delete, skip, ignore or weaken a test or check to get a green run. Record what you tried, the exact failing output and your best hypothesis in the implementation notes, leave the task In Progress, and ask the user how to proceed.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 Actions: search, read, write (note and bullet), forget
- [x] #2 Writes are format-validated, secret-scanned and rejected if they look like task state or session summaries
- [x] #3 Each write is a commit referencing the run id
- [x] #4 Tool appears only for agents with memory read or read-write
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
1. tool/guard.rs: format check, secret scan via harness_events Redactor, task-state/session-summary/rediscoverable heuristics.
2. tool.rs: MemoryArgs (search/read/write/forget), MemoryTool over a SessionCheckout, spec()/schema() per MemoryAccess (None when off), effective_access for [memory] enabled/headless_access.
3. Each write/forget commits via sync::SessionCheckout::write with 'Run: harness://run/<id>' trailer; returns MemoryChange for EventKind::Memory.
4. Integration tests in tests/memory.rs.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Follows harness-tools conventions: call(&ToolContext, Value)->Result<ToolOutput,ToolError>, run() returns MemoryOutcome incl. MemoryChange{action: MemoryAction, path, bullets, commit: Published}. Git work runs in spawn_blocking. Read-only agents get a schema with only search/read and writes return ToolErrorKind::Unsupported. project/<topic> is shorthand for projects/<project-key>/<topic>.md. Agent integration (harness-agent): add spec(effective_access(agent.memory, &cfg.memory, headless)) to request tools and dispatch 'memory' calls to MemoryTool; Agent::request_tools in harness-config already lists memory only when memory != off.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Implemented harness_memory::tool (MemoryTool, MemoryArgs, spec/schema, effective_access) and tool::guard. Verified by tests/memory.rs: tool_search_read_write_forget, writes_are_validated_scanned_and_screened, each_write_is_a_commit_referencing_the_run, tool_appears_only_for_agents_with_memory, plus guard unit tests. clippy -D warnings clean. DoD#4 N/A.
<!-- SECTION:FINAL_SUMMARY:END -->
