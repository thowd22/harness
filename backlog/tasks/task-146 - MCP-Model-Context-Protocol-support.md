---
id: TASK-146
title: MCP (Model Context Protocol) support
status: To Do
assignee: []
created_date: '2026-10-23 06:01'
updated_date: '2026-10-23 06:01'
labels:
  - tools
  - mcp
  - security
  - problem-6
  - problem-8
milestone: m-2
dependencies: []
references:
  - 'https://modelcontextprotocol.io/specification'
documentation:
  - backlog/docs/doc-1 - Architecture-and-Problem-Map.md
priority: high
type: feature
ordinal: 146000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Users want to extend agents with tools from MCP servers (GitHub, databases, browsers, internal services) without writing harness code. harness should be an MCP client that follows the current MCP specification while keeping its own guarantees: servers are granted only by the user (harness.ini), every MCP call goes through the policy engine and appears in the event log, credentials never leak, and tool schemas do not bloat the initial context (problem 6). This parent task tracks the subtasks.

## Loop guard (for agents)

Limit how many times you test and fix. If the same test, build or check still fails after **3 fix attempts**, or you have made no measurable progress in about 30 minutes, **stop**. Do not keep retrying, and never delete, skip, ignore or weaken a test or check to get a green run. Record what you tried, the exact failing output and your best hypothesis in the implementation notes, leave the task In Progress, and ask the user how to proceed.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 All subtasks are Done
- [ ] #2 An end-to-end test runs an agent against a local MCP test server over stdio and over streamable HTTP: the agent lists and calls a tool, the call is policy-checked, shown as a tool card and recorded in the run log
- [ ] #3 docs/mcp.md explains configuration, permissions, auth, context cost and troubleshooting
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 Tests: automated tests cover every acceptance criterion (unit and/or integration as appropriate) and pass locally and in CI; no existing test is deleted, skipped, ignored or weakened to get a green run
- [ ] #2 Docs: user-facing behaviour is documented (docs/ and, if the design changed, the relevant backlog doc), public APIs have rustdoc, and any new config keys appear in the harness.ini reference
- [ ] #3 Loop guard respected: no more than 3 fix attempts per failing test or check; if still failing, stop, record attempts, output and hypothesis in implementation notes, and ask the user instead of looping
- [ ] #4 UI interaction: if this task makes anything visible in the TUI, every visible interactive element is clickable with the mouse and fully operable with the keyboard (arrows/Tab to focus, Enter to activate, Esc to back out), with tests for both input paths; tasks with no visible UI mark this N/A
<!-- DOD:END -->
