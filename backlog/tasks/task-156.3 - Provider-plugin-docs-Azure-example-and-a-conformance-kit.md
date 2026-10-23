---
id: TASK-156.3
title: 'Provider plugin docs, Azure example and a conformance kit'
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
  - TASK-156.2
parent_task_id: TASK-156
priority: medium
ordinal: 163000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Write docs/providers-plugins.md: when to use a custom provider vs an auth plugin, the config reference, the exec protocol (versioned JSON schema, reasons, caching, errors), the security model (user harness.ini only, protected path, redaction, env scrubbing) and worked examples: Azure OpenAI / AI Foundry with an Entra ID token from 'az account get-access-token', and a generic company SSO script. Ship example plugins under examples/auth-plugins/ (shell and Python) and a 'harness provider test <id>' command that runs a tiny conformance check (auth, one streamed completion, tool call round trip, usage parsing) and prints a clear report.
## Loop guard (for agents)

Limit how many times you test and fix. If the same test, build or check still fails after **3 fix attempts**, or you have made no measurable progress in about 30 minutes, **stop**. Do not keep retrying, and never delete, skip, ignore or weaken a test or check to get a green run. Record what you tried, the exact failing output and your best hypothesis in the implementation notes, leave the task In Progress, and ask the user how to proceed.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 Examples run against the local mock router in CI; harness provider test <id> passes for them and fails clearly on a broken plugin
- [ ] #2 The Azure example is documented step by step and verified at least against a mock (live Azure only if the user provides access)
- [ ] #3 README links the plugin docs; docs explain what stays built in (Claude, OpenAI, OpenRouter)
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 Tests: automated tests cover every acceptance criterion (unit and/or integration as appropriate) and pass locally and in CI; no existing test is deleted, skipped, ignored or weakened to get a green run
- [ ] #2 Docs: user-facing behaviour is documented (docs/ and, if the design changed, the relevant backlog doc), public APIs have rustdoc, and any new config keys appear in the harness.ini reference
- [ ] #3 Loop guard respected: no more than 3 fix attempts per failing test or check; if still failing, stop, record attempts, output and hypothesis in implementation notes, and ask the user instead of looping
- [ ] #4 UI interaction: if this task makes anything visible in the TUI, every visible interactive element is clickable with the mouse and fully operable with the keyboard (arrows/Tab to focus, Enter to activate, Esc to back out), with tests for both input paths; tasks with no visible UI mark this N/A
<!-- DOD:END -->
