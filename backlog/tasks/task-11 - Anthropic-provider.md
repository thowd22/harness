---
id: TASK-11
title: Anthropic provider
status: Done
assignee:
  - '@providers-anthropic'
created_date: '2026-10-22 14:30'
updated_date: '2026-10-22 15:21'
labels:
  - provider
  - problem-1
milestone: m-1
dependencies:
  - TASK-8
  - TASK-9
  - TASK-10
documentation:
  - backlog/docs/doc-1 - Architecture-and-Problem-Map.md
priority: high
type: feature
ordinal: 11000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Claude is a primary target. The provider must support streaming, tools, extended thinking or effort, and prompt caching so long sessions stay cheap.

## Loop guard (for agents)

Limit how many times you test and fix. If the same test, build or check still fails after **3 fix attempts**, or you have made no measurable progress in about 30 minutes, **stop**. Do not keep retrying, and never delete, skip, ignore or weaken a test or check to get a green run. Record what you tried, the exact failing output and your best hypothesis in the implementation notes, leave the task In Progress, and ask the user how to proceed.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 Messages API streaming with text, tool use (including parallel) and thinking blocks
- [x] #2 Thinking blocks and signatures are preserved across tool loops exactly as the API requires
- [x] #3 Cache control breakpoints are placed by the request builder from the stable-prefix layout
- [x] #4 Effort or thinking budget maps from agent effort setting
- [x] #5 Usage including cache read and write tokens is reported
- [x] #6 Thinking content is streamed as live reasoning deltas, not only delivered at block end
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
1. anthropic::request: build Messages API body (stream, system/tools/messages, thinking blocks + signatures and redacted_thinking round-tripped verbatim, images, tool results, tool_choice, stop sequences). Cache breakpoints (max 4): last tool, system, last message, previous user turn. Thinking: adaptive models -> thinking adaptive (display summarized) + output_config.effort; budget models -> enabled budget_tokens; effort<->budget mapping. Drops temperature (API version 2026-10-21 rejects sampling params).
2. anthropic::stream: SSE -> StreamEvent mapping (text/thinking/signature/tool input deltas live, opaque blocks emitted at block stop, usage incl. cache read/write from message_start + message_delta, error events mapped to retryable errors, truncated stream -> Connection error).
3. AnthropicProvider implementing Provider over HttpClient::open + retry_stream, capabilities from registry.
4. Mock-server integration tests + unit tests per AC, docs/providers-anthropic.md.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Implemented anthropic.rs (AnthropicProvider, AnthropicConfig, NAME, DEFAULT_BASE_URL, MESSAGES_PATH, API_VERSION=2026-10-21), anthropic/request.rs (build_body, CacheMode, ModelProfile, RequestOptions, effort_budget, budget_effort, convert_message) and anthropic/stream.rs (Decoder, decode_stream, stream_error).
Decisions: thinking uses display=summarized so reasoning deltas stream live; temperature is never sent (API 2026-10-21 rejects sampling params); unsigned reasoning and non-anthropic opaque blocks are dropped from requests; unknown Claude models get adaptive thinking with conservative caps; forced tool_choice is passed through as-is (Opus 5.5/Sonnet 5.5 reject it with 400; agent should use auto on those).
Validation: tests/anthropic_provider.rs against a local mock server (live thinking/text/parallel tool streaming + usage with cache counters + request headers/cache breakpoints; two-turn tool loop with thinking + redacted_thinking round-tripped byte-for-byte; mid-stream overloaded error retried with discard_partial; 400/401 mapping without leaking the key). Unit tests for request building (breakpoints, effort/budget mapping, tool_choice, images) and SSE decoding. Live smoke test is #[ignore] + HARNESS_LIVE_ANTHROPIC=1. clippy/rustdoc -D warnings clean. Docs: docs/providers-anthropic.md. DoD#4 N/A (no TUI).
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Anthropic Messages API provider: streaming text, parallel tool use and live thinking deltas; thinking/signatures/redacted_thinking preserved exactly across tool loops; cache_control breakpoints from the tools->system->messages layout; effort<->budget mapping per model thinking style; usage incl. cache read/write. Verified by mock-server integration tests and unit tests; documented in docs/providers-anthropic.md.
<!-- SECTION:FINAL_SUMMARY:END -->
