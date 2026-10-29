# Performance

How fast a turn is, where the time goes, and how to measure it. The numbers
here are the TASK-161 baseline (2026-10-23) and the measurements of each
change since; [Final results](#final-results-task-161) compares the two.
Rerun `harness bench` to get your own.

## Running the benchmark

`harness bench` runs a bundled coding task in a fresh temporary git
repository and data directory: a small Python library (`inventory/`) whose
`total_value_cents` ignores quantities, with a unit-test suite. Turn 1 asks
the agent to find and fix the bug and run the tests; turn 2 asks for a new
method with a test. The task lives in `crates/harness-eval/bench/`.

```sh
# Fake provider: offline, deterministic, harness overhead only (5 runs).
harness bench --fake
harness bench --fake --runs 10 --json fake.json

# A real model (costs money; each run is one ~10-call session).
harness bench --model opus --runs 1 --budget-usd 1.0 --json live.json --net
```

Options: `--runs N`, `--label TEXT` (stored in the report, for A/B runs),
`--json FILE` (`-` prints only the JSON), `--budget-usd`, `--max-turns`,
`--mode`, `--checkpoints` (snapshot before each turn, as the TUI does),
`--turns N`, `--no-render`, `--net` (cold/warm connection probe, no tokens),
`--keep` (keep the temporary repos), `--compaction` (with `--fake`: a
variant that compacts once, see [Background
compaction](#background-compaction)). `harness bench --help` lists them.

Use a release build (`cargo build --release -p harness`) for numbers you
compare; a debug build is about 10x slower on the harness side. For A/B
comparisons of settings, give each configuration its own `HARNESS_CONFIG`,
run one session per process and interleave the runs (A B A B): model
latency drifts by seconds over minutes.

## What it measures

The binary installs a timing probe (`harness_providers::timing`) before the
session starts. Without `harness bench` the probe does not exist:
`timing::wrap` and `harness_agent::timing::wrap_hook` return their argument
unchanged, so the normal path does no extra work.

Per model call (wrapping the provider the agent loop calls):

| Column | Meaning |
|---|---|
| `ttfb` | `Provider::stream` entered → first stream event (headers + first SSE chunk) |
| `ttft` | → first text, reasoning or tool-use event |
| `call` | → `message_stop` |
| `tok/s` | output tokens / (call end − first token) |
| `in`, `cache_r`, `cache_w` | uncached input, cache-read and cache-write tokens |
| `body B` | request body bytes; the JSON also splits system / tools / messages |
| `prefix%` | share of the previous request found byte-identical at the start of this one (tools, system, messages; `cache_control` markers ignored). 100 = the cached prefix is intact |
| `early` | first complete tool-use block → message end: how much sooner a read-only tool could start with streaming tool execution |

Per gap (previous call's stream end, or the turn start, → next request):
`tools` (first `tool_call` → last `tool_result`), `toolsum` (sum of tool
durations; larger than `tools` when calls overlapped), `verify`, `hooks`
(request hooks: context/compaction, budget, todo nudge, job notices; a
model call that starts inside the context hook is reported as `compaction`,
and the gap's `compaction_ms` is the part of it the hook waited for) and
`overhead` = gap − tools − verify − hooks: response finishing, event
recording, redaction, history updates and request build. `overhead` is split
into `finish` (stream end → `model_call_finished` recorded), `build` (last
hook or tool result → `model_call_started`) and `send` (→ provider call).

Per turn: wall = model + tools + verify + hooks + overhead. Per run: session
start (config, model catalog, tools, prompt), and with `prewarm_connection`
the pre-warm time. Across runs: p50/p90/min/max of each metric, first-call vs
later-call TTFB, cache-hit share and prefix breaks.

Also reported: tool-definition sizes, event-recording cost (the run's events
replayed into a fresh run log, by event type), TUI render cost (the events
replayed into the app on a 120x40 test backend, one draw per dirty frame) and
with `--net` the time to response headers on a new client (cold: DNS, TCP,
TLS) and on a reused one (warm).

Caveats: event times are wall clock and probe times monotonic, aligned at
probe start (sub-millisecond error over a session). Tool durations come
from `tool_result.duration_ms` (whole milliseconds). Through OpenRouter's
chat API the first stream event is the first content chunk, so `ttfb` and
`ttft` coincide; a reply that starts with a tool call whose arguments the
upstream buffers shows a late `ttft` and a very high `tok/s`.

## Baseline (2026-10-23)

Machine: Linux x86_64, 16 CPUs, harness 0.3.0, release build. Network: home
broadband to `openrouter.ai` over HTTP/2.

### Fake provider (10 runs, release)

| Metric | p50 | p90 | max |
|---|---|---|---|
| Harness overhead per gap | 0.33 ms | 0.59 ms | 0.87 ms |
| Turn start (prompt → first request) | 0.42 ms | 0.52 ms | 0.57 ms |
| Request hooks per gap (all four) | <0.01 ms | 0.02 ms | 0.03 ms |
| Session start (fake, no catalog) | 3.5 ms | 3.9 ms | 4.5 ms |
| Run overhead (2 turns, 10 calls) | 4.0 ms | 5.2 ms | 7.3 ms |
| TUI frame draw (120x40) | 0.32 ms | 0.39 ms | 0.52 ms |

Event recording, per event (mean): `model_call_started` 43 µs (max 226 µs,
grows with the history it logs), `model_call_finished` 14 µs, `delta`
1.3 µs, `tool_result` 4 µs. The previous request was an exact prefix of the
next one in 90/90 requests. A debug build measures about 3.2 ms overhead
per gap (the CI guard should compare like with like).

### Live: OpenRouter Opus 5.5 (4 sessions, 38 calls)

Configurations, interleaved A B A B, one session per process:
A = defaults, B = `[experimental] prewarm_connection` + `openrouter_session_id`
(the latter is now `[provider.openrouter] session_id`, on by default).

| Metric | A (2 runs) | B (2 runs) |
|---|---|---|
| TTFB p50 per run | 2961 / 1417 ms | 1566 / 1649 ms |
| TTFB p90 per run | 3756 / 3185 ms | 3225 / 3609 ms |
| TTFB first call | 2961 / 1320 ms | 1566 / 2105 ms |
| Call time p50 | 4101 / 3240 ms | 2904 / 3302 ms |
| Output tok/s p50 | 173 / 167 | 169 / 167 |
| Harness overhead per gap p50 | 0.72 / 0.79 ms | 0.62 / 0.63 ms |
| Cache-read share, calls 2+ (p50) | 94.7 / 91.9 % | 93.1 / 91.9 % |
| Session start | 819 / 330 ms | 292 / 442 ms |
| Run wall | 37.7 / 30.8 s | 27.9 / 33.6 s |
| Cost | $0.094 / $0.099 | $0.083 / $0.093 |

Streamed calls cluster at 1.3-1.7 s TTFB; the 3-4 s outliers are replies
that open with a large tool call (edit) whose arguments arrive in one burst.
Model calls are 98.5-99.6% of a turn's wall time; tools 0.3-1.5% (a
`python3 -m unittest` run is ~50-65 ms); harness overhead ~0.02%.

Connection probe (`GET /api/v1/key`, HTTP/2): cold 25-49 ms, warm 12-21 ms.
The first `curl` of the day to `openrouter.ai` took 485 ms for TLS alone.

Request sizes: system 4.7 KB (~1.2k tokens), 8 tool definitions 4.4-4.6 KB
(~1.1k tokens; `grep` alone 1 KB), messages growing 1.5-3 KB per call; the
body reaches 26 KB by call 9. Cache writes are a few hundred tokens per call,
uncached input 2-4 tokens: the prefix is stable (0 breaks in 38 calls).

Experiments, measured: neither setting moved TTFB, cache share or wall time
beyond the run-to-run noise (TTFB p50 spread 1.4-3.0 s within one
configuration). The pre-warm takes 32-39 ms and still overlaps the first
request; OpenRouter already routes a conversation stickily by hashing its
first system and user messages, so `session_id` adds stickiness only from
the first request on. The pre-warm stays off by default; `session_id`
(no cost, insurance after long pauses) is on by default since TASK-161.1.

Live spend for the baseline: $0.368 (4 sessions).

## CI overhead guard

The `test` job in CI (Ubuntu and macOS) runs `scripts/bench-guard.sh`
after the test suite. It runs `harness bench --fake` on the dev-profile
binary the tests already built (`target/debug/harness`, so the guard adds
no compile time and always compares the same profile) in a fresh temporary
HOME, XDG and TMPDIR, and fails when either metric is over its limit:

| Metric | Limit | Report field | Dev-profile baseline |
|---|---|---|---|
| Overhead per call | 15 ms | `summary.overhead_per_call_ms.min` | 3.2-3.3 ms |
| Session start | 50 ms | `summary.session_start_ms.min` | 10.8-11.4 ms |

Noise handling: one attempt runs the benchmark best of 7 runs in one process
and takes the minimum of each metric (the per-run p50 for overhead), so a
run slowed by a busy runner does not count; an attempt over a limit is
retried, and the guard fails only when all 3 attempts are over. The limits
leave about 4x headroom over the baseline (Linux x86_64, 16 CPUs) for slower
shared runners; a debug build is about 10x slower than release on the
harness side, so a limit hit here is a real regression of several
milliseconds per call in release terms. One attempt takes about 1.5 s.

Run it locally the same way:

```sh
cargo build -p harness
scripts/bench-guard.sh                       # target/debug/harness
BENCH_GUARD_MAX_OVERHEAD_MS=5 scripts/bench-guard.sh path/to/harness
```

`BENCH_GUARD_MAX_OVERHEAD_MS`, `BENCH_GUARD_MAX_SESSION_START_MS`,
`BENCH_GUARD_RUNS` and `BENCH_GUARD_ATTEMPTS` override the defaults. When
the guard fails, profile with `harness bench --fake --runs 10` (the table
splits overhead into `finish`, `build` and `send`). To change a limit, for
an intended cost or after a measured speed-up, edit the default in
`scripts/bench-guard.sh` and the table above in the same change, with the
before/after numbers in the change description;
`crates/harness/tests/bench_guard.rs` fails if the two disagree.

## Where the time goes (ranked)

1. **Model latency: TTFB 1.3-1.7 s typical, up to 4 s; ~170 output tok/s.**
   Over 99% of a turn. Levers are upstream: routing, prompt size,
   thinking/effort, fast tiers (cost trades, opt-in only).
2. **Session start: 0.3-0.8 s** (baseline), almost all of it the
   OpenRouter model catalog (`GET /models`, 792 KB) fetched on every
   session start before the first request, on its own reqwest client.
   *Fixed in TASK-161.1:* the check reads the cached list in
   `<data_dir>/model-lists/` (see "Session start and connections" below);
   the download happens only without a usable cached list, on the shared
   chat connection pool.
3. **Buffered tool arguments: +1.5-2.5 s TTFT** on replies that start with a
   large tool call. At the baseline the OpenRouter stream mapper emitted
   every `block_stop` only at the end of the message, so nothing could start
   early (`early` = 0 in every live call). Since TASK-161.2 it closes a tool
   call when the next one or text starts and at `finish_reason`, before the
   usage chunk and `[DONE]` (live: `early` 5.7 ms for two reads, 110 ms for
   a trailing call); see [Early tool start](#early-tool-start).
   Fine-grained tool streaming (`eager_input_streaming`, Anthropic direct)
   would shorten the burst itself; see the evaluation there.
4. **New HTTP client per provider instance** (baseline): subagents
   (`crates/harness/src/core.rs:244`), session summaries
   (`crates/harness/src/sessions.rs:219`) and dreaming
   (`crates/harness/src/dream.rs:68`) build fresh providers, each paying a
   cold connection (25-49 ms, up to ~0.5 s for a first TLS handshake) on its
   first call. *Fixed in TASK-161.1:* one shared connection pool per base
   URL (see below).
5. **Tools**: `bash` ~50-65 ms per test run (process start + Python);
   `read`/`edit`/`grep`/`glob` under 1-5 ms on the bench repository (tens of
   ms on a large one). Consecutive read-only calls run in parallel after
   the message ends; with `[experimental] early_tool_start` the leading
   read-only calls start while the reply still streams.
6. **Harness overhead: 0.3-0.8 ms per call (release).** Largest parts:
   `model_call_started` recording, which serializes the new messages twice
   (blob threshold check `crates/harness-events/src/blobs.rs:83`, then the log
   line `crates/harness-events/src/store.rs:390`); the scheduler's relay task
   per call (`crates/harness-agent/src/scheduler.rs:415`, part of `send`
   ~0.25 ms); tool specs rebuilt three times and the tools JSON hashed for
   every request (`crates/harness-agent/src/agent_loop.rs:805,901,912`) and the
   full history cloned into each request (`agent_loop.rs:900`). None of this
   is visible next to model latency.
7. **TUI**: 0.3 ms per frame at 120x40, drawn only when dirty, off the
   request path.

## Session start and connections (TASK-161.1)

- **Cached model catalog.** The OpenRouter tool-support check at session
  start (`check_agent_model`) reads `<data_dir>/model-lists/openrouter.json`,
  the list `/model` and `harness models` already cache, while it is younger
  than `[general] model_list_ttl` (default 24h) and came from the same
  `base_url`. A stale list is used and refreshed in the background; a model
  the list lacks or marks tool-less is checked against the live catalog
  (offline, the cached answer stands), so tool-less models are still
  rejected. Every download writes the cache.
- **One connection pool per base URL.** All `HttpClient`s for the same base
  URL and connect timeout share one process-wide reqwest client (per tokio
  runtime): the main agent, subagents, session summaries, dreaming and the
  catalog download reuse one warm HTTP/2 connection. The shared client
  carries no credentials; each provider sends its own headers with each
  request. Pool settings: idle connections kept 300 s, TCP keepalive 30 s,
  HTTP/2 pings every 30 s while idle (10 s timeout).
- **`session_id` on by default** for OpenRouter (`[provider.openrouter]
  session_id = false` turns it off): measured at no cost in the baseline,
  it keeps sticky routing after pauses longer than OpenRouter's own
  stickiness window.

Before/after, live OpenRouter Opus 5.5, debug build, one session per
process, `--turns 1 --max-turns 2 --net`, sessions sharing one data
directory (2026-10-23):

| Metric | Before (2 runs) | After, first run (no cache yet) | After, cached (2 runs) |
|---|---|---|---|
| Session start | 529 / 153 ms | 654 ms | 30.3 / 29.7 ms |
| First call of a newly built provider (`net.new_client_ms`; before = `cold_ms`, a new client) | 28-34 ms | 13.5-13.6 ms | 12-21 ms (one 75 ms outlier) |
| Cold connection on an isolated client (`cold_ms`) | 28-34 ms | 31-33 ms | 29-45 ms |
| Warm, same client (`warm_ms`) | 11-17 ms | 16-21 ms | 12-18 ms |

A session start with a fresh cached list saves the whole catalog download
(0.12-0.6 s here, up to 0.8 s in the baseline); the fake-provider session
start in a debug build is ~11 ms, so the remaining ~19 ms is reading the
156 KB cached list and building the session. Each subagent, summary or
dreaming provider now starts on a warm connection: ~15 ms saved per first
call here, up to ~0.5 s when the first TLS handshake of the day is slow.
Live spend for these runs: $0.105 (5 sessions).

## Experimental settings

`[experimental]` in harness.ini (all `false` by default):

| Key | Effect | Measured |
|---|---|---|
| `prewarm_connection` | opens the provider's connection (`GET /key` on OpenRouter, `/v1/models` on Anthropic, `/models` on OpenAI) in the background while the session starts | 32-39 ms, overlaps the first request; no measurable TTFB change |
| `early_tool_start` | starts a reply's leading read-only tool calls that policy allows without asking as soon as each call is complete in the stream ([Early tool start](#early-tool-start)) | paced multi-call turn: 275 → 229 ms (-46 ms, the tool time hidden behind streaming); bundled fake bench and harness overhead unchanged |
| `cache_prewarm` | writes the prompt cache for the next request with a non-generating call ([Prompt-cache options](#prompt-cache-options)) | Anthropic only; mock-server tested, no Anthropic-direct measurement yet |
| `stagger_subagents` | sibling subagent requests with the same cached prefix wait for the first response to begin ([Prompt-cache options](#prompt-cache-options)) | order and answers tested; fake bench unchanged |

### Early tool start

Every provider decoder emits a tool call's `block_stop` as soon as its
arguments are complete: Anthropic at `content_block_stop`, OpenAI
(Responses API) at `response.output_item.done`, OpenRouter when
the next tool call or text starts, or at `finish_reason` (before the usage
chunk and `[DONE]`). With `early_tool_start = true` the agent loop then
starts the call right away when all of these hold:

- the tool is read-only and built in (MCP tools wait), and its arguments
  parse and validate;
- the gate would allow it without asking (no approval prompt, no protected
  path; the check records nothing);
- every tool call before it in the reply also started early (nothing runs
  ahead of an earlier mutating or asked-about call).

The call runs on its own task; its tool events are held back. After the
message ends the calls are prepared, reviewed and recorded exactly as
without the setting (same `tool_call` and `policy_decision` events, same
single approval prompt), and the dispatcher awaits the early task in place
of running the call, then sends its held-back events. An early `read` also
holds back its read-tracker record (what `write`/`edit` treat as seen) until
its result is used, so a discarded early read never satisfies the
read-before-write or stale-read checks. An early result is
used only when the final call has the same id, tool and arguments, is still
read-only and allowed, and sits in the first batch; otherwise it is
discarded and the call runs as usual. A stream error, a retry that discards
partial output, or cancellation cancels every early call. Subagents do not
use it yet. A read result reflects the file when its call finished
streaming, not after a later approval prompt.

Measured (dev profile, Linux x86_64, best of 7, interleaved): a scripted
turn streaming text and four read-only calls (two `grep`s, `glob`, `read`
over an 8.5 MB, 451-file Rust tree, 2 ms between 8-character deltas) took
275 ms off and 229 ms on: the 46 ms is the tools' run time, now overlapped
with the rest of the stream. The bundled `harness bench --fake` (no stream
delays) is unchanged: overhead per call 3.20-3.42 ms off and on, run wall
157 ms. Live through OpenRouter Opus 5.5 (one 2-call session, $0.027),
`early` was 5.7 ms for a reply with two reads (arguments still arrive in
one burst) and 110 ms for a reply ending with a `bash` call: the
`finish_reason` chunk precedes the usage chunk by about 0.1 s. The gain per
turn is therefore bounded by the read-only tools' own time; it matters
most for slow reads (large greps) and for multi-call replies that keep
streaming after the first call.

`eager_input_streaming` (Anthropic fine-grained tool streaming, evaluated,
not enabled): it streams tool arguments without server-side buffering or
JSON validation, which shortens the 1.5-2.5 s argument bursts seen on large
`write`/`edit`/`bash` calls. Those are mutating and never start early, so
the gain is only visible streaming (the delta channel), and a reply cut at
`max_tokens` can end with invalid JSON (the harness's argument repair then
reports an actionable error). It needs a direct Anthropic key to measure;
it belongs with the Anthropic request options (TASK-161.4) as an opt-in.

### Background compaction

`[context] background_compaction = true` (off by default; with
`background_compact_at`, default 0.7) starts the handoff summary in the
background when the next request reaches 0.7 of the window and swaps it in at
`compact_at` (0.8), so the turn that crosses 0.8 no longer waits for the
summary call. The behaviour, the history it keeps and the fallbacks are in
[context.md](context.md#background-compaction-experimental-off-by-default).
The summary request goes out inside the context hook, before the agent's
next request, and the rest of the call runs on its own task.

`harness bench --fake --compaction` runs the bundled task in a 20k-token
window that compacts once (default `keep_recent_turns = 6`), with 1.5 s
agent calls and a 2.5 s summary reply. The report adds `compaction call`
(whole summary call) and `compaction wait per run` (time the context hook
waited for it).

Measured (dev profile, Linux x86_64, 1 run each, interleaved A B A B;
A = defaults, B = `background_compaction = true`):

| Metric | A (in-line) | B (background) |
|---|---|---|
| Compaction call | 2501.6 / 2501.9 ms | 2502.3 / 2501.8 ms |
| Compaction wait per run | 2501.6 / 2501.9 ms | 0.06 / 0.06 ms |
| Turn 1 wall (14 calls) | 22.15 / 22.15 s | 19.65 / 19.68 s |
| Run wall (2 turns, 20 calls) | 31.25 / 31.26 s | 28.75 / 28.79 s |
| Summary request body | 5037 B | 3769 B |

The whole summary time comes off the turn when two agent calls (3 s) fit
between 0.7 and 0.8; a summary slower than that blocks only for its rest.
The summary covers a shorter history (it starts earlier), and the turns
added meanwhile stay verbatim. The bundled `harness bench --fake` (no
compaction) is unchanged with the setting on: overhead per call 3.3-3.9 ms
(dev, machine shared with other builds), 0 prefix breaks.

Existing settings that affect speed: `[provider.openrouter] cache_ttl = 1h`
(longer cache life for long or idle sessions, at a higher write price),
`[provider.anthropic] gzip_requests`, and the cost trades `[provider.openrouter]
service_tier = fast` and `:nitro` model ids ([Fast mode, :nitro, reasoning and
effort](#fast-mode-nitro-reasoning-and-effort-task-1617)).

## Prompt-cache options

TASK-161.4. All off by default; request bodies with the defaults are byte-identical
(golden snapshots `request_*` unchanged, the opt-in shapes are in
`request_options_coder`).

| Setting | What changes | When it helps |
|---|---|---|
| `[provider.anthropic] thinking_display = omitted` | thinking blocks come back empty (signature only); same thinking and cost | the first visible token may come earlier; nothing streams into the thinking view |
| `[provider.anthropic] thinking_display = updates` | only between-tool progress notes come back (beta header added) | long tool-calling turns that should not look silent |
| `[provider.openrouter] reasoning_exclude = true` | `reasoning.exclude: true`; same effort and cost | the OpenRouter counterpart of `omitted`; measured by the TASK-161.7 A/B |
| `[provider.anthropic] cache_ttl = 1h` / `mixed` | one-hour breakpoints (all, or tools and system only) | pauses of 5 to 60 minutes between requests that share the prefix |
| `[provider.anthropic] cache_ttl_auto_1h = true` | one-hour breakpoints while a background job runs, or for 30 min after a 3 min+ start-to-start gap inside a run | long test runs, builds, waits for jobs |
| `[experimental] cache_prewarm = true` | a `max_tokens: 0` call writes the cache: at interactive session start and resume (while the user types; the first turn waits up to 10 s for it), after `/compact`, and with `stagger_subagents` before a fan-out; recorded as a `cache-prewarm` node under the turn it warms (shown in the map, not as a chat turn) and charged to the `[budget]` limits | a cold first request on a large prefix; never in headless runs, which send their first request at once |
| `[experimental] stagger_subagents = true` | same-prefix subagent and workflow-agent requests that start together go one by one until the first response begins (at most 30 s), then together | several siblings of one agent type: one cache write instead of one per sibling |
| `[provider.openai] prompt_cache_retention = 24h` | `prompt_cache_retention: "24h"` on models that support extended caching (TASK-161.3; see [providers-openai.md](providers-openai.md)) | pauses longer than the in-memory cache lasts (5-60 min); check your organisation's retention posture first |

Cost model (Opus 5.5, $4/M input, cache writes 1.25x for 5m and 2x for 1h,
reads 0.05x): a subagent prefix of about 3,000 tokens (system prompt plus
tool schemas) fanned out to 3 siblings at once is written 3 times
(3 x 3,000 x $5/M = $0.045); staggered it is written once and read twice
($0.015 + $0.0012 = $0.016, -64%). The followers start after the leader's
first byte (1.3-1.7 s in the live baseline) and then read instead of
write; on prefixes this small the cold write costs little time, so the
win is cost, and time only for large prefixes. With OpenRouter the
followers reach the same upstream as the leader through `session_id`, so
the read hits. `mixed` costs 2x instead of 1.25x on the stable prefix only
(written once per session and then read), and nothing extra on the
growing tail.

OpenRouter has no pre-warm: a live call on 2026-10-23 (Opus 5.5,
1,923-token cached system prompt, `stream: false`, `max_tokens: 0`) was
answered with 22 output tokens ($0.0101: the cache write plus an answer),
so `max_tokens: 0` is ignored there. The follow-up call read all 1,923
tokens from the cache ($0.0005). Spend for this task: $0.0106.

Harness overhead with the defaults (fake bench, dev profile, 10 runs,
interleaved before/after/before/after): overhead per call p50 3.24 / 3.23
/ 3.24 / 3.23 ms, run wall p50 157.7 / 157.2 / 155.4 / 156.1 ms: the new
`long-wait` request hook adds nothing measurable.

Tests: `anthropic::request` unit tests (lifetimes per breakpoint, display
values, pre-warm body), `tests/anthropic_provider.rs` (pre-warm call, then
the real call, over a mock server), `harness-agent` `stagger::tests`
(leader first, failed leader hands over, pre-warm opens the gate, key
covers only the cached prefix), `subagents::tests::staggered_fan_out_keeps_requests_and_answers`,
`tests/long_wait.rs`, and `crates/harness/tests/cache_prewarm.rs` (the
binary against a mock Messages API: the pre-warm goes first and the real
requests and answers are the same as without it).

## Fast mode, :nitro, reasoning and effort (TASK-161.7)

Live A/B through OpenRouter, Claude Opus 5.5, release build, 2026-10-23.
Bench arms ran one session per process, interleaved (A B C A B C for the
tiers, A B A B for the others), 2 sessions each, `--budget-usd 1.0`. TTFT
and call times are over every call of both sessions; "tok/s (stream)" is
output tokens / time from first token to message end, summed over calls
(the per-call p50 is skewed by tool-argument bursts); turn is the bench's
two turns; cost is per session (2 turns, 7-12 calls).

Arms: `std` = defaults (the coder sends no `reasoning` object; Opus 5.5
still reasons adaptively); `fast` = `[provider.openrouter] service_tier =
fast`; `nitro` = model `anthropic/claude-opus-5.5:nitro`; `eff-*` = a
global `agents/coder.yaml` copy with `effort: low | medium | high` (the levels then);
`eff-med-excl` = effort medium + `reasoning_exclude = true`.

| Arm | Calls | TTFT p50 | TTFT p90 | Call p50 | Call p90 | tok/s p50 per call | tok/s (stream) | Turn p50 | Turn p90 | Session wall | Cost per session |
|---|---|---|---|---|---|---|---|---|---|---|---|
| std | 18 | 1766 ms | 3988 ms | 3333 ms | 4801 ms | 171 | 217 | 15.3 s | 17.9 s | 32.4 / 30.6 s | $0.108 / $0.089 |
| fast | 19 | 760 ms | 1869 ms | 1302 ms | 2042 ms | 441 | 560 | 7.0 s | 7.8 s | 13.5 / 14.1 s | $0.169 / $0.172 |
| nitro | 17 | 1041 ms | 1576 ms | 1513 ms | 2299 ms | 483 | 643 | 6.9 s | 7.1 s | 12.8 / 13.9 s | $0.195 / $0.194 |
| eff-low | 18 | 1414 ms | 3170 ms | 2743 ms | 4148 ms | 168 | 186 | 11.7 s | 17.1 s | 21.0 / 31.5 s | $0.057 / $0.085 |
| eff-med | 18 | 1604 ms | 3830 ms | 3504 ms | 4813 ms | 165 | 182 | 14.9 s | 19.5 s | 29.8 / 32.4 s | $0.080 / $0.092 |
| eff-med-excl | 18 | 1392 ms | 3656 ms | 2923 ms | 4266 ms | 170 | 191 | 13.9 s | 18.6 s | 27.9 / 30.8 s | $0.090 / $0.091 |
| eff-high | 24 | 1596 ms | 3514 ms | 3116 ms | 4300 ms | 173 | 238 | 19.1 s | 21.0 s | 35.9 / 40.5 s | $0.112 / $0.120 |

Eval pass rates (`harness eval run --model opus`, same isolated configs;
starter = 6 tasks x 1, search + identity = 2 tasks x 2; cost is the
suite total, wall the mean per attempt):

| Arm | starter | search + identity | Starter cost | Starter wall / attempt | Output tokens (starter) |
|---|---|---|---|---|---|
| std | 6/6 | 4/4 | $0.265 | 16.3 s | 6,386 |
| fast | 6/6 | - | $0.575 | 7.7 s | 5,861 |
| eff-low | 6/6 | 4/4 | $0.158 | 11.6 s | 3,225 |
| eff-med | 6/6 | - | $0.252 | 18.4 s | 5,627 |
| eff-high | 6/6 | 3/4 | $0.279 | 19.9 s | 6,533 |

The eff-high miss is one `find-session-spawns` attempt that listed the
vendored copy: one failure in two attempts, not evidence against high
effort.

Findings:

- **Fast mode works through OpenRouter** (`service_tier: "fast"`, or
  Anthropic's `speed: "fast"`; OpenRouter's `*-fast` model ids are
  deprecated). It routes to the `anthropic/fast` endpoint (service tier
  `priority`, $8/$40 per million tokens, 2x). Turns are 2.2x faster
  (15.3 → 7.0 s p50), TTFT 1.77 → 0.76 s p50 and streaming output 2.6x
  (217 → 560 tok/s); cost per session +73% (prompt cache reads keep it
  under 2x), and the starter eval cost 2.2x. Pass rate unchanged. It stays
  **opt-in** (`[provider.openrouter] service_tier = fast`): it doubles the
  token price.
- **`:nitro` picks fast mode by itself** for Opus 5.5 (its cost per call is
  the fast price), with the same speed as `service_tier = fast` (turn
  6.9 s p50) and a little more spend in these runs ($0.19 vs $0.17 per
  session). Use it when you want the fastest endpoint whatever its tier,
  with OpenRouter's fallback to the next fastest; use `service_tier = fast`
  for a fixed tier. The harness now accepts `:nitro` and `:floor` model ids
  (the catalog check uses the base model's entry).
- **Reasoning excluded vs shown**: no measurable difference (TTFT p50 1.39
  vs 1.60 s, turn p50 13.9 vs 14.9 s, same cost) within the run-to-run
  spread; `reasoning_exclude` stays off by default.
- **Effort**: low is faster and cheaper (turn p50 11.7 s vs 15.3 s default,
  starter cost -40%, output tokens -50%) with every eval passing; high is
  slower (turn p50 19.1 s, +33% calls). These suites are saturated (every
  arm passes), so they cannot show a quality loss from low effort on harder
  work: the coder's default stays unchanged (no `effort`), and a lower
  effort is a per-agent choice (`effort:` in the agent YAML; the built-in
  `researcher` already uses `low`) or a per-session one
  ([`/effort low`](commands.md#effort)). Proposal: add a harder, discriminating
  eval task before changing the coder's effort.

No default changed. Live spend for TASK-161.7: $3.62 (bench $1.65 in 14
sessions, evals $1.97).

## Final results (TASK-161)

All TASK-161 changes together, with the defaults, against the baseline
above. Same machine (Linux x86_64, 16 CPUs), release build, 2026-10-23.
Fake: `harness bench --fake --runs 10`, three passes, best pass (the
lowest p50; the three passes were within 0.01 ms of each other).

| Metric | Baseline | Final | Change |
|---|---|---|---|
| Harness overhead per gap, p50 / p90 / max | 0.33 / 0.59 / 0.87 ms | 0.26 / 0.49 / 0.54 ms | -21% p50 |
| Turn start (prompt → first request), p50 | 0.42 ms | 0.30 ms | -0.12 ms |
| Request hooks per gap (five hooks now), p90 | 0.02 ms | 0.02 ms | same |
| Session start, fake, p50 | 3.5 ms | 3.2 ms | -0.3 ms |
| Run overhead (2 turns, 10 calls), p50 | 4.0 ms | 3.3 ms | -0.7 ms |
| `model_call_started` recording, mean | 43 µs | 27 µs | -16 µs |
| TUI frame draw (120x40), p50 | 0.32 ms | 0.29 ms | same within noise |
| Prefix breaks (previous request not a byte-exact prefix) | 0 / 90 | 0 / 90 | same |
| **Session start, live OpenRouter** | **292-819 ms** | **6.2 ms** (cached model list) | **-0.3 to -0.8 s per session** |
| First call of a new subagent, summary or dreaming provider | cold connection, 28-34 ms | warm pool, 12.6-15.7 ms | ~15 ms, up to ~0.5 s on a slow first TLS handshake |
| CI guard, dev profile, overhead per call / session start | 3.2-3.3 / 10.8-11.4 ms | 3.19 / 10.66 ms | within the limits (15 / 50 ms) |

The live session-start number is one release `harness bench --model opus
--turns 1 --max-turns 1 --net` session after `harness models --refresh`
filled the cache in its data directory ($0.0199). The whole of the
difference is the 792 KB catalog download that the baseline fetched on
every start; without a cached list the first start still downloads it
once (654 ms in the TASK-161.1 run). Connection probe in the same run:
cold 28-34 ms, warm 12-15 ms, a new provider on the shared pool 12.6-15.7
ms.

Model time is still over 98% of a turn, so the rest of the gain comes from
opt-in settings that trade cost or need a provider this machine has no key
for:

| Setting (opt-in) | Measured effect | Trade |
|---|---|---|
| `[provider.openrouter] service_tier = fast` | turn p50 15.3 → 7.0 s, TTFT p50 1.77 → 0.76 s | +73% cost per session |
| model `...:nitro` | turn p50 15.3 → 6.9 s, TTFT p50 1.77 → 1.04 s | +97% cost per session |
| `[experimental] early_tool_start` | paced multi-call turn 275 → 229 ms | none measured; tool work starts before the reply ends |
| `[context] background_compaction` | wait for the summary 2.5 s → 0.06 ms, compacting turn 22.15 → 19.65 s ([Background compaction](#background-compaction)) | the summary runs in the background on the same budget |
| `[experimental] cache_prewarm`, `stagger_subagents`, `[provider.anthropic] cache_ttl` | mock-server tested; -64% cache-write cost on a 3-way fan-out (cost model) | Anthropic direct; not yet measured live |
| effort `low` (per agent) | turn p50 -24%, eval cost -40%, all evals pass | quality on harder work not yet measured |

Not measured live yet: OpenAI prompt-cache hints (TASK-161.3; needs an
OpenAI API key, TASK-128) and the Anthropic-direct options (no Anthropic
key on this machine).
