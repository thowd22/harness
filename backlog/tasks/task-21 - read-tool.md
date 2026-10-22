---
id: TASK-21
title: read tool
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
  - TASK-20
priority: high
type: feature
ordinal: 21000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Reading is the most frequent operation, so it must be cheap in tokens and safe on huge or binary files.

## Loop guard (for agents)

Limit how many times you test and fix. If the same test, build or check still fails after **3 fix attempts**, or you have made no measurable progress in about 30 minutes, **stop**. Do not keep retrying, and never delete, skip, ignore or weaken a test or check to get a green run. Record what you tried, the exact failing output and your best hypothesis in the implementation notes, leave the task In Progress, and ask the user how to proceed.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 Supports offset and limit, with line numbers in output
- [x] #2 Caps output size and says how to read further
- [x] #3 Detects binary files, reads images for providers that support them
- [x] #4 Records a content hash used by edit for stale-read detection
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
1. tool.rs: shared ToolContext (call_id, supports_images, event sink), ToolOutput (Text|Image), ToolError with kind + recovery message, ToolEvent/ToolEventSink. 2. tracker.rs: ReadTracker keyed by resolved path storing sha256 of full file. 3. config.rs: ToolsConfig limits ([tools] keys). 4. read.rs: offset/limit with cat -n style numbering, line/byte caps with continuation hint, binary detection (NUL/invalid UTF-8), image magic sniffing returning ToolOutput::Image when ctx.supports_images, hash recorded on every workspace read. 5. Tests per AC.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Validation: cargo test -p harness-tools (50 unit + 1 doctest pass), cargo clippy -p harness-tools --all-targets -- -D warnings clean, cargo doc warning-free. Image support is per call via ToolContext::supports_images; ToolOutput::Image carries raw bytes (provider encodes). New [tools] read_* keys added to doc-2 and docs/tools.md. DoD#4 N/A.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
harness_tools::read::ReadTool: cat -n numbering, offset/limit, line/byte/line-length caps with an 'offset=N' continuation hint, size limit, binary detection, PNG/JPEG/GIF/WebP returned as images when the model supports them, sha256 of the full file recorded in ReadTracker on every read; also pages spill:<id> pointers. Verified by read::tests.
<!-- SECTION:FINAL_SUMMARY:END -->
