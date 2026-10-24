---
id: TASK-161.2
title: Per-call tool completion events and opt-in early start of read-only tools
status: Done
assignee:
  - '@claude-161-2'
created_date: '2026-10-23 21:08'
updated_date: '2026-10-23 22:38'
labels:
  - performance
dependencies: []
parent_task_id: TASK-161
priority: high
ordinal: 170000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Read-only tool calls run in parallel only after the whole message ends (dispatch.rs:980, agent_loop.rs:1080). The OpenRouter stream mapper only emits block-stop events at message end (openrouter/stream.rs:222-237), so nothing can start early, and large tool arguments arrive in one burst (1.5-2.5 s on some calls). Fix: emit a per-tool-call completion event in every stream decoder as soon as a call's arguments are complete (Anthropic content_block_stop, OpenAI output_item.done, OpenRouter when the next tool call index or content starts / finish), then behind [experimental] early_tool_start (off by default) start auto-allowed read-only calls immediately; dispatch awaits the existing handles; results and event-log order unchanged; cancelled and discarded on stream errors or retries. Also evaluate eager_input_streaming for Anthropic write/edit/bash tools.

Context: TASK-161 notes (baseline report, web research) and docs/performance.md; measure with harness bench (--fake for overhead, live via OpenRouter Opus 5.5, targeted and budget-capped).
## Loop guard (for agents)

Limit how many times you test and fix. If the same test, build or check still fails after **3 fix attempts**, or you have made no measurable progress in about 30 minutes, **stop**. Do not keep retrying, and never delete, skip, ignore or weaken a test or check to get a green run. Record what you tried, the exact failing output and your best hypothesis in the implementation notes, leave the task In Progress, and ask the user how to proceed.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 Each decoder emits a per-call completion event at the earliest correct point (unit tests per provider with chunked fixtures)
- [x] #2 With early_tool_start on, read-only allowed calls start before message end, results and event order are identical to off, and stream errors/retries cancel speculative work (tests)
- [x] #3 Policy prompts, mutating tools and approvals behave exactly as before; bench shows before/after on a multi-call turn
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
1. Decoders: keep Anthropic content_block_stop and OpenAI output_item.done stops (already per call), make OpenRouter close a tool call when the next tool call or text starts and at finish_reason; chunked-fixture tests per provider.
2. ResponseBuilder::tool_use(index) to read a stopped call as finish() builds it.
3. Dispatcher: ToolGate::auto_allows (pure, no record/prompt; PolicyGate/CappedGate/AllowAll), Dispatcher::start_early (read-only built-in, valid args, auto-allowed, spawned task with held-back tool events), run_with_early (use an early task only for the same id/tool/args in the first batch after the normal review; others cancelled on drop).
4. Agent loop: LoopConfig.early_tool_start; start leading startable calls at BlockStop; clear on discard-partial retry; dropped on stream error/cancel/budget stop.
5. Config [experimental] early_tool_start (off), core.rs wiring; docs (performance.md, harness-ini guide, doc-2), CHANGELOG.
6. Tests: agent integration (start before end, identical events/messages, mutating-first, gate asks, errors, retries, cancellation, changed args), dispatch policy equivalence, harness E2E on/off; bench before/after (fake, paced multi-call, one small live run); evaluate eager_input_streaming.

7. Review fix: hold back ReadTracker records of early calls (StagedReads via ToolContext), commit only when the early outcome is used; real read/write agent tests for retry, stream error, cancellation and used-read.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Implementation (2026-10-23, @claude-161-2):
- Decoders: Anthropic (content_block_stop) and OpenAI Responses (output_item.done) already stopped each call at the earliest point; added chunked tests. OpenRouter (crates/harness-providers/src/openrouter/stream.rs) now emits BlockStop for open tool calls when the next tool call or text starts and at finish_reason (before usage chunk and [DONE]); finalize emits only the rest.
- crates/harness-providers/src/provider.rs: ResponseBuilder::tool_use(index).
- crates/harness-agent/src/dispatch.rs: ToolGate::auto_allows (default false; AllowAll true; PolicyGate/CappedGate via PolicyGate::allows_in using NoDecisions, no event, no prompt), Dispatcher derives Clone, start_early, run_with_early (run delegates), EarlyCalls (drop = cancel+abort), held-back tool events replayed when awaited. MCP tools are never started early. Panicked early task -> call runs again.
- crates/harness-agent/src/agent_loop.rs: LoopConfig.early_tool_start (default false); streaming starts the leading run of startable calls at each BlockStop, clears on Retry{discard_partial}, early calls dropped on error/cancel/budget stop; dispatch passes them to run_with_early.
- Config: [experimental] early_tool_start (harness-config), core.rs one line. Subagents do not use it (documented).
- Docs: docs/performance.md (Early tool start section, table row, ranked items 3 and 5), docs/guide/harness-ini.md, doc-2 via backlog doc update, CHANGELOG Unreleased/Added. No default changed; defaults_changelog test passes unchanged, so no Defaults entry.

Tests: harness-providers each_tool_call_stops_at_its_content_block_stop, each_function_call_stops_at_its_output_item_done, openrouter tool_calls_stop_when_the_next_one_starts_and_at_finish, text_after_a_tool_call_completes_it, tool_use_returns_a_stopped_call_as_finish_builds_it; harness-agent tests/early_tool_start.rs (8: start before message end with identical events and messages vs off, reads after a mutating call wait, gate-asked calls not started and one review, stream error / retry / cancellation cancel early calls, changed arguments rerun, off by default); dispatch::tests::early_start_keeps_policy_prompts_and_decisions (real PolicyGate, Ask mode, protected path: same outcomes, 1 prompt, same 3 policy_decision events, only the allowed read started early); harness tests/early_tool_start.rs (binary E2E on vs off: same tool calls/results/event sequence). nextest -p harness -p harness-agent -p harness-providers -p harness-config -p harness-eval: 854 passed (excluding harness-eval golden coder_request_options, a new test from another subtask with no accepted snapshot yet; not touched by this task). clippy -D warnings clean for the four crates; rustfmt clean for touched files.

Bench (dev profile, Linux x86_64 16 CPU, interleaved):
- Bundled harness bench --fake --runs 10, 2x each: overhead per call p50 off 3.244/3.233 ms, on 3.229/3.422 ms; run wall p50 off 156.8/156.7, on 157.0/167.8 ms (noise); tools p50 1.30 -> 1.10-1.13 ms. No stream delays in that script, so no wall gain expected.
- Paced multi-call turn (harness run --fake-script, text + grep count + glob + grep files + read over an 8.5 MB 451-file Rust tree, 8-char deltas 2 ms apart), best of 7: off 275 ms (275-290), on 229 ms (229-241): -46 ms = the read-only tools' run time overlapped with streaming.
- Live OpenRouter Opus 5.5, one 2-call bench session with early_tool_start on (--turns 1 --max-turns 2, budget 0.3): bench 'early' = 5.7 ms (reply with two reads, args in a burst) and 109.6 ms (reply ending with bash; finish_reason chunk precedes the usage chunk by ~0.1 s). Baseline was 0.0 in every live call. Spend: $0.027.
eager_input_streaming evaluated (not implemented): shortens arg bursts of large write/edit/bash calls but those are mutating and never start early, so it only speeds the visible delta stream; partial/invalid JSON risk at max_tokens (repair layer handles); needs an Anthropic-direct key to measure and touches anthropic/request.rs (TASK-161.4 scope) -> proposed as an opt-in provider option there.

DoD #4: N/A, nothing visible in the TUI.

Review fix (2026-10-23): both blocking findings were real (same bug). With early_tool_start on, a discarded early read (retry with discard_partial, stream error, cancel, changed args, review deny) still recorded the file in the shared ReadTracker, so a later write/edit skipped the NotRead/StaleRead refusal. Fix: harness-tools tracker.rs StagedReads (held-back (tracker, path, hash) records; commit applies, drop discards); ToolContext.staged_reads + with_staged_reads + record_read; ReadTool records via ctx.record_read. dispatch.rs: Dispatcher.staged_reads (set only on an early call's runner, passed into run_one's ToolContext); EarlyCall.reads committed in EarlyCall::join only on Some(Ok(outcome)) (used result), never on cancel/panic/drop. Normal (non-early) calls record directly as before. Tests: harness-agent tests/early_tool_start.rs +4 with the real read/write tools: a_read_discarded_by_a_retry_does_not_count_as_seen (write refused with 'you have not read it', messages and events identical to off, file unchanged), ..._by_a_stream_error_..., ..._by_cancellation_... (direct write afterwards fails NotRead), a_used_early_read_counts_as_seen (on == off, write allowed). The three discard tests fail without the fix (verified by disabling staging) and pass with it. Unit tests: tracker::staged_reads_apply_only_on_commit, read::staged_context_holds_back_the_read_record. nextest -p harness-agent -p harness-tools: 273 passed; -p harness --test early_tool_start passes; clippy -D warnings and fmt clean. docs/performance.md Early tool start paragraph notes the held-back tracker record. No default, prompt or request change; no live calls made (spend $0).
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Every decoder now reports a tool call's end as soon as its arguments are complete (OpenRouter newly closes calls at the next call/text and at finish_reason). Behind [experimental] early_tool_start (off by default) the agent loop starts the leading read-only, auto-allowed built-in calls during streaming. The dispatcher awaits them after the normal review, holding back their events and their read-tracker records, so results, approvals, the event log and the write/edit read-before-write and stale-read checks are identical to off. Stream errors, discard retries and cancellation cancel them and drop their records. Verified with per-provider chunked decoder tests, 12 agent integration tests (4 with the real read/write tools for discarded and used early reads), a PolicyGate equivalence test, a binary E2E on/off test, clippy and fmt clean. A paced multi-call turn went from 275 to 229 ms, fake-bench overhead is unchanged, and the live OpenRouter early window went from 0 to 5.7/110 ms ($0.027).
<!-- SECTION:FINAL_SUMMARY:END -->
