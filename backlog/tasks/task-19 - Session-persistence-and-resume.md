---
id: TASK-19
title: Session persistence and resume
status: Done
assignee:
  - '@agent-loop'
created_date: '2026-10-22 14:30'
updated_date: '2026-10-22 16:15'
labels:
  - core
  - agent
  - problem-4
milestone: m-1
dependencies:
  - TASK-16
  - TASK-7
priority: medium
type: feature
ordinal: 19000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Users lose work when the harness exits mid-task. Sessions must be resumable from the event log with the exact conversation state.

## Loop guard (for agents)

Limit how many times you test and fix. If the same test, build or check still fails after **3 fix attempts**, or you have made no measurable progress in about 30 minutes, **stop**. Do not keep retrying, and never delete, skip, ignore or weaken a test or check to get a green run. Record what you tried, the exact failing output and your best hypothesis in the implementation notes, leave the task In Progress, and ask the user how to proceed.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 Sessions are saved continuously and listed with title, project, time and cost
- [x] #2 Resume rebuilds the provider conversation exactly, including reasoning blocks
- [x] #3 Resuming a session whose files changed on disk warns the user
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
1. harness-agent::session: SessionMeta (title, project, times, agent, model, cost, tokens, runs, file hashes) saved atomically to <data>/sessions/<id>/session.json after every loop run; SessionStore list/load/find by prefix.
2. rebuild_conversation(events, blobs): fold model_call_started (messages_from + messages delta, system), model_call_finished (assistant message incl. reasoning/opaque blocks) and trailing tool_result events, then ensure_tool_results.
3. Resume = new run in the same session (no appends to a possibly torn events.jsonl); compare saved file hashes of every path the conversation read/wrote/edited and warn on drift.
4. Tests: save/list, exact rebuild incl. signed reasoning + opaque blocks and blob-spilled payloads, resumed request equals prior history, drift warning.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
session.rs: SessionMeta/SessionStore (atomic save, list newest first, find by id/prefix/'last', resume), SessionHandle (start/add_run/record after every loop run), rebuild_conversation(events, blobs) (folds messages_from deltas, assistant responses incl. signed reasoning + opaque blocks, trailing tool_result events, blob-spilled payloads; then ensure_tool_results), touched_paths/snapshot_files/file_drift -> FileDrift warnings. Resume starts a new run in the same session, so a torn events.jsonl is never appended to (the events-engineer truncation concern does not arise); Replay.truncated becomes a warning. Tests: src/session/tests.rs (4) and tests/session.rs (5: exact rebuild incl. reasoning/opaque, blob spill, cancel mid-tool, save/list/resume/continue with byte-identical history, drift warning) plus binary test resume_continues_a_saved_session and sessions listing. Docs: docs/sessions.md. The TUI does not yet show Resumed.warnings (no TUI resume flow exists); headless prints them on stderr.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Continuous session metadata, listing (harness sessions), exact conversation rebuild from the event log incl. reasoning blocks, and drift warnings on resume; verified by unit, integration and binary tests.
<!-- SECTION:FINAL_SUMMARY:END -->
