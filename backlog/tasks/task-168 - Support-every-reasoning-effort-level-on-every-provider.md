---
id: TASK-168
title: Support every reasoning effort level on every provider
status: Done
assignee:
  - '@claude'
created_date: '2026-10-28 03:14'
updated_date: '2026-10-29 18:08'
labels:
  - providers
  - tui
dependencies: []
priority: high
ordinal: 182000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
harness only knows low/medium/high, but providers now accept more: Claude 4.7+ take xhigh and max (4.6 takes max), OpenAI API gpt-5 takes minimal and newer models xhigh, OpenRouter takes minimal and xhigh, and the ChatGPT plan models report low/medium/high/xhigh/max/ultra in supported_reasoning_levels. Users cannot pick them in /effort or agent YAML. Levels differ per model, so a level a model does not accept must be sent as the nearest one it does instead of failing the request.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 Agent YAML effort and /effort accept minimal, low, medium, high, xhigh, max and ultra
- [x] #2 Each model knows the levels it accepts: from the live list where it says (ChatGPT supported_reasoning_levels, Anthropic capabilities.effort), else a built-in per-model or per-provider default
- [x] #3 A level the model does not accept is sent as the nearest accepted level (the highest one below it, else the lowest above), for the main agent and subagents
- [x] #4 Anthropic sends low..max as output_config.effort on adaptive models and maps every level to a thinking budget on budget models; OpenAI and ChatGPT send the level as reasoning.effort; OpenRouter sends minimal..xhigh
- [x] #5 /effort lists the levels the current model accepts and says when the chosen level is sent as another
- [x] #6 Cached live lists from before the change are refreshed so new effort facts arrive without a manual refresh
- [x] #7 Unit tests cover parsing, nearest-level mapping and each provider's request body; live checks confirm Anthropic xhigh/max and a ChatGPT plan model at xhigh or above
- [x] #8 docs and CHANGELOG describe the levels
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 Tests: automated tests cover every acceptance criterion (unit and/or integration as appropriate) and pass locally and in CI; no existing test is deleted, skipped, ignored or weakened to get a green run
- [ ] #2 Docs: user-facing behaviour is documented (docs/ and, if the design changed, the relevant backlog doc), public APIs have rustdoc, and any new config keys appear in the harness.ini reference
- [ ] #3 Loop guard respected: no more than 3 fix attempts per failing test or check; if still failing, stop, record attempts, output and hypothesis in implementation notes, and ask the user instead of looping
- [ ] #4 UI interaction: if this task makes anything visible in the TUI, every visible interactive element is clickable with the mouse and fully operable with the keyboard (arrows/Tab to focus, Enter to activate, Esc to back out), with tests for both input paths; tasks with no visible UI mark this N/A
<!-- DOD:END -->

## Implementation Plan

<!-- SECTION:PLAN:BEGIN -->
1. ReasoningEffort and config Effort: Minimal, Low, Medium, High, XHigh, Max, Ultra (wire names, ordering, nearest())
2. ModelInfo.efforts: built-in per-model sets; live lists carry efforts (ChatGPT levels, Anthropic capabilities.effort); defaults per provider; list origins tagged so old caches refresh
3. Providers: Anthropic (adaptive clamp via profile, budgets for all levels), OpenAI/ChatGPT (registry clamp, any level string), OpenRouter (clamp to minimal..xhigh)
4. One Effort->ReasoningEffort mapping used by wiring and subagents
5. /effort: all choices, buttons for accepted levels, 'sent as' note; status line
6. Tests, docs, CHANGELOG; live checks Anthropic + ChatGPT
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Implemented: ReasoningEffort/Effort gain minimal, xhigh, max, ultra with nearest(); ModelInfo.efforts (builtin defaults per provider/family, live ChatGPT supported_reasoning_levels and Anthropic capabilities.effort); providers clamp themselves (Anthropic profile.efforts incl. budget models, OpenAI via registry, OpenRouter to minimal..xhigh) so main agent, subagents and prewarm all agree; /effort shows accepted levels and 'sent as'; list origins tagged #facts:2 so old caches refetch. Tests: workspace 2153 pass, clippy/fmt clean. Live Anthropic (claude-haiku-5-5 all 7 levels, sonnet-5-5 xhigh, opus-5-5 max, sonnet-4-6 xhigh->high, haiku-4-5 max budget) all accepted. ChatGPT live check pending: the machine's ChatGPT sign-in was cleared at 00:15 UTC (tokens null), so no plan request possible until the user signs in again. Not committed yet.

Live ChatGPT plan checks (signed in again, 2026-10-29): gpt-6-astra xhigh ok, gpt-6-luna ultra (sent as max) ok, gpt-6.1-sol minimal ok, gpt-6.1-sol ultra first failed with HTTP 400 "Invalid value: ultra. Supported values are: none, minimal, low, medium, high, xhigh, and max" although supported_reasoning_levels lists ultra. Fix: parse_chatgpt drops ultra (sent as max), FACTS_TAG bumped to #facts:3 so cached lists with ultra are refetched; tests/docs/CHANGELOG updated. Rerun: gpt-6.1-sol ultra (sent as max) ok.
Also fixed: login::tests::logout_asks_and_removes_all_credential_keys used the default data dir and deleted the real ChatGPT tokens (the 2026-10-28 00:15 sign-out); it now has its own data_dir. Reproduced with seeded fake tokens in a sandbox HOME before and after.
Validation: fmt, clippy -D warnings, nextest workspace 2153 passed / 24 skipped, doc tests, hermetic-guard and bench-guard ok; real token file intact after the run. Not committed yet.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Every effort level (minimal..ultra) works on every provider: levels per model come from live lists (Anthropic capabilities.effort, ChatGPT supported_reasoning_levels minus ultra, which the Responses API rejects) or per-family defaults, and an unsupported level is sent as the nearest accepted one for the main agent and subagents; /effort shows the levels and the sent level. Verified with unit tests, live Anthropic runs (all levels, xhigh/max, 4.6 clamping, Haiku 4.5 budgets) and live ChatGPT plan runs (astra xhigh, luna ultra→max, sol ultra→max, sol minimal). CI green on 3628d53.
<!-- SECTION:FINAL_SUMMARY:END -->
