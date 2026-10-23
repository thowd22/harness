---
id: TASK-146.4
title: 'MCP in the UI: /mcp, prompts and resources'
status: To Do
assignee: []
created_date: '2026-10-23 06:01'
updated_date: '2026-10-23 06:01'
labels:
  - tui
  - mcp
  - commands
milestone: m-4
dependencies:
  - TASK-146.1
  - TASK-146.2
  - TASK-36
  - TASK-45
references:
  - 'https://modelcontextprotocol.io/specification'
documentation:
  - backlog/docs/doc-3 - YAML-Spec-Prompts-Agents-Workflows-Skills.md
parent_task_id: TASK-146
priority: medium
type: feature
ordinal: 150000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Users need to see which servers are connected and what they offer, and MCP prompts and resources should be usable without leaving the TUI.

## Loop guard (for agents)

Limit how many times you test and fix. If the same test, build or check still fails after **3 fix attempts**, or you have made no measurable progress in about 30 minutes, **stop**. Do not keep retrying, and never delete, skip, ignore or weaken a test or check to get a green run. Record what you tried, the exact failing output and your best hypothesis in the implementation notes, leave the task In Progress, and ask the user how to proceed.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 /mcp opens a panel listing servers with status (connected, starting, failed with reason, disabled), transport, tool count and auth state, with actions to restart, disable for the session or view logs; everything clickable and keyboard-operable
- [ ] #2 MCP prompts appear as slash commands namespaced by server (e.g. /github:review-pr) with their arguments, and in the palette
- [ ] #3 MCP resources can be attached to a message from the palette (search by server and name) and are labeled as untrusted content
- [ ] #4 The status line shows a compact MCP indicator when any server has failed
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 Tests: automated tests cover every acceptance criterion (unit and/or integration as appropriate) and pass locally and in CI; no existing test is deleted, skipped, ignored or weakened to get a green run
- [ ] #2 Docs: user-facing behaviour is documented (docs/ and, if the design changed, the relevant backlog doc), public APIs have rustdoc, and any new config keys appear in the harness.ini reference
- [ ] #3 Loop guard respected: no more than 3 fix attempts per failing test or check; if still failing, stop, record attempts, output and hypothesis in implementation notes, and ask the user instead of looping
- [ ] #4 UI interaction: if this task makes anything visible in the TUI, every visible interactive element is clickable with the mouse and fully operable with the keyboard (arrows/Tab to focus, Enter to activate, Esc to back out), with tests for both input paths; tasks with no visible UI mark this N/A
<!-- DOD:END -->
