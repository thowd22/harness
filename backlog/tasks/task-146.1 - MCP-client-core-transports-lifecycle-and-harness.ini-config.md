---
id: TASK-146.1
title: 'MCP client core: transports, lifecycle and harness.ini config'
status: To Do
assignee: []
created_date: '2026-10-23 06:01'
updated_date: '2026-10-23 06:01'
labels:
  - tools
  - mcp
  - config
milestone: m-2
dependencies:
  - TASK-2
  - TASK-3
  - TASK-20
references:
  - 'https://modelcontextprotocol.io/specification'
documentation:
  - backlog/docs/doc-2 - harness.ini-Reference.md
parent_task_id: TASK-146
priority: high
type: feature
ordinal: 147000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Foundation for MCP: connecting to servers and keeping them healthy. Servers are user-granted, so they are defined only in harness.ini; project YAML can reference them but never add one.

## Loop guard (for agents)

Limit how many times you test and fix. If the same test, build or check still fails after **3 fix attempts**, or you have made no measurable progress in about 30 minutes, **stop**. Do not keep retrying, and never delete, skip, ignore or weaken a test or check to get a green run. Record what you tried, the exact failing output and your best hypothesis in the implementation notes, leave the task In Progress, and ask the user how to proceed.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 mcp.<name> sections in harness.ini define stdio servers (command, args, env, cwd) or remote servers (url, headers), with enabled, timeout and startup settings; secrets in env or headers use the same api_key/_cmd/_env resolution and redaction as provider keys
- [ ] #2 The client implements the MCP lifecycle (initialize, capability negotiation, ping, shutdown) and JSON-RPC over stdio and streamable HTTP, following the current specification
- [ ] #3 Stdio servers start lazily on first use, run in their own process group and session, are restarted with backoff on crash, and are always cleaned up on exit (no orphans), using killpg, never kill with a negative id
- [ ] #4 Server stderr is captured to the log (redacted), not the terminal; slow or hung servers time out per request
- [ ] #5 harness doctor checks each configured server (starts, initializes, lists tools) and reports actionable errors
- [ ] #6 Tests use a bundled minimal MCP test server for both transports, including crash, timeout and malformed-message cases
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 Tests: automated tests cover every acceptance criterion (unit and/or integration as appropriate) and pass locally and in CI; no existing test is deleted, skipped, ignored or weakened to get a green run
- [ ] #2 Docs: user-facing behaviour is documented (docs/ and, if the design changed, the relevant backlog doc), public APIs have rustdoc, and any new config keys appear in the harness.ini reference
- [ ] #3 Loop guard respected: no more than 3 fix attempts per failing test or check; if still failing, stop, record attempts, output and hypothesis in implementation notes, and ask the user instead of looping
- [ ] #4 UI interaction: if this task makes anything visible in the TUI, every visible interactive element is clickable with the mouse and fully operable with the keyboard (arrows/Tab to focus, Enter to activate, Esc to back out), with tests for both input paths; tasks with no visible UI mark this N/A
<!-- DOD:END -->
