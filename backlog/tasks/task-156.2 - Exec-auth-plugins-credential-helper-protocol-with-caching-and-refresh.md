---
id: TASK-156.2
title: 'Exec auth plugins: credential-helper protocol with caching and refresh'
status: To Do
assignee: []
created_date: '2026-10-23 18:51'
updated_date: '2026-10-23 18:51'
labels:
  - providers
  - auth
  - plugins
dependencies:
  - TASK-156.1
parent_task_id: TASK-156
priority: medium
ordinal: 162000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Add auth = exec to any [provider.<id>] (custom or openai-compatible): auth_command runs a user program that speaks a versioned JSON protocol on stdin/stdout and returns headers (and optional query params) plus expires_at. Generalize the per-request auth path (today Auth::ChatGpt) into an AuthSource used by every wire format. Cache per provider until expires_at minus a skew; refresh on expiry, and once on 401/403 with reason = unauthorized; single-flight so parallel agents do not stampede the plugin. Run plugins with a timeout, in their own process group (killpg on timeout), scrubbed env (like api_key_cmd), stdin closed after the request, stderr to the log redacted, bounded stdout. harness login <id> calls the plugin with reason = login and interactive = true (it may open a browser); headless runs pass interactive = false. Output values are secrets: never logged, shown or written to events.
## Loop guard (for agents)

Limit how many times you test and fix. If the same test, build or check still fails after **3 fix attempts**, or you have made no measurable progress in about 30 minutes, **stop**. Do not keep retrying, and never delete, skip, ignore or weaken a test or check to get a green run. Record what you tried, the exact failing output and your best hypothesis in the implementation notes, leave the task In Progress, and ask the user how to proceed.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 A test plugin returning short-lived tokens is called once per expiry window, again after expiry, and once after a 401, with single-flight under parallel calls
- [ ] #2 Plugin timeouts, non-zero exits, bad JSON and unknown protocol versions give actionable errors naming the provider; no orphaned processes
- [ ] #3 Tokens and headers from plugins never appear in logs, events, errors, snapshots or the request inspector
- [ ] #4 harness login <id> runs the plugin interactively; headless runs never prompt; the protocol is documented with a version field
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 Tests: automated tests cover every acceptance criterion (unit and/or integration as appropriate) and pass locally and in CI; no existing test is deleted, skipped, ignored or weakened to get a green run
- [ ] #2 Docs: user-facing behaviour is documented (docs/ and, if the design changed, the relevant backlog doc), public APIs have rustdoc, and any new config keys appear in the harness.ini reference
- [ ] #3 Loop guard respected: no more than 3 fix attempts per failing test or check; if still failing, stop, record attempts, output and hypothesis in implementation notes, and ask the user instead of looping
- [ ] #4 UI interaction: if this task makes anything visible in the TUI, every visible interactive element is clickable with the mouse and fully operable with the keyboard (arrows/Tab to focus, Enter to activate, Esc to back out), with tests for both input paths; tasks with no visible UI mark this N/A
<!-- DOD:END -->
