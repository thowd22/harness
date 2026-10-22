---
id: TASK-124
title: Timestamps and per-turn metadata
status: Done
assignee:
  - '@tui-views'
created_date: '2026-10-22 14:43'
updated_date: '2026-10-22 19:17'
labels:
  - tui
  - visibility
  - problem-6
milestone: m-3
dependencies:
  - TASK-34
  - TASK-39
  - TASK-9
documentation:
  - backlog/docs/doc-7 - Visual-Design-Dracula-Theme-Indicators-Tool-Cards.md
  - backlog/docs/doc-2 - harness.ini-Reference.md
priority: medium
type: feature
ordinal: 124000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Detailed, informative output means the user can always tell when something happened, how long it took and what it cost.

## Loop guard (for agents)

Limit how many times you test and fix. If the same test, build or check still fails after **3 fix attempts**, or you have made no measurable progress in about 30 minutes, **stop**. Do not keep retrying, and never delete, skip, ignore or weaken a test or check to get a green run. Record what you tried, the exact failing output and your best hypothesis in the implementation notes, leave the task In Progress, and ask the user how to proceed.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 User messages, assistant turns, tool cards and event cards show a right-aligned muted timestamp
- [x] #2 ui timestamps supports time, relative and off
- [x] #3 Each assistant turn ends with a metadata line: model, input tokens with cache percentage, output and reasoning tokens, cost, duration and stop reason
- [x] #4 Timestamps come from the event log so live view and replay show identical times
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
1. chat::timefmt::TimeFormat (time/relative/off, fixed UtcOffset). 2. Right-aligned muted timestamps on user, assistant, notice and subagent entries and on tool cards / groups. 3. TurnMeta metadata line from ModelCallFinished + Cost. 4. All times from RunEvent.ts; live vs replay test.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Implemented in crates/harness-tui/src/chat/{timefmt,transcript}.rs and cards.rs. Tests: timefmt::time_relative_and_off, transcript timestamp_modes, assistant_turn_streams_markdown_and_ends_with_metadata (exact metadata line 'sonnet · ↑12.4k (cache 96%) ↓812 · reasoning 1.2k · $0.021 · 4.2s · end_turn'), live_and_replay_show_identical_times (same events, wall clocks 30h apart, identical output), header test in cards. REMAINING (AC#4): every logged entity takes its time from RunEvent.ts, but user messages are not in the event log (harness-events has no user-message EventKind), so their timestamp is the submit time passed to Transcript::push_user and they are absent from replays. Needs harness-events to add e.g. EventKind::UserMessage{text} emitted by the agent loop; then Transcript::apply should create user entries from it (a few lines) and AC#4/DoD#1 can be checked.

Integration: EventKind::UserMessage emitted by the agent loop; the transcript takes user entries from the log so live and replay times match. Tests: transcript live_and_replay_show_identical_times and user message tests, agent_loop sequence test.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Integration: EventKind::UserMessage emitted by the agent loop; the transcript takes user entries from the log so live and replay times match. Tests: transcript live_and_replay_show_identical_times and user message tests, agent_loop sequence test. Workspace green: cargo fmt --check, clippy -D warnings, cargo test --workspace (1142 passed, 0 failed).
<!-- SECTION:FINAL_SUMMARY:END -->
