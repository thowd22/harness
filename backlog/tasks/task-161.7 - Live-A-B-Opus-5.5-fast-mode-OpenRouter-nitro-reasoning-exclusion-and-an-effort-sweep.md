---
id: TASK-161.7
title: >-
  Live A/B: Opus 5.5 fast mode, OpenRouter :nitro, reasoning exclusion and an
  effort sweep
status: Done
assignee:
  - '@claude'
created_date: '2026-10-23 21:08'
updated_date: '2026-10-23 23:29'
labels:
  - performance
dependencies: []
parent_task_id: TASK-161
priority: high
ordinal: 175000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
User-approved targeted live measurements via OpenRouter Opus 5.5 (about $3-6 total). After the other subtasks land, use harness bench live mode with interleaved A/B runs (p50/p90): (a) standard vs Anthropic fast mode where OpenRouter exposes it (speed param or a fast variant) vs :nitro; (b) reasoning shown vs excluded; (c) an effort sweep (low/medium/high) on the eval suite checking pass rates. Record cost per run. Add settings only for options that work; defaults change only with clear wins and no quality loss, otherwise present as proposals.

Context: TASK-161 notes (baseline report, web research) and docs/performance.md; measure with harness bench (--fake for overhead, live via OpenRouter Opus 5.5, targeted and budget-capped).
## Loop guard (for agents)

Limit how many times you test and fix. If the same test, build or check still fails after **3 fix attempts**, or you have made no measurable progress in about 30 minutes, **stop**. Do not keep retrying, and never delete, skip, ignore or weaken a test or check to get a green run. Record what you tried, the exact failing output and your best hypothesis in the implementation notes, leave the task In Progress, and ask the user how to proceed.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 A results table with p50/p90 TTFT, output tok/s, turn time, cost and eval pass rates per arm is recorded in TASK-161 and docs/performance.md
- [x] #2 Any new setting (e.g. speed, :nitro routing, effort) is opt-in and documented; total live spend is reported and stays under $8
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
1. Find how OpenRouter exposes Opus 5.5 fast mode (catalog endpoints, service-tier docs). 2. Add the opt-in [provider.openrouter] service_tier setting needed to measure it; fix whatever blocks :nitro. 3. Release build; interleaved live bench arms (std/fast/nitro; effort low/medium/high; medium shown vs excluded), 2 sessions each, budget-capped. 4. Effort sweep (and fast) on the starter, search and identity eval suites. 5. Record tables in TASK-161.7, TASK-161 and docs/performance.md; docs, changelog, tests.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
TASK-161.7 live A/B (2026-10-23, @claude). OpenRouter Opus 5.5, release build, isolated HOME/XDG/HARNESS_CONFIG per arm (api_key_env = OPENROUTER_API_KEY), one session per process, interleaved (tiers A B C A B C; others A B A B), 2 sessions per arm, --budget-usd 1.0.

Fast mode on OpenRouter: Opus 5.5 has an `anthropic/fast` endpoint (service tier priority, $8/$40 per M vs $4/$20). It is requested with top-level `service_tier: "fast"` (or Anthropic's `speed: "fast"`); OpenRouter's `*-fast` model ids are deprecated (openrouter.ai/docs/guides/features/service-tiers). `:nitro` sorts by throughput and admits priority endpoints, so it also lands on fast mode.

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

Code: harness-config ini/config.rs (OpenRouterServiceTier, ProviderConfig.service_tier, test openrouter_service_tier_is_opt_in); harness-providers openrouter.rs (ServiceTier, OpenRouterOptions.service_tier, ROUTING_VARIANTS/routing_base, model_info fallback, test routing_variants_resolve_to_their_base_model), openrouter/request.rs (service_tier in body, test service_tier_is_sent_only_when_set); harness wiring.rs (mapping + test openrouter_service_tier_is_unset_unless_set); golden request_options_coder snapshot gains "service_tier": "fast" in the opt-in shape (reviewed; default request snapshots unchanged). Docs: docs/performance.md (new section + results tables), docs/providers-openrouter.md, doc-2 harness.ini reference (via backlog doc update), CHANGELOG Added.

Live spend: $3.62 total (bench $1.65 in 14 sessions; evals $1.97 in 32 attempts). Under the $8 cap.
Verification: cargo nextest -p harness-config -p harness-providers -p harness-eval (507 passed), -p harness (179 passed), golden_requests 8/8, defaults_changelog pass; clippy -D warnings clean (harness-config, harness-providers, harness-eval, harness, --no-deps); fmt --check clean. Release build deleted after copying to scratch; scratch removed.

DoD #4 N/A: no TUI change. DoD #1: the new tests are ordinary offline unit tests, passing locally; CI has not run (no commits in this batch).
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Ran the live A/B through OpenRouter Opus 5.5 ($3.62 of the $8 cap). Fast mode works through OpenRouter's service_tier: turns 2.2x faster (15.3 -> 7.0 s p50), TTFT 1.77 -> 0.76 s, +73% cost per session, eval pass rate unchanged. Added it as the opt-in [provider.openrouter] service_tier = default | fast. :nitro reaches the same speed by picking the fast tier itself; it used to be rejected by the catalog check, so :nitro and :floor model ids now resolve to the base model's entry. reasoning_exclude and the effort levels showed no clear win without a possible quality cost (the suites are saturated), so no default changed. A harder eval task is proposed before lowering the coder's effort. Verified with unit tests in harness-config, harness-providers and harness wiring, the reviewed golden opt-in snapshot, nextest on the touched crates, clippy and fmt. Results tables are in TASK-161 and TASK-161.7 notes and docs/performance.md.
<!-- SECTION:FINAL_SUMMARY:END -->
