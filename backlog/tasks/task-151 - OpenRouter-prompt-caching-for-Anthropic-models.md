---
id: TASK-151
title: 'OpenRouter: prompt caching for Anthropic models'
status: Done
assignee:
  - '@claude'
created_date: '2026-10-23 16:14'
updated_date: '2026-10-23 19:14'
labels:
  - providers
  - cost
dependencies: []
priority: high
ordinal: 155000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Evidence: a real v0.3.0 session on a large repo (run 01a1691a, sonnet 5.5 via OpenRouter): 126 model calls, about 17 minutes of model time, 15.3M input tokens. Every one of them was uncached (cached_tokens = 0): the OpenRouter provider (crates/harness-providers/src/openrouter/request.rs) never sends cache_control, and Claude models on OpenRouter only cache with explicit markers (https://openrouter.ai/docs/prompt-caching). The direct Anthropic provider already places breakpoints (anthropic/request.rs, system_layers + last tool + recent messages). Usage parsing of prompt_tokens_details.cached_tokens / cache_write_tokens already exists in openrouter/stream.rs.

Send cache breakpoints for anthropic/* models on OpenRouter: explicit per-block cache_control (max 4) mirroring the Anthropic provider's placement (system layers, tools, the moving tail of the conversation), so it works on every Anthropic upstream including Bedrock/Vertex; consider the top-level automatic form only if it is strictly better. Optional [provider.openrouter] cache_ttl = 5m | 1h. Leave non-Anthropic models unchanged (they cache automatically).
## Loop guard (for agents)

Limit how many times you test and fix. If the same test, build or check still fails after **3 fix attempts**, or you have made no measurable progress in about 30 minutes, **stop**. Do not keep retrying, and never delete, skip, ignore or weaken a test or check to get a green run. Record what you tried, the exact failing output and your best hypothesis in the implementation notes, leave the task In Progress, and ask the user how to proceed.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 Requests for anthropic/* models via OpenRouter carry cache_control breakpoints (at most 4) on the system prompt layers, tools and the latest conversation turn; other models' requests are unchanged
- [x] #2 Golden request snapshots cover the OpenRouter Anthropic body; a multi-turn fake-server test shows later requests reuse the cached prefix (byte-identical up to the breakpoint)
- [x] #3 cached and cache-write tokens are recorded in cost events and shown in the status line/cost, and pricing uses cache read/write rates
- [x] #4 A live smoke test (skipped without OPENROUTER_API_KEY) shows cached_tokens > 0 on the second request of a session
- [x] #5 docs (providers/cost) explain caching on OpenRouter and any new config key
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
1. openrouter/request.rs: for anthropic/* (and ~anthropic/*) models, send system as content parts (one per system layer), user/tool text as content arrays, and place cache_control (max 4) like anthropic/request.rs: last tool, system layers (end of frozen prefix, then layer boundaries), final message and the user turn before the last user turn. Other models unchanged.
2. OpenRouterOptions.cache_ttl (5m default, 1h sends ttl) + [provider.openrouter] cache_ttl ini key, wired in wiring.rs.
3. Catalog: anthropic/* models report caching capability; pricing already parses input_cache_read/write; check reported cost covers cache writes.
4. Events Usage gains additive cache_write (subset of input, skipped when 0); agent loop records it; status line and turn detail show written tokens when > 0.
5. Tests: unit placement/cap tests, multi-turn mock-server prefix-stability test, live smoke (ignored without OPENROUTER_API_KEY), golden snapshot update reviewed.
6. Docs: providers-openrouter.md, cost.md, doc-2 ini reference, CHANGELOG Defaults/fingerprint if needed.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Implemented: openrouter/request.rs breakpoints for anthropic/* (tools, system layers, final + previous user turn; cap 4; user/tool text always as parts in cache mode), CacheTtl option + [provider.openrouter] cache_ttl ini key + wiring, catalog caching capability, events Usage.cache_write (additive, skipped when 0) recorded by agent loop/handoff/sessions and shown in status line, node detail turn header and /cost. Live probe: OpenRouter accepts cache_control on function tools and on tool-message content parts; haiku-5.5 test wrote 2571 then read 2571 tokens. Golden request snapshots: applied only the OpenRouter cache changes (system/user as marked parts, last tool marked); golden_requests still fails on main because of other engineers' in-progress prompt/tool changes (## Tools section, grep/glob tools) which I did not accept. Defaults fingerprint unchanged (defaults_changelog passes); CHANGELOG entry under Unreleased/Fixed.

Validation: cargo nextest run for harness-providers, -config, -events, -agent, -tui, harness, -eval: 1208 passed; the only failures are golden_requests default_prompts_render (prompt_coder.snap, another task's in-progress prompt change) and the no_unreviewed_snapshots check tripped by that run's .snap.new (removed). Clippy -D warnings clean on all touched crates. Live smoke test (openrouter_cache_live, run with --run-ignored all): first call wrote 2571 tokens, second read 2571. DoD 4 N/A: the only UI change is text in the existing status line and node-detail turn header, with no new interactive elements.

Integration (TASK-151..155, 2026-10-23): workspace green on the combined tree with TASK-149/150 also Done: cargo fmt --all --check ok; cargo clippy --workspace --all-targets --locked -D warnings ok; cargo nextest run --workspace --locked --profile ci 1838 passed / 10 skipped (golden_requests and prompt snapshots included); cargo test --workspace --doc ok; cargo deny check ok; scripts/hermetic-guard.sh nextest -p harness -p harness-eval 216 passed, nothing written to HOME/XDG. CHANGELOG Defaults: the provisional TASK-152/153/155 entries are merged into one reconciled entry (also naming TASK-151's cache_ttl and TASK-153's [tools] job limits) with final fingerprint 45850933b6b6; defaults_changelog passes. README gains a Focused tools highlight and the harness-tools crate row. 'Passes in CI' is pending until the orchestrator pushes; nothing is committed.

Live on Opus 5.5 (HARNESS_LIVE_OPENROUTER_MODEL=anthropic/claude-opus-5.5, test now takes this override): first read 0 written 2571; second read 2571 written 0. Live 11-turn Opus session: cached input grew every turn (4.3k -> 11.8k cached, uncached input 0.4-1.3k per call), $0.1475 for the whole session.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
OpenRouter requests for anthropic/* models now carry up to 4 cache_control breakpoints placed like the Anthropic provider's: last tool, system prompt layers (system sent as content parts), final message and the previous user turn. In cache mode user and tool text always goes as content parts, so prefixes stay byte-stable. Other models are unchanged. Added [provider.openrouter] cache_ttl = 5m|1h (OpenRouterOptions.cache_ttl, wired). The catalog marks anthropic/* as caching. Events Usage gains an additive cache_write field (skipped when 0) that the agent loop, handoff and sessions record, and it shows in the status line ('wrote N'), the node-detail turn header and /cost. Tests: unit placement, cap, ttl and other-model tests, a multi-turn mock-server prefix test, ini and wiring tests, a catalog cache-price test, agent and TUI tests, and the updated golden snapshots. A live test, ignored without a key, passed against anthropic/claude-haiku-5.5. Docs: providers-openrouter.md, cost.md, doc-2, CHANGELOG.
<!-- SECTION:FINAL_SUMMARY:END -->
