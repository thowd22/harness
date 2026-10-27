---
id: TASK-166
title: Live Anthropic API verification
status: Done
assignee:
  - '@claude'
created_date: '2026-10-27 20:55'
updated_date: '2026-10-27 22:48'
labels: []
dependencies: []
priority: high
ordinal: 180000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Several Anthropic features were only ever tested against local mock servers, because no Anthropic key was available: gzip request bodies (TASK-127), thinking display and cache TTL options (TASK-161.4), the login key check (TASK-132) and the live smoke tests (TASK-15). The user has now provided a temporary Anthropic test key (ANTHROPIC_API_KEY in ~/.bashrc, interactive shells only, so use `bash -ic`). Mocks can drift from the real API, so this task runs targeted live checks against api.anthropic.com and fixes what the real API disagrees with. Keep spend low: mostly Haiku, small Sonnet/Opus checks only where a feature needs them.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 The existing opt-in live Anthropic tests (anthropic_provider live_smoke, conformance live anthropic, memory_evals live_correction_eval) pass against the real API, or each failure is fixed with an offline test
- [x] #2 Features previously verified only on mocks are checked live: gzip request bodies, streaming thinking with signatures round-tripped across a tool loop, prompt cache write/read counters, cache_ttl 1h and mixed, thinking_display omitted, and count_tokens
- [x] #3 The login key check has a live Anthropic test: the real key is accepted and a bogus key is rejected as an auth error
- [x] #4 The installed harness binary completes a headless run and a TUI turn with provider anthropic, including a tool call
- [x] #5 Findings, any fixes and the total live spend are recorded in the task notes; spend stays under $3
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
1. Run the existing opt-in live Anthropic tests (anthropic_provider live_smoke, conformance live anthropic, memory_evals live) via bash -ic; fix real-API mismatches with offline tests.
2. Add an ignored live test file (anthropic_live.rs, HARNESS_LIVE_ANTHROPIC=1): gzip body accepted, thinking + signature round-trip across a tool loop, cache write then read counters, cache_ttl 1h and mixed, thinking_display omitted, count_tokens.
3. Add live_anthropic_key_check to login_verify.rs.
4. Drive the installed harness binary with an isolated config (provider anthropic, api_key_env): headless run with a tool call, and a TUI turn in a private tmux socket.
5. Record findings and spend (usage tokens x pricing) in notes; keep under $3.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Live results 2026-10-27 (Haiku 5.5 unless noted):
- Existing live tests pass: anthropic_provider live_smoke, conformance live_anthropic (claude-haiku-4-5, text + tool call), memory_evals live_correction_eval.
- New tests/anthropic_live.rs (5 ignored live tests) all pass: gzip body accepted (34009 -> 4276 bytes, latch not tripped); thinking signatures round-trip through a tool loop for display summarized and omitted (omitted returns empty text with signatures, the API accepts them back); cache write then read for cache_ttl 5m, 1h and mixed (~12.8k tokens written, then read in full); prewarm writes the cache and the next call reads it; count_tokens equals billed input exactly (12802).
- Finding: under cargo, SSL_CERT_FILE/SSL_CERT_DIR are set in the test process (not in the shell), so gzip_requests=auto stays raw in cargo test/run. Running the test binary directly: auto enables gzip and it is accepted. Not a product bug; the live test uses GzipRequests::Always and docs/providers-anthropic.md notes it.
- login_verify live_anthropic_key_check added: real key accepted, bogus key -> ProviderError::Auth.

Installed binary (harness 0.5.0), isolated HARNESS_CONFIG/HARNESS_DATA_DIR/XDG dirs, provider anthropic via api_key_env, model claude-haiku-5-5:
- harness run --json: read tool call on notes.txt, correct answer, exit 0, 2 turns, $0.0005. Request bodies went gzip (installed binary is not affected by the cargo SSL_CERT_* env); turn 2 read the 3634-token cache written by turn 1. The key appears in no file under the data/config dirs.
- TUI (private tmux socket): glob + read tool calls, correct answer, cache 96-97% on later calls; /effort high turn streamed thinking (collapsed "thinking · 2 lines") and answered 0.05.
Spend: all live calls were Haiku 5.5 (plus a few Haiku 4.5 calls in conformance and the memory eval); estimated under $0.10 total from the reported usage, far under the $3 cap.
Offline gates: cargo fmt --check, clippy --workspace --all-targets -D warnings, nextest 2140 passed / 16 skipped (6 new ignored live tests), hermetic guard ok. No fix attempts hit the loop guard (the gzip test needed one diagnosis round). DoD #4 N/A: no UI changes.

CI: run 40591852268 on e87df57 failed on ubuntu in an unrelated test, early_tool_start_keeps_results_and_event_order: glob listed b.txt/a.txt in a different order between the on and off runs (newest-first sort, files written back to back). Fixed in 8c685ee by giving both files one fixed mtime (assertions unchanged, 5/5 local runs). CI run 40592742975 on 8c685ee: all jobs green on Linux and macOS.

Follow-up live checks (user asked whether all API features work), 8 more tests in anthropic_live.rs, all pass:
- thinking_display=updates: beta header accepted, tool loop completes.
- Haiku 4.5 budget thinking (thinking.type enabled): signed thinking round-trips through a tool loop.
- Parallel tool calls: several tool_use blocks in one message, results returned together, answer uses them.
- The API records each cache_ttl as requested (non-streamed body from build_body): 5m -> ephemeral_5m 25232 / 1h 0; 1h -> 1h 25229 / 5m 0; mixed -> 1h 12726 (tools+system) and 5m 12503 (message tail).
- count_tokens accepts a gzip body (latch not tripped).
- Effort low/medium/high on Haiku 5.5, and low on Sonnet 5.5 and Opus 5.5: accepted, correct answer, end_turn.
- max_tokens 5 -> StopKind::MaxTokens.
- Base64 PNG image input understood ("red").
Not checkable live: redacted_thinking (cannot be triggered on demand), mid-stream overload/rate-limit retries and the gzip rejection fallback (the real API never produces them on request); these stay covered by mock tests. Extra spend about $0.01.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Verified the Anthropic provider against the real API with a temporary key. Existing live tests (provider smoke, conformance, memory eval) pass. New opt-in tests/anthropic_live.rs covers gzip bodies, thinking signatures across a tool loop (summarized and omitted), cache write/read for 5m/1h/mixed TTLs, pre-warm and count_tokens; login_verify gains a live Anthropic key check. The installed 0.5.0 binary completed a headless run and TUI turns (tool calls, caching, live thinking) with the key never written to disk. No product bugs found; documented that cargo run/test export SSL_CERT_* so gzip auto stays raw there. Live spend under $0.10. Also fixed an order-dependent flake in early_tool_start found by CI. CI green on 8c685ee.
<!-- SECTION:FINAL_SUMMARY:END -->
