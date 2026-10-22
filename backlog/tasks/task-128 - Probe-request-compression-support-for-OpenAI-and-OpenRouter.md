---
id: TASK-128
title: Probe request-compression support for OpenAI and OpenRouter
status: In Progress
assignee:
  - '@claude'
created_date: '2026-10-22 19:32'
updated_date: '2026-10-22 22:55'
labels:
  - provider
  - spike
  - problem-6
milestone: m-1
dependencies:
  - TASK-12
  - TASK-13
  - TASK-127
references:
  - 'https://github.com/anthropics/claude-code/issues/102929'
priority: low
type: spike
ordinal: 128000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
The Anthropic gzip win may also apply to OpenAI and OpenRouter, but their support for compressed request bodies is undocumented. Turning it on blind could break requests, so support must be measured before enabling anything.

## Loop guard (for agents)

Limit how many times you test and fix. If the same test, build or check still fails after **3 fix attempts**, or you have made no measurable progress in about 30 minutes, **stop**. Do not keep retrying, and never delete, skip, ignore or weaken a test or check to get a green run. Record what you tried, the exact failing output and your best hypothesis in the implementation notes, leave the task In Progress, and ask the user how to proceed.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 Opt-in live probe sends gzip and zstd encoded count or small completion requests to each provider and records accepted encodings
- [x] #2 Findings and the decision (enable, leave off) are recorded in the task notes and docs/providers.md
- [ ] #3 If a provider accepts gzip, a follow-up task is created to enable it using the same fallback design as the Anthropic task
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
1. Add harness_providers::probe: Encoding {Identity,Gzip,Zstd}, ProbeTarget (openai count, openrouter 1-token completion), run(), classify(), ProbeReport::{accepted,decision,needs_follow_up,render}.
2. Offline tests against local mock servers that decode or ignore Content-Encoding; opt-in ignored live tests gated on HARNESS_LIVE_TESTS=1 + key.
3. Run live probe on OpenRouter (only key available); record findings in notes and docs/providers.md.
4. OpenAI: cannot probe without key; leave task In Progress for that half.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Live probe 2026-10-22 against OpenRouter (HARNESS_LIVE_TESTS=1 cargo test -p harness-providers --test compression_probe -- --ignored --nocapture), mistralai/mistral-nemo, max_tokens 1:
  identity 112 bytes accepted
  gzip     114 bytes rejected 400: JSON parsing failed
  zstd     102 bytes rejected 400: JSON parsing failed
  decision: LeaveOff
Confirmed independently with curl (same 400 for both encodings). OpenRouter ignores Content-Encoding on requests and parses the compressed bytes as JSON. Decision: leave off for OpenRouter; no follow-up task (gzip not accepted).

OpenAI: NOT probed live. No OPENAI_API_KEY is available in this environment; an unauthenticated POST /v1/responses/input_tokens returns 401 'Missing bearer or basic authentication' for both raw and gzip bodies, so auth is checked before the body and that tells us nothing. Decision for OpenAI: leave off until probed. To finish: OPENAI_API_KEY=... HARNESS_LIVE_TESTS=1 cargo test -p harness-providers --test compression_probe live_probe_openai -- --ignored --nocapture, then update the findings table in docs/providers.md and, if gzip is accepted, create the follow-up task (reuse RequestCompression + GzipLatch, same fallback as TASK-127). The OpenAI count endpoint path responses/input_tokens is also unverified with a key.

Added zstd.workspace = true to harness-providers (already linked into the binary via harness-remote).

Status: In Progress. AC#1 is met for OpenRouter only. The OpenAI live run is still pending. AC#3 is open until OpenAI has been probed; OpenRouter needs no follow-up.

Review follow-up 2026-10-22: reviewer finding confirmed. OPENAI_API_KEY is still absent (env check), and live_probe_openai only prints 'skipping live probe: OPENAI_API_KEY is not set' and returns, so its 'ok' result verifies nothing. No code change can close AC#1/AC#3 for OpenAI. Offline tests: cargo test -p harness-providers --test compression_probe -> 5 passed, 2 ignored. Task stays In Progress until someone with a key runs the OpenAI probe (command above).

Integration audit 2026-10-22: status stays In Progress. AC#1 and AC#3 still need a live OpenAI probe, and no OPENAI_API_KEY exists in this environment (only OPENROUTER_API_KEY). To finish: OPENAI_API_KEY=... HARNESS_LIVE_TESTS=1 cargo test -p harness-providers --test compression_probe live_probe_openai -- --ignored --nocapture. Then record the accepted encodings in docs/providers.md and, if gzip is accepted, ask the user before creating the follow-up task. Offline workspace suite is green.
<!-- SECTION:NOTES:END -->
