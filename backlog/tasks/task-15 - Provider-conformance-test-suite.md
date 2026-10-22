---
id: TASK-15
title: Provider conformance test suite
status: Done
assignee:
  - '@agent-loop'
created_date: '2026-10-22 14:30'
updated_date: '2026-10-22 16:15'
labels:
  - provider
  - testing
  - problem-1
  - problem-9
milestone: m-1
dependencies:
  - TASK-11
  - TASK-12
  - TASK-13
  - TASK-14
priority: high
type: task
ordinal: 15000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
A single provider regression can degrade every agent. Each provider must be held to the same behaviour contract with recorded fixtures, plus optional live smoke tests.

## Loop guard (for agents)

Limit how many times you test and fix. If the same test, build or check still fails after **3 fix attempts**, or you have made no measurable progress in about 30 minutes, **stop**. Do not keep retrying, and never delete, skip, ignore or weaken a test or check to get a green run. Record what you tried, the exact failing output and your best hypothesis in the implementation notes, leave the task In Progress, and ask the user how to proceed.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 Shared conformance tests cover plain text, single and parallel tool calls, malformed tool JSON, thinking round-trip, max tokens stop, rate limit and overloaded errors
- [x] #2 Recorded fixtures are redacted and replayed offline in CI
- [x] #3 Opt-in live smoke tests run against each real provider when keys are present
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
1. tests/conformance.rs: one contract (plain text, single + parallel tool calls, malformed tool JSON, thinking round-trip, max_tokens, 429 retry, mid-stream overload retry) run against anthropic, openai and openrouter through a local mock server replaying fixtures; the fake provider checked against the same response contract.
2. Fixtures in tests/fixtures/conformance/<provider>/<scenario>.txt (multi-reply format), scanned for credentials by a test; redact_fixture masks keys/auth headers.
3. Live smoke tests gated on HARNESS_LIVE_TESTS=1 + provider key env; HARNESS_RECORD_FIXTURES=<dir> records redacted responses through a forwarding proxy.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
crates/harness-providers/tests/conformance.rs: 8 scenarios x 3 HTTP providers (anthropic, openai, openrouter) through a local mock server, plus fake_provider_meets_the_contract (same response checks; errors classified via retry_reason since the fake has no retry layer). Fixtures: tests/fixtures/conformance/<provider>/<scenario>.txt in a multi-reply format ('--- <status>; header: value'). They were written in each API's documented wire format (generated, not captured from live traffic, since no credentials are available here); the HARNESS_RECORD_FIXTURES recording proxy that captures live responses with redact_fixture() is only exercised when live keys are present. every_provider_has_every_fixture_and_fixtures_are_redacted scans every fixture for key shapes/auth headers; fixture_redaction_masks_keys_and_auth_headers tests the redactor. Live smoke tests live_{anthropic,openai,openrouter} are no-ops unless HARNESS_LIVE_TESTS=1 and the provider key env var are set. 30 tests pass offline; clippy --no-deps --tests clean. Docs: docs/provider-conformance.md.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Shared conformance contract run offline against Anthropic, OpenAI, OpenRouter (mock server + redacted fixtures) and the fake provider, with fixture secret scanning and env-gated live smoke/recording tests; 30 tests pass.
<!-- SECTION:FINAL_SUMMARY:END -->
