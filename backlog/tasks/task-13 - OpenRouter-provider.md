---
id: TASK-13
title: OpenRouter provider
status: Done
assignee:
  - '@providers-openai-openrouter'
created_date: '2026-10-22 14:30'
updated_date: '2026-10-22 19:16'
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
ordinal: 13000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
OpenRouter gives access to many models through one key. Its OpenAI-compatible API has its own extensions for routing, reasoning and cost accounting.

## Loop guard (for agents)

Limit how many times you test and fix. If the same test, build or check still fails after **3 fix attempts**, or you have made no measurable progress in about 30 minutes, **stop**. Do not keep retrying, and never delete, skip, ignore or weaken a test or check to get a green run. Record what you tried, the exact failing output and your best hypothesis in the implementation notes, leave the task In Progress, and ask the user how to proceed.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 Chat completions streaming with tools and reasoning tokens
- [x] #2 provider_order and allow_fallbacks from harness.ini are sent as routing preferences
- [x] #3 Usage accounting is requested and the reported cost is used when available
- [x] #4 App attribution headers are set from app_name
- [x] #5 Models that lack tool support are rejected at agent load with a clear error
- [x] #6 Reasoning deltas are streamed live when the upstream model provides them, and their absence is flagged
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
1. Chat Completions request builder: tools, reasoning (effort/max_tokens), usage.include, provider.order/allow_fallbacks.
2. Attribution headers (X-Title, HTTP-Referer) from app_name.
3. Stream mapper: content, reasoning, reasoning_details (round-tripped as Opaque), tool_call deltas, usage incl. cost (harness cost notice), absent-reasoning notice.
4. /models catalog for capabilities and check_agent_model that rejects non-tool models with a clear error.
5. Mock-server tests; docs/providers-openrouter.md.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Implemented crates/harness-providers/src/openrouter.rs plus openrouter/{request,stream,models}.rs. It reuses openai::transport and openai::notice (re-exported as openrouter::notice).
- POST chat/completions with stream=true and usage.include=true. Tools are function tools, tool_choice is mapped, and reasoning comes from ThinkingConfig (effort, else max_tokens, else enabled).
- OpenRouterOptions{app_name, provider_order, allow_fallbacks} -> provider{order, allow_fallbacks}. app_name -> X-Title, and HTTP-Referer too when it is an http(s) URL.
- Stream: delta.reasoning -> live ReasoningDelta (falls back to reasoning_details text). reasoning_details are merged by index into an opaque 'openrouter' block that is resent as assistant.reasoning_details. If reasoning was requested or billed but not streamed, a Notice::ReasoningHidden is emitted. usage.cost -> Notice::Cost, read with notice::reported_cost(&Response). Mid-stream error chunks are mapped by code and retried.
- models: GET /models catalog (cached), ModelInfo, check_tools. OpenRouterProvider::check_agent_model(model, needs_tools) fails for unknown models and with a clear 'does not support tool calling' error. stream() refuses tool requests to catalogued non-tool models.
Validation: cargo test -p harness-providers passes (mock-server tests for routing body, headers, usage+cost, reasoning round trip, absence flag, catalog rejection, upstream-error retry). clippy -D warnings is clean. Docs: docs/providers-openrouter.md. No new ini keys (app_name, provider_order and allow_fallbacks are already in doc-2).
REMAINING (cross-crate, so the task stays In Progress):
AC#3: cost is requested and exposed, but nothing uses it yet. The cost/pricing owner (TASK-9 pricing, or the agent's cost accounting) must prefer openrouter::notice::reported_cost(&response) over the price-table estimate, with a test.
AC#5: the rejection exists as OpenRouterProvider::check_agent_model and is tested, but the agent loader (harness-agent) must call it when an agent that uses tools is loaded on an openrouter model, with a test.
DoD#1 and DoD#2 wait on those integrations (tests and docs at the integration point). A better long-term fix is adding cost_usd to StreamEvent::MessageStop/Response in provider.rs (owned by provider-core).

Integration: the agent loop prefers openai::notice::reported_cost over the price table (harness-agent tests/agent_loop.rs cost_budget_uses_cost_fn_and_reported_cost_wins); the binary's async provider calls OpenRouterProvider::check_agent_model at agent load (crates/harness/tests/commands.rs openrouter_models_without_tools_are_rejected_at_agent_load, local stub server).
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Integration: the agent loop prefers openai::notice::reported_cost over the price table (harness-agent tests/agent_loop.rs cost_budget_uses_cost_fn_and_reported_cost_wins); the binary's async provider calls OpenRouterProvider::check_agent_model at agent load (crates/harness/tests/commands.rs openrouter_models_without_tools_are_rejected_at_agent_load, local stub server). Workspace green: cargo fmt --check, clippy -D warnings, cargo test --workspace (1142 passed, 0 failed).
<!-- SECTION:FINAL_SUMMARY:END -->
