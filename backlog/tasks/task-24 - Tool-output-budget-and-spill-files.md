---
id: TASK-24
title: Tool output budget and spill files
status: Done
assignee:
  - '@claude'
created_date: '2026-10-22 14:30'
updated_date: '2026-10-22 15:11'
labels:
  - tools
  - problem-6
milestone: m-2
dependencies:
  - TASK-21
  - TASK-23
  - TASK-4
priority: medium
type: feature
ordinal: 24000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Huge tool outputs waste tokens and push useful context out (problem 6). The full output should remain available on demand.

## Loop guard (for agents)

Limit how many times you test and fix. If the same test, build or check still fails after **3 fix attempts**, or you have made no measurable progress in about 30 minutes, **stop**. Do not keep retrying, and never delete, skip, ignore or weaken a test or check to get a green run. Record what you tried, the exact failing output and your best hypothesis in the implementation notes, leave the task In Progress, and ask the user how to proceed.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 Outputs over a configurable size are stored as blobs and replaced with a summary and pointer
- [x] #2 The model can page the full output with the read tool
- [x] #3 Token savings are recorded in events
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
1. spill.rs: BlobSink trait (put/get, content-addressed 16-hex ids), FileBlobSink, MemoryBlobSink. 2. Spiller::apply: over spill_over_bytes -> store blob, return head/tail with notice + spill:<id> pointer, emit ToolEvent::OutputSpilled(SpillRecord) with byte and token estimates. 3. read accepts spill:<id> paths. 4. Tests.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Validation: cargo test -p harness-tools (50 unit + 1 doctest pass), cargo clippy -p harness-tools --all-targets -- -D warnings clean, cargo doc warning-free. Token savings are emitted as ToolEvent::OutputSpilled(SpillRecord{original_tokens, kept_tokens, tokens_saved()}); the agent loop must forward it into the harness_events run store, and the events blob store should implement harness_tools::spill::BlobSink (FileBlobSink is a working stand-in). DoD#4 N/A.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
harness_tools::spill: BlobSink + FileBlobSink/MemoryBlobSink, Spiller (configurable spill_over_bytes/head/tail lines) replacing large outputs by head/tail + spill:<id> pointer and emitting OutputSpilled events with token savings; read pages spill pointers. Verified by spill::tests, read::tests::pages_spilled_output and bash large-output test.
<!-- SECTION:FINAL_SUMMARY:END -->
