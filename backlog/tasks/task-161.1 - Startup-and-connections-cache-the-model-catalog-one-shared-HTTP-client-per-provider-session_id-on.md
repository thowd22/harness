---
id: TASK-161.1
title: >-
  Startup and connections: cache the model catalog, one shared HTTP client per
  provider, session_id on
status: Done
assignee:
  - '@claude-161-1'
created_date: '2026-10-23 21:08'
updated_date: '2026-10-23 22:13'
labels:
  - performance
dependencies: []
parent_task_id: TASK-161
priority: high
ordinal: 169000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Session start fetches OpenRouter's 792 KB /models catalog before the first request on a separate client (wiring.rs:336, openrouter.rs:149,205), costing 0.3-0.8 s per session. Subagents, session summaries and dreaming build fresh providers, each paying a cold connection (core.rs:244, sessions.rs:219, dream.rs:68; cold 25-49 ms vs warm 12-21 ms). Fix: use the on-disk model-list cache (model_list_ttl) for check_agent_model and refresh it in the background; one process-wide reqwest client per (provider, base_url) with per-request auth headers, pool/keepalive tuned (idle ~300 s, TCP and HTTP/2 keepalive), shared by the catalog and chat calls. Turn OpenRouter session_id on by default (stable per session; insurance for gaps over 10 minutes, measured zero cost).

Context: TASK-161 notes (baseline report, web research) and docs/performance.md; measure with harness bench (--fake for overhead, live via OpenRouter Opus 5.5, targeted and budget-capped).
## Loop guard (for agents)

Limit how many times you test and fix. If the same test, build or check still fails after **3 fix attempts**, or you have made no measurable progress in about 30 minutes, **stop**. Do not keep retrying, and never delete, skip, ignore or weaken a test or check to get a green run. Record what you tried, the exact failing output and your best hypothesis in the implementation notes, leave the task In Progress, and ask the user how to proceed.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 Session start no longer waits on a catalog download when a fresh cached list exists; tool-capability checks still reject tool-less models (tests); bench shows the before/after session-start time
- [x] #2 All provider instances for the same (provider, base_url) share one client; subagent, summary and dream calls reuse warm connections (bench/test evidence); credentials never leak across providers
- [x] #3 OpenRouter requests carry a stable session_id by default with an off switch; golden snapshots updated; Defaults CHANGELOG entry
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
1. Baseline: live bench (OpenRouter Opus 5.5, --turns 1, budget-capped, temp HARNESS_CONFIG/HOME/XDG) for session start + --net probe extended with a 'new provider client' column (what a subagent/summary/dream provider pays on its first call).
2. Catalog cache: OpenRouterProvider gets an optional ModelListCache + TTL (wiring passes <data_dir>/model-lists and [general] model_list_ttl). check_agent_model seeds the catalog from the cached list (same base_url); fresh -> no network; stale -> used and refreshed in the background; model missing or rejected by the cached list -> live fetch decides (offline: cached verdict). Live fetches write the cache in the CatalogService format. Tool-less models still rejected (tests on mock server: fresh cache no request, stale cache background refresh, rejection from cache, unknown model refetch).
3. Shared HTTP clients: http.rs keeps a process-wide pool of reqwest clients keyed by (base_url, connect_timeout, tokio runtime id) without default headers; HttpClient applies its own default headers (auth) per request, so credentials stay per provider instance. Tuned: pool idle 300 s, TCP keepalive 30 s, HTTP/2 keepalive 30 s while idle. OpenRouter catalog GET uses the same client. HttpClient::isolated for the bench cold probe. Tests: keep-alive mock counts TCP connections across two clients with different keys (one connection, each request its own key; other base_url separate); wiring test that two provider_sync OpenRouter providers share one connection.
4. session_id default on: [provider.openrouter] session_id = true|false (default true), replacing the unreleased [experimental] openrouter_session_id; golden snapshots carry a fixed session_id; CHANGELOG Defaults entry with fingerprint; docs (performance.md, providers-openrouter.md, harness.ini reference, guide).
5. After bench, fmt/clippy/nextest for touched crates, notes with evidence.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Implemented (2026-10-23, @claude-161-1):
- Catalog cache: OpenRouterProvider::with_model_cache(ModelListCache, ttl) (crates/harness-providers/src/openrouter.rs); wiring::openrouter passes <data_dir>/model-lists + [general] model_list_ttl. check_agent_model: fresh cached list (same base_url) -> no request; stale -> used + background refresh; model missing or tool-less in cache -> live catalog decides (offline: cached verdict stands); every download writes the cache (CatalogService format). ModelInfo::from_live (openrouter/models.rs). The separate catalog reqwest client is gone (HttpClient::get_text on the shared pool).
- Shared HTTP clients: crates/harness-providers/src/http.rs keeps a process-wide reqwest client per (base_url, connect_timeout, tokio runtime id), no default headers; each HttpClient sends its own default headers (auth) per request. Pool idle 300 s, TCP keepalive 30 s, HTTP/2 keepalive 30 s while idle, 10 s timeout. HttpClient::isolated for the bench cold probe; shares_connections_with for tests. Runtime id in the key because a pooled connection's driver task lives on the runtime that opened it (tests use many runtimes).
- session_id: [provider.openrouter] session_id (bool, default on) replaces the pre-release [experimental] openrouter_session_id (removed; an old key now errors as unknown - other subtasks' temp configs must use the provider key). Golden snapshots (coder/researcher/reviewer request) gain only "session_id": "harness-SESSION" (reviewed diff). CHANGELOG Defaults entry (fingerprint unchanged 46b4cecb7b8e, provider keys are not fingerprinted) + Changed entry.
- Bench: net probe gains new_client_ms (first call of a freshly built provider client on the shared pool) and cold_ms now uses an isolated client.
- Docs: docs/performance.md (new section with before/after), docs/providers-openrouter.md, docs/models.md, docs/guide/harness-ini.md, doc-2 harness.ini reference.
Tests: openrouter::tests cached_entries_match_the_live_catalog, fresh_cached_catalog_skips_the_download_and_still_rejects_tool_less_models, cached_rejection_stands_offline_and_unknown_models_are_fetched, stale_or_foreign_cache_is_refreshed_and_downloads_are_cached; tests/shared_pool.rs (keep-alive mock counting TCP connections: two clients with different keys -> 1 connection, each request its own Authorization; other base_url / isolated -> own connections); harness wiring::pool_tests (main provider + 3 provider_sync providers as subagent/summary/dream + next session: 1 /models request, 1 TCP connection for 6 requests; tool-less model rejected with cached catalog); config openrouter_session_id_defaults_on_with_an_off_switch; wiring openrouter_session_id_is_on_by_default_and_stable_per_process.
Bench (live OpenRouter Opus 5.5, debug build, --turns 1 --max-turns 2 --net, one session per process, shared temp data dir): session start before 529 / 153 ms; after: 654 ms first run (no cache yet), then 30.3 / 29.7 ms. New provider client first call: before = cold 28-34 ms; after 12-21 ms (one 75 ms outlier), cold isolated 29-45 ms, warm 11-21 ms. Live spend $0.105 (before $0.0446 in 2 sessions, after $0.0606 in 3).
Checks: nextest -p harness-providers -p harness-eval -p harness-config 504 passed; -p harness 176/177 (tui::tests::jobs_panel_lists_opens_and_stops_background_jobs failed once under load, passed 3/3 alone; known flaky, noted in TASK-161); clippy -D warnings clean for harness, harness-providers, harness-eval, harness-config (--no-deps); fmt clean for touched files (cargo fmt --check reports only other engineers' in-progress files in harness-agent and config.rs tests).
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Session start reads the cached OpenRouter model list instead of downloading the 792 KB catalog (529/153 ms -> ~30 ms with a fresh cache; tool-less models still rejected, live catalog decides on cache misses/rejections); all HttpClients for one base URL share one tuned process-wide connection pool with per-request credentials, so subagent/summary/dream providers start warm (first call 28-34 ms cold -> 12-21 ms); OpenRouter session_id on by default via [provider.openrouter] session_id (golden snapshots + CHANGELOG Defaults). Verified with new unit/integration tests (mock servers counting connections and catalog requests), full crate test suites, clippy, and a budget-capped live bench ($0.105). DoD#4 N/A: no TUI change.
<!-- SECTION:FINAL_SUMMARY:END -->
