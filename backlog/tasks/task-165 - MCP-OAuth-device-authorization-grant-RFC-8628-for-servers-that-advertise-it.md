---
id: TASK-165
title: 'MCP OAuth: device authorization grant (RFC 8628) for servers that advertise it'
status: To Do
assignee: []
created_date: '2026-10-27 16:31'
labels:
  - auth
  - login
  - mcp
  - ssh
dependencies:
  - TASK-162
references:
  - 'https://www.rfc-editor.org/rfc/rfc8628'
  - 'https://www.rfc-editor.org/rfc/rfc8414'
documentation:
  - docs/mcp.md
priority: low
ordinal: 179000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Follow-up to TASK-162. Remote and headless MCP sign-in now works by pasting back the redirect address or by an `ssh -L` port forward, but both need the user to move text or a tunnel between machines. Some authorization servers publish a `device_authorization_endpoint` in their RFC 8414 metadata; for them, harness could show a short user code and verification URL that the user opens on any device, with no loopback redirect at all. The MCP authorization spec does not require or mention the device grant, so this is an optional path for servers that support it, never a replacement for the loopback flow. ChatGPT sign-in is out of scope: OpenAI documents only the loopback flow for this kind of client, and the Codex CLI device login uses the Codex client id (see TASK-162 notes).

## Loop guard (for agents)

Limit how many times you test and fix. If the same test, build or check still fails after **3 fix attempts**, or you have made no measurable progress in about 30 minutes, **stop**. Do not keep retrying, and never delete, skip, ignore or weaken a test or check to get a green run. Record what you tried, the exact failing output and your best hypothesis in the implementation notes, leave the task In Progress, and ask the user how to proceed.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 When the authorization server metadata advertises `device_authorization_endpoint` and the session is remote or headless, `harness login mcp <name>` and `/login mcp <name>` offer the device flow and show the user code and verification URL (full, copyable, hyperlinked)
- [ ] #2 Polling honours `interval`, `slow_down`, `authorization_pending`, `access_denied` and `expired_token` per RFC 8628, and the flow times out and can be cancelled (Esc in the TUI)
- [ ] #3 Servers without the endpoint keep the loopback, paste-back and port-forward flow unchanged
- [ ] #4 Tokens go to the existing MCP token store; device codes, user codes and tokens never reach logs (TRACE leak test)
- [ ] #5 Offline tests against the mock OAuth server cover success, slow_down, denial, expiry and cancel; docs/mcp.md describes when the device flow is used
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 Tests: automated tests cover every acceptance criterion (unit and/or integration as appropriate) and pass locally and in CI; no existing test is deleted, skipped, ignored or weakened to get a green run
- [ ] #2 Docs: user-facing behaviour is documented (docs/ and, if the design changed, the relevant backlog doc), public APIs have rustdoc, and any new config keys appear in the harness.ini reference
- [ ] #3 Loop guard respected: no more than 3 fix attempts per failing test or check; if still failing, stop, record attempts, output and hypothesis in implementation notes, and ask the user instead of looping
- [ ] #4 UI interaction: if this task makes anything visible in the TUI, every visible interactive element is clickable with the mouse and fully operable with the keyboard (arrows/Tab to focus, Enter to activate, Esc to back out), with tests for both input paths; tasks with no visible UI mark this N/A
<!-- DOD:END -->
