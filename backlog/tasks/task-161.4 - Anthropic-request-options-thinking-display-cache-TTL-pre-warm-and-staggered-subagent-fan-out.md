---
id: TASK-161.4
title: >-
  Anthropic request options: thinking display, cache TTL, pre-warm and staggered
  subagent fan-out
status: Done
assignee:
  - '@claude-161-4'
created_date: '2026-10-23 21:08'
updated_date: '2026-10-23 22:59'
labels:
  - performance
dependencies: []
parent_task_id: TASK-161
priority: high
ordinal: 172000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
anthropic/request.rs hardcodes thinking display summarized and the 5m cache TTL. Add [provider.anthropic] thinking_display = summarized|omitted|updates (default unchanged in the TUI; omitted for headless, eval and subagent runs only if measured faster and tested), cache_ttl = 5m|1h|mixed (1h on tools+system, 5m on messages) and an opt-in auto-1h while a background job or long tool runs, a cache pre-warm (non-streaming max_tokens 0 with identical thinking/effort) at session start, after compaction and before subagent fan-out (opt-in), and staggered subagent fan-out (first sibling starts, the rest wait for its first byte; opt-in). Apply the OpenRouter equivalents where they exist (reasoning exclusion, cache_ttl already exists).

Context: TASK-161 notes (baseline report, web research) and docs/performance.md; measure with harness bench (--fake for overhead, live via OpenRouter Opus 5.5, targeted and budget-capped).
## Loop guard (for agents)

Limit how many times you test and fix. If the same test, build or check still fails after **3 fix attempts**, or you have made no measurable progress in about 30 minutes, **stop**. Do not keep retrying, and never delete, skip, ignore or weaken a test or check to get a green run. Record what you tried, the exact failing output and your best hypothesis in the implementation notes, leave the task In Progress, and ask the user how to proceed.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 Each option is configurable, defaults unchanged unless measured, and covered by request-body tests and golden snapshots
- [x] #2 Pre-warm and staggered fan-out send the right requests in the right order (fake-server tests) and never change answers
- [x] #3 doc-2 and docs/providers updated; Defaults CHANGELOG entry
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
1. Config (harness-config): [provider.anthropic] thinking_display = summarized|omitted|updates (default summarized), cache_ttl = 5m|1h|mixed (default 5m; mixed rejected for openrouter), cache_ttl_auto = off|1h (opt-in auto-1h); [provider.openrouter] reasoning_exclude (default false); [experimental] cache_prewarm, stagger_subagents (default off). Tests in config.rs.
2. Anthropic request builder: RequestOptions gains thinking_display and cache_ttl (mixed = 1h on tools+system, 5m on messages); auto-1h via a process-wide LongWait hint (harness-providers cache_hint module) set while a background job runs or after a long tool round; build_prewarm_body (stream false, max_tokens 0, same thinking/effort, breakpoints only on the shared prefix, placeholder user message unmarked, forced tool_choice dropped, skipped for budget-thinking models). updates adds beta thinking-display-updates-2026-08-18. Unit tests.
3. Provider trait: can_prewarm_cache + prewarm_cache (default unsupported), forwarded by ScheduledProvider and TimedProvider; Anthropic implements it (POST non-streaming, parse usage).
4. OpenRouter: reasoning.exclude when reasoning_exclude = true (request.rs reasoning block only).
5. harness-agent prewarm module: records a cache-prewarm child node (model call, usage, cost) like the session summary. Core: background pre-warm at session start/resume and after /compact (cache_prewarm). Staggered fan-out: provider wrapper with a shared gate keyed by the request prefix (model+system+tools): first request leads, same-prefix requests wait for its first stream event (or end/cancel); with cache_prewarm the leader pre-warms and releases the followers on completion. Installed for subagent children by SubagentRuntime.
6. Wiring (wiring.rs, minimal): map ini to AnthropicConfig/OpenRouterOptions; LongWait hooks (JobNotices before_request, agent loop tool round duration).
7. Tests: request-body unit tests, golden snapshot for non-default options (new snapshot only), mock-server tests for pre-warm body/order and stagger order, answers unchanged (fake provider).
8. Docs: doc-2, docs/providers-anthropic.md, providers-openrouter.md, performance.md, subagents.md; CHANGELOG Defaults entry if fingerprint changes. Bench before/after (fake) recorded in notes.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Implementation (2026-10-23, @claude-161-4):
- Config: [provider.anthropic] thinking_display (summarized|omitted|updates, default summarized), cache_ttl (5m|1h|mixed, shared ProviderConfig.cache_ttl field; [provider.openrouter] cache_ttl parses through OpenRouterCacheTtl so it still accepts only 5m|1h), cache_ttl_auto_1h (default false); [provider.openrouter] reasoning_exclude (default false); [experimental] cache_prewarm, stagger_subagents (default false). New fields skip serialization when unset: config hash and defaults fingerprint unchanged (46b4cecb7b8e).
- harness-providers: anthropic/request.rs CacheTtl/ThinkingDisplay in RequestOptions, per-breakpoint lifetimes (mixed = 1h tools+system, 5m messages), build_prewarm_body (stream false, max_tokens 0, same tools/system/thinking/effort, unmarked 'warmup' placeholder after the shared messages, forced tool_choice dropped, None for budget thinking or no caching); anthropic.rs adds the updates beta header, LongWait upgrade, prewarm_cache; provider.rs trait methods can_prewarm_cache/prewarm_cache (default unsupported), forwarded by TimedProvider and ScheduledProvider (takes a call slot); cache_hint.rs process-wide LongWait (background job running, or 30 min after a >=3 min start-to-start gap in one run); openrouter reasoning.exclude.
- harness-agent: long_wait.rs LongWaitHook (installed for the top agent and every child; never changes requests), prewarm.rs (cache-prewarm child node with model call, usage, cost), stagger.rs FanOut + StaggeredProvider (keyed by provider/model/thinking/system/tools; leader alone until its first non-retry stream event, failed leader hands over, MAX_WAIT 30 s, WARM_FOR 4 min; with cache_prewarm the leader pre-warms tools+system and opens the gate when it returns), SubagentRuntime::with_fan_out.
- harness: wiring maps the keys; Core::spawn_prewarm (TUI session start/resume, after /compact; bench mimics session start), turn waits up to 10 s for an in-flight pre-warm, finish cancels it; runtime gets FanOut when stagger_subagents.
- Not done: thinking_display=omitted for headless/eval/subagents stays off (no Anthropic-direct key to measure; OpenRouter has no display; reasoning_exclude is the OpenRouter equivalent for the TASK-161.7 A/B). OpenRouter pre-warm: live check showed max_tokens 0 is ignored (22 output tokens), so none.
Measurements: fake bench dev 10 runs interleaved before/after/before/after: overhead per call p50 3.24/3.23/3.24/3.23 ms, run wall p50 157.7/157.2/155.4/156.1 ms (no regression from the hook). Live: 2 OpenRouter Opus 5.5 calls, $0.0106 (max_tokens 0 probe $0.0101 incl. 1,923-token cache write + 22-token answer; follow-up read 1,923 cached tokens $0.0005). Cost model for stagger (3 siblings, ~3k-token prefix, Opus 5.5): $0.045 -> $0.016 per fan-out.
Tests: anthropic::request (ttl per breakpoint, display values, pre-warm body), anthropic.rs (beta header, LongWait upgrade, usage), tests/anthropic_provider.rs cache_prewarm_sends_a_non_generating_call_first, openrouter reasoning exclusion, cache_hint, harness-config option tests, harness-agent stagger::tests (4), prewarm::tests (2), subagents staggered_fan_out_keeps_requests_and_answers, tests/long_wait.rs (3), harness tests/cache_prewarm.rs (binary vs mock Messages API: pre-warm first, real requests and answers unchanged), wiring tests, golden snapshot request_options_coder (new; request_* unchanged by this task).
Validation: fmt; clippy -D warnings clean for harness-providers/config/agent/eval and harness --no-deps; nextest --workspace 2030/2031 passed; the one failure harness::early_tool_start early_tool_start_keeps_results_and_event_order (TASK-161.2's test) failed once under full-suite load and passed 3/3 alone. harness-agent background_compaction cancelling_the_run_cancels_the_summary... (TASK-161.5, in progress) failed once earlier mid-edit and passed in later full runs.

DoD #4 UI interaction: N/A (no visible TUI element added; pre-warm shows only as a cache-prewarm node in the existing map).

Review fixes (2026-10-23, @claude-161-4):
- Finding 1 confirmed (TUI): a cache-prewarm node before the first turn counted as the main agent in Transcript/Activity (main = None), so it added an empty '◆ assistant' entry and moved the activity line; its NodeFinished could reset an active turn to Idle. The earlier DoD #4 note ('shows only as a cache-prewarm node in the map') was wrong. Fix: ChatView::apply_run_event (harness-tui chat/view.rs) tracks nodes started with name/agent cache-prewarm and keeps their events out of the transcript, activity line and notifications; only their ModelCallFinished (tokens) and Cost reach the status line, and the map still shows the node. harness-tui has harness-agent only as a dev-dependency, so view.rs has its own PREWARM_NODE const with a test pinning it to harness_agent::prewarm::PREWARM_NODE. Parent: Core::prewarm_parent uses the latest turn's node, or before the first turn the node that turn will run on (Core::next_turn_node, taken by Core::turn), instead of NodeId::random(). (A parent is required: the recorder copies it onto every event of the node, which keeps the pre-warm out of rebuild_conversation and the status line's context figure.) Correction to the review evidence: on resume / after /compact the node is NodeKind::Agent, so no subagent card was shown; only the main = None case leaked. Tests: chat::view::tests a_session_start_prewarm_is_not_a_turn, a_prewarm_finishing_during_a_turn_does_not_end_it (both fail with the filter disabled), the_prewarm_node_name_matches_the_agent_crate; tests/cache_prewarm.rs now also asserts the pre-warm node's parent is the turn's root node (fails with the old random parent).
- Finding 2 confirmed (budget): the stagger leader's pre-warm cost went only into a Cost event. Fix: FanOut::charge_to(meter) (shared OnceLock, set in Core::start right after the session meter is built); StaggeredProvider charges report.cost_usd on success and skips the pre-warm while meter.check() reports exceeded, as Core::spawn_prewarm does. Scope note: like the session pre-warm and the summary, it charges the shared [budget] scopes (run/session/day), not a child's per-loop limit. Tests: stagger::tests the_leaders_prewarm_is_charged_to_the_budget, no_prewarm_while_a_budget_is_exhausted.
- DoD #4: no new interactive element; the pre-warm is now hidden from the chat (covered by the chat-view tests above) and appears only as an ordinary node in the existing map, which is already mouse/keyboard operable.
- Docs: performance.md (pre-warm row: node placement, budget), subagents.md (fan-out pre-warm charged, skipped when exhausted). No ini default, prompt or request change: golden_requests and defaults_changelog pass unchanged, no CHANGELOG Defaults entry needed.
- Validation: cargo fmt --check clean; clippy -D warnings clean for harness-agent, harness-tui (all targets) and harness (--no-deps); nextest -p harness-agent -p harness-tui -p harness 845/845 passed; harness-eval golden_requests + defaults_changelog 9/9. No live calls ($0).
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Added opt-in Anthropic/OpenRouter request options and cache experiments with defaults unchanged: [provider.anthropic] thinking_display, cache_ttl (5m|1h|mixed), cache_ttl_auto_1h (LongWait hint from background jobs and long tool rounds); [provider.openrouter] reasoning_exclude; [experimental] cache_prewarm (max_tokens 0 non-streaming pre-warm at TUI session start/resume, after /compact, before fan-out; recorded as a cache-prewarm node under the turn it warms, hidden from the chat transcript and activity line, and charged to the [budget] meter, including the stagger leader's fan-out pre-warm, which is skipped while a budget is exhausted) and stagger_subagents (same-prefix sibling requests wait for the first response to begin). Verified with request-body unit tests, a new golden snapshot (request_options_coder; default snapshots unchanged), mock-server tests (provider pre-warm order, binary end to end incl. pre-warm parent), stagger/fan-out order, answer-equality and budget-charge tests, chat-view tests that a pre-warm adds no turn and does not end an active one, long-wait hook tests; fake bench overhead unchanged (3.23-3.24 ms/call); live spend $0.0106 (OpenRouter ignores max_tokens 0, so no OpenRouter pre-warm). Docs: doc-2, providers-anthropic/openrouter, performance, subagents, guide; CHANGELOG Added + Defaults entry.
<!-- SECTION:FINAL_SUMMARY:END -->
