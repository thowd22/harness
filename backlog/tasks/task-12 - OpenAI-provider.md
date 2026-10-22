---
id: TASK-12
title: OpenAI provider
status: Done
assignee:
  - '@providers-openai-openrouter'
created_date: '2026-10-22 14:30'
updated_date: '2026-10-22 15:22'
labels:
  - provider
  - problem-1
milestone: m-1
dependencies:
  - TASK-8
  - TASK-9
  - TASK-10
priority: high
type: feature
ordinal: 12000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
OpenAI is a primary target. Reasoning models need their reasoning items carried between turns, otherwise quality drops silently in tool loops.

## Loop guard (for agents)

Limit how many times you test and fix. If the same test, build or check still fails after **3 fix attempts**, or you have made no measurable progress in about 30 minutes, **stop**. Do not keep retrying, and never delete, skip, ignore or weaken a test or check to get a green run. Record what you tried, the exact failing output and your best hypothesis in the implementation notes, leave the task In Progress, and ask the user how to proceed.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 Responses API streaming with function tools and parallel tool calls
- [x] #2 Runs stateless (store false) and round-trips encrypted reasoning items between turns
- [x] #3 Reasoning effort maps from agent effort setting
- [x] #4 Usage including cached and reasoning tokens is reported
- [x] #5 Reasoning summaries are requested and streamed as live reasoning deltas, and encrypted-only reasoning is flagged as not exposed
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
1. Shared SSE->StreamEvent mapper helper and in-band harness notice blocks (openai::notice).
2. Responses API request builder: store=false, include reasoning.encrypted_content, reasoning.effort from ThinkingConfig, summary=auto, function tools, parallel_tool_calls.
3. Stream mapper for response.* events: text, reasoning summary deltas, function calls (parallel), encrypted reasoning items as Opaque blocks round-tripped verbatim, hidden-reasoning notice, usage incl. cached/reasoning tokens.
4. OpenAiProvider implementing Provider via retry_stream + HttpClient::open.
5. Tests against a local mock HTTP server; docs/providers-openai.md.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Implemented crates/harness-providers/src/openai.rs plus openai/{request,stream,transport,notice,mock}.rs.
- Responses API, stream=true, store=false; function tools with parallel_tool_calls=true; output item k -> block 2k (text/tool/summary), 2k+1 (opaque encrypted reasoning item, provider 'openai', resent verbatim as an input item).
- Reasoning models (o1/o3/o4*, gpt-5* except -chat, codex*) get reasoning.summary=auto, include=[reasoning.encrypted_content], effort from ThinkingConfig (effort_for: explicit effort, else budget bucket <=4096 low, <=16384 medium, else high). Temperature omitted for reasoning models.
- Summary deltas stream as ReasoningDelta; encrypted-only reasoning emits a harness notice block (openai::notice::Notice::ReasoningHidden{reason: EncryptedOnly}). Notices exist because StreamEvent has no dedicated variant.
- Usage: input excludes cached, cache_read=cached_tokens, reasoning_tokens reported.
- In-stream error/response.failed mapped to typed retryable errors; a truncated stream is a Connection error, so it is retried.
- Shared transport::open_stream (retry_stream + HttpClient::open + sse_stream + per-attempt EventMapper) is reused by openrouter.
- DoD#4 N/A: no TUI surface.
Validation: cargo test -p harness-providers (103 unit tests pass; tests use a local mock HTTP server: parallel tools, encrypted reasoning round trip, 429 and mid-stream retry with discard_partial, auth not retried, truncated stream), clippy -D warnings clean, cargo doc clean. Docs: docs/providers-openai.md. No new ini keys.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
OpenAI Responses API provider (OpenAiProvider) with streaming function tools and parallel calls, stateless encrypted-reasoning round trip, effort mapping, cached/reasoning usage, live reasoning summaries and a hidden-reasoning notice. Verified with mock-server tests in openai.rs and unit tests in openai/*.rs; clippy clean.
<!-- SECTION:FINAL_SUMMARY:END -->
