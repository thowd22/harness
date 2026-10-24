---
id: TASK-161.3
title: OpenAI prompt cache hints
status: Done
assignee:
  - '@claude'
created_date: '2026-10-23 21:08'
updated_date: '2026-10-23 21:52'
labels:
  - performance
dependencies: []
parent_task_id: TASK-161
priority: high
ordinal: 171000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Harness sends neither prompt_cache_key nor prompt_cache_retention to OpenAI. Add prompt_cache_key = a stable session-family id (shared by subagents with the same prefix) and prompt_cache_retention = 24h where the model supports it (GPT-5.5 supports only 24h; check model capabilities), omitted for ChatGPT mode if unsupported. Unit tests and golden snapshots only; a live check waits for an OpenAI key (TASK-128).

Context: TASK-161 notes (baseline report, web research) and docs/performance.md; measure with harness bench (--fake for overhead, live via OpenRouter Opus 5.5, targeted and budget-capped).
## Loop guard (for agents)

Limit how many times you test and fix. If the same test, build or check still fails after **3 fix attempts**, or you have made no measurable progress in about 30 minutes, **stop**. Do not keep retrying, and never delete, skip, ignore or weaken a test or check to get a green run. Record what you tried, the exact failing output and your best hypothesis in the implementation notes, leave the task In Progress, and ask the user how to proceed.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 API-key OpenAI requests carry prompt_cache_key and the right retention per model; ChatGPT mode stays valid (tests)
- [x] #2 Golden snapshots updated; docs note the live check is pending an OpenAI key
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
1. request.rs: add prompt_cache_key (harness- + truncated SHA-256 of instructions + tool specs, i.e. the reusable prefix; stable across turns, shared by subagents with the same prefix, needs no wiring) to every body.
2. Add prompt_cache_retention=24h only for models OpenAI lists for extended retention (gpt-5.5/-pro, 5.4, 5.2, 5.1 family, gpt-5, gpt-5-codex, gpt-4.1 + dated snapshots); omit elsewhere (5.6+ uses prompt_cache_options instead).
3. ChatGPT mode: for_chatgpt already strips prompt_cache_retention (UNSUPPORTED_REQUEST_FIELDS); keep the key. Test it.
4. Unit tests, golden snapshot update (review diff), docs/providers-openai.md, overhead measurement.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Implemented in crates/harness-providers/src/openai/request.rs: prompt_cache_key() = 'harness-' + first 12 bytes (24 hex) of SHA-256(instructions, then name/description/schema of each tool, NUL-separated), via aws-lc-rs (already a dep). Content-derived so no wiring/ChatRequest change was needed: every turn of a session, retries and subagents with identical instructions+tools share the key; any prefix change gives a new key; the model and conversation are not part of it. EXTENDED_RETENTION_MODELS + cache_retention_for(): prompt_cache_retention='24h' for gpt-5.5, gpt-5.5-pro, gpt-5.4, gpt-5.2, gpt-5.1-codex-max, gpt-5.1, gpt-5.1-codex, gpt-5.1-codex-mini, gpt-5.1-chat-latest, gpt-5, gpt-5-codex, gpt-4.1 (+ -YYYY-MM-DD snapshots), per OpenAI's prompt caching guide (developers.openai.com/api/docs/guides/prompt-caching, read 2026-10-23: GPT-5.5/5.5 Pro accept only 24h; GPT-5.6+ ignore prompt_cache_retention and use prompt_cache_options.ttl 30m/12h + explicit breakpoints; key guidance ~15 req/min per key). Omitted for every other model (gpt-5-mini, gpt-4o, o-series, 5.3, 5.6+). ChatGPT mode: for_chatgpt already removes prompt_cache_retention (in chatgpt::UNSUPPORTED_REQUEST_FIELDS); the key is kept (the Codex CLI sends it on that route).
Tests (new, request.rs): cache_retention_per_model, every_body_has_a_cache_key_and_retention_only_where_supported, cache_key_is_stable_across_turns_and_changes_with_the_prefix (later turns, subagent same prefix, changed system/tool description/no tools), chatgpt_bodies_keep_the_cache_key_and_drop_retention (also asserts no UNSUPPORTED_REQUEST_FIELDS remain). Golden snapshots (harness-eval golden_requests coder/reviewer/researcher, model gpt-5.1): diff reviewed, only +prompt_cache_key and +prompt_cache_retention:24h in the openai body. Note: the snapshot key is a hash of the instructions+tools, so any future prompt/tool change also changes this line (intended).
Validation: cargo nextest -p harness-providers -p harness-eval: 324 passed, 9 skipped; clippy -p harness-providers --all-targets -D warnings clean; request.rs fmt clean (fmt --check diffs seen in cache_hint.rs/http.rs/openrouter.rs belong to other in-progress subtasks, untouched). defaults_changelog passes; no ini default, prompt or tool description changed, so no Defaults CHANGELOG entry.
Bench: harness bench --fake does not exercise the OpenAI provider (no change expected; 5-run fake bench unchanged in shape). Added overhead measured with a temporary best-of-200 microbench (removed afterwards) on a 12 KB system prompt + 20 tools (~25 KB key material): 28 us per request in release, 233 us in debug; negligible next to 1.3-1.7 s TTFB. The win (cache hit rate and TTFB on cache-warm turns, longer 24h retention across gaps) needs a live OpenAI API key: pending TASK-128, documented in docs/providers-openai.md. Live spend for this task: $0 (no live calls; OpenRouter does not reach OpenAI's Responses API path).
Open risk for the live check: OpenAI does not say whether ZDR organisations accept 24h (they default to in_memory); if they 400, the fix is a per-provider off switch, which needs wiring (outside this subtask's file scope).

Review fix (blocking finding 1, confirmed real): always sending prompt_cache_retention=24h changed default request behaviour with no measured benefit (non-ZDR orgs already default to 24h on these models) and could override a ZDR org's in_memory policy or 400 with no off switch. Fixed: build_body no longer sends it; new opt-in [provider.openai] prompt_cache_retention = default | 24h (config enum OpenAiCacheRetention, default unset = default, skipped in serialized config so the hash is unchanged); OpenAiProvider::with_extended_cache_retention(bool) + request::apply_cache_retention add 24h only for EXTENDED_RETENTION_MODELS on API-key requests (ChatGPT mode never). Wiring: openai_extended_cache_retention(cfg) in provider_sync (minimal additive edit). Golden snapshots: prompt_cache_retention lines removed, they only gain prompt_cache_key vs. before TASK-161.3. Tests: request.rs every_body_has_a_cache_key_and_retention_only_when_opted_in (renamed; now asserts default omission + opt-in), chatgpt test uses apply_cache_retention, openai.rs extended_cache_retention_is_opt_in_for_api_key_requests, config.rs openai_prompt_cache_retention_is_opt_in (parse, invalid value, openrouter rejects key, unset not serialized), wiring openai_extended_cache_retention_is_opt_in (end-to-end config->provider body). Docs: providers-openai.md, doc-2 (via backlog doc update), CHANGELOG Unreleased/Added. Validation: nextest -p harness-config -p harness-providers -p harness-eval 503 passed/9 skipped; harness bin wiring tests 13 passed; harness-agent request_layout 6 passed; defaults_changelog passes (no default changed; new key defaults to previous no-field behaviour); clippy -p harness-providers -p harness-config -D warnings clean (clippy -p harness blocked by another engineer's harness-agent compaction/manager.rs large_enum_variant error, not touched); cargo fmt clean for touched hunks. Live spend: $0.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
OpenAI Responses bodies carry prompt_cache_key (harness- + truncated SHA-256 of instructions+tools: stable per session, shared by subagents with the same prefix). prompt_cache_retention is not sent by default, so the org's own retention applies (24h for non-ZDR orgs on extended-caching models, in_memory for ZDR); opt-in [provider.openai] prompt_cache_retention = 24h sends 24h for EXTENDED_RETENTION_MODELS on API-key requests. ChatGPT mode keeps the key and never sends retention. Verified with unit tests in request.rs/openai.rs/config.rs, a wiring end-to-end test, reviewed golden snapshots (+prompt_cache_key only), nextest 503 passed, clippy clean. Live cache check pending an OpenAI key (TASK-128), noted in docs/providers-openai.md. DoD#4 N/A (no UI).
<!-- SECTION:FINAL_SUMMARY:END -->
