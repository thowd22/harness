---
id: TASK-83
title: Cache-stable request layout
status: Done
assignee:
  - '@context-cost-commands'
created_date: '2026-10-22 14:30'
updated_date: '2026-10-22 19:16'
labels:
  - context
  - problem-4
  - problem-6
  - problem-9
milestone: m-8
dependencies:
  - TASK-43
  - TASK-11
priority: high
type: feature
ordinal: 83000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
A caching bug that cleared history each turn degraded a major harness while the model was unchanged (problem 9). Prefix stability must be tested.

## Loop guard (for agents)

Limit how many times you test and fix. If the same test, build or check still fails after **3 fix attempts**, or you have made no measurable progress in about 30 minutes, **stop**. Do not keep retrying, and never delete, skip, ignore or weaken a test or check to get a green run. Record what you tried, the exact failing output and your best hypothesis in the implementation notes, leave the task In Progress, and ask the user how to proceed.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 Request order is system, tools, memory index, skill listing, then history, with cache breakpoints at layer boundaries
- [x] #2 Memory index and skill listing are frozen at session start
- [x] #3 Tests assert the serialized prefix is byte-identical across turns for each provider
- [x] #4 Cache hit rate is recorded per call
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
1. context::prefix::SessionPrefix::freeze(system, memory_index, skills) builds the frozen system text in layer order with byte ranges and sha256.
2. Tools are fixed per session (ToolSet) and travel in the request's tools field; providers put breakpoints at end of tools and end of system (anthropic::request).
3. tests/request_layout.rs builds real Anthropic/OpenAI/OpenRouter bodies over a multi-run tool loop and asserts byte-identical prefixes.
4. Cache hit rate per call from recorded usage (cost::CallRecord::cache_hit_rate).
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Done: AC#2 (SessionPrefix frozen; memory/skill changes after freeze do not alter it), AC#3 (serialized_prefix_is_byte_identical_across_turns_for_each_provider), AC#4 (cache hit rate per call from model_call_finished usage; /cost shows it). Open AC#1: the order system -> tools -> memory index -> skills -> history is implemented and tested, but only two breakpoints sit at layer boundaries (end of tools, end of the frozen system text). Separate breakpoints between system / memory index / skills need ChatRequest to carry system layers (harness-providers) so anthropic::request::build_body can emit one system block per layer, and the binary (crates/harness/src/headless.rs, TUI session setup) must build Conversation::system from SessionPrefix::freeze(prompt, Some(SessionIndex.text()), Some(skills::listing(..))). Both are other owners' crates.

Integration: ChatRequest.system_layers; Anthropic build_body emits one system block per layer (system, tools, memory index, skill listing) with breakpoints at layer boundaries, then history. Tests: providers anthropic request tests, harness-agent request_layout layered prefix test, golden snapshots.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Integration: ChatRequest.system_layers; Anthropic build_body emits one system block per layer (system, tools, memory index, skill listing) with breakpoints at layer boundaries, then history. Tests: providers anthropic request tests, harness-agent request_layout layered prefix test, golden snapshots. Workspace green: cargo fmt --check, clippy -D warnings, cargo test --workspace (1142 passed, 0 failed).
<!-- SECTION:FINAL_SUMMARY:END -->
