---
id: TASK-161
title: >-
  Model speed: measure and remove every avoidable latency without losing
  precision or features
status: In Progress
assignee:
  - '@claude-bench'
created_date: '2026-10-23 19:49'
updated_date: '2026-10-23 23:45'
labels:
  - performance
  - providers
  - agent
dependencies: []
priority: high
ordinal: 168000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Goal: leave zero performance on the table. Turns should be as fast as the model allows, with no loss of answer quality, tool use or features. Start with measurement, then rank ideas by measured gain, and implement the no-regret ones (each as a subtask after user approval for anything that trades cost or behaviour).

Measure first: a 'harness bench' (or eval-suite) run that records, per turn and per model call: time to first byte/token, output tokens/s, end-to-end turn time, tool execution time, harness overhead between stream end and next request, request build/serialize time, compaction and summary time, and TUI render time. Use the fake provider for harness overhead and targeted live runs (OpenRouter Opus 5.5 is authorized, small and targeted) for real latency. Record a baseline report in the task.

Ideas to evaluate (measure each, keep only real wins):
- Prompt caching: 1h TTL for long sessions, keeping the cached prefix stable (tool order, system layers, no per-turn churn), cache warm-up of the system prompt at session start, caching subagent prefixes.
- Streaming tool execution: start read-only tool calls as soon as their tool_use block is complete in the stream instead of after message end; run independent calls in parallel (check current dispatcher behaviour).
- Connection reuse: one HTTP/2 client per provider, TLS/DNS pre-warm at startup and after idle, keepalive tuning, gzip where it helps (Anthropic request gzip exists; measure for OpenRouter).
- Provider routing: OpenRouter provider sort by latency/throughput and pinning the fastest upstream for a model, measured per model.
- Less work per call: compact tool schemas and descriptions, avoid resending unchanged large tool results (elision already exists; measure), smarter compaction timing, background session-summary calls off the critical path.
- Adaptive thinking/max_tokens settings that do not reduce quality (measure with the eval suite), speculative prefetch of files the model is likely to read next, faster local tools (grep/glob/read throughput on large repos).
- Harness overhead: event recording, redaction and TUI rendering must never sit on the request path.

Quality guard: every change is checked against the eval suites (pass rates and answers unchanged or better) and the golden request snapshots; no feature is removed.
## Loop guard (for agents)

Limit how many times you test and fix. If the same test, build or check still fails after **3 fix attempts**, or you have made no measurable progress in about 30 minutes, **stop**. Do not keep retrying, and never delete, skip, ignore or weaken a test or check to get a green run. Record what you tried, the exact failing output and your best hypothesis in the implementation notes, leave the task In Progress, and ask the user how to proceed.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 A repeatable benchmark (fake-provider and live modes) reports TTFT, tokens/s, turn time, tool time and harness overhead per call; a baseline report is recorded in the task
- [x] #2 Each idea above is measured or reasoned out with numbers, and ranked by gain, risk and cost in the task notes
- [x] #3 No-regret wins are implemented with before/after numbers and no eval or snapshot regression; anything trading cost or behaviour is proposed as a subtask for the user
- [ ] #4 A CI check guards harness overhead (fake provider) against regressions; docs/performance.md explains the measurements and settings
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 Tests: automated tests cover every acceptance criterion (unit and/or integration as appropriate) and pass locally and in CI; no existing test is deleted, skipped, ignored or weakened to get a green run
- [x] #2 Docs: user-facing behaviour is documented (docs/ and, if the design changed, the relevant backlog doc), public APIs have rustdoc, and any new config keys appear in the harness.ini reference
- [x] #3 Loop guard respected: no more than 3 fix attempts per failing test or check; if still failing, stop, record attempts, output and hypothesis in implementation notes, and ask the user instead of looping
- [x] #4 UI interaction: if this task makes anything visible in the TUI, every visible interactive element is clickable with the mouse and fully operable with the keyboard (arrows/Tab to focus, Enter to activate, Esc to back out), with tests for both input paths; tasks with no visible UI mark this N/A
<!-- DOD:END -->

## Implementation Plan

<!-- SECTION:PLAN:BEGIN -->
Measurement half (AC#1 + evidence base):
1. Study harness-eval runner, agent loop, providers (fake + openrouter/anthropic), events to find timing hook points.
2. Add a bench module in harness-eval (reusing runner/suite infra) with fake mode (deterministic scripted multi-turn coding task, offline, fast) and live mode (real provider), reporting per call/turn TTFB, TTFT, tok/s, call time, tool time per tool, harness overhead, compaction time, cached vs uncached tokens, render time; table + JSON output. Timing derived from event timestamps / Instant captures only when benching.
3. Expose as 'harness bench' subcommand (thin wiring).
4. Static measurements: request body sizes by part, tool schema tokens, stable prefix length per request, parallel read-only tools, HTTP client reuse/HTTP2, request-path I/O timings.
5. Live baseline: <=5 small OpenRouter Opus 5.5 sessions with --budget-usd caps, temp HOME/XDG/HARNESS_CONFIG and temp git repo; each config twice.
6. Write docs/performance.md and append baseline to task notes; fmt/clippy/nextest for touched crates.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
2026-10-23: the user is willing to get experimental and try bleeding-edge techniques. Experimental optimizations go behind opt-in settings (e.g. an [experimental] section, off by default) with before/after numbers and no eval or snapshot regression; safe defaults stay unchanged.

Web research (2026-10-23, read-only agent), ranked by gain per effort; sources are official docs unless marked:
1. OpenRouter: send session_id (sticky upstream routing keeps the Anthropic/Bedrock/Vertex cache warm; provider.order disables stickiness). openrouter.ai/docs/features/prompt-caching, guides/best-practices/latency-and-performance.
2. Anthropic thinking.display = omitted (faster time to first text token; default on 5.x; harness hardcodes summarized) or updates (beta thinking-display-updates-2026-08-18). platform.claude.com/docs/en/build-with-claude/thinking.
3. One shared reqwest client per (provider, base_url) + pool/keepalive tuning + background TLS pre-warm at startup and after idle (harness rebuilds providers per subagent, summary, dream).
4. Streaming tool execution: start auto-allowed read-only calls at content_block_stop / output_item.done (Claude Code StreamingToolExecutor, third-party sources).
5. Anthropic cache_ttl (5m/1h/mixed with 1h on tools+system), max_tokens:0 non-streaming cache pre-warm at session start/after compaction/before subagent fan-out, staggered subagent fan-out (cache entry exists only after the first response starts).
6. OpenAI prompt_cache_key + prompt_cache_retention 24h (GPT-5.5 supports only 24h; 5.6+ explicit breakpoints 30m/12h + prewarm).
7. Golden cache-stability test: serialized prefix byte-identical across turns; deterministic MCP tool order; per-turn notices only in trailing messages.
8. Effort sweep per model on evals; low effort on a cheap model for summaries/titles/dreaming; never change effort mid-session (per-message beta exists).
9. Opt-in cost trades: Anthropic speed=fast (up to 2.5x output tok/s on Opus 5.5, 2x price, fixed per session, no fallback flips), OpenAI service_tier=priority, OpenRouter :nitro. Anthropic Priority Tier discontinued.
10. Background compaction: start the handoff at ~0.7, swap at 0.8, then pre-warm; Anthropic server-side compaction beta compact-2026-09-04 as experimental.
11. OpenRouter provider.sort / preferred_max_latency (soft), endpoint latency stats for bench.
12. eager_input_streaming on write/edit/bash tools.
13. Smaller requests; skip Anthropic clear_tool_uses (duplicates elision, breaks cache).
14. EXPERIMENTAL: OpenAI Responses WebSocket mode (up to ~40% faster for 20+ tool calls per OpenAI).
15. EXPERIMENTAL: speculative read-only prefetch after grep/glob (PASTE arXiv 2603.18897).
Do not: lower max_tokens, change cached-prefix settings mid-session, pin provider.order, fan out subagents cold, flip fast/standard on 429, compact early, use flex/batch interactively, force HTTP/1.1.

Measurement half (AC#1 evidence base), 2026-10-23, @claude-bench:

Benchmark: `harness bench --fake [--runs N]` (offline, deterministic, harness overhead; ~0.1 s/run release, <1 s/run debug) and `harness bench --model REF --runs 1 --budget-usd 1 --json out.json --net` (live). Bundled task: crates/harness-eval/bench/ (Python inventory lib with a bug + tests, 2 prompts, 10-call fake script). Probe: crates/harness-providers/src/timing.rs (provider wrapper, installed only by bench; wrap() is identity otherwise), crates/harness-agent/src/timing.rs (hook spans); analysis/table/JSON: crates/harness-eval/src/bench.rs; command: crates/harness/src/bench.rs. Hooks into the normal path: 3 one-line wraps in crates/harness/src/core.rs (resolve_model, resolver, Core::turn hooks). Tests: harness-providers timing::tests, harness-eval bench::tests, crates/harness/tests/bench.rs (fake end to end incl. cache-prefix stability golden check: previous request is a byte-exact prefix of the next, cache_control ignored).

Baseline, Linux x86_64 16 CPU, release:
- Fake (10 runs): harness overhead per gap p50 0.33 ms / p90 0.59 / max 0.87; turn start 0.42 ms; all 4 request hooks <0.03 ms; run overhead (2 turns, 10 calls) 4.0 ms; TUI frame 0.32 ms p50 (120x40); emit model_call_started 43 us mean (max 226), delta 1.3 us; prefix breaks 0/90. Debug build: ~3.2 ms per gap.
- Live OpenRouter Opus 5.5, A B A B interleaved, one session per process (A=defaults, B=[experimental] prewarm_connection+openrouter_session_id): TTFB p50 per run A 2961/1417 ms, B 1566/1649 ms; p90 3185-3756 ms; streamed calls cluster 1.3-1.7 s, 3-4 s outliers are replies opening with a big tool call (args burst). Output ~167-173 tok/s p50. Call p50 2.9-4.1 s. Harness overhead per gap 0.62-0.79 ms. Cache-read share calls 2+ 92-95%, uncached input 2-4 tokens/call, 0 prefix breaks in 38 calls. Session start 292-819 ms (fake: 3.5 ms) = OpenRouter /models catalog (792 KB) fetch. Connection probe HTTP/2: cold 25-49 ms, warm 12-21 ms. Model time 98.5-99.6% of turn wall; tools 0.3-1.5% (bash+python ~50-65 ms); overhead ~0.02%.
- Experiments: prewarm 32-39 ms, still overlapping the first call; session_id: no change in cache share/TTFB (OpenRouter default stickiness hashes first system+user msg). Both no measurable gain within noise; off by default.
- Streaming tool execution through OpenRouter: early-start opportunity 0.0 ms in every live call: the stream mapper emits block_stop only at finalize (openrouter/stream.rs:222-237) and tool args arrive in a burst.
- Live spend: $0.368 total (4 sessions: 0.0935, 0.0827, 0.0989, 0.0928).

Ranked costs (see docs/performance.md for detail): 1 model TTFB/throughput (upstream); 2 session start 0.3-0.8 s OpenRouter catalog fetch every start on a separate client (wiring.rs:336, openrouter.rs:149,205) -> cache it / share chat client; 3 buffered tool args +1.5-2.5 s TTFT and no per-block stops (openrouter/stream.rs:222-237) -> prerequisite for early tool start; 4 fresh HTTP client per subagent/summary/dream provider (core.rs:244, sessions.rs:219, dream.rs:68), cold connect 25-49 ms+; 5 tools (bash process+python); 6 harness overhead <1 ms/call: model_call_started double serialization (blobs.rs:83, store.rs:390), scheduler relay task (scheduler.rs:415), specs rebuilt 3x + tools JSON hashed per request + history clone (agent_loop.rs:805,900,901,912); 7 TUI 0.3 ms/frame off the request path.

Not done here (for the implementation half): AC#2 numbers for 1h TTL / cache pre-warm / thinking.display / fast tiers (no Anthropic-direct key in scope; TTL has no effect inside a short bench), AC#3 implementations, AC#4 CI job (the fake bench is ready for it: compare summary.overhead_per_call_ms like-for-like build profile).

AC#1 verified: crates/harness/tests/bench.rs (fake end to end, 2 tests), harness-eval bench::tests (6), harness-providers timing::tests (3), harness-config experimental test, wiring session_id test all pass; fmt clean for touched crates; clippy -D warnings clean for harness-providers/agent/config/eval and harness --no-deps (harness-tui currently fails clippy in TASK-160 in-progress files: diff.rs, memory/*, panel.rs, chat/copy.rs; not mine). harness bin test tui::tests::jobs_panel_lists_opens_and_stops_background_jobs failed once under full-suite load and passed 3/3 alone (flaky, unrelated). Task left In Progress for AC#2-4.

TASK-161.7 live A/B results (see TASK-161.7 notes and docs/performance.md, section 'Fast mode, :nitro, reasoning and effort'):
Bench (bundled 2-turn task; TTFT/call over all calls of both sessions; tok/s (stream) = output tokens / (call end - first token) summed):
| Arm | Calls | TTFT p50 | TTFT p90 | Call p50 | Call p90 | tok/s p50 | tok/s stream | Turn p50 | Turn p90 | Session wall | Cost/session |
| std (defaults, no reasoning object) | 18 | 1766 | 3988 | 3333 | 4801 | 171 | 217 | 15.3 s | 17.9 s | 32.4/30.6 s | $0.108/$0.089 |
| fast (service_tier = fast) | 19 | 760 | 1869 | 1302 | 2042 | 441 | 560 | 7.0 s | 7.8 s | 13.5/14.1 s | $0.169/$0.172 |
| nitro (model :nitro) | 17 | 1041 | 1576 | 1513 | 2299 | 483 | 643 | 6.9 s | 7.1 s | 12.8/13.9 s | $0.195/$0.194 |
| effort low | 18 | 1414 | 3170 | 2743 | 4148 | 168 | 186 | 11.7 s | 17.1 s | 21.0/31.5 s | $0.057/$0.085 |
| effort medium | 18 | 1604 | 3830 | 3504 | 4813 | 165 | 182 | 14.9 s | 19.5 s | 29.8/32.4 s | $0.080/$0.092 |
| effort medium + reasoning_exclude | 18 | 1392 | 3656 | 2923 | 4266 | 170 | 191 | 13.9 s | 18.6 s | 27.9/30.8 s | $0.090/$0.091 |
| effort high | 24 | 1596 | 3514 | 3116 | 4300 | 173 | 238 | 19.1 s | 21.0 s | 35.9/40.5 s | $0.112/$0.120 |
(times in ms unless marked)

Evals (harness eval run --model opus; starter 6 tasks x1; search+identity 2 tasks x2):
| Arm | starter | search+identity | starter cost | wall/attempt | output tok |
| std | 6/6 | 4/4 | $0.265 | 16.3 s | 6386 |
| fast | 6/6 | - | $0.575 | 7.7 s | 5861 |
| low | 6/6 | 4/4 | $0.158 | 11.6 s | 3225 |
| medium | 6/6 | - | $0.252 | 18.4 s | 5627 |
| high | 6/6 | 3/4 | $0.279 | 19.9 s | 6533 |
The high miss: one find-session-spawns attempt listed vendor/ (1 of 2; noise at n=2).

Conclusions: fast mode = 2.2x faster turns, TTFT 1.77->0.76 s, stream 2.6x, +73% cost per session, pass rate unchanged -> opt-in setting `[provider.openrouter] service_tier = fast|default` (new). :nitro = same speed, slightly more spend, auto-picks fast tier; was REJECTED by the catalog check ("unknown OpenRouter model ...:nitro") -> fixed: `:nitro`/`:floor` resolve to the base model's catalog entry (OpenRouterProvider::model_info, routing_base). reasoning_exclude: no measurable effect, stays off. Effort: low -24% turn p50 and -40% eval cost with all passing, high slower; suites are saturated, so no default change; proposal: a harder discriminating eval task before lowering the coder's effort. No default changed (defaults_changelog fingerprint unchanged).
Live spend TASK-161.7: $3.62.

Integration (2026-10-23, @claude-bench), final results and ranking (AC#2/#3):

Workspace gates after integration: cargo fmt --all --check clean; cargo clippy --workspace --all-targets --locked -D warnings clean; cargo nextest run --workspace --locked --profile ci 3x: 2043 passed, 10 skipped each; cargo test --workspace --doc ok; cargo deny check ok; scripts/hermetic-guard.sh nextest run -p harness -p harness-eval: 248 passed, nothing written to HOME/XDG; no .snap.new; defaults_changelog ok (fingerprint 46b4cecb7b8e, unchanged); scripts/bench-guard.sh ok (3.19 ms/call, session start 10.66 ms, dev profile).
Flake fixed: tui::tests::jobs_panel_lists_opens_and_stops_background_jobs failed 1 in 5 under 16-core CPU load (nextest --stress-count) at the 'serving' output assertion. Cause: the pid file proves the shell printed 'serving', but the job's pump task appends it to the log asynchronously, so log_tail could read an empty log. The test now refreshes the output panel until the line appears (bounded at 100 x 50 ms, no fixed-sleep assertion). After the fix: 100/100 under the same load. Test-only change in crates/harness/src/tui/tests.rs.

Before/after, fake bench (release, --runs 10, best of 3 passes): overhead per gap p50/p90/max 0.33/0.59/0.87 -> 0.26/0.49/0.54 ms; turn start 0.42 -> 0.30 ms; session start 3.5 -> 3.2 ms; run overhead 4.0 -> 3.3 ms; model_call_started record 43 -> 27 us; TUI frame 0.32 -> 0.29 ms; prefix breaks 0/90. Combined live session start (release, cached model list, OpenRouter Opus 5.5, 1 call, $0.0199): 6.2 ms vs 292-819 ms baseline. New provider first call on the shared pool 12.6-15.7 ms vs 28-34 ms cold. Table in docs/performance.md 'Final results (TASK-161)'.

Ideas ranked by measured gain / risk / cost:
1. Fast tier ([provider.openrouter] service_tier = fast, or :nitro): turn p50 15.3 -> 7.0 s, TTFT 1.77 -> 0.76 s; risk none to quality (evals equal); cost +73% (+97% nitro). Opt-in (cost trade, TASK-161.7).
2. Cached model catalog at session start: -0.3 to -0.8 s per session; no risk (tool-less models still rejected); free. Default (TASK-161.1).
3. Effort low: turn p50 -24%, eval cost -40%, all evals pass; risk: quality on harder work unmeasured (suites saturated). Per-agent opt-in; proposal: a discriminating eval first.
4. Background compaction: the 2.5 s summary wait leaves the compacting turn (22.15 -> 19.65 s); risk: summary covers a shorter history; free. Opt-in ([context] background_compaction, TASK-161.5).
5. Shared HTTP client per base URL + keepalive: ~15 ms per new provider, up to ~0.5 s on a slow first TLS handshake; no risk (per-request credentials); free. Default (TASK-161.1).
6. Prompt caching on OpenRouter Anthropic models (TASK-151, earlier) and prefix stability: 92-95% cache reads, 0 prefix breaks in 38 live calls. Kept; golden prefix test guards it.
7. Staggered subagent fan-out / cache pre-warm / Anthropic cache_ttl 1h or mixed: cost model -64% cache writes on a 3-way fan-out, time gain only on large prefixes; mock-server tested; Anthropic direct not measured live (no key); OpenRouter ignores max_tokens 0. Opt-in (TASK-161.4).
8. Early tool start: paced multi-call turn 275 -> 229 ms; live window 5.7-110 ms; risk handled (cancel on retry/error, identical event order). Opt-in ([experimental] early_tool_start, TASK-161.2).
9. OpenAI prompt_cache_key / 24h retention: on by default (key only, derived from prefix); live hit rate pending an OpenAI key (TASK-128) (TASK-161.3).
10. Thinking display omitted / reasoning_exclude: no measurable TTFT change through OpenRouter (1.39 vs 1.60 s p50, within spread). Opt-in, off.
11. session_id: no measured change (OpenRouter already sticky); free insurance after pauses. Default on.
12. Connection pre-warm at startup: 32-39 ms, overlaps the first request, no TTFB change. Opt-in, off.
13. Harness overhead (event recording, hooks, request build, TUI): 0.26 ms/call release = ~0.02% of a turn; no further work worth its risk. CI guard (TASK-161.6) protects it.
14. Reasoned out, not implemented: gzip for OpenRouter request bodies (9-26 KB per request, ~2 ms upload at 100 Mbit/s, all but the tail cached upstream; gain below noise); compacting tool schemas (8 tools 4.4 KB ~1.1k tokens, cache-read after call 1); speculative read-only prefetch and faster local tools (tools are 0.3-1.5% of turn wall, read/grep/glob 0-5 ms on the bench repo); provider.order pinning (disables sticky routing, so it loses the cache; :nitro/service_tier cover the speed case); lowering max_tokens (no latency gain, truncation risk); OpenAI WebSocket mode (experimental, needs an OpenAI key to measure).
Remaining for AC#4 / DoD#1: the CI guard has not run on hosted Ubuntu/macOS runners yet (no push in this batch; TASK-161.6 stays In Progress until a green CI run).
Live spend this integration step: $0.0199.

Parent DoD: #2 every new key (session_id, prewarm_connection, early_tool_start, cache_prewarm, stagger_subagents, prompt_cache_retention, thinking_display, cache_ttl(_auto_1h), reasoning_exclude, background_compaction(_at), service_tier) is in doc-2 harness.ini Reference; docs/performance.md, context.md, providers-*.md, ci.md updated. #3 loop guard respected (flake fixed on the first attempt). #4 UI: the only visible addition is the cache-prewarm map node from TASK-161.4, covered by its own DoD#4; no new interactive element. AC#4 and DoD#1 stay open until CI runs the bench guard and the new tests on hosted Ubuntu/macOS runners (needs a push by the orchestrator); then check them with TASK-161.6 AC#1/DoD#1 and move both to Done.
<!-- SECTION:NOTES:END -->
