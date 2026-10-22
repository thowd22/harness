---
id: TASK-14
title: Fake scripted provider for deterministic tests
status: Done
assignee:
  - '@provider-core'
created_date: '2026-10-22 14:30'
updated_date: '2026-10-22 15:09'
labels:
  - provider
  - testing
milestone: m-1
dependencies:
  - TASK-8
priority: high
type: feature
ordinal: 14000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
The agent loop, workflows, map view and evals all need deterministic tests without network or cost.

## Loop guard (for agents)

Limit how many times you test and fix. If the same test, build or check still fails after **3 fix attempts**, or you have made no measurable progress in about 30 minutes, **stop**. Do not keep retrying, and never delete, skip, ignore or weaken a test or check to get a green run. Record what you tried, the exact failing output and your best hypothesis in the implementation notes, leave the task In Progress, and ask the user how to proceed.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 Fake provider replays a scripted sequence of responses including tool calls, errors and delays
- [x] #2 Scripts can be written in YAML for use in eval and integration tests
- [x] #3 Fake provider records the requests it received for assertions
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
1. fake.rs: FakeScript/FakeTurn (content blocks, stop, usage, delays, errors) deserializable from YAML.
2. FakeProvider implements Provider, replays turns in order, streams deltas, honours cancellation, records requests.
3. Tests for tool calls, errors, delays (paused tokio time), YAML loading, request recording.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
fake.rs: FakeBlock, FakeErrorKind, FakeError, FakeTurn (delay_ms, chunk_delay_ms, chunk_size, content, stop, usage, error), FakeScript (YAML via serde_norway singleton_map_recursive so blocks are '- text: hi'), FAKE_CAPABILITIES, FakeProvider (Clone shares state; new, with_turns, from_yaml, push, requests, last_request, remaining). Delays use tokio time (start_paused friendly) and honour cancellation. Fixture tests/fixtures/fake_tool_loop.yaml + tests/fake_script.rs. Validation: cargo test -p harness-providers (40 unit + 12 mock-server transport + 1 YAML fixture test, all pass); cargo clippy -p harness-providers --all-targets -- -D warnings clean; cargo doc clean. Docs: docs/providers.md. No UI (DoD#4 N/A).
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Scripted fake provider with YAML scripts, delays, errors, tool calls and request recording; verified by unit and fixture tests.
<!-- SECTION:FINAL_SUMMARY:END -->
