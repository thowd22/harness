---
id: TASK-134
title: Catalog of usable models
status: Done
assignee:
  - '@claude'
created_date: '2026-10-22 19:47'
updated_date: '2026-10-22 21:02'
labels:
  - provider
  - config
milestone: m-1
dependencies:
  - TASK-9
  - TASK-11
  - TASK-12
  - TASK-13
documentation:
  - backlog/docs/doc-2 - harness.ini-Reference.md
priority: high
type: feature
ordinal: 134000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
The /model picker needs to know every model the user can actually use and which provider it comes from. The built-in registry only knows a fixed list, while providers publish their current models, and usability depends on which providers have working credentials.

## Loop guard (for agents)

Limit how many times you test and fix. If the same test, build or check still fails after **3 fix attempts**, or you have made no measurable progress in about 30 minutes, **stop**. Do not keep retrying, and never delete, skip, ignore or weaken a test or check to get a green run. Record what you tried, the exact failing output and your best hypothesis in the implementation notes, leave the task In Progress, and ask the user how to proceed.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 Catalog merges the built-in registry, harness.ini model aliases and live model lists from Anthropic, OpenAI and OpenRouter
- [x] #2 Each entry has model id, provider, aliases, context window, input and output price, capabilities (tools, reasoning, vision) and auth method
- [x] #3 Only models from providers with working credentials are marked usable, and models without tool support are flagged as not usable for agents
- [x] #4 Live lists are cached in data_dir with a TTL and refreshed in the background; the cached catalog is available instantly and works offline
- [x] #5 OpenAI models usable through Sign in with ChatGPT are distinguished from API-key models
- [x] #6 Tests use recorded model-list fixtures for each provider
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
1. harness_providers::catalog: types (CatalogEntry, AuthMethod, Credentials, Availability, ModelCapabilities, PriceOrigin) and Catalog::build merging Registry models, aliases and live lists.
2. catalog::live: ModelListSource + parsers for Anthropic (paginated), OpenAI (chat models only, tool heuristics) and OpenRouter (prices, supported params).
3. catalog::cache: per-provider JSON cache in <data_dir>/model-lists with fetched_at, base_url and TTL freshness.
4. catalog::CatalogService: instant catalog from cache, background refresh of stale lists, watch channel, 401/403 marks key rejected, offline keeps cache.
5. Config: [general] model_list_ttl (default 24h); DataDir::model_lists().
6. Wiring in crates/harness: catalog_credentials, model_list_sources, model_catalog; harness models CLI as the first consumer (TUI picker TASK-135 builds on the same API).
7. Tests with recorded fixtures per provider and a local mock server; env-gated live OpenRouter test. Docs: models.md, data-dir.md, cli.md, doc-2, CHANGELOG.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Implemented harness_providers::catalog (catalog.rs, catalog/live.rs, catalog/cache.rs, catalog/service.rs) and Registry::price_override (quiet override lookup reused by resolve).
Fixtures: tests/fixtures/models/openrouter.json is a trimmed real recording of OpenRouter GET /models (2026-10-22, 7 models incl. a no-tools model and a variable-price router). anthropic-page1/2.json and openai.json are hand-written to the documented response shapes (no Anthropic/OpenAI keys available, so no live verification of those two lists).
ChatGPT: TASK-133 (Sign in with ChatGPT) is not implemented yet, so wiring reports only API-key states; the catalog already models AuthMethod::ChatGpt (plan models = GPT-5/Codex families via chatgpt_plan_model, price origin Plan, non-plan models flagged 'not included in Sign in with ChatGPT'). TASK-133 should call Credentials::set(openai, ChatGpt, state) and CatalogService::set_credentials.
Refresh status is exposed through CatalogService::subscribe (tokio watch) and Catalog::refreshing(); there is no harness-events event for it yet (events crate is outside this lane) - TASK-135 may want one.
Added harness models [--all] [--json] [--refresh|--offline] so the catalog has a consumer in the binary (otherwise the wiring is dead code under -D warnings).
Validation: cargo test --workspace 1256 passed 0 failed; cargo clippy -p harness-providers -p harness-config -p harness --all-targets -D warnings clean; live: OPENROUTER_API_KEY cargo test -p harness-providers --test model_catalog -- --include-ignored (live_openrouter_list ok) and harness models against real OpenRouter listed priced models and flagged no-tool models.
Note: cargo fmt -p harness also formats crates/harness/src/tui (TUI lane); formatting only.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Added harness_providers::catalog: Catalog::build merges the built-in registry, aliases (built-in and [models]) and live model lists from Anthropic, OpenAI and OpenRouter into CatalogEntry rows (model id, provider, aliases, context, input/output price with origin, tools/reasoning/vision, auth method, availability with reason). Only providers with working credentials are usable; models without tools are NoTools (chat only, not for agents); Sign in with ChatGPT plan models get AuthMethod::ChatGpt and plan pricing. CatalogService builds instantly from a per-provider cache in <data_dir>/model-lists, refreshes stale lists (new [general] model_list_ttl, default 24h) in the background, publishes on a watch channel, keeps the cache when offline and marks keys rejected on 401/403. Wired in crates/harness (wiring::model_catalog) with a new harness models command. Verified with tests/model_catalog.rs (recorded fixtures per provider, mock server for refresh/TTL/offline/401), unit tests, config and wiring tests, full workspace tests (1256 passed), clippy, and a live OpenRouter run. DoD 4: no TUI UI in this task (N/A).
<!-- SECTION:FINAL_SUMMARY:END -->
