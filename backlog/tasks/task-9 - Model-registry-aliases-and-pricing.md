---
id: TASK-9
title: 'Model registry, aliases and pricing'
status: Done
assignee:
  - '@providers-anthropic'
created_date: '2026-10-22 14:30'
updated_date: '2026-10-22 15:21'
labels:
  - provider
  - config
  - problem-6
  - problem-7
milestone: m-1
dependencies:
  - TASK-8
  - TASK-2
documentation:
  - backlog/docs/doc-2 - harness.ini-Reference.md
priority: high
type: feature
ordinal: 9000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Users want to pick the model for each agent explicitly instead of a black-box router (problem 7). Cost must be computed per call (problem 6).

## Loop guard (for agents)

Limit how many times you test and fix. If the same test, build or check still fails after **3 fix attempts**, or you have made no measurable progress in about 30 minutes, **stop**. Do not keep retrying, and never delete, skip, ignore or weaken a test or check to get a green run. Record what you tried, the exact failing output and your best hypothesis in the implementation notes, leave the task In Progress, and ask the user how to proceed.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 Model refs use provider/model syntax and models section aliases resolve to refs
- [x] #2 Built-in table of context windows, capabilities and prices for current Claude, OpenAI and common OpenRouter models
- [x] #3 pricing section overrides built-in prices
- [x] #4 Unknown models work with conservative defaults and a visible warning that cost is unknown
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
1. harness_providers::pricing: Price {input, output, cache_read?, cache_write?} USD/MTok with cost(&Usage), defaults cache_read=0.1x input, cache_write=1.25x input.
2. harness_providers::registry: ModelRef (provider/model parse; openrouter keeps nested vendor/model), ModelInfo (capabilities, ThinkingStyle, builtin price), built-in table (Claude, OpenAI, common OpenRouter), builtin aliases (opus/sonnet/haiku/fable) overridable by [models], price overrides from [pricing] (keys may be refs or aliases), resolve() -> Resolved with price source and warnings; unknown models get conservative caps and a 'cost unknown' warning.
3. No harness-config dependency: the registry takes plain maps; the binary converts Config.models / Config.pricing.
4. Tests for each AC, docs/models.md, rustdoc.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Implemented harness_providers::registry (ModelRef, ThinkingStyle, ModelInfo, Registry, Resolved, PriceSource, RegistryError, BUILTIN_ALIASES, CONSERVATIVE_CAPABILITIES) and harness_providers::pricing (Price, cost, default cache factors 0.1x/1.25x).
Decision: harness-providers does not depend on harness-config; Registry takes plain maps. The binary/agent builds it as Registry::builtin().with_aliases(cfg.models).with_price_overrides(cfg.pricing mapped to Price{input,output,cache_read,cache_write}).
Built-in aliases fable/opus/sonnet/haiku so the default_model=sonnet works without [models]. No new ini keys. DoD#4 N/A (no TUI). Resolved.warnings carries the unknown-model / unknown-cost warnings for the status line (TASK-37) to show.
Validation: cargo test -p harness-providers (registry/pricing unit tests: refs, built-in table coverage, aliases incl. user override, pricing overrides by ref and alias, unknown model defaults + warning), clippy -D warnings clean, rustdoc -D warnings clean. Docs: docs/models.md.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Model registry with provider/model refs, built-in + [models] aliases, built-in Claude/OpenAI/OpenRouter table (context, capabilities, thinking style, prices), [pricing] overrides and conservative defaults with cost-unknown warnings for unknown models. Verified by unit tests in registry.rs/pricing.rs; documented in docs/models.md.
<!-- SECTION:FINAL_SUMMARY:END -->
