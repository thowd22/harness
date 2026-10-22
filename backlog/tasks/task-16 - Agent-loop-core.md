---
id: TASK-16
title: Agent loop core
status: Done
assignee:
  - '@agent-loop'
created_date: '2026-10-22 14:30'
updated_date: '2026-10-22 16:15'
labels:
  - core
  - agent
milestone: m-1
dependencies:
  - TASK-8
  - TASK-4
  - TASK-14
documentation:
  - backlog/docs/doc-1 - Architecture-and-Problem-Map.md
priority: high
type: feature
ordinal: 16000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
The agent loop turns model output into tool calls and back. It must be a clean state machine that emits events for everything it does so it can be visualized, resumed and evaluated.

## Loop guard (for agents)

Limit how many times you test and fix. If the same test, build or check still fails after **3 fix attempts**, or you have made no measurable progress in about 30 minutes, **stop**. Do not keep retrying, and never delete, skip, ignore or weaken a test or check to get a green run. Record what you tried, the exact failing output and your best hypothesis in the implementation notes, leave the task In Progress, and ask the user how to proceed.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 Turn state machine handles request, streaming, tool dispatch and result submission until end turn
- [x] #2 Stops on end turn, max_turns, budget exhaustion, user cancel or unrecoverable error, each with a distinct status
- [x] #3 Cancellation propagates to in-flight model calls and tools
- [x] #4 Every state transition emits events consumed by the run store
- [x] #5 Integration tests use the fake provider
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
1. harness-agent::agent_loop: explicit TurnState machine (Request -> Streaming -> Dispatch -> Submit -> Done) over Arc<dyn Provider>, recording every transition through RunRecorder::emit.
2. StopReason {EndTurn, MaxTurns, Budget, Cancelled, Error} with distinct NodeStatus/error codes; LoopLimits for max_turns/tokens/usd with a pluggable cost fn.
3. CancellationToken passed to Provider::stream and raced against every tool future (dropping the future kills bash).
4. Conversation history built from the redacted forms the recorder logs (closes TASK-5 AC#3).
5. Integration tests with FakeProvider + temp data dir, reading events back with read_run.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Implemented harness_agent::agent_loop (AgentLoop, TurnState, StopReason with codes::*, Conversation, LoopConfig/LoopLimits, CostFn, Verifier, LoopOutcome). History holds exactly what the recorder logs (messages redacted via recorder.redactor().redact_json, tool results labeled with provenance::Labeler or redacted), which closes TASK-5 AC#3 on the agent side. model_call_started payload = {provider, model, system_sha256, system (first call of a run or when changed), tools, tool_choice, max_tokens, thinking, messages_from, messages (delta)}; model_call_finished.response = the (prepared, redacted) assistant Message JSON. Provider-reported cost (openai::notice::reported_cost, OpenRouter) is preferred over the price fn (TASK-13 AC#3 integration, tested). Stream retries become recoverable error events ('retrying in Ns (reason, attempt k/N)'). Tests: crates/harness-agent/tests/agent_loop.rs (14 fake-provider integration tests: end turn, tool loop, max_turns, token + cost budget, cancel during stream and during tool, provider error, redaction, loop detection, malformed/unknown calls, pause/context limit, verifier, delta logging) + src/agent_loop/tests.rs. cargo test -p harness-agent and cargo clippy -p harness-agent --no-deps --all-targets -D warnings pass. Docs: docs/agent-loop.md. No new ini keys. DoD#4 N/A (no TUI surface).
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Agent loop state machine over Provider/RunRecorder with distinct stop reasons, cancellation into model calls and tools, events for every transition and exact redacted history; verified by 14 fake-provider integration tests plus unit tests.
<!-- SECTION:FINAL_SUMMARY:END -->
