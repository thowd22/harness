---
id: TASK-156.1
title: 'Custom providers by config: wire formats, endpoints, headers and model facts'
status: To Do
assignee: []
created_date: '2026-10-23 18:51'
labels:
  - providers
  - auth
  - plugins
dependencies: []
parent_task_id: TASK-156
priority: medium
ordinal: 161000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Let [provider.<id>] define a provider harness does not know: wire = anthropic | openai-responses | openai-chat, base_url, optional path and query.<k> (Azure api-version, deployment paths), header.<Name> with _cmd/_env like [mcp.<name>], plus model facts (context window, max output, tools/images/thinking/caching flags, prices) via [models] and [pricing] or a new [model.<provider/model>] section. wiring.rs builds a generic provider from the wire kind; '<id>/<model>' refs and /model picker work (static model list from config, or an optional models endpoint for openai-compatible servers). doctor checks reachability and auth. Unknown wire kinds and missing base_url are config errors with clear messages. Built-in provider ids keep their behaviour.
## Loop guard (for agents)

Limit how many times you test and fix. If the same test, build or check still fails after **3 fix attempts**, or you have made no measurable progress in about 30 minutes, **stop**. Do not keep retrying, and never delete, skip, ignore or weaken a test or check to get a green run. Record what you tried, the exact failing output and your best hypothesis in the implementation notes, leave the task In Progress, and ask the user how to proceed.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 A [provider.acme] with wire = openai-chat (and one with openai-responses, one with anthropic) and a model alias runs a headless agent turn with tools against a local mock server
- [ ] #2 query.* and header.* (static/_cmd/_env) reach the server; header values are redacted in logs, events and errors
- [ ] #3 Model facts from config drive context window, capabilities and cost; /model lists the provider's models; doctor reports reachability and auth
- [ ] #4 harness.ini reference (doc-2) and docs updated; config errors are actionable
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 Tests: automated tests cover every acceptance criterion (unit and/or integration as appropriate) and pass locally and in CI; no existing test is deleted, skipped, ignored or weakened to get a green run
- [ ] #2 Docs: user-facing behaviour is documented (docs/ and, if the design changed, the relevant backlog doc), public APIs have rustdoc, and any new config keys appear in the harness.ini reference
- [ ] #3 Loop guard respected: no more than 3 fix attempts per failing test or check; if still failing, stop, record attempts, output and hypothesis in implementation notes, and ask the user instead of looping
- [ ] #4 UI interaction: if this task makes anything visible in the TUI, every visible interactive element is clickable with the mouse and fully operable with the keyboard (arrows/Tab to focus, Enter to activate, Esc to back out), with tests for both input paths; tasks with no visible UI mark this N/A
<!-- DOD:END -->
