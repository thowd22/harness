---
id: TASK-167
title: Live model list for Sign in with ChatGPT
status: Done
assignee:
  - '@claude'
created_date: '2026-10-28 00:02'
updated_date: '2026-10-28 00:24'
labels: []
dependencies: []
priority: high
ordinal: 181000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
With only Sign in with ChatGPT configured, the model catalog shows four built-in models (gpt-5, gpt-5-mini, gpt-5-nano, gpt-5.1). Newer plan models that Codex shows (for example GPT-5.6 Sol and the Astra models) are missing: the OpenAI live list is fetched only with an API key, and plan access is guessed from a gpt-5/codex name prefix. OpenAI documents the account-specific list for these tokens (https://developers.openai.com/siwc/token-sharing-open-source/models-and-inference): GET https://api.openai.com/v1/models with the ChatGPT access token returns a `models` array with slug, display_name and visibility; show visibility "list" entries in server order, display display_name, send slug. The user also asked to confirm the OpenRouter and Anthropic catalogs are fetched live.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 With auth = chatgpt, the catalog fetches the account model list with the ChatGPT access token (refreshing it when needed), parses the `models` array, keeps visibility "list" entries in server order and shows display_name
- [x] #2 Plan availability comes from that list when it is available; the name-prefix rule is only the fallback when the list cannot be fetched, and the built-in models remain as a fallback
- [x] #3 Models unknown to the built-in table are usable with conservative limits and plan pricing, and requests send the slug
- [x] #4 The list is cached like other live lists and refreshed on sign-in and when the ChatGPT account changes; the token never appears in logs, errors or the cache
- [x] #5 Offline tests cover parsing, filtering, ordering, fallback and token handling against a mock endpoint; a live check against the signed-in account lists the plan models
- [x] #6 The OpenRouter and Anthropic catalogs are verified to fetch live model lists, and docs (providers-openai.md, models.md) describe the ChatGPT list
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
1. Probe the real endpoint with the signed-in token (done: GET api.openai.com/v1/models -> {"models":[...]} with slug, display_name, visibility, context_window, input_modalities, supported_reasoning_levels, supported_in_api).
2. live.rs: ListKind::ChatGpt, ModelListSource::chatgpt(auth, base, account) fetching a fresh token per refresh; origin = base#chatgpt:<sha256(sub)[..6]> so another account is stale; parse_chatgpt keeps visibility list + supported_in_api, server order as rank, plan=true.
3. catalog.rs: plan availability from the account list when present (name rule fallback, now gpt-5+ via openai::is_gpt5_or_later); server-ordered entries first; live_model_info for runtime facts.
4. registry.rs: with_live_models (built-in and OpenRouter vendor fallback win); wiring::registry loads all cached lists; wiring builds the ChatGPT source; login drops the cached openai list after ChatGPT sign-in.
5. service.rs: a refused ChatGPT list marks the ChatGPT sign-in (not an API key) as rejected.
6. Tests, docs, CHANGELOG; live checks for ChatGPT, Anthropic, OpenRouter.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Probe 2026-10-28 (user account): 10 models, 8 visibility=list (gpt-6.1-sol, gpt-6-astra, gpt-6.1-terra, gpt-6-sol, gpt-6-luna, gpt-5.6-sol, gpt-5.6-terra, gpt-5.6-luna), 2 hide (gpt-reserve, codex-auto-review). None of the built-in gpt-5/gpt-5-mini/gpt-5-nano/gpt-5.1 are listed. Models also report reasoning levels xhigh/max/ultra beyond harness low/medium/high (not in scope; possible follow-up).
Found and fixed: unknown models (any provider) ran with CONSERVATIVE_CAPABILITIES (32k ctx, no thinking/images) even when a live list described them; runtime registry now learns them from the cached lists. gpt-6* was not treated as a reasoning model (no reasoning effort sent); now gpt-5 and later are.
Live checks with the dev build: harness models --refresh with the real config lists the 8 plan models in server order with 500k/272k context and ChatGPT auth; OpenRouter live list 464 models (fresh); Anthropic (isolated config, test key) 14 models. harness run --model openai/gpt-6-astra over the ChatGPT plan: read tool call, correct answer, $0, no conservative-defaults warning.
Gates: fmt, clippy -D warnings, nextest 2146 passed / 24 skipped (opt-in live), doctests, hermetic + bench guards.

CI run 40607673704 on 5226547: all jobs green (Linux, macOS). DoD #4 N/A: no new interactive UI; the existing /model picker shows the new entries.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Sign in with ChatGPT now lists the account plan models from OpenAI (GET /v1/models with the ChatGPT token): visibility "list" entries in server order with display names, cached per account and refetched after sign-in; plan availability comes from that list with the gpt-5+/codex name rule as fallback. Runs on models only a live list knows (ChatGPT, Anthropic, OpenRouter) now use the list facts instead of 32k conservative defaults; gpt-6+ treated like gpt-5. Verified with offline tests against a mock endpoint, live refreshes (ChatGPT 8 plan models, OpenRouter 464, Anthropic 14) and a real gpt-6-astra run over the plan; CI green.
<!-- SECTION:FINAL_SUMMARY:END -->
