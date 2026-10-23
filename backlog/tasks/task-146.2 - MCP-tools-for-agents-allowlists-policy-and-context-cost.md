---
id: TASK-146.2
title: 'MCP tools for agents: allowlists, policy and context cost'
status: To Do
assignee: []
created_date: '2026-10-23 06:01'
updated_date: '2026-10-23 06:01'
labels:
  - tools
  - mcp
  - policy
  - problem-6
  - problem-7
  - problem-8
milestone: m-2
dependencies:
  - TASK-146.1
  - TASK-25
  - TASK-31
  - TASK-44
references:
  - 'https://modelcontextprotocol.io/specification'
documentation:
  - backlog/docs/doc-3 - YAML-Spec-Prompts-Agents-Workflows-Skills.md
parent_task_id: TASK-146
priority: high
type: feature
ordinal: 148000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
MCP tools must behave like built-in tools: visible, permission-checked and cheap in tokens. Large servers expose dozens of tools, so sending every schema on every request would blow the initial-context budget.

## Loop guard (for agents)

Limit how many times you test and fix. If the same test, build or check still fails after **3 fix attempts**, or you have made no measurable progress in about 30 minutes, **stop**. Do not keep retrying, and never delete, skip, ignore or weaken a test or check to get a green run. Record what you tried, the exact failing output and your best hypothesis in the implementation notes, leave the task In Progress, and ask the user how to proceed.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 Agent and workflow YAML select MCP tools explicitly, e.g. tools: [read, edit, mcp:github/create_issue, mcp:db/*]; nothing from MCP is exposed unless selected, and project YAML can only reference servers defined in harness.ini
- [ ] #2 Every MCP tool call goes through the policy engine with allow, ask or deny rules per server and tool (for example mcp.github.ask = create_*), outward-action defaults for tools that write externally, and session-scoped approvals
- [ ] #3 MCP tool results are labeled as untrusted content (provenance markers), redacted, size-capped with spill files, and recorded as events with server and tool names
- [ ] #4 Context cost is bounded: when selected MCP tools exceed a configurable schema token budget, they are exposed through a single search-and-load tool instead of full schemas; the initial-context CI budget still passes with a large test server configured
- [ ] #5 Tool cards, the map and node detail show MCP calls with server name, tool name, duration and result, clickable and keyboard-operable
- [ ] #6 Server tool-list changes (notifications) refresh the available tools without restarting the session, with the stable prompt prefix only changing at session boundaries
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 Tests: automated tests cover every acceptance criterion (unit and/or integration as appropriate) and pass locally and in CI; no existing test is deleted, skipped, ignored or weakened to get a green run
- [ ] #2 Docs: user-facing behaviour is documented (docs/ and, if the design changed, the relevant backlog doc), public APIs have rustdoc, and any new config keys appear in the harness.ini reference
- [ ] #3 Loop guard respected: no more than 3 fix attempts per failing test or check; if still failing, stop, record attempts, output and hypothesis in implementation notes, and ask the user instead of looping
- [ ] #4 UI interaction: if this task makes anything visible in the TUI, every visible interactive element is clickable with the mouse and fully operable with the keyboard (arrows/Tab to focus, Enter to activate, Esc to back out), with tests for both input paths; tasks with no visible UI mark this N/A
<!-- DOD:END -->
