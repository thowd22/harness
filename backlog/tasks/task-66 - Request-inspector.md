---
id: TASK-66
title: Request inspector
status: Done
assignee:
  - '@visibility-tui'
created_date: '2026-10-22 14:30'
updated_date: '2026-10-22 19:16'
labels:
  - visibility
  - problem-9
milestone: m-6
dependencies:
  - TASK-65
  - TASK-5
priority: high
type: feature
ordinal: 66000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Harness-only changes caused real quality regressions (problem 9). Seeing the exact request and response is the fastest way to diagnose them.

## Loop guard (for agents)

Limit how many times you test and fix. If the same test, build or check still fails after **3 fix attempts**, or you have made no measurable progress in about 30 minutes, **stop**. Do not keep retrying, and never delete, skip, ignore or weaken a test or check to get a green run. Record what you tried, the exact failing output and your best hypothesis in the implementation notes, leave the task In Progress, and ask the user how to proceed.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 Shows redacted exact provider request payload and raw response for any model call
- [x] #2 Shows prompt-layer breakdown with token counts and cache breakpoints
- [x] #3 Payload can be exported to a file
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
1. inspector.rs Inspection::from_events: recorded request payload, full request rebuilt from the node's earlier calls (messages_from), Anthropic body via anthropic::request::build_body, response + streamed deltas + usage. 2. Prompt layers (tools, system, messages) with estimated tokens, new-in-call marks and cache breakpoints from the rebuilt body. 3. Optional re-redaction, export to JSON. 4. InspectorView tabs + export button, mouse and keyboard tests.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Implemented harness_tui::inspector (Inspection, InspectorView, estimate_tokens) with tests in inspector/tests.rs: rebuilds_the_exact_request_and_raw_response, payloads_are_redacted, layers_show_tokens_and_cache_breakpoints (AC2), export_writes_the_payloads (AC3, click + 'e' key), tabs_switch_by_click_and_keyboard.
Why AC1 stays open: the inspector shows the redacted request payload exactly as model_call_started records it, the full message history rebuilt from the log and the raw response message. But the agent loop records only tool *names* (not schemas) and not the provider wire body, so the 'exact provider request payload' can only be approximated (the Anthropic body is rebuilt with empty tool schemas; OpenAI/OpenRouter bodies are not rebuilt). To finish: harness-agent (agent_loop request payload) should record the provider body (e.g. Provider::build_body / request_body, already public for Anthropic, OpenAI and OpenRouter) or full ToolSpecs in model_call_started.request; then Inspection can show it verbatim (Inspection::provider_body) and AC1 + DoD1 can be checked.

Integration: the inspector shows the redacted exact provider body (Provider::request_body, wired via App::with_request_body) and the raw response message; assistant turns rebuilt from model_call_finished. Tests: harness-tui tests/inspector_exact.rs and app inspector body test.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Integration: the inspector shows the redacted exact provider body (Provider::request_body, wired via App::with_request_body) and the raw response message; assistant turns rebuilt from model_call_finished. Tests: harness-tui tests/inspector_exact.rs and app inspector body test. Workspace green: cargo fmt --check, clippy -D warnings, cargo test --workspace (1142 passed, 0 failed).
<!-- SECTION:FINAL_SUMMARY:END -->
