---
id: TASK-8
title: Normalized message model and Provider trait
status: Done
assignee:
  - '@provider-core'
created_date: '2026-10-22 14:30'
updated_date: '2026-10-22 15:09'
labels:
  - provider
  - core
  - problem-1
milestone: m-1
dependencies:
  - TASK-1
documentation:
  - backlog/docs/doc-1 - Architecture-and-Problem-Map.md
priority: high
type: feature
ordinal: 8000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Anthropic, OpenAI and OpenRouter have different message, tool and streaming formats. Most harness bugs come from integration errors (problem 1), so the rest of the system must only see one normalized model.

## Loop guard (for agents)

Limit how many times you test and fix. If the same test, build or check still fails after **3 fix attempts**, or you have made no measurable progress in about 30 minutes, **stop**. Do not keep retrying, and never delete, skip, ignore or weaken a test or check to get a green run. Record what you tried, the exact failing output and your best hypothesis in the implementation notes, leave the task In Progress, and ask the user how to proceed.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 Content blocks cover text, image, tool use, tool result, reasoning or thinking, and provider-opaque blocks that must round-trip
- [x] #2 Provider trait exposes a stream of normalized events plus a final usage record with input, output, cache read, cache write and reasoning tokens
- [x] #3 Capabilities describe tools, parallel tool calls, thinking, caching, images and context window
- [x] #4 Stop reasons are normalized with the raw provider value preserved
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
1. model.rs: Message/Role/ContentBlock (text, image, tool_use, tool_result, reasoning, opaque), ToolSpec, ChatRequest, Usage, StopReason{kind,raw} with Anthropic/OpenAI mappers, Capabilities.
2. provider.rs: ProviderError, StreamEvent, EventStream, async Provider trait, ResponseBuilder/collect accumulator.
3. Unit tests for serde round-trip, usage, stop mapping, accumulator.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
model.rs: Role, Message, ContentBlock {Text, Image, ToolUse, ToolResult, Reasoning{signature}, Opaque{provider,data}}, ToolSpec, ToolChoice, ThinkingConfig, ChatRequest, Usage (5 counters, Add), StopKind/StopReason{kind,raw} with from_anthropic/from_openai, Capabilities. provider.rs: ProviderError (typed, retry_reason/retry_after/root), StreamEvent, EventStream, async Provider trait (name, capabilities, stream(req, CancellationToken), complete), ResponseBuilder + collect (discards partial output on Retry{discard_partial}). Invalid tool JSON kept as Value::String for actionable errors. Validation: cargo test -p harness-providers (40 unit + 12 mock-server transport + 1 YAML fixture test, all pass); cargo clippy -p harness-providers --all-targets -- -D warnings clean; cargo doc clean. Docs: docs/providers.md. No UI (DoD#4 N/A).
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Normalized message model and Provider trait in harness_providers::{model,provider}; covered by serde round-trip, stop-mapping, usage and accumulator tests.
<!-- SECTION:FINAL_SUMMARY:END -->
