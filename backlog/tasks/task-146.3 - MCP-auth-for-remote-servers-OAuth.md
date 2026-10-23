---
id: TASK-146.3
title: MCP auth for remote servers (OAuth)
status: To Do
assignee: []
created_date: '2026-10-23 06:01'
updated_date: '2026-10-23 06:01'
labels:
  - mcp
  - security
  - problem-8
milestone: m-2
dependencies:
  - TASK-146.1
  - TASK-133
references:
  - 'https://modelcontextprotocol.io/specification'
documentation:
  - backlog/docs/doc-2 - harness.ini-Reference.md
parent_task_id: TASK-146
priority: medium
type: feature
ordinal: 149000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Remote MCP servers increasingly require OAuth. Tokens are personal credentials and must be stored and refreshed as safely as the ChatGPT sign-in tokens.

## Loop guard (for agents)

Limit how many times you test and fix. If the same test, build or check still fails after **3 fix attempts**, or you have made no measurable progress in about 30 minutes, **stop**. Do not keep retrying, and never delete, skip, ignore or weaken a test or check to get a green run. Record what you tried, the exact failing output and your best hypothesis in the implementation notes, leave the task In Progress, and ask the user how to proceed.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 Remote servers that require authorization are supported via the OAuth flow defined by the MCP specification (authorization code with PKCE, loopback redirect, dynamic client registration where the server supports it)
- [ ] #2 harness login mcp <name> and a /login palette action run the flow; tokens are stored in a 0600 file under data_dir that is a built-in protected path, refreshed automatically, never logged or sent to remote hosts
- [ ] #3 Static bearer tokens or API keys via headers keep working with the standard secret resolution
- [ ] #4 Headless and eval runs refuse interactive OAuth and report how to authorize first
- [ ] #5 Tests use a local mock authorization server: success, denied consent, refresh, expired refresh token and revoked token
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 Tests: automated tests cover every acceptance criterion (unit and/or integration as appropriate) and pass locally and in CI; no existing test is deleted, skipped, ignored or weakened to get a green run
- [ ] #2 Docs: user-facing behaviour is documented (docs/ and, if the design changed, the relevant backlog doc), public APIs have rustdoc, and any new config keys appear in the harness.ini reference
- [ ] #3 Loop guard respected: no more than 3 fix attempts per failing test or check; if still failing, stop, record attempts, output and hypothesis in implementation notes, and ask the user instead of looping
- [ ] #4 UI interaction: if this task makes anything visible in the TUI, every visible interactive element is clickable with the mouse and fully operable with the keyboard (arrows/Tab to focus, Enter to activate, Esc to back out), with tests for both input paths; tasks with no visible UI mark this N/A
<!-- DOD:END -->
